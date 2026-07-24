//! `atelier doctor` — self-diagnostics for the whole setup: the binary, the
//! document store, and the installed skills — plus the optional MCP add-on
//! (the background daemon and per-client MCP registrations). One aligned row
//! per check (`ok` / `note` / `FAIL`), exit 1 when anything FAILs, and every
//! FAIL row prints its fix. The CLI needs none of the MCP machinery, so an
//! absent registration is a note, never a failure.
//!
//! The one network-ish check (probing the local daemon) is a hand-written
//! HTTP/1.1 POST over `std::net::TcpStream` — localhost only, so the default
//! build still links no HTTP client.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::Duration;

use atelier_studio::{HomeOrigin, Studio};

use crate::{service, skills};

/// The default daemon endpoint (`service::DEFAULT_BIND`).
const DEFAULT_ADDR: &str = service::DEFAULT_BIND;

/// How a check came out: `Ok` = verified good, `Note` = informational (nothing
/// to fix — e.g. a client that isn't installed), `Fail` = broken, with a fix.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Level {
    Ok,
    Note,
    Fail,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Note => "note",
            Self::Fail => "FAIL",
        }
    }
}

/// One printed line: `{level:<5} {name:<width} — {detail}`.
struct Row {
    level: Level,
    name: String,
    detail: String,
}

impl Row {
    fn at(level: Level, name: impl Into<String>, detail: impl Into<String>) -> Row {
        Row {
            level,
            name: name.into(),
            detail: detail.into(),
        }
    }
    fn ok(name: impl Into<String>, detail: impl Into<String>) -> Row {
        Self::at(Level::Ok, name, detail)
    }
    fn note(name: impl Into<String>, detail: impl Into<String>) -> Row {
        Self::at(Level::Note, name, detail)
    }
    fn fail(name: impl Into<String>, detail: impl Into<String>) -> Row {
        Self::at(Level::Fail, name, detail)
    }
}

/// Entry point for `atelier doctor`. Returns a process exit code.
pub fn run(args: &[String]) -> i32 {
    if !args.is_empty() {
        eprintln!("atelier doctor: takes no arguments — it checks everything");
        return 2;
    }
    let mut rows = vec![check_binary(), check_store(), check_daemon()];
    rows.extend(check_clients());
    rows.extend(check_skills());

    let width = rows.iter().map(|r| r.name.len()).max().unwrap_or(0);
    let failed = rows.iter().any(|r| r.level == Level::Fail);
    for r in &rows {
        println!(
            "{:<5} {:width$} — {}",
            r.level.label(),
            r.name,
            r.detail,
            width = width
        );
    }
    if failed {
        eprintln!("\ndoctor: failures above — each FAIL row names its fix");
        1
    } else {
        0
    }
}

/// "~"-collapse a path under the user's home for display (the CLI talks about
/// `~/.atelier`, not an absolute machine-specific path).
fn tilde(p: &Path) -> String {
    match service::home() {
        Some(h) if p.starts_with(&h) => {
            format!("~/{}", p.strip_prefix(&h).unwrap_or(p).display())
        }
        _ => p.display().to_string(),
    }
}

// -- 1. binary (informational) ------------------------------------------------

fn check_binary() -> Row {
    Row::note("binary", format!("atelier {}", env!("CARGO_PKG_VERSION")))
}

// -- 2. store -------------------------------------------------------------------

fn check_store() -> Row {
    let (home, origin) = Studio::default_home_with_origin();
    let docs = home.join("documents");
    if let Err(e) = std::fs::create_dir_all(&docs) {
        return Row::fail(
            "store",
            format!(
                "cannot create {}: {e} — fix: create that directory, or point ATELIER_HOME elsewhere",
                docs.display()
            ),
        );
    }
    // Same writability proof the store itself needs: create then remove a
    // probe file beside the documents.
    let probe = docs.join(".doctor-probe");
    let writable = std::fs::write(&probe, b"doctor").and_then(|_| std::fs::remove_file(&probe));
    if let Err(e) = writable {
        return Row::fail(
            "store",
            format!(
                "{} is not writable: {e} — fix: check permissions/ownership on that directory",
                docs.display()
            ),
        );
    }
    let scope = match origin {
        HomeOrigin::Env => "ATELIER_HOME",
        HomeOrigin::Project => "project store",
        HomeOrigin::Global => "global",
    };
    let count = Studio::new().list_docs()["count"].as_u64().unwrap_or(0);
    Row::ok(
        "store",
        format!("{} ({count} documents, {scope})", tilde(&home)),
    )
}

// -- 3. daemon ------------------------------------------------------------------

/// POST an MCP `initialize` to the local daemon over a raw TcpStream and check
/// the answer. Thin OS glue (untested, like service.rs's launchd glue) — the
/// verdict lives in the pure [`initialize_ok`].
fn probe_initialize(addr: &str) -> bool {
    let sa = addr
        .to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
        .unwrap_or_else(|| ([127, 0, 0, 1], 8765).into());
    let mut stream = match TcpStream::connect_timeout(&sa, Duration::from_secs(2)) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"doctor","version":"0"}}}"#;
    // `connection: close` so read_to_string ends at the response instead of
    // blocking on keep-alive. Chunked framing is fine: the substring check
    // reads through it.
    let request = format!(
        "POST /mcp HTTP/1.1\r\nhost: {addr}\r\ncontent-type: application/json\r\naccept: application/json, text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut resp = String::new();
    if stream.read_to_string(&mut resp).is_err() {
        return false;
    }
    let status = resp
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    let body = resp.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
    initialize_ok(status, body)
}

/// The daemon answered an MCP initialize: HTTP 200 and a JSON-RPC result that
/// carries `serverInfo`. Anything else (405, a dead socket's RST page, a 200
/// from some other service) is not an atelier daemon.
fn initialize_ok(status: u16, body: &str) -> bool {
    status == 200 && body.contains("\"serverInfo\"")
}

/// Bare TCP connect — something is accepting at `addr`.
fn tcp_listening(addr: &str) -> bool {
    addr.to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
        .is_some_and(|sa| TcpStream::connect_timeout(&sa, Duration::from_millis(500)).is_ok())
}

fn check_daemon() -> Row {
    let installed = service::daemon_installed();
    let running = service::daemon_running();
    let addr = std::env::var("ATELIER_HTTP").unwrap_or_else(|_| DEFAULT_ADDR.into());
    if !installed && !tcp_listening(&addr) {
        return Row::note(
            "daemon",
            "not installed (MCP add-on; the CLI needs no daemon)",
        );
    }
    let state = match (installed, running) {
        (true, true) => format!("installed + running ({})", service::manager()),
        (true, false) => format!("installed but not running ({})", service::manager()),
        (false, true) => format!("running ({}) but no unit file", service::manager()),
        (false, false) => format!("listening at {addr} (not installed as a daemon)"),
    };
    if probe_initialize(&addr) {
        Row::ok("daemon", format!("{state}; MCP initialize ok at {addr}"))
    } else if installed {
        let logs = service::default_home().join("logs");
        Row::fail(
            "daemon",
            format!(
                "{state}; MCP probe failed at {addr} — fix: atelier uninstall && atelier install (logs: {})",
                tilde(&logs)
            ),
        )
    } else {
        Row::note(
            "daemon",
            format!("something else listens at {addr} but is not an atelier MCP server"),
        )
    }
}

// -- 4. client registrations ----------------------------------------------------

/// What an agent's MCP config says about atelier.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Registration {
    /// http shape: `{"url": "..."}`.
    Http(String),
    /// stdio shape: `{"command": "..."}`.
    Stdio(String),
    /// No `mcpServers.atelier` entry.
    Absent,
    /// The file doesn't parse, or the entry is neither url nor command shape.
    Malformed,
}

#[derive(Clone, Copy)]
enum RegistrationFormat {
    Json,
    Toml,
}

/// Parse one agent config body for its `mcpServers.atelier` entry.
fn registration(body: &str) -> Registration {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Registration::Malformed;
    };
    let Some(entry) = v.get("mcpServers").and_then(|m| m.get("atelier")) else {
        return Registration::Absent;
    };
    if let Some(url) = entry.get("url").and_then(|u| u.as_str()) {
        Registration::Http(url.to_string())
    } else if let Some(cmd) = entry.get("command").and_then(|c| c.as_str()) {
        Registration::Stdio(cmd.to_string())
    } else {
        Registration::Malformed
    }
}

/// Codex keeps the equivalent registration in TOML under
/// `[mcp_servers.atelier]`.
fn codex_registration(body: &str) -> Registration {
    let Ok(document) = body.parse::<toml_edit::DocumentMut>() else {
        return Registration::Malformed;
    };
    let Some(servers) = document.get("mcp_servers") else {
        return Registration::Absent;
    };
    let Some(servers) = servers.as_table_like() else {
        return Registration::Malformed;
    };
    let Some(entry) = servers.get("atelier") else {
        return Registration::Absent;
    };
    let Some(entry) = entry.as_table_like() else {
        return Registration::Malformed;
    };
    let url = entry.get("url").and_then(toml_edit::Item::as_str);
    let command = entry.get("command").and_then(toml_edit::Item::as_str);
    match (url, command) {
        (Some(url), None) => Registration::Http(url.to_string()),
        (None, Some(command)) => Registration::Stdio(command.to_string()),
        _ => Registration::Malformed,
    }
}

/// Project-scoped atelier entries: Claude Code also registers MCP servers per
/// project under `projects.<path>.mcpServers` in ~/.claude.json — an entry
/// there works only inside that directory, so it is not the user-scope
/// registration `registration()` looks for, but it is not "unregistered"
/// either. Returns the project paths carrying a usable atelier entry.
fn project_registrations(body: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    v.get("projects")
        .and_then(|p| p.as_object())
        .map(|projects| {
            projects
                .iter()
                .filter(|(_, proj)| {
                    proj.get("mcpServers")
                        .and_then(|m| m.get("atelier"))
                        .is_some_and(|e| e.get("url").is_some() || e.get("command").is_some())
                })
                .map(|(path, _)| path.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// The agents' config files and the fix line that registers atelier for each.
fn clients() -> Vec<(&'static str, PathBuf, RegistrationFormat, String)> {
    let home = service::home().unwrap_or_default();
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"));
    let kimi_home = std::env::var_os("KIMI_CODE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".kimi-code"));
    let http_fix = |path: &str| {
        format!(
            "add \"atelier\": {{\"url\": \"http://{DEFAULT_ADDR}/mcp\"}} to mcpServers in {path}"
        )
    };
    vec![
        (
            "claude",
            home.join(".claude.json"),
            RegistrationFormat::Json,
            "atelier clients install --for claude --mode http".into(),
        ),
        (
            "codex",
            codex_home.join("config.toml"),
            RegistrationFormat::Toml,
            "atelier clients install --for codex --mode http".into(),
        ),
        (
            "kimi",
            kimi_home.join("mcp.json"),
            RegistrationFormat::Json,
            "atelier clients install --for kimi --mode http".into(),
        ),
        (
            "cursor",
            home.join(".cursor/mcp.json"),
            RegistrationFormat::Json,
            http_fix("~/.cursor/mcp.json"),
        ),
    ]
}

fn check_clients() -> Vec<Row> {
    clients()
        .into_iter()
        .map(|(name, config, format, fix)| {
            let Ok(body) = std::fs::read_to_string(&config) else {
                return Row::note(name, format!("{} not found (client not installed)", tilde(&config)));
            };
            let found = match format {
                RegistrationFormat::Json => registration(&body),
                RegistrationFormat::Toml => codex_registration(&body),
            };
            match found {
                Registration::Http(url) => Row::ok(name, format!("registered (http: {url})")),
                Registration::Stdio(cmd) => Row::ok(name, format!("registered (stdio: {cmd})")),
                Registration::Absent => {
                    let projects = if name == "claude" {
                        project_registrations(&body)
                    } else {
                        Vec::new()
                    };
                    if projects.is_empty() {
                        Row::note(
                            name,
                            format!("not registered (the CLI needs nothing) — MCP add-on: {fix}"),
                        )
                    } else {
                        Row::note(
                            name,
                            format!(
                                "registered at project scope only ({}) — fix for every project: {fix}",
                                projects.join(", ")
                            ),
                        )
                    }
                }
                Registration::Malformed => Row::fail(
                    name,
                    format!(
                        "atelier registration in {} is malformed (need exactly one of \"url\" or \"command\") — fix: repair or remove that entry, then run: {fix}",
                        tilde(&config)
                    ),
                ),
            }
        })
        .collect()
}

// -- 5. skills ------------------------------------------------------------------

/// One installed skill file vs the baked renderer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SkillFile {
    Current,
    Stale,
    Missing,
}

impl SkillFile {
    fn label(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Stale => "stale",
            Self::Missing => "missing",
        }
    }
}

/// Compare every skill under `<dir>/skills/<name>/SKILL.md` against the baked
/// `skill_md()` — byte equality is the bar: a one-byte drift is a stale skill.
fn check_skills_in(dir: &Path) -> Vec<(&'static str, SkillFile)> {
    skills::ALL
        .iter()
        .map(|sk| {
            let state =
                match std::fs::read_to_string(dir.join("skills").join(sk.name).join("SKILL.md")) {
                    Ok(body) if body == sk.skill_md() => SkillFile::Current,
                    Ok(_) => SkillFile::Stale,
                    Err(_) => SkillFile::Missing,
                };
            (sk.name, state)
        })
        .collect()
}

fn check_skills() -> Vec<Row> {
    let home = service::home().unwrap_or_default();
    ["claude", "codex", "kimi", "cursor"]
        .into_iter()
        .map(|agent| {
            let name = format!("skills {agent}");
            let dir = crate::skill_target_root(agent, &home)
                .expect("check_skills lists only supported skill targets");
            if !dir.is_dir() {
                return Row::note(
                    name,
                    format!("{} not found (agent not installed)", tilde(&dir)),
                );
            }
            let bad: Vec<String> = check_skills_in(&dir)
                .into_iter()
                .filter(|(_, s)| *s != SkillFile::Current)
                .map(|(skill, s)| format!("{skill} {}", s.label()))
                .collect();
            if bad.is_empty() {
                Row::ok(name, format!("{} skills current", skills::ALL.len()))
            } else {
                Row::fail(
                    name,
                    format!(
                        "{} — fix: atelier skills install --for {agent}",
                        bad.join(", ")
                    ),
                )
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skills_report_missing_current_stale() {
        let dir =
            std::env::temp_dir().join(format!("atelier-doctor-skills-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // Nothing written: all three missing.
        let states = check_skills_in(&dir);
        assert!(states.iter().all(|(_, s)| *s == SkillFile::Missing));
        // Write the exact rendered content: current.
        for sk in skills::ALL {
            let p = dir.join("skills").join(sk.name).join("SKILL.md");
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, sk.skill_md()).unwrap();
        }
        let states = check_skills_in(&dir);
        assert!(states.iter().all(|(_, s)| *s == SkillFile::Current));
        // Corrupt one byte of one skill: stale, the others stay current.
        let p = dir
            .join("skills")
            .join(skills::SPRITE.name)
            .join("SKILL.md");
        let mut body = std::fs::read_to_string(&p).unwrap();
        body.replace_range(10..11, "X");
        std::fs::write(&p, body).unwrap();
        let states = check_skills_in(&dir);
        let by_name = |n: &str| states.iter().find(|(name, _)| *name == n).unwrap().1;
        assert_eq!(by_name(skills::SPRITE.name), SkillFile::Stale);
        assert_eq!(by_name(skills::SCENE.name), SkillFile::Current);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn registration_parses_url_and_command_shapes() {
        let http = r#"{"mcpServers": {"atelier": {"url": "http://127.0.0.1:8765/mcp"}}}"#;
        assert_eq!(
            registration(http),
            Registration::Http("http://127.0.0.1:8765/mcp".into())
        );
        let stdio = r#"{"mcpServers": {"atelier": {"command": "atelier"}}}"#;
        assert_eq!(registration(stdio), Registration::Stdio("atelier".into()));
        assert_eq!(registration(r#"{"mcpServers": {}}"#), Registration::Absent);
    }

    #[test]
    fn registration_rejects_garbage_and_wrong_shape() {
        assert_eq!(registration("not json"), Registration::Malformed);
        assert_eq!(
            registration(r#"{"mcpServers": {"atelier": {"transport": "http"}}}"#),
            Registration::Malformed
        );
    }

    #[test]
    fn codex_registration_parses_toml_shapes() {
        assert_eq!(
            codex_registration(
                r#"
[mcp_servers.atelier]
url = "http://127.0.0.1:8765/mcp"
"#
            ),
            Registration::Http("http://127.0.0.1:8765/mcp".into())
        );
        assert_eq!(
            codex_registration(
                r#"
[mcp_servers.atelier]
command = "/opt/atelier"
args = []
"#
            ),
            Registration::Stdio("/opt/atelier".into())
        );
        assert_eq!(
            codex_registration("model = \"gpt-5\"\n"),
            Registration::Absent
        );
        assert_eq!(
            codex_registration("[mcp_servers]\natelier = \"wrong\"\n"),
            Registration::Malformed
        );
    }

    #[test]
    fn project_scoped_entries_are_found_but_are_not_user_scope() {
        // Claude Code keeps per-project servers under `projects`: registered
        // there ≠ registered for every project, and ≠ unregistered.
        let body = r#"{"mcpServers": {}, "projects": {
            "/work/game": {"mcpServers": {"atelier": {"url": "http://127.0.0.1:8765/mcp"}}},
            "/work/other": {"mcpServers": {}}
        }}"#;
        assert_eq!(registration(body), Registration::Absent);
        assert_eq!(project_registrations(body), vec!["/work/game".to_string()]);
        assert!(project_registrations(r#"{"mcpServers": {}}"#).is_empty());
        assert!(project_registrations("not json").is_empty());
    }

    #[test]
    fn initialize_ok_needs_200_and_server_info() {
        assert!(initialize_ok(
            200,
            r#"{"result": {"serverInfo": {"name": "atelier"}}}"#
        ));
        assert!(!initialize_ok(405, r#"{"result": {"serverInfo": {}}}"#));
        assert!(!initialize_ok(200, "garbage"));
        assert!(!initialize_ok(200, r#"{"result": {"capabilities": {}}}"#));
    }

    #[test]
    fn run_rejects_arguments() {
        assert_eq!(run(&["--fix".to_string()]), 2);
    }
}
