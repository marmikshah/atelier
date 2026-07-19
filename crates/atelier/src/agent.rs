//! `atelier agent` — the one ONLINE mode.
//!
//! Drives an OpenAI-style chat-completions API through an agentic loop so the
//! binary can draw a task on its own, with no external client. This is the
//! single place atelier reaches the network and reads an API key; everything
//! else (server, daemon, replay, every drawing op) stays offline and
//! deterministic. The whole module is gated behind the `agent` cargo feature,
//! so a default build links no HTTP/TLS stack at all.
//!
//! Wiring: the model talks to us over HTTPS; we execute its tool calls
//! in-process through `Atelier::dispatch` — the same single path the MCP
//! transports and `atelier call` take — so the entire validated tool path
//! (schemas, arg-checking, journaling) is reused rather than reimplemented.
//! `doc_look` images are fed back to the model as base64 data URIs.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use atelier_mcp::server::Atelier;
use atelier_studio::Studio;
use rmcp::model::CallToolResult;

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

use crate::skills::Skill;

/// The skill body the agent injects: either a built-in from the typed registry,
/// or a custom file.
enum SkillSource {
    /// A shipped skill; the registry renders its system prompt.
    Builtin(&'static Skill),
    /// A user-supplied `--skill-file`, injected verbatim as the prose body.
    Custom(String),
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
    let mut skill = SkillSource::Builtin(&crate::skills::SPRITE);
    let mut model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o".into());
    let mut base_url =
        std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".into());
    let mut out: Option<String> = None;
    let mut max_steps = 40usize;
    let mut home: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        let need = |i: usize| -> Result<String, String> {
            match args.get(i + 1) {
                // A following flag is a missing value, not the value —
                // `--task --skill scene` must not make the task "--skill".
                Some(v) if v.starts_with("--") => Err(format!("{} needs a value", args[i])),
                Some(v) => Ok(v.clone()),
                None => Err(format!("{} needs a value", args[i])),
            }
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
                let name = need(i)?;
                let sk = Skill::by_short(&name).ok_or_else(|| {
                    format!("unknown skill '{name}' — expected sprite, scene or review (or --skill-file <path>)")
                })?;
                skill = SkillSource::Builtin(sk);
                i += 2;
            }
            "--skill-file" => {
                let path = need(i)?;
                skill = SkillSource::Custom(
                    std::fs::read_to_string(&path)
                        .map_err(|e| format!("cannot read skill {path}: {e}"))?,
                );
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
    if max_steps == 0 {
        return Err("--max-steps must be at least 1".into());
    }
    let api_key = std::env::var("OPENAI_API_KEY")
        .map_err(|_| "OPENAI_API_KEY is not set (this is the one command that needs it)")?;

    let base_url = base_url.trim_end_matches('/').to_string();
    // The API key rides every request as a bearer token — refuse to send it in
    // cleartext. Plain http is allowed only to a loopback host for local dev.
    if !is_safe_base_url(&base_url) {
        return Err(format!(
            "base-url '{base_url}' is not https — the API key would go over cleartext \
             (http is allowed only to localhost/127.0.0.1)"
        ));
    }

    // Render the system prompt from the chosen skill. A built-in goes through
    // the typed renderer; a custom file is treated as the prose body.
    let system = match &skill {
        SkillSource::Builtin(sk) => sk.agent_prompt(out.as_deref()),
        SkillSource::Custom(body) => crate::skills::agent_prompt(body, out.as_deref()),
    };

    Ok(Some(Config {
        task: task.trim().to_string(),
        system,
        model,
        base_url,
        api_key,
        out,
        max_steps,
        home,
    }))
}

/// True for an https URL, or an http URL pointing at loopback (dev only).
fn is_safe_base_url(url: &str) -> bool {
    if url.starts_with("https://") {
        return true;
    }
    if let Some(rest) = url.strip_prefix("http://") {
        // A bracketed IPv6 host contains ':' — take through the ']' first, or
        // splitting on ':' would truncate "[::1]:8000" to "[".
        let host = if rest.starts_with('[') {
            rest.split(']')
                .next()
                .map(|h| format!("{h}]"))
                .unwrap_or_default()
        } else {
            rest.split(['/', ':']).next().unwrap_or("").to_string()
        };
        return host == "localhost" || host == "127.0.0.1" || host == "[::1]";
    }
    false
}

async fn drive(cfg: Config) -> Result<(), String> {
    let atelier = match &cfg.home {
        Some(dir) => Atelier::with_studio(Arc::new(Mutex::new(Studio::with_docs_dir(dir.into())))),
        None => Atelier::new(),
    };
    run_loop(&cfg, &atelier).await
}

/// The agentic loop: ask the model, execute its tool calls, feed images back.
async fn run_loop(cfg: &Config, atelier: &Atelier) -> Result<(), String> {
    let oai_tools = to_openai_tools(&Atelier::registry_tools());
    eprintln!(
        "agent: {} tools, model {}, drawing: {}",
        oai_tools.len(),
        cfg.model,
        first_line(&cfg.task)
    );

    // A per-request timeout so a stalled API (connection open, no response)
    // cannot hang the run forever; generous for slow models.
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let mut messages = vec![
        json!({"role": "system", "content": cfg.system}),
        json!({"role": "user", "content": cfg.task}),
    ];

    for step in 1..=cfg.max_steps {
        let (reply, finish_reason) = match chat(&http, cfg, &messages, &oai_tools).await {
            Ok(r) => r,
            Err(e) => {
                // Report the export the model may already have written before
                // the API died — the work is on disk either way.
                let _ = finish(cfg);
                return Err(e);
            }
        };
        let calls = reply
            .get("tool_calls")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        // Persist the assistant turn verbatim so tool_call ids line up.
        messages.push(reply.clone());

        if calls.is_empty() {
            let text = reply.get("content").and_then(Value::as_str).unwrap_or("");
            // No tool calls and nothing said is not "done" — it is the model
            // running out (finish_reason "length") or misfiring.
            if text.trim().is_empty() {
                let _ = finish(cfg);
                return Err(format!(
                    "model returned an empty message with no tool calls at step {step} \
                     (finish_reason: {finish_reason:?})"
                ));
            }
            eprintln!("agent: done in {step} step(s). {}", first_line(text));
            return finish(cfg);
        }

        // Execute every call, collect any images to show the model next turn.
        let mut images: Vec<String> = Vec::new();
        for call in &calls {
            let id = call.get("id").and_then(Value::as_str).unwrap_or("");
            let func = call.get("function").cloned().unwrap_or_default();
            let name = func.get("name").and_then(Value::as_str).unwrap_or("");
            let raw = func.get("arguments").and_then(Value::as_str).unwrap_or("");
            let args: Value = if raw.trim().is_empty() {
                json!({})
            } else {
                match serde_json::from_str(raw) {
                    Ok(v) => v,
                    Err(e) => {
                        // Coercing bad JSON to {} would run the tool with no
                        // args and blame it — tell the model whose fault it is
                        // so it can fix its own call in one round.
                        let msg = format!("error: your tool-call arguments were not valid JSON ({e}); resend the call with corrected arguments");
                        eprintln!("  → {name} {}", first_line(&msg));
                        messages.push(json!({"role": "tool", "tool_call_id": id, "content": msg}));
                        continue;
                    }
                }
            };

            let (text, image) = match atelier.dispatch(name, args, "agent").await {
                Ok(result) => tool_reply(&result),
                // A tool error is the model's problem to recover from, not ours.
                Err(e) => (format!("error: {e}"), None),
            };
            eprintln!("  → {name} {}", first_line(&text));
            messages.push(json!({"role": "tool", "tool_call_id": id, "content": text}));
            if let Some(png) = image {
                images.push(png);
            }
        }

        // OpenAI tool messages are text-only; hand any tool image back as a
        // user turn so the model can actually SEE what doc_look returned.
        if !images.is_empty() {
            // Only the newest frame stays live: drop the base64 from prior image
            // turns, or every past doc_look is re-uploaded on every later round —
            // quadratic bytes, exploding token cost, eventual context-limit death.
            drop_stale_images(&mut messages);
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

    // Even on the limit path, report the export the model may already have
    // written (and the missing-output warning if it didn't).
    let _ = finish(cfg);
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

/// One chat-completions round, with a bounded retry on transient failures
/// (429/5xx/connection errors) — one rate-limit blip on step 35 of 40 must not
/// discard the whole run. Returns `(assistant message, finish_reason)`.
async fn chat(
    http: &reqwest::Client,
    cfg: &Config,
    messages: &[Value],
    tools: &[Value],
) -> Result<(Value, String), String> {
    let body = json!({
        "model": cfg.model,
        "messages": messages,
        "tools": tools,
        "tool_choice": "auto",
    });
    const ATTEMPTS: u32 = 3;
    let mut last_err = String::new();
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            let wait = std::time::Duration::from_secs(2u64.pow(attempt + 1)); // 4s, 8s
            eprintln!("agent: {last_err} — retrying in {}s", wait.as_secs());
            tokio::time::sleep(wait).await;
        }
        let resp = match http
            .post(format!("{}/chat/completions", cfg.base_url))
            .bearer_auth(&cfg.api_key)
            .json(&body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                last_err = format!("request failed: {e}");
                continue;
            }
        };
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| format!("reading response failed: {e}"))?;
        if status.as_u16() == 429 || status.is_server_error() {
            last_err = format!("API {status}: {}", first_line(&text));
            continue;
        }
        if !status.is_success() {
            // 4xx other than 429 is our request's fault; retrying resends the
            // same mistake.
            return Err(format!("API {status}: {}", first_line(&text)));
        }
        let v: Value =
            serde_json::from_str(&text).map_err(|e| format!("bad JSON from API: {e}"))?;
        let choice = v
            .get("choices")
            .and_then(|c| c.get(0))
            .ok_or_else(|| format!("no choices in response: {}", first_line(&text)))?;
        let message = choice
            .get("message")
            .cloned()
            .ok_or_else(|| format!("no message in response: {}", first_line(&text)))?;
        let finish = choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        return Ok((message, finish));
    }
    Err(format!("{last_err} (after {ATTEMPTS} attempts)"))
}

/// Convert the advertised tool registry into the OpenAI function-tool shape.
fn to_openai_tools(mcp: &[rmcp::model::Tool]) -> Vec<Value> {
    mcp.iter()
        .map(|t| {
            json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description.as_deref().unwrap_or(""),
                    "parameters": Value::Object((*t.input_schema).clone()),
                }
            })
        })
        .collect()
}

/// Replace every past image user-turn's content with a short text stub, so only
/// the frame we are about to add is uploaded. Keeps the turn (no structural gap
/// in the tool/assistant sequence), drops the megabytes.
fn drop_stale_images(messages: &mut [Value]) {
    for m in messages.iter_mut() {
        let is_image_turn = m.get("role").and_then(Value::as_str) == Some("user")
            && m.get("content").and_then(Value::as_array).is_some_and(|c| {
                c.iter()
                    .any(|p| p.get("type").and_then(Value::as_str) == Some("image_url"))
            });
        if is_image_turn {
            *m = json!({"role": "user", "content": "(earlier frame; superseded by a newer doc_look)"});
        }
    }
}

fn first_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    // Truncate by CHARS, not bytes — a byte slice at 100 can split a multi-byte
    // UTF-8 sequence (a task with an accent, tool output with →/ΔE) and panic.
    if line.chars().count() > 100 {
        format!("{}…", line.chars().take(100).collect::<String>())
    } else {
        line.to_string()
    }
}

/// Condense one tool result into (text summary, optional base64 PNG). The
/// image data rides back already-base64, ready for a data URI.
fn tool_reply(result: &CallToolResult) -> (String, Option<String>) {
    let mut text = String::new();
    let mut image = None;
    for part in &result.content {
        if let Some(t) = part.as_text() {
            text.push_str(&t.text);
        } else if let Some(i) = part.as_image() {
            image = Some(i.data.clone());
        }
    }
    if text.is_empty() {
        text = "(ok)".into();
    }
    (text, image)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_tools_carry_name_description_and_schema() {
        // The real registry is the input; every advertised tool must convert.
        let registry = Atelier::registry_tools();
        let out = to_openai_tools(&registry);
        assert_eq!(out.len(), registry.len());
        let look = out
            .iter()
            .find(|t| t["function"]["name"] == "doc_look")
            .expect("doc_look converted");
        assert_eq!(look["type"], "function");
        assert!(
            look["function"]["description"]
                .as_str()
                .unwrap_or_default()
                .contains("SEE"),
            "the description rides along: {look}"
        );
        assert_eq!(look["function"]["parameters"]["type"], "object");
    }

    // Skill rendering (the built-ins, frontmatter, out-path, selectors) is
    // tested in crate::skills; the agent just calls those renderers.

    #[test]
    fn first_line_does_not_panic_on_multibyte_at_the_boundary() {
        // A byte-slice at 100 would split the '→' straddling that offset.
        let s = "a".repeat(99) + &"→".repeat(5);
        let out = first_line(&s); // must not panic
        assert!(out.ends_with('…'));
        assert!(out.chars().count() <= 101);
    }

    #[test]
    fn is_safe_base_url_rejects_cleartext_except_loopback() {
        assert!(is_safe_base_url("https://api.openai.com/v1"));
        assert!(is_safe_base_url("http://localhost:8000/v1"));
        assert!(is_safe_base_url("http://127.0.0.1:1234"));
        // Bracketed IPv6 loopback, with and without a port — splitting on ':'
        // naively truncates "[::1]:8000" to "[".
        assert!(is_safe_base_url("http://[::1]:8000/v1"));
        assert!(is_safe_base_url("http://[::1]/v1"));
        assert!(!is_safe_base_url("http://[2001:db8::1]:8000/v1"));
        assert!(!is_safe_base_url("http://evil.example.com/v1"));
        assert!(!is_safe_base_url("ftp://x"));
    }

    #[test]
    fn stale_image_turns_are_stripped_before_a_new_one() {
        let mut msgs = vec![
            json!({"role": "system", "content": "s"}),
            json!({"role": "user", "content": [
                {"type": "text", "text": "Tool image output:"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}}
            ]}),
            json!({"role": "assistant", "content": "ok"}),
        ];
        drop_stale_images(&mut msgs);
        // the old image turn is now a text stub, not a base64 blob
        assert!(msgs[1]["content"].is_string(), "image turn not stubbed");
        assert!(!msgs[1]["content"].as_str().unwrap().contains("base64"));
        // untouched turns stay
        assert_eq!(msgs[0]["content"], "s");
        assert_eq!(msgs[2]["content"], "ok");
    }
}
