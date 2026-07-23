//! Agent-client setup for the optional MCP transport.
//!
//! This is deliberately a CLI command instead of shell-script JSON/TOML
//! surgery. The installer can ask the human two separate questions
//! (registration, then broad tool approval), while this module owns
//! idempotent, tested config merges and refuses malformed files.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use toml_edit::{value, ArrayOfTables, DocumentMut, Item, Table};

use crate::service;

pub(crate) const DEFAULT_MCP_URL: &str = "http://127.0.0.1:8765/mcp";

const USAGE: &str =
    "atelier clients install --for <claude|codex|kimi> --mode <http|stdio> [--allow-tools]";
const TOOL_PATTERN: &str = "mcp__atelier__*";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Client {
    Claude,
    Codex,
    Kimi,
}

impl Client {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "kimi" => Some(Self::Kimi),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Kimi => "kimi",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Http,
    Stdio,
}

impl Mode {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "http" => Some(Self::Http),
            "stdio" => Some(Self::Stdio),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Stdio => "stdio",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Options {
    client: Client,
    mode: Mode,
    allow_tools: bool,
}

#[derive(Clone, Debug)]
struct ConfigHomes {
    user: PathBuf,
    codex: PathBuf,
    kimi: PathBuf,
}

impl ConfigHomes {
    fn from_env() -> Result<Self, String> {
        let user = service::home()
            .ok_or_else(|| "cannot find your home directory (HOME is unset)".to_string())?;
        let codex = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| user.join(".codex"));
        let kimi = std::env::var_os("KIMI_CODE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| user.join(".kimi-code"));
        Ok(Self { user, codex, kimi })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RegistrationChange {
    Added,
    Preserved,
}

fn implicit_table() -> Item {
    let mut table = Table::new();
    table.set_implicit(true);
    Item::Table(table)
}

/// Entry point for `atelier clients`.
pub(crate) fn run(args: &[String]) -> i32 {
    let options = match parse_options(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("atelier clients: {error}\nusage: {USAGE}");
            return 2;
        }
    };
    let homes = match ConfigHomes::from_env() {
        Ok(homes) => homes,
        Err(error) => {
            eprintln!("atelier clients: {error}");
            return 1;
        }
    };
    let binary = match std::env::current_exe() {
        Ok(binary) => binary,
        Err(error) => {
            eprintln!("atelier clients: cannot locate the atelier binary: {error}");
            return 1;
        }
    };

    match install_at(&homes, &binary, &options) {
        Ok(change) => {
            match change {
                RegistrationChange::Added => println!(
                    "{}: atelier MCP registered ({})",
                    options.client.name(),
                    options.mode.name()
                ),
                RegistrationChange::Preserved => println!(
                    "{}: atelier MCP already registered; preserved its existing transport",
                    options.client.name()
                ),
            }
            if options.allow_tools {
                println!(
                    "{}: pre-approved all atelier MCP tools ({TOOL_PATTERN})",
                    options.client.name()
                );
            } else {
                println!(
                    "{}: atelier MCP tools will keep using the client's normal approval prompts",
                    options.client.name()
                );
            }
            0
        }
        Err(error) => {
            eprintln!("atelier clients: {}: {error}", options.client.name());
            1
        }
    }
}

fn parse_options(args: &[String]) -> Result<Options, String> {
    if args.first().map(String::as_str) != Some("install") {
        return Err("expected the `install` command".into());
    }
    let mut client = None;
    let mut mode = None;
    let mut allow_tools = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--for" => {
                if client.is_some() {
                    return Err("--for may only be passed once".into());
                }
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--for needs a value".to_string())?;
                client =
                    Some(Client::parse(value).ok_or_else(|| {
                        format!("unknown client '{value}' (claude | codex | kimi)")
                    })?);
                index += 2;
            }
            "--mode" => {
                if mode.is_some() {
                    return Err("--mode may only be passed once".into());
                }
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--mode needs a value".to_string())?;
                mode = Some(
                    Mode::parse(value)
                        .ok_or_else(|| format!("unknown mode '{value}' (http | stdio)"))?,
                );
                index += 2;
            }
            "--allow-tools" => {
                if allow_tools {
                    return Err("--allow-tools may only be passed once".into());
                }
                allow_tools = true;
                index += 1;
            }
            unknown => return Err(format!("unknown argument '{unknown}'")),
        }
    }
    Ok(Options {
        client: client.ok_or_else(|| "--for is required".to_string())?,
        mode: mode.ok_or_else(|| "--mode is required".to_string())?,
        allow_tools,
    })
}

fn install_at(
    homes: &ConfigHomes,
    binary: &Path,
    options: &Options,
) -> Result<RegistrationChange, String> {
    match options.client {
        Client::Claude => {
            let registration = configure_json_registration(
                &homes.user.join(".claude.json"),
                Client::Claude,
                options.mode,
                binary,
            )?;
            if options.allow_tools {
                configure_claude_approval(&homes.user.join(".claude/settings.json"))?;
            }
            Ok(registration)
        }
        Client::Codex => configure_codex(
            &homes.codex.join("config.toml"),
            options.mode,
            binary,
            options.allow_tools,
        ),
        Client::Kimi => {
            let registration = configure_json_registration(
                &homes.kimi.join("mcp.json"),
                Client::Kimi,
                options.mode,
                binary,
            )?;
            if options.allow_tools {
                configure_kimi_approval(&homes.kimi.join("config.toml"))?;
            }
            Ok(registration)
        }
    }
}

fn desired_json_registration(client: Client, mode: Mode, binary: &Path) -> Value {
    match (client, mode) {
        (Client::Claude, Mode::Http) => {
            serde_json::json!({"type": "http", "url": DEFAULT_MCP_URL})
        }
        (Client::Claude, Mode::Stdio) => serde_json::json!({
            "type": "stdio",
            "command": binary.to_string_lossy(),
            "args": []
        }),
        (_, Mode::Http) => serde_json::json!({"url": DEFAULT_MCP_URL}),
        (_, Mode::Stdio) => serde_json::json!({
            "command": binary.to_string_lossy(),
            "args": []
        }),
    }
}

fn valid_json_registration(value: &Value) -> bool {
    let Some(entry) = value.as_object() else {
        return false;
    };
    let url = entry.get("url").and_then(Value::as_str);
    let command = entry.get("command").and_then(Value::as_str);
    matches!((url, command), (Some(_), None) | (None, Some(_)))
}

fn command_matches(existing: &str, binary: &Path) -> bool {
    existing == "atelier" || existing == binary.to_string_lossy()
}

fn json_registration_matches(value: &Value, mode: Mode, binary: &Path) -> bool {
    let Some(entry) = value.as_object() else {
        return false;
    };
    let registration_matches = match mode {
        Mode::Http => {
            entry.get("url").and_then(Value::as_str) == Some(DEFAULT_MCP_URL)
                && entry.get("command").is_none()
        }
        Mode::Stdio => {
            entry.get("url").is_none()
                && entry
                    .get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|command| command_matches(command, binary))
        }
    };
    let type_matches = match entry.get("type").and_then(Value::as_str) {
        None => true,
        Some("http") => mode == Mode::Http,
        Some("stdio") => mode == Mode::Stdio,
        Some(_) => false,
    };
    registration_matches && type_matches
}

fn configure_json_registration(
    path: &Path,
    client: Client,
    mode: Mode,
    binary: &Path,
) -> Result<RegistrationChange, String> {
    let mut root = read_json_object(path)?;
    let servers = object_field(&mut root, "mcpServers", path)?;
    if let Some(existing) = servers.get("atelier") {
        if !valid_json_registration(existing) {
            return Err(format!(
                "{} has a malformed mcpServers.atelier entry; expected exactly one of url or command",
                path.display()
            ));
        }
        if !json_registration_matches(existing, mode, binary) {
            return Err(format!(
                "{} already registers atelier with a different transport, endpoint, or command; refusing to replace it",
                path.display()
            ));
        }
        return Ok(RegistrationChange::Preserved);
    }
    servers.insert(
        "atelier".into(),
        desired_json_registration(client, mode, binary),
    );
    write_json(path, &root)?;
    Ok(RegistrationChange::Added)
}

fn configure_claude_approval(path: &Path) -> Result<(), String> {
    let mut root = read_json_object(path)?;
    let permissions = object_field(&mut root, "permissions", path)?;
    let allow = permissions
        .entry("allow")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| {
            format!(
                "{} has a non-array permissions.allow entry; refusing to replace it",
                path.display()
            )
        })?;
    if allow
        .iter()
        .any(|entry| entry.as_str() == Some(TOOL_PATTERN))
    {
        return Ok(());
    }
    allow.push(Value::String(TOOL_PATTERN.into()));
    write_json(path, &root)
}

fn read_json_object(path: &Path) -> Result<Value, String> {
    match std::fs::read_to_string(path) {
        Ok(body) => {
            let value: Value = serde_json::from_str(&body)
                .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
            if !value.is_object() {
                return Err(format!(
                    "{} must contain a JSON object; refusing to replace it",
                    path.display()
                ));
            }
            Ok(value)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Value::Object(Map::new())),
        Err(error) => Err(format!("cannot read {}: {error}", path.display())),
    }
}

fn object_field<'a>(
    root: &'a mut Value,
    key: &str,
    path: &Path,
) -> Result<&'a mut Map<String, Value>, String> {
    root.as_object_mut()
        .expect("read_json_object always returns an object")
        .entry(key)
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| {
            format!(
                "{} has a non-object {key} entry; refusing to replace it",
                path.display()
            )
        })
}

fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    let mut body = serde_json::to_string_pretty(value)
        .map_err(|error| format!("cannot encode {}: {error}", path.display()))?;
    body.push('\n');
    write_text(path, &body)
}

fn read_toml(path: &Path) -> Result<DocumentMut, String> {
    match std::fs::read_to_string(path) {
        Ok(body) => body
            .parse::<DocumentMut>()
            .map_err(|error| format!("cannot parse {}: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(error) => Err(format!("cannot read {}: {error}", path.display())),
    }
}

fn configure_codex(
    path: &Path,
    mode: Mode,
    binary: &Path,
    allow_tools: bool,
) -> Result<RegistrationChange, String> {
    let mut document = read_toml(path)?;
    let servers = document
        .as_table_mut()
        .entry("mcp_servers")
        .or_insert_with(implicit_table)
        .as_table_like_mut()
        .ok_or_else(|| {
            format!(
                "{} has a non-table mcp_servers entry; refusing to replace it",
                path.display()
            )
        })?;

    let change = if let Some(existing) = servers.get_mut("atelier") {
        if !valid_toml_registration(existing) {
            return Err(format!(
                "{} has a malformed mcp_servers.atelier entry; expected exactly one of url or command",
                path.display()
            ));
        }
        if !toml_registration_matches(existing, mode, binary) {
            return Err(format!(
                "{} already registers atelier with a different transport, endpoint, or command; refusing to replace it",
                path.display()
            ));
        }
        RegistrationChange::Preserved
    } else {
        let mut atelier = Table::new();
        match mode {
            Mode::Http => {
                atelier.insert("url", value(DEFAULT_MCP_URL));
            }
            Mode::Stdio => {
                atelier.insert("command", value(binary.to_string_lossy().as_ref()));
                atelier.insert("args", value(toml_edit::Array::new()));
            }
        }
        servers.insert("atelier", Item::Table(atelier));
        RegistrationChange::Added
    };

    if allow_tools {
        let atelier = servers
            .get_mut("atelier")
            .and_then(Item::as_table_like_mut)
            .expect("a valid atelier registration is table-like");
        let already_allowed = atelier
            .get("default_tools_approval_mode")
            .and_then(Item::as_str)
            == Some("approve");
        if !already_allowed {
            atelier.insert("default_tools_approval_mode", value("approve"));
        }
    }

    let body = document.to_string();
    let old = std::fs::read_to_string(path).ok();
    if old.as_deref() != Some(body.as_str()) {
        write_text(path, &body)?;
    }
    Ok(change)
}

fn valid_toml_registration(item: &Item) -> bool {
    let Some(entry) = item.as_table_like() else {
        return false;
    };
    let url = entry.get("url").and_then(Item::as_str);
    let command = entry.get("command").and_then(Item::as_str);
    matches!((url, command), (Some(_), None) | (None, Some(_)))
}

fn toml_registration_matches(item: &Item, mode: Mode, binary: &Path) -> bool {
    let Some(entry) = item.as_table_like() else {
        return false;
    };
    match mode {
        Mode::Http => {
            entry.get("url").and_then(Item::as_str) == Some(DEFAULT_MCP_URL)
                && entry.get("command").is_none()
        }
        Mode::Stdio => {
            entry.get("url").is_none()
                && entry
                    .get("command")
                    .and_then(Item::as_str)
                    .is_some_and(|command| command_matches(command, binary))
        }
    }
}

fn configure_kimi_approval(path: &Path) -> Result<(), String> {
    let mut document = read_toml(path)?;
    let permission = document
        .as_table_mut()
        .entry("permission")
        .or_insert_with(implicit_table)
        .as_table_like_mut()
        .ok_or_else(|| {
            format!(
                "{} has a non-table permission entry; refusing to replace it",
                path.display()
            )
        })?;
    let rules = permission
        .entry("rules")
        .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()))
        .as_array_of_tables_mut()
        .ok_or_else(|| {
            format!(
                "{} has a non-array-of-tables permission.rules entry; refusing to replace it",
                path.display()
            )
        })?;
    let exists = rules.iter().any(|rule| {
        rule.get("decision").and_then(Item::as_str) == Some("allow")
            && rule.get("pattern").and_then(Item::as_str) == Some(TOOL_PATTERN)
    });
    if !exists {
        let mut rule = Table::new();
        rule.insert("decision", value("allow"));
        rule.insert("pattern", value(TOOL_PATTERN));
        rules.push(rule);
    }

    let body = document.to_string();
    let old = std::fs::read_to_string(path).ok();
    if old.as_deref() != Some(body.as_str()) {
        write_text(path, &body)?;
    }
    Ok(())
}

fn write_text(path: &Path, body: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    std::fs::write(path, body).map_err(|error| format!("cannot write {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "atelier-clients-test-{}-{serial}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn homes(temp: &TempDir) -> ConfigHomes {
        ConfigHomes {
            user: temp.0.join("home"),
            codex: temp.0.join("codex"),
            kimi: temp.0.join("kimi"),
        }
    }

    fn options(client: Client, mode: Mode, allow_tools: bool) -> Options {
        Options {
            client,
            mode,
            allow_tools,
        }
    }

    #[test]
    fn options_require_explicit_client_and_mode() {
        assert_eq!(
            parse_options(&[
                "install".into(),
                "--for".into(),
                "codex".into(),
                "--mode".into(),
                "http".into(),
                "--allow-tools".into(),
            ])
            .unwrap(),
            options(Client::Codex, Mode::Http, true)
        );
        assert!(parse_options(&["install".into()]).is_err());
        assert!(parse_options(&[
            "install".into(),
            "--for".into(),
            "cursor".into(),
            "--mode".into(),
            "http".into()
        ])
        .is_err());
    }

    #[test]
    fn claude_merge_preserves_settings_and_is_idempotent() {
        let temp = TempDir::new();
        let homes = homes(&temp);
        std::fs::create_dir_all(&homes.user).unwrap();
        std::fs::write(
            homes.user.join(".claude.json"),
            r#"{"theme":"dark","mcpServers":{"other":{"command":"other"}}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(homes.user.join(".claude")).unwrap();
        std::fs::write(
            homes.user.join(".claude/settings.json"),
            r#"{"permissions":{"allow":["Bash(git status)"]},"model":"opus"}"#,
        )
        .unwrap();
        let opts = options(Client::Claude, Mode::Http, true);

        assert_eq!(
            install_at(&homes, Path::new("/opt/atelier"), &opts).unwrap(),
            RegistrationChange::Added
        );
        assert_eq!(
            install_at(&homes, Path::new("/opt/atelier"), &opts).unwrap(),
            RegistrationChange::Preserved
        );

        let registration: Value = serde_json::from_str(
            &std::fs::read_to_string(homes.user.join(".claude.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(registration["theme"], "dark");
        assert_eq!(
            registration["mcpServers"]["atelier"]["url"],
            DEFAULT_MCP_URL
        );
        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(homes.user.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(settings["model"], "opus");
        let allow = settings["permissions"]["allow"].as_array().unwrap();
        assert_eq!(
            allow
                .iter()
                .filter(|entry| entry.as_str() == Some(TOOL_PATTERN))
                .count(),
            1
        );
    }

    #[test]
    fn conflicting_valid_json_registration_is_rejected_not_replaced() {
        let temp = TempDir::new();
        let homes = homes(&temp);
        std::fs::create_dir_all(&homes.kimi).unwrap();
        let existing = r#"{"mcpServers":{"atelier":{"url":"https://remote.example/mcp","headers":{"x":"y"}}}}"#;
        std::fs::write(homes.kimi.join("mcp.json"), existing).unwrap();

        let result = install_at(
            &homes,
            Path::new("/opt/atelier"),
            &options(Client::Kimi, Mode::Stdio, false),
        );
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(homes.kimi.join("mcp.json")).unwrap(),
            existing
        );
    }

    #[test]
    fn matching_json_registration_is_preserved_byte_for_byte() {
        let temp = TempDir::new();
        let homes = homes(&temp);
        std::fs::create_dir_all(&homes.kimi).unwrap();
        let existing =
            r#"{"mcpServers":{"atelier":{"command":"atelier","args":[]},"other":{"url":"x"}}}"#;
        std::fs::write(homes.kimi.join("mcp.json"), existing).unwrap();

        let change = install_at(
            &homes,
            Path::new("/opt/atelier"),
            &options(Client::Kimi, Mode::Stdio, false),
        )
        .unwrap();
        assert_eq!(change, RegistrationChange::Preserved);
        assert_eq!(
            std::fs::read_to_string(homes.kimi.join("mcp.json")).unwrap(),
            existing
        );
    }

    #[test]
    fn malformed_json_config_is_not_overwritten() {
        let temp = TempDir::new();
        let homes = homes(&temp);
        std::fs::create_dir_all(&homes.kimi).unwrap();
        let path = homes.kimi.join("mcp.json");
        std::fs::write(&path, "not json\n").unwrap();

        assert!(install_at(
            &homes,
            Path::new("/opt/atelier"),
            &options(Client::Kimi, Mode::Http, false)
        )
        .is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "not json\n");
    }

    #[test]
    fn codex_merge_preserves_comments_and_preapproves_once() {
        let temp = TempDir::new();
        let homes = homes(&temp);
        std::fs::create_dir_all(&homes.codex).unwrap();
        let path = homes.codex.join("config.toml");
        std::fs::write(&path, "# keep me\nmodel = \"gpt-5\"\n").unwrap();
        let opts = options(Client::Codex, Mode::Stdio, true);

        assert_eq!(
            install_at(&homes, Path::new("/opt/atelier"), &opts).unwrap(),
            RegistrationChange::Added
        );
        let once = std::fs::read_to_string(&path).unwrap();
        assert!(once.contains("# keep me"));
        assert!(once.contains("model = \"gpt-5\""));
        assert!(once.contains("command = \"/opt/atelier\""));
        assert!(once.contains("default_tools_approval_mode = \"approve\""));

        assert_eq!(
            install_at(&homes, Path::new("/opt/atelier"), &opts).unwrap(),
            RegistrationChange::Preserved
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), once);
        assert!(install_at(&homes, Path::new("/different/atelier"), &opts).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), once);
    }

    #[test]
    fn kimi_merge_preserves_config_and_adds_one_allow_rule() {
        let temp = TempDir::new();
        let homes = homes(&temp);
        std::fs::create_dir_all(&homes.kimi).unwrap();
        let config = homes.kimi.join("config.toml");
        std::fs::write(&config, "# keep\nmodel = \"kimi\"\n").unwrap();
        let opts = options(Client::Kimi, Mode::Http, true);

        install_at(&homes, Path::new("/opt/atelier"), &opts).unwrap();
        install_at(&homes, Path::new("/opt/atelier"), &opts).unwrap();

        let body = std::fs::read_to_string(config).unwrap();
        assert!(body.contains("# keep"));
        assert!(body.contains("model = \"kimi\""));
        assert_eq!(body.matches("[[permission.rules]]").count(), 1);
        assert_eq!(body.matches("pattern = \"mcp__atelier__*\"").count(), 1);
        let mcp: Value =
            serde_json::from_str(&std::fs::read_to_string(homes.kimi.join("mcp.json")).unwrap())
                .unwrap();
        assert_eq!(mcp["mcpServers"]["atelier"]["url"], DEFAULT_MCP_URL);
    }
}
