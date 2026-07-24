//! OS daemon management: install / uninstall / status for the HTTP MCP server,
//! so atelier runs in the background and survives logout/reboot.
//!
//! Detects the OS: macOS uses a per-user **launchd** LaunchAgent
//! (`~/Library/LaunchAgents/com.atelier.server.plist`, KeepAlive); Linux
//! uses a **systemd --user** unit (`~/.config/systemd/user/atelier.service`,
//! Restart=on-failure). Logs land in `~/.atelier/logs/`.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

const LABEL: &str = "com.atelier.server";
pub(crate) const DEFAULT_BIND: &str = "127.0.0.1:8765";
pub(crate) const DEFAULT_MCP_URL: &str = "http://127.0.0.1:8765/mcp";
const DEFAULT_PORT: u16 = 8765;

#[derive(Debug, PartialEq, Eq)]
struct InstallOptions {
    bind: Option<String>,
    port: Option<u16>,
    home: Option<PathBuf>,
}

/// Entry point for `atelier <install|uninstall|status>`. Returns a process exit
/// code. Interactive installs ask for a port; scripts can pass `--port`, while
/// `--bind` remains the advanced host-and-port override.
pub fn run(args: &[String]) -> i32 {
    let cmd = args.first().map(|s| s.as_str());
    match cmd {
        Some("install") => {
            let options = match parse_install_options(args) {
                Ok(options) => options,
                Err(error) => {
                    eprintln!("atelier: {error}");
                    return 2;
                }
            };
            let current_bind = installed_bind();
            let prompted_port = if options.bind.is_none()
                && options.port.is_none()
                && std::io::stdin().is_terminal()
            {
                let default = current_bind
                    .as_deref()
                    .and_then(bind_port)
                    .unwrap_or(DEFAULT_PORT);
                if let Some(current) = &current_bind {
                    eprintln!("Existing daemon bind: {current}");
                }
                match prompt_port(default) {
                    Ok(port) => Some(port),
                    Err(error) => {
                        eprintln!("atelier: {error}");
                        return 2;
                    }
                }
            } else {
                None
            };
            let bind = match select_bind(&options, current_bind.as_deref(), prompted_port) {
                Ok(bind) => bind,
                Err(error) => {
                    eprintln!("atelier: {error}");
                    return 2;
                }
            };
            let home_dir = options.home.unwrap_or_else(global_home);
            if let Err(error) = validate_manifest_values(&bind, &home_dir) {
                eprintln!("atelier: {error}");
                return 2;
            }
            install(&bind, &home_dir)
        }
        Some("uninstall") if args.len() == 1 => uninstall(),
        Some("status") if args.len() == 1 => status(),
        _ => {
            eprintln!(
                "usage: atelier install [--port PORT | --bind ADDR] [--home DIR]\n\
                 \x20      atelier <uninstall|status>\n\
                 \n\
                 install    set up or reconfigure the background daemon; asks for a port\n\
                 uninstall  stop + remove the daemon\n\
                 status     show whether the daemon is running and where logs live"
            );
            2
        }
    }
}

fn parse_install_options(args: &[String]) -> Result<InstallOptions, String> {
    let mut options = InstallOptions {
        bind: None,
        port: None,
        home: None,
    };
    let mut index = 1;
    while index < args.len() {
        let flag = args[index].as_str();
        let value = || {
            args.get(index + 1)
                .filter(|value| !value.starts_with("--"))
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag {
            "--bind" => {
                if options.bind.is_some() {
                    return Err("--bind may only be passed once".into());
                }
                options.bind = Some(value()?.clone());
                index += 2;
            }
            "--port" => {
                if options.port.is_some() {
                    return Err("--port may only be passed once".into());
                }
                options.port = Some(parse_port(value()?)?);
                index += 2;
            }
            "--home" => {
                if options.home.is_some() {
                    return Err("--home may only be passed once".into());
                }
                options.home = Some(PathBuf::from(value()?));
                index += 2;
            }
            unknown => return Err(format!("unknown install argument '{unknown}'")),
        }
    }
    if options.bind.is_some() && options.port.is_some() {
        return Err("--port and --bind are mutually exclusive".into());
    }
    if let Some(bind) = &options.bind {
        validate_bind(bind)?;
    }
    Ok(options)
}

fn parse_port(value: &str) -> Result<u16, String> {
    match value.parse::<u16>() {
        Ok(0) | Err(_) => Err(format!(
            "invalid port '{value}' (expected an integer from 1 to 65535)"
        )),
        Ok(port) => Ok(port),
    }
}

fn bind_port(bind: &str) -> Option<u16> {
    let (host, port) = bind.rsplit_once(':')?;
    if host.is_empty() {
        return None;
    }
    parse_port(port).ok()
}

fn validate_bind(bind: &str) -> Result<(), String> {
    bind_port(bind).map(|_| ()).ok_or_else(|| {
        format!("invalid bind address '{bind}' (expected HOST:PORT with port 1 to 65535)")
    })
}

fn replace_port(bind: &str, port: u16) -> String {
    let host = bind
        .rsplit_once(':')
        .map(|(host, _)| host)
        .filter(|host| !host.is_empty())
        .unwrap_or("127.0.0.1");
    format!("{host}:{port}")
}

fn select_bind(
    options: &InstallOptions,
    current_bind: Option<&str>,
    prompted_port: Option<u16>,
) -> Result<String, String> {
    if let Some(bind) = &options.bind {
        return Ok(bind.clone());
    }
    let base = current_bind.unwrap_or(DEFAULT_BIND);
    let port = options
        .port
        .or(prompted_port)
        .or_else(|| bind_port(base))
        .unwrap_or(DEFAULT_PORT);
    let bind = replace_port(base, port);
    validate_bind(&bind)?;
    Ok(bind)
}

fn prompt_port_with_io(
    input: &mut impl BufRead,
    output: &mut impl Write,
    default: u16,
) -> Result<u16, String> {
    loop {
        write!(output, "MCP HTTP port [{default}]: ")
            .and_then(|_| output.flush())
            .map_err(|error| format!("cannot write port prompt: {error}"))?;
        let mut answer = String::new();
        let read = input
            .read_line(&mut answer)
            .map_err(|error| format!("cannot read port: {error}"))?;
        if read == 0 || answer.trim().is_empty() {
            return Ok(default);
        }
        match parse_port(answer.trim()) {
            Ok(port) => return Ok(port),
            Err(error) => {
                writeln!(output, "{error}")
                    .map_err(|write_error| format!("cannot write port prompt: {write_error}"))?;
            }
        }
    }
}

fn prompt_port(default: u16) -> Result<u16, String> {
    let stdin = std::io::stdin();
    let stderr = std::io::stderr();
    prompt_port_with_io(&mut stdin.lock(), &mut stderr.lock(), default)
}

/// These values are interpolated into a launchd plist / systemd unit. Reject
/// manifest-breaking control characters and systemd quoting/specifier syntax.
fn validate_manifest_values(bind: &str, home_dir: &Path) -> Result<(), String> {
    for (name, val) in [
        ("--bind", bind),
        ("--home", home_dir.to_string_lossy().as_ref()),
    ] {
        if val.chars().any(|c| c.is_control() || c == '"' || c == '%') {
            return Err(format!(
                "{name} may not contain control characters, '\"' or '%'"
            ));
        }
    }
    Ok(())
}

pub(crate) fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Default studio home (env → project `./.atelier` → global): the studio owns
/// the policy (`Studio::default_home`); this delegates so the two never drift.
pub(crate) fn default_home() -> PathBuf {
    atelier_studio::Studio::default_home()
}

/// The global store only (`ATELIER_HOME` or `~/.atelier`). The daemon pins
/// this at install time: a shared background server has no "current
/// directory", so a project store must never become its default just because
/// `atelier install` ran from inside one. (`--home` still overrides.)
pub(crate) fn global_home() -> PathBuf {
    atelier_studio::Studio::global_home()
}

fn log_dir() -> PathBuf {
    global_home().join("logs")
}

/// The current numeric UID (needed for the launchd gui domain). Errors loud —
/// guessing a UID would bootstrap another user's domain without a word.
fn current_uid() -> Result<String, String> {
    let out = Command::new("id")
        .arg("-u")
        .output()
        .map_err(|e| format!("cannot run `id -u`: {e}"))?;
    let uid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if uid.is_empty() {
        return Err("`id -u` returned nothing".into());
    }
    Ok(uid)
}

/// The launchd LaunchAgent plist (macOS).
/// Escape the five XML metacharacters so a path/address containing `&` or `<`
/// can't break the plist (a malformed plist fails to load silently).
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn launchd_plist(bin: &str, bind: &str, home_dir: &str, logs: &str) -> String {
    let (bin, bind, home_dir, logs) = (
        xml_escape(bin),
        xml_escape(bind),
        xml_escape(home_dir),
        xml_escape(logs),
    );
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{bin}</string>
        <string>--http</string>
        <string>{bind}</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>ATELIER_HOME</key><string>{home_dir}</string>
    </dict>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>StandardOutPath</key><string>{logs}/atelier.out.log</string>
    <key>StandardErrorPath</key><string>{logs}/atelier.err.log</string>
</dict>
</plist>
"#
    )
}

/// The systemd --user unit (Linux).
/// Paths are double-quoted: systemd splits ExecStart/Environment on whitespace,
/// so a binary or ATELIER_HOME containing a space ("/home/user name/…") would
/// otherwise install a service that fails to start. `"` and `%` are rejected
/// at the argument layer, so plain quoting is sufficient here.
fn systemd_unit(bin: &str, bind: &str, home_dir: &str) -> String {
    format!(
        "[Unit]\n\
         Description=atelier MCP pixel-art server\n\
         After=network.target\n\
         \n\
         [Service]\n\
         ExecStart=\"{bin}\" --http \"{bind}\"\n\
         Environment=\"ATELIER_HOME={home_dir}\"\n\
         Restart=on-failure\n\
         RestartSec=2\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n"
    )
}

fn launchd_manifest(home: &Path) -> PathBuf {
    home.join("Library")
        .join("LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

fn systemd_manifest(home: &Path) -> PathBuf {
    home.join(".config")
        .join("systemd")
        .join("user")
        .join("atelier.service")
}

fn daemon_manifest() -> Option<PathBuf> {
    let home = home()?;
    match std::env::consts::OS {
        "macos" => Some(launchd_manifest(&home)),
        "linux" => Some(systemd_manifest(&home)),
        _ => None,
    }
}

fn parse_launchd_bind(plist: &str) -> Option<String> {
    let after_flag = plist.split_once("<string>--http</string>")?.1;
    let after_open = after_flag.split_once("<string>")?.1;
    let encoded = after_open.split_once("</string>")?.0;
    let bind = xml_unescape(encoded.trim());
    validate_bind(&bind).ok()?;
    Some(bind)
}

fn parse_systemd_bind(unit: &str) -> Option<String> {
    let line = unit
        .lines()
        .find(|line| line.trim_start().starts_with("ExecStart="))?;
    let after_flag = line.split_once(" --http \"")?.1;
    let bind = after_flag.split_once('"')?.0.to_string();
    validate_bind(&bind).ok()?;
    Some(bind)
}

/// Convert a server bind into an endpoint local clients can actually dial.
/// Wildcard addresses are valid listeners but invalid destinations.
fn mcp_url_for_bind(bind: &str) -> String {
    let (host, port) = bind
        .rsplit_once(':')
        .expect("validated bind addresses always contain a port");
    let host = match host {
        "0.0.0.0" => "127.0.0.1",
        "[::]" => "[::1]",
        _ => host,
    };
    format!("http://{host}:{port}/mcp")
}

/// Bind address persisted in the installed launchd/systemd manifest.
pub(crate) fn installed_bind() -> Option<String> {
    let body = std::fs::read_to_string(daemon_manifest()?).ok()?;
    match std::env::consts::OS {
        "macos" => parse_launchd_bind(&body),
        "linux" => parse_systemd_bind(&body),
        _ => None,
    }
}

/// Endpoint client registration should use for the installed daemon.
pub(crate) fn installed_mcp_url() -> String {
    installed_bind()
        .map(|bind| mcp_url_for_bind(&bind))
        .unwrap_or_else(|| DEFAULT_MCP_URL.to_string())
}

// Asymmetry by design: install fails loud (a half-set-up service is useless),
// uninstall is best-effort (clear out whatever's there, never block teardown).
fn install(bind: &str, home_dir: &std::path::Path) -> i32 {
    let Ok(bin) = std::env::current_exe() else {
        eprintln!("cannot resolve the atelier binary path");
        return 1;
    };
    let bin = bin.to_string_lossy().into_owned();
    let logs = log_dir();
    // The comment below says install fails loud — creating the dirs it
    // depends on is part of that contract.
    for d in [&logs, &home_dir.to_path_buf()] {
        if let Err(e) = std::fs::create_dir_all(d) {
            eprintln!("cannot create {}: {e}", d.display());
            return 1;
        }
    }
    let hd = home_dir.to_string_lossy();
    let endpoint = mcp_url_for_bind(bind);

    match std::env::consts::OS {
        "macos" => {
            let Some(home) = home() else {
                eprintln!("no HOME directory");
                return 1;
            };
            let agents = home.join("Library").join("LaunchAgents");
            let _ = std::fs::create_dir_all(&agents);
            let plist_path = launchd_manifest(&home);
            let plist = launchd_plist(&bin, bind, &hd, &logs.to_string_lossy());
            if let Err(e) = std::fs::write(&plist_path, plist) {
                eprintln!("failed to write {}: {e}", plist_path.display());
                return 1;
            }
            let uid = match current_uid() {
                Ok(u) => u,
                Err(e) => {
                    eprintln!("atelier: {e}");
                    return 1;
                }
            };
            // Clear any stale registration, then bootstrap (fall back to the
            // legacy `load -w` on older macOS).
            let _ = Command::new("launchctl")
                .args(["bootout", &format!("gui/{uid}/{LABEL}")])
                .output();
            let ok = Command::new("launchctl")
                .args(["bootstrap", &format!("gui/{uid}")])
                .arg(&plist_path)
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
                || Command::new("launchctl")
                    .args(["load", "-w"])
                    .arg(&plist_path)
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);
            if !ok {
                eprintln!(
                    "launchctl failed to start the service (see logs in {})",
                    logs.display()
                );
                return 1;
            }
            println!(
                "✓ launchd service installed and started\n  label:    {LABEL}\n  plist:    {}\n  bind:     {bind}\n  endpoint: {endpoint}\n  home:     {hd}\n  logs:     {}/atelier.{{out,err}}.log\n\nConnect a client:\n  claude mcp add --transport http atelier {endpoint}",
                plist_path.display(),
                logs.display()
            );
            0
        }
        "linux" => {
            let Some(home) = home() else {
                eprintln!("no HOME directory");
                return 1;
            };
            let unit_path = systemd_manifest(&home);
            let Some(unit_dir) = unit_path.parent() else {
                eprintln!("cannot resolve the systemd user unit directory");
                return 1;
            };
            let _ = std::fs::create_dir_all(unit_dir);
            if let Err(e) = std::fs::write(&unit_path, systemd_unit(&bin, bind, &hd)) {
                eprintln!("failed to write {}: {e}", unit_path.display());
                return 1;
            }
            let reload = Command::new("systemctl")
                .args(["--user", "daemon-reload"])
                .status();
            let enable = Command::new("systemctl")
                .args(["--user", "enable", "--now", "atelier"])
                .status();
            if !(reload.map(|s| s.success()).unwrap_or(false)
                && enable.map(|s| s.success()).unwrap_or(false))
            {
                eprintln!("systemctl failed — is this a systemd user session?");
                return 1;
            }
            println!(
                "✓ systemd --user service installed and started\n  unit:     {}\n  bind:     {bind}\n  endpoint: {endpoint}\n  home:     {hd}\n  logs:     journalctl --user -u atelier -f\n\nConnect a client:\n  claude mcp add --transport http atelier {endpoint}",
                unit_path.display()
            );
            0
        }
        other => {
            eprintln!(
                "no native daemon support for '{other}'. Run `atelier --http {bind}` under your\n\
                 service manager of choice (e.g. NSSM or Task Scheduler on Windows)."
            );
            1
        }
    }
}

fn uninstall() -> i32 {
    match std::env::consts::OS {
        "macos" => {
            let Some(home) = home() else {
                eprintln!("no HOME directory");
                return 1;
            };
            let plist_path = launchd_manifest(&home);
            // Best-effort teardown: an unknown uid skips the bootout (with a
            // note) but never blocks removing the plist.
            match current_uid() {
                Ok(uid) => {
                    let _ = Command::new("launchctl")
                        .args(["bootout", &format!("gui/{uid}/{LABEL}")])
                        .output();
                }
                Err(e) => eprintln!("atelier: {e} — skipping launchctl bootout"),
            }
            let _ = std::fs::remove_file(&plist_path);
            println!(
                "✓ launchd service stopped and removed ({})",
                plist_path.display()
            );
            0
        }
        "linux" => {
            let Some(home) = home() else {
                eprintln!("no HOME directory");
                return 1;
            };
            let unit_path = systemd_manifest(&home);
            let _ = Command::new("systemctl")
                .args(["--user", "disable", "--now", "atelier"])
                .output();
            let _ = std::fs::remove_file(&unit_path);
            let _ = Command::new("systemctl")
                .args(["--user", "daemon-reload"])
                .output();
            println!(
                "✓ systemd service stopped and removed ({})",
                unit_path.display()
            );
            0
        }
        other => {
            eprintln!("no native daemon support for '{other}'");
            1
        }
    }
}

/// What the OS service manager says about the daemon: `Err` when the probe
/// itself can't run (e.g. no UID), else `(running, detail)` where detail is
/// the `state = …` line `status` prints (empty where the OS gives none). The
/// one launchd/systemd probe — `status` reports it, `doctor` gates on it.
fn daemon_probe() -> Result<(bool, String), String> {
    match std::env::consts::OS {
        "macos" => {
            let uid = current_uid()?;
            let out = Command::new("launchctl")
                .args(["print", &format!("gui/{uid}/{LABEL}")])
                .output();
            match out {
                Ok(o) if o.status.success() => {
                    let text = String::from_utf8_lossy(&o.stdout);
                    let state = text
                        .lines()
                        .find(|l| l.trim_start().starts_with("state ="))
                        .map(|l| l.trim().to_string())
                        .unwrap_or_else(|| "state = unknown".into());
                    Ok((true, state))
                }
                _ => Ok((false, String::new())),
            }
        }
        "linux" => {
            let running = Command::new("systemctl")
                .args(["--user", "status", "atelier", "--no-pager"])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            Ok((running, String::new()))
        }
        other => Ok((false, format!("no native daemon support for '{other}'"))),
    }
}

/// The daemon's unit file (launchd plist / systemd unit) exists on disk.
pub(crate) fn daemon_installed() -> bool {
    daemon_manifest().is_some_and(|path| path.exists())
}

/// The OS service manager reports the daemon loaded/active.
pub(crate) fn daemon_running() -> bool {
    daemon_probe().map(|(running, _)| running).unwrap_or(false)
}

/// The OS service manager the daemon installs into ("launchd" / "systemd").
pub(crate) fn manager() -> &'static str {
    match std::env::consts::OS {
        "macos" => "launchd",
        "linux" => "systemd",
        other => other,
    }
}

fn status() -> i32 {
    let logs = log_dir();
    let bind = installed_bind().unwrap_or_else(|| DEFAULT_BIND.to_string());
    let endpoint = mcp_url_for_bind(&bind);
    match std::env::consts::OS {
        "macos" => match daemon_probe() {
            Err(e) => {
                eprintln!("atelier: {e}");
                1
            }
            Ok((true, state)) => {
                println!(
                    "● {LABEL}: loaded ({state})\n  bind: {bind}\n  endpoint: {endpoint}\n  logs: {}/atelier.{{out,err}}.log",
                    logs.display()
                );
                0
            }
            Ok((false, _)) => {
                println!("○ {LABEL}: not installed (run `atelier install`)");
                1
            }
        },
        "linux" => {
            if daemon_running() {
                println!(
                    "● atelier.service: active\n  bind: {bind}\n  endpoint: {endpoint}\n  logs: journalctl --user -u atelier -f"
                );
                0
            } else if daemon_installed() {
                println!(
                    "○ atelier.service: installed but not running\n  bind: {bind}\n  endpoint: {endpoint}\n  logs: journalctl --user -u atelier -f"
                );
                1
            } else {
                println!("○ atelier.service: not installed (run `atelier install`)");
                1
            }
        }
        other => {
            eprintln!("no native daemon support for '{other}'");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn install_args(rest: &[&str]) -> Vec<String> {
        std::iter::once("install")
            .chain(rest.iter().copied())
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn install_options_accept_port_or_bind_but_not_both() {
        assert_eq!(
            parse_install_options(&install_args(&["--port", "9123", "--home", "/tmp/art"]))
                .unwrap(),
            InstallOptions {
                bind: None,
                port: Some(9123),
                home: Some(PathBuf::from("/tmp/art")),
            }
        );
        assert_eq!(
            parse_install_options(&install_args(&["--bind", "0.0.0.0:9010"])).unwrap(),
            InstallOptions {
                bind: Some("0.0.0.0:9010".into()),
                port: None,
                home: None,
            }
        );
        assert!(parse_install_options(&install_args(&[
            "--port",
            "9000",
            "--bind",
            "127.0.0.1:9000"
        ]))
        .is_err());
    }

    #[test]
    fn install_options_reject_bad_ports_missing_values_and_duplicates() {
        for port in ["0", "65536", "-1", "nope"] {
            assert!(parse_install_options(&install_args(&["--port", port])).is_err());
        }
        assert!(parse_install_options(&install_args(&["--port"])).is_err());
        assert!(
            parse_install_options(&install_args(&["--port", "9000", "--port", "9001"])).is_err()
        );
        assert!(parse_install_options(&install_args(&["--unknown", "x"])).is_err());
    }

    #[test]
    fn bind_selection_reuses_reinstall_host_and_port() {
        let defaults = parse_install_options(&install_args(&[])).unwrap();
        assert_eq!(
            select_bind(&defaults, Some("0.0.0.0:9010"), None).unwrap(),
            "0.0.0.0:9010"
        );

        let changed = parse_install_options(&install_args(&["--port", "9123"])).unwrap();
        assert_eq!(
            select_bind(&changed, Some("0.0.0.0:9010"), None).unwrap(),
            "0.0.0.0:9123"
        );
        assert_eq!(
            select_bind(&defaults, Some("127.0.0.1:9010"), Some(9456)).unwrap(),
            "127.0.0.1:9456"
        );
        assert_eq!(select_bind(&defaults, None, None).unwrap(), DEFAULT_BIND);
    }

    #[test]
    fn port_prompt_uses_default_and_retries_invalid_input() {
        let mut blank = Cursor::new(b"\n");
        let mut output = Vec::new();
        assert_eq!(
            prompt_port_with_io(&mut blank, &mut output, DEFAULT_PORT).unwrap(),
            DEFAULT_PORT
        );

        let mut retry = Cursor::new(b"0\n70000\n9123\n");
        output.clear();
        assert_eq!(
            prompt_port_with_io(&mut retry, &mut output, DEFAULT_PORT).unwrap(),
            9123
        );
        let prompt = String::from_utf8(output).unwrap();
        assert_eq!(prompt.matches("MCP HTTP port [8765]:").count(), 3);
        assert!(prompt.contains("invalid port '0'"));
        assert!(prompt.contains("invalid port '70000'"));
    }

    #[test]
    fn launchd_plist_includes_label_binary_bind_and_logs() {
        let plist = launchd_plist(
            "/usr/local/bin/atelier",
            "127.0.0.1:8765",
            "/home/u/.atelier",
            "/home/u/.atelier/logs",
        );
        assert!(plist.contains(LABEL));
        assert!(plist.contains("/usr/local/bin/atelier"));
        assert!(plist.contains("127.0.0.1:8765"));
        assert!(plist.contains("/home/u/.atelier/logs/atelier.out.log"));
        assert!(plist.contains("/home/u/.atelier/logs/atelier.err.log"));
        assert_eq!(
            parse_launchd_bind(&plist).as_deref(),
            Some("127.0.0.1:8765")
        );
    }

    #[test]
    fn systemd_unit_includes_execstart_bind_and_restart() {
        let unit = systemd_unit(
            "/usr/local/bin/atelier",
            "127.0.0.1:8765",
            "/home/u/.atelier",
        );
        // Fields are quoted so paths with spaces survive systemd's whitespace split.
        assert!(unit.contains("ExecStart=\"/usr/local/bin/atelier\" --http \"127.0.0.1:8765\""));
        assert!(unit.contains("Environment=\"ATELIER_HOME=/home/u/.atelier\""));
        assert!(unit.contains("Restart=on-failure"));
        assert_eq!(parse_systemd_bind(&unit).as_deref(), Some("127.0.0.1:8765"));
    }

    #[test]
    fn manifest_bind_parsers_reject_missing_or_invalid_ports() {
        assert!(parse_launchd_bind("<string>--http</string><string>localhost</string>").is_none());
        assert!(parse_systemd_bind("ExecStart=\"atelier\" --http \"localhost\"").is_none());
    }

    #[test]
    fn client_urls_replace_wildcard_binds_with_loopback() {
        assert_eq!(
            mcp_url_for_bind("0.0.0.0:9123"),
            "http://127.0.0.1:9123/mcp"
        );
        assert_eq!(mcp_url_for_bind("[::]:9123"), "http://[::1]:9123/mcp");
        assert_eq!(
            mcp_url_for_bind("192.0.2.10:9123"),
            "http://192.0.2.10:9123/mcp"
        );
    }
}
