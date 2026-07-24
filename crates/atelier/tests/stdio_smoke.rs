//! End-to-end smoke for the stdio MCP transport: spawn the real binary and
//! speak line-delimited JSON-RPC to it. `call` and `replay` dispatch
//! in-process, so this is the one test that still proves the stdio server
//! answers a real client.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use atelier_mcp::server::Atelier;
use serde_json::{Value, json};

fn advertised_tool_names(list: &Value) -> Vec<String> {
    let mut names: Vec<String> = list["tools"]
        .as_array()
        .expect("tools/list returned an array")
        .iter()
        .map(|tool| {
            tool["name"]
                .as_str()
                .expect("advertised tool has a name")
                .to_string()
        })
        .collect();
    names.sort();
    names
}

fn registry_tool_names() -> Vec<String> {
    let mut names: Vec<String> = Atelier::registry_tools()
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect();
    names.sort();
    names
}

/// A spawned `atelier` stdio server with a reader thread pumping protocol
/// lines into a channel (a recv_timeout then bounds any server hang).
struct Session {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Value>,
    next_id: i64,
}

impl Session {
    fn spawn() -> Self {
        // An isolated store: the smoke doc must never land in a real one.
        let home = std::env::temp_dir().join(format!("atelier-stdio-smoke-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let mut child = Command::new(env!("CARGO_BIN_EXE_atelier"))
            .env("ATELIER_HOME", &home)
            .env("ATELIER_LOG", "off") // stdout is the protocol stream; silence the log
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn atelier");
        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if tx.send(v).is_err() {
                    break;
                }
            }
        });
        Session {
            child,
            stdin,
            rx,
            next_id: 0,
        }
    }

    /// One request/response round: send with a fresh id, wait for its answer.
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let msg = self
                .rx
                .recv_timeout(Duration::from_secs(30))
                .expect("server answered within 30s");
            if msg.get("id").and_then(Value::as_i64) == Some(id) {
                assert!(msg.get("error").is_none(), "{method} failed: {msg}");
                return msg["result"].clone();
            }
        }
    }

    fn notify(&mut self, method: &str) {
        self.send(&json!({"jsonrpc": "2.0", "method": method}));
    }

    fn send(&mut self, msg: &Value) {
        writeln!(self.stdin, "{msg}").expect("write to server");
        self.stdin.flush().expect("flush to server");
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn stdio_server_handshakes_lists_and_calls() {
    let mut s = Session::spawn();
    let init = s.request(
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "stdio-smoke", "version": "0"}
        }),
    );
    assert!(
        init["capabilities"]["tools"].is_object(),
        "tools capability advertised: {init}"
    );
    s.notify("notifications/initialized");

    let list = s.request("tools/list", json!({}));
    assert_eq!(
        advertised_tool_names(&list),
        registry_tool_names(),
        "stdio must advertise the canonical registry exactly"
    );

    let created = s.request(
        "tools/call",
        json!({
            "name": "doc_create",
            "arguments": {"name": "smoke", "width": 8, "height": 8}
        }),
    );
    let text = created["content"]
        .as_array()
        .and_then(|c| c.iter().find_map(|b| b.get("text")))
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(text.contains("\"smoke\""), "create payload: {text}");
}
