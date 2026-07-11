//! OS daemon management: install / uninstall / status for the HTTP MCP server,
//! so atelier runs in the background and survives logout/reboot.
//!
//! Detects the OS: macOS uses a per-user **launchd** LaunchAgent
//! (`~/Library/LaunchAgents/com.atelier.server.plist`, KeepAlive); Linux
//! uses a **systemd --user** unit (`~/.config/systemd/user/atelier.service`,
//! Restart=on-failure). Logs land in `~/.atelier/logs/`.

use std::path::PathBuf;
use std::process::Command;

const LABEL: &str = "com.atelier.server";
const DEFAULT_BIND: &str = "127.0.0.1:8765";

/// Entry point for `atelier service <install|uninstall|status> [--bind ADDR]
/// [--home DIR]`. Returns a process exit code.
pub fn run(args: &[String]) -> i32 {
    let cmd = args.first().map(|s| s.as_str());
    let bind = flag_value(args, "--bind").unwrap_or_else(|| DEFAULT_BIND.to_string());
    let home_dir = flag_value(args, "--home")
        .map(PathBuf::from)
        .unwrap_or_else(default_home);
    // These values are interpolated into a launchd plist / systemd unit; a
    // control char (esp. a newline) could inject an extra directive. Reject
    // them before writing any manifest. XML metacharacters in the plist are
    // handled by escaping at format time.
    for (name, val) in [
        ("--bind", bind.as_str()),
        ("--home", &home_dir.to_string_lossy()),
    ] {
        if val.chars().any(|c| c.is_control()) {
            eprintln!("atelier service: {name} may not contain control characters");
            return 2;
        }
    }
    match cmd {
        Some("install") => install(&bind, &home_dir),
        Some("uninstall") => uninstall(),
        Some("status") => status(),
        _ => {
            eprintln!(
                "usage: atelier service <install|uninstall|status> [--bind ADDR] [--home DIR]\n\
                 \n\
                 install    set up + start the background service (launchd / systemd --user)\n\
                 uninstall  stop + remove the service\n\
                 status     show whether the service is running and where logs live"
            );
            2
        }
    }
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Default studio home (matches `studio::Studio`: `ATELIER_HOME` or `~/.atelier`).
fn default_home() -> PathBuf {
    if let Some(p) = std::env::var_os("ATELIER_HOME") {
        return PathBuf::from(p);
    }
    let Some(h) = home() else {
        return std::env::temp_dir().join("atelier");
    };
    h.join(".atelier")
}

fn log_dir() -> PathBuf {
    default_home().join("logs")
}

fn current_uid() -> String {
    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        // '501' is the typical first-user UID on macOS.
        .unwrap_or_else(|| "501".to_string())
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
fn systemd_unit(bin: &str, bind: &str, home_dir: &str) -> String {
    format!(
        "[Unit]\n\
         Description=atelier MCP pixel-art server\n\
         After=network.target\n\
         \n\
         [Service]\n\
         ExecStart={bin} --http {bind}\n\
         Environment=ATELIER_HOME={home_dir}\n\
         Restart=on-failure\n\
         RestartSec=2\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n"
    )
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
    let _ = std::fs::create_dir_all(&logs);
    let _ = std::fs::create_dir_all(home_dir);
    let hd = home_dir.to_string_lossy();

    match std::env::consts::OS {
        "macos" => {
            let Some(home) = home() else {
                eprintln!("no HOME directory");
                return 1;
            };
            let agents = home.join("Library").join("LaunchAgents");
            let _ = std::fs::create_dir_all(&agents);
            let plist_path = agents.join(format!("{LABEL}.plist"));
            let plist = launchd_plist(&bin, bind, &hd, &logs.to_string_lossy());
            if let Err(e) = std::fs::write(&plist_path, plist) {
                eprintln!("failed to write {}: {e}", plist_path.display());
                return 1;
            }
            let uid = current_uid();
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
                "✓ launchd service installed and started\n  label:    {LABEL}\n  plist:    {}\n  endpoint: http://{bind}/mcp\n  home:     {hd}\n  logs:     {}/atelier.{{out,err}}.log\n\nConnect a client:\n  claude mcp add --transport http atelier http://{bind}/mcp",
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
            let unit_dir = home.join(".config").join("systemd").join("user");
            let _ = std::fs::create_dir_all(&unit_dir);
            let unit_path = unit_dir.join("atelier.service");
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
                "✓ systemd --user service installed and started\n  unit:     {}\n  endpoint: http://{bind}/mcp\n  home:     {hd}\n  logs:     journalctl --user -u atelier -f\n\nConnect a client:\n  claude mcp add --transport http atelier http://{bind}/mcp",
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
            let plist_path = home
                .join("Library")
                .join("LaunchAgents")
                .join(format!("{LABEL}.plist"));
            let uid = current_uid();
            let _ = Command::new("launchctl")
                .args(["bootout", &format!("gui/{uid}/{LABEL}")])
                .output();
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
            let unit_path = home
                .join(".config")
                .join("systemd")
                .join("user")
                .join("atelier.service");
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

fn status() -> i32 {
    let logs = log_dir();
    match std::env::consts::OS {
        "macos" => {
            let uid = current_uid();
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
                    println!(
                        "● {LABEL}: loaded ({state})\n  logs: {}/atelier.{{out,err}}.log",
                        logs.display()
                    );
                    0
                }
                _ => {
                    println!("○ {LABEL}: not installed (run `atelier service install`)");
                    1
                }
            }
        }
        "linux" => {
            let st = Command::new("systemctl")
                .args(["--user", "status", "atelier", "--no-pager"])
                .status();
            st.map(|s| if s.success() { 0 } else { 1 }).unwrap_or(1)
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
    }

    #[test]
    fn systemd_unit_includes_execstart_bind_and_restart() {
        let unit = systemd_unit(
            "/usr/local/bin/atelier",
            "127.0.0.1:8765",
            "/home/u/.atelier",
        );
        assert!(unit.contains("ExecStart=/usr/local/bin/atelier --http 127.0.0.1:8765"));
        assert!(unit.contains("127.0.0.1:8765"));
        assert!(unit.contains("Restart=on-failure"));
    }
}
