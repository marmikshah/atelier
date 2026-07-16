//! `atelier agent` — the one ONLINE mode.
//!
//! Drives an OpenAI-style chat-completions API through an agentic loop so the
//! binary can draw a task on its own, with no external client. This is the
//! single place atelier reaches the network and reads an API key; everything
//! else (server, daemon, replay, every drawing op) stays offline and
//! deterministic. The whole module is gated behind the `agent` cargo feature,
//! so a default build links no HTTP/TLS stack at all.
//!
//! Wiring: the model talks to us over HTTPS; we execute its tool calls against a
//! child `atelier` stdio MCP server (the same server a real client spawns), so
//! the entire validated tool path — schemas, arg-checking, journaling — is
//! reused rather than reimplemented. `doc_look` images are fed back to the model
//! as base64 data URIs.

use std::process::Stdio;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

const USAGE: &str = "\
usage: atelier agent --task <text|@file> [options]

  --task <text|@file>   what to draw (literal, or @path to read from a file)
  --skill <name>        built-in skill to inject: sprite (default) | scene | review
  --skill-file <path>   inject a custom SKILL.md instead of a built-in
  --model <name>        model id (or OPENAI_MODEL; default gpt-4o)
  --base-url <url>      API base (or OPENAI_BASE_URL; default https://api.openai.com/v1)
  --out <path>          expected export path; the run warns if it is missing
  --max-steps <n>       tool-call rounds before giving up (default 40)
  --home <dir>          ATELIER_HOME for the drawing session (isolated store)

env: OPENAI_API_KEY (required). OPENAI_BASE_URL, OPENAI_MODEL as above.

The atelier-sprite/scene/review skills are baked into the binary, so a bare
`atelier agent --task \"...\"` works with no files. This is the only atelier
command that uses the network or an API key.";

/// The skills, compiled into the binary so agent mode needs no files on disk.
/// An installed user has no repo checkout, so a `--skill <path>` would have been
/// a broken default; these are always here.
const SKILL_SPRITE: &str = include_str!("../../../.claude/skills/atelier-sprite/SKILL.md");
const SKILL_SCENE: &str = include_str!("../../../.claude/skills/atelier-scene/SKILL.md");
const SKILL_REVIEW: &str = include_str!("../../../.claude/skills/atelier-review/SKILL.md");

/// Resolve a built-in skill name to its embedded text.
fn builtin_skill(name: &str) -> Result<&'static str, String> {
    match name {
        "sprite" => Ok(SKILL_SPRITE),
        "scene" => Ok(SKILL_SCENE),
        "review" => Ok(SKILL_REVIEW),
        other => Err(format!(
            "unknown skill '{other}' — expected sprite, scene or review (or --skill-file <path>)"
        )),
    }
}

struct Config {
    task: String,
    system: String,
    model: String,
    base_url: String,
    api_key: String,
    out: Option<String>,
    max_steps: usize,
    home: Option<String>,
}

/// Entry point for the `agent` subcommand. Returns a process exit code.
pub async fn run(args: &[String]) -> i32 {
    let cfg = match parse_args(args) {
        Ok(Some(c)) => c,
        Ok(None) => return 0, // --help
        Err(e) => {
            eprintln!("agent: {e}\n\n{USAGE}");
            return 2;
        }
    };
    match drive(cfg).await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("agent: {e}");
            1
        }
    }
}

fn parse_args(args: &[String]) -> Result<Option<Config>, String> {
    let mut task: Option<String> = None;
    // The skill is the built-in sprite unless a name or a file overrides it.
    let mut skill: String = SKILL_SPRITE.to_string();
    let mut model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o".into());
    let mut base_url =
        std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".into());
    let mut out: Option<String> = None;
    let mut max_steps = 40usize;
    let mut home: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        let need = |i: usize| -> Result<String, String> {
            args.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{} needs a value", args[i]))
        };
        match args[i].as_str() {
            "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(None);
            }
            "--task" => {
                let v = need(i)?;
                task = Some(match v.strip_prefix('@') {
                    Some(path) => std::fs::read_to_string(path)
                        .map_err(|e| format!("cannot read task file {path}: {e}"))?,
                    None => v,
                });
                i += 2;
            }
            "--skill" => {
                skill = builtin_skill(&need(i)?)?.to_string();
                i += 2;
            }
            "--skill-file" => {
                let path = need(i)?;
                skill = std::fs::read_to_string(&path)
                    .map_err(|e| format!("cannot read skill {path}: {e}"))?;
                i += 2;
            }
            "--model" => {
                model = need(i)?;
                i += 2;
            }
            "--base-url" => {
                base_url = need(i)?;
                i += 2;
            }
            "--out" => {
                out = Some(need(i)?);
                i += 2;
            }
            "--max-steps" => {
                max_steps = need(i)?
                    .parse()
                    .map_err(|_| "--max-steps must be a number".to_string())?;
                i += 2;
            }
            "--home" => {
                home = Some(need(i)?);
                i += 2;
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }

    let task = task.ok_or("--task is required")?;
    let api_key = std::env::var("OPENAI_API_KEY")
        .map_err(|_| "OPENAI_API_KEY is not set (this is the one command that needs it)")?;

    Ok(Some(Config {
        task: task.trim().to_string(),
        system: build_system_prompt(&skill, out.as_deref()),
        model,
        base_url: base_url.trim_end_matches('/').to_string(),
        api_key,
        out,
        max_steps,
        home,
    }))
}

/// The system prompt: the injected skills, then the ground rules the loop needs
/// the model to follow (drive the tools, look, and export at the end).
fn build_system_prompt(skill: &str, out: Option<&str>) -> String {
    // Strip YAML frontmatter — the model wants the body, not the metadata.
    let body = skill
        .strip_prefix("---")
        .and_then(|r| r.split_once("\n---").map(|(_, b)| b))
        .unwrap_or(skill);
    let mut s = String::new();
    s.push_str(body.trim());
    s.push_str("\n\n");
    s.push_str(
        "You are drawing through the atelier tools. Call them to build the art. \
         After a burst of edits, call doc_look to SEE the frame before continuing — \
         it returns the image. When the piece is finished, call the export tool, \
         then reply with a one-line summary and stop. Do not ask the user \
         questions; you have everything you need.",
    );
    // The caller chose the file; tell the model, or it picks its own name and the
    // export lands somewhere the caller is not looking (as a live run showed).
    if let Some(path) = out {
        s.push_str(&format!(
            " Export the finished piece to exactly this path: {path}"
        ));
    }
    s
}

async fn drive(cfg: Config) -> Result<(), String> {
    let mut server = McpServer::spawn(cfg.home.as_deref()).await?;
    let tools = server.list_tools().await?;
    let oai_tools = to_openai_tools(&tools);
    eprintln!(
        "agent: {} tools, model {}, drawing: {}",
        oai_tools.len(),
        cfg.model,
        first_line(&cfg.task)
    );

    let http = reqwest::Client::new();
    let mut messages = vec![
        json!({"role": "system", "content": cfg.system}),
        json!({"role": "user", "content": cfg.task}),
    ];

    for step in 1..=cfg.max_steps {
        let reply = chat(&http, &cfg, &messages, &oai_tools).await?;
        let calls = reply
            .get("tool_calls")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        // Persist the assistant turn verbatim so tool_call ids line up.
        messages.push(reply.clone());

        if calls.is_empty() {
            let text = reply.get("content").and_then(Value::as_str).unwrap_or("");
            eprintln!("agent: done in {step} step(s). {}", first_line(text));
            return finish(&cfg);
        }

        // Execute every call, collect any images to show the model next turn.
        let mut images: Vec<String> = Vec::new();
        for call in &calls {
            let id = call.get("id").and_then(Value::as_str).unwrap_or("");
            let func = call.get("function").cloned().unwrap_or_default();
            let name = func.get("name").and_then(Value::as_str).unwrap_or("");
            let args: Value = func
                .get("arguments")
                .and_then(Value::as_str)
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_else(|| json!({}));

            let (text, image) = server.call_tool(name, args).await?;
            eprintln!("  → {name} {}", first_line(&text));
            messages.push(json!({"role": "tool", "tool_call_id": id, "content": text}));
            if let Some(png) = image {
                images.push(png);
            }
        }

        // OpenAI tool messages are text-only; hand any tool image back as a
        // user turn so the model can actually SEE what doc_look returned.
        if !images.is_empty() {
            let mut content = vec![json!({"type": "text", "text": "Tool image output:"})];
            for png in images {
                content.push(json!({
                    "type": "image_url",
                    "image_url": {"url": format!("data:image/png;base64,{png}")}
                }));
            }
            messages.push(json!({"role": "user", "content": content}));
        }
    }

    Err(format!(
        "hit the {}-step limit without finishing (raise --max-steps)",
        cfg.max_steps
    ))
}

/// Confirm the expected output exists, if the caller named one.
fn finish(cfg: &Config) -> Result<(), String> {
    if let Some(out) = &cfg.out {
        if std::path::Path::new(out).exists() {
            eprintln!("agent: wrote {out}");
        } else {
            eprintln!("agent: WARNING — expected output {out} was not written");
        }
    }
    Ok(())
}

/// One chat-completions round. Returns the assistant message object.
async fn chat(
    http: &reqwest::Client,
    cfg: &Config,
    messages: &[Value],
    tools: &[Value],
) -> Result<Value, String> {
    let body = json!({
        "model": cfg.model,
        "messages": messages,
        "tools": tools,
        "tool_choice": "auto",
    });
    let resp = http
        .post(format!("{}/chat/completions", cfg.base_url))
        .bearer_auth(&cfg.api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("reading response failed: {e}"))?;
    if !status.is_success() {
        return Err(format!("API {status}: {}", first_line(&text)));
    }
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("bad JSON from API: {e}"))?;
    v.get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .cloned()
        .ok_or_else(|| format!("no choices in response: {}", first_line(&text)))
}

/// Convert MCP tool definitions to the OpenAI function-tool shape.
fn to_openai_tools(mcp: &[Value]) -> Vec<Value> {
    mcp.iter()
        .filter_map(|t| {
            let name = t.get("name")?.as_str()?;
            let params = t
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| json!({"type": "object"}));
            Some(json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": t.get("description").and_then(Value::as_str).unwrap_or(""),
                    "parameters": params,
                }
            }))
        })
        .collect()
}

fn first_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    if line.len() > 100 {
        format!("{}…", &line[..100])
    } else {
        line.to_string()
    }
}

/// A child `atelier` stdio MCP server, driven over JSON-RPC — the same transport
/// a real client uses, so the whole validated tool path is reused.
struct McpServer {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    next_id: i64,
}

impl McpServer {
    async fn spawn(home: Option<&str>) -> Result<Self, String> {
        let exe = std::env::current_exe()
            .map_err(|e| format!("cannot locate the atelier binary: {e}"))?;
        let mut cmd = Command::new(&exe);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if let Some(dir) = home {
            cmd.env("ATELIER_HOME", dir);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("failed to spawn the atelier server: {e}"))?;
        let stdin = child.stdin.take().ok_or("child stdin unavailable")?;
        let reader = BufReader::new(child.stdout.take().ok_or("child stdout unavailable")?);
        let mut s = Self {
            child,
            stdin,
            reader,
            next_id: 0,
        };
        s.handshake().await?;
        Ok(s)
    }

    async fn handshake(&mut self) -> Result<(), String> {
        let id = self.id();
        self.send(&json!({
            "jsonrpc": "2.0", "id": id, "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "atelier-agent", "version": env!("CARGO_PKG_VERSION")}
            }
        }))
        .await?;
        let resp = self.recv_id(id).await?;
        if let Some(err) = resp.get("error") {
            return Err(format!("server rejected initialize: {err}"));
        }
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await?;
        Ok(())
    }

    async fn list_tools(&mut self) -> Result<Vec<Value>, String> {
        let id = self.id();
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": "tools/list", "params": {}}))
            .await?;
        let resp = self.recv_id(id).await?;
        Ok(resp
            .get("result")
            .and_then(|r| r.get("tools"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// Run one tool. Returns (text summary, optional base64 PNG from the result).
    async fn call_tool(
        &mut self,
        name: &str,
        args: Value,
    ) -> Result<(String, Option<String>), String> {
        let id = self.id();
        self.send(&json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": name, "arguments": args}
        }))
        .await?;
        let resp = self.recv_id(id).await?;
        if let Some(err) = resp.get("error") {
            // A tool error is the model's problem to recover from, not ours.
            return Ok((format!("error: {err}"), None));
        }
        let content = resp
            .get("result")
            .and_then(|r| r.get("content"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let mut text = String::new();
        let mut image = None;
        for part in &content {
            match part.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(t) = part.get("text").and_then(Value::as_str) {
                        text.push_str(t);
                    }
                }
                Some("image") => {
                    // rmcp returns already-base64 image data.
                    image = part.get("data").and_then(Value::as_str).map(str::to_string);
                }
                _ => {}
            }
        }
        if text.is_empty() {
            text = "(ok)".into();
        }
        Ok((text, image))
    }

    fn id(&mut self) -> i64 {
        self.next_id += 1;
        self.next_id
    }

    async fn send(&mut self, msg: &Value) -> Result<(), String> {
        let line = format!("{msg}\n");
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| format!("write to server failed: {e}"))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| format!("flush failed: {e}"))
    }

    /// Read line-delimited JSON until the response with `want` arrives, skipping
    /// notifications and any interleaved traffic.
    async fn recv_id(&mut self, want: i64) -> Result<Value, String> {
        loop {
            let mut line = String::new();
            let n = self
                .reader
                .read_line(&mut line)
                .await
                .map_err(|e| format!("read from server failed: {e}"))?;
            if n == 0 {
                return Err("server closed the connection".into());
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let v: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => continue, // non-JSON log line on the pipe
            };
            if v.get("id").and_then(Value::as_i64) == Some(want) {
                return Ok(v);
            }
        }
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        // Best-effort: closing stdin ends the server's stdio loop.
        let _ = self.child.start_kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_tools_carry_name_description_and_schema() {
        let mcp = vec![json!({
            "name": "doc_look",
            "description": "SEE a frame",
            "inputSchema": {"type": "object", "properties": {"doc_id": {"type": "string"}}}
        })];
        let out = to_openai_tools(&mcp);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["type"], "function");
        assert_eq!(out[0]["function"]["name"], "doc_look");
        assert_eq!(out[0]["function"]["description"], "SEE a frame");
        assert_eq!(out[0]["function"]["parameters"]["type"], "object");
    }

    #[test]
    fn a_tool_missing_a_schema_still_converts() {
        let out = to_openai_tools(&[json!({"name": "list_docs", "description": "list"})]);
        assert_eq!(out[0]["function"]["parameters"]["type"], "object");
    }

    #[test]
    fn frontmatter_is_stripped_from_the_injected_skill() {
        let skill = "---\nname: x\ndescription: y\n---\n\n# Body\nreal guidance";
        let sys = build_system_prompt(skill, None);
        assert!(sys.contains("Body"), "{sys}");
        assert!(sys.contains("real guidance"));
        assert!(!sys.contains("description: y"), "frontmatter leaked: {sys}");
        assert!(sys.contains("call doc_look"), "ground rules missing");
    }

    #[test]
    fn the_built_in_skills_are_embedded_and_selectable() {
        // include_str! makes agent mode work with no files on disk — the whole
        // point. If a skill file is renamed, this fails at compile time.
        for name in ["sprite", "scene", "review"] {
            let body = builtin_skill(name).unwrap();
            assert!(body.contains("name: atelier-"), "{name} skill looks wrong");
            // And the default (no --skill) is the sprite skill, frontmatter-stripped.
        }
        assert!(builtin_skill("nope").is_err());
        let default_sys = build_system_prompt(SKILL_SPRITE, None);
        assert!(default_sys.contains("one subject") || default_sys.contains("sprite"));
        assert!(
            !default_sys.starts_with("---"),
            "frontmatter leaked into default"
        );
    }
}
