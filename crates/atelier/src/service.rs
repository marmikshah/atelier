//! Ubuntu daemon management for the HTTP MCP server.
//!
//! `atelier install` owns one **systemd --user** unit at
//! `~/.config/systemd/user/atelier.service`. Logs are available through the
//! user journal.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) const DEFAULT_BIND: &str = "127.0.0.1:8765";
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

/// These values are interpolated into a systemd unit. Reject manifest-breaking
/// control characters and systemd quoting/specifier syntax.
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
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Default studio home (env → project `./.atelier` → global): the studio owns
/// the policy (`Studio::default_home`); this delegates so the two never drift.
pub(crate) fn default_home() -> PathBuf {
    atelier_studio::Studio::default_home()
}

/// The global store only (`ATELIER_HOME` or `~/.atelier`). The daemon pins
/// this at install time: a shared background server has no "current
/// directory", so a local store must never become its default just because
/// `atelier install` ran from inside one. (`--home` still overrides.)
pub(crate) fn global_home() -> PathBuf {
    atelier_studio::Studio::global_home()
}

/// The systemd --user unit (Ubuntu).
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

fn systemd_manifest(home: &Path) -> PathBuf {
    home.join(".config")
        .join("systemd")
        .join("user")
        .join("atelier.service")
}

fn daemon_manifest() -> Option<PathBuf> {
    home().map(|home| systemd_manifest(&home))
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

/// Bind address persisted in the installed systemd manifest.
pub(crate) fn installed_bind() -> Option<String> {
    let body = std::fs::read_to_string(daemon_manifest()?).ok()?;
    parse_systemd_bind(&body)
}

// Asymmetry by design: install fails loud (a half-set-up service is useless),
// uninstall is best-effort (clear out whatever's there, never block teardown).
fn install(bind: &str, home_dir: &std::path::Path) -> i32 {
    let Ok(bin) = std::env::current_exe() else {
        eprintln!("cannot resolve the atelier binary path");
        return 1;
    };
    let bin = bin.to_string_lossy().into_owned();
    if let Err(e) = std::fs::create_dir_all(home_dir) {
        eprintln!("cannot create {}: {e}", home_dir.display());
        return 1;
    }
    let hd = home_dir.to_string_lossy();
    let endpoint = mcp_url_for_bind(bind);
    let Some(home) = home() else {
        eprintln!("no HOME directory");
        return 1;
    };
    let unit_path = systemd_manifest(&home);
    let Some(unit_dir) = unit_path.parent() else {
        eprintln!("cannot resolve the systemd user unit directory");
        return 1;
    };
    if let Err(e) = std::fs::create_dir_all(unit_dir) {
        eprintln!("cannot create {}: {e}", unit_dir.display());
        return 1;
    }
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
        eprintln!("systemctl failed — is this an Ubuntu systemd user session?");
        return 1;
    }
    println!(
        "✓ systemd --user service installed and started\n  unit:     {}\n  bind:     {bind}\n  endpoint: {endpoint}\n  home:     {hd}\n  logs:     journalctl --user -u atelier -f\n\nConnect a client:\n  claude mcp add --transport http atelier {endpoint}",
        unit_path.display()
    );
    0
}

fn uninstall() -> i32 {
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

/// Whether the systemd user service is active.
fn daemon_probe() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", "atelier"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// The daemon's systemd unit exists on disk.
pub(crate) fn daemon_installed() -> bool {
    daemon_manifest().is_some_and(|path| path.exists())
}

/// The OS service manager reports the daemon loaded/active.
pub(crate) fn daemon_running() -> bool {
    daemon_probe()
}

fn status() -> i32 {
    let bind = installed_bind().unwrap_or_else(|| DEFAULT_BIND.to_string());
    let endpoint = mcp_url_for_bind(&bind);
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
        assert!(
            parse_install_options(&install_args(&[
                "--port",
                "9000",
                "--bind",
                "127.0.0.1:9000"
            ]))
            .is_err()
        );
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
    fn systemd_bind_parser_rejects_missing_or_invalid_ports() {
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
