//! A child `atelier` stdio MCP server, driven over line-delimited JSON-RPC.
//!
//! Shared by `atelier replay` (always built) and `atelier agent` (behind the
//! `agent` feature): both are MCP *clients* that spawn this very binary
//! (`current_exe`) with no args, so the whole validated tool path — schemas,
//! arg-checking, journaling — is reused rather than reimplemented.
//!
//! No rmcp client dependency: that would pull in `process-wrap` (child-process
//! transport) which we don't otherwise need. Hand-rolled line-delimited
//! JSON-RPC over the child's stdin/stdout keeps the dep tree unchanged.

use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// How long one response may take. Every tool is a local raster op — even a
/// large export finishes in seconds — so a silent minute means the child is
/// wedged, and an unbounded read would hang the run forever.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(120);

/// How a request can fail. The distinction matters to the agent loop: an
/// `Rpc` error (unknown tool, malformed args) is the model's problem to
/// recover from next round, while a `Transport` error means the child is gone
/// and the run is over.
pub(crate) enum RpcError {
    /// The pipe broke, timed out, or the server spoke garbage.
    Transport(String),
    /// The server answered with a JSON-RPC error object.
    Rpc(Value),
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RpcError::Transport(e) => write!(f, "{e}"),
            RpcError::Rpc(err) => match err.get("message").and_then(Value::as_str) {
                Some(msg) => write!(f, "{msg}"),
                None => write!(f, "{err}"),
            },
        }
    }
}

/// The child server plus the strictly-sequenced JSON-RPC dialogue with it.
pub(crate) struct StdioClient {
    child: Child,
    /// `Option` so `shutdown` can close the pipe (dropping it is the signal
    /// that lets the child's stdio loop end and flush its journals).
    stdin: Option<ChildStdin>,
    reader: BufReader<ChildStdout>,
    next_id: i64,
}

impl StdioClient {
    /// Spawn the child server and complete the MCP handshake. `client_name`
    /// identifies the caller in `clientInfo`; `home` overrides `ATELIER_HOME`
    /// for an isolated store.
    pub(crate) async fn spawn(client_name: &str, home: Option<&str>) -> Result<Self, String> {
        let exe = std::env::current_exe()
            .map_err(|e| format!("cannot locate the atelier binary: {e}"))?;
        let mut cmd = Command::new(&exe);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        // The child must be the stdio server no matter what the parent's
        // environment says: an inherited ATELIER_HTTP would make it bind an
        // HTTP port and never touch stdout (the handshake would hang), and an
        // inherited ATELIER_RECORD would silently append this session to the
        // user's recording.
        cmd.env_remove("ATELIER_HTTP").env_remove("ATELIER_RECORD");
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
            stdin: Some(stdin),
            reader,
            next_id: 0,
        };
        s.handshake(client_name).await.map_err(|e| e.to_string())?;
        Ok(s)
    }

    async fn handshake(&mut self, client_name: &str) -> Result<(), RpcError> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": client_name, "version": env!("CARGO_PKG_VERSION")}
            }),
        )
        .await
        .map_err(|e| match e {
            RpcError::Rpc(err) => RpcError::Transport(format!("server rejected initialize: {err}")),
            other => other,
        })?;
        self.notify("notifications/initialized").await
    }

    /// One request/response round: send with a fresh id, wait (bounded) for
    /// the matching response. Returns the `result` field.
    pub(crate) async fn request(&mut self, method: &str, params: Value) -> Result<Value, RpcError> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await?;
        let resp = self.recv_id(id).await?;
        if let Some(err) = resp.get("error") {
            return Err(RpcError::Rpc(err.clone()));
        }
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Fire-and-forget notification (no id, no response).
    async fn notify(&mut self, method: &str) -> Result<(), RpcError> {
        self.send(&json!({"jsonrpc": "2.0", "method": method}))
            .await
    }

    /// `tools/call` convenience: the result on success, `Rpc` on a JSON-RPC
    /// error (the caller decides whether that is fatal).
    pub(crate) async fn call_tool(&mut self, name: &str, args: Value) -> Result<Value, RpcError> {
        self.request("tools/call", json!({"name": name, "arguments": args}))
            .await
    }

    /// `tools/list` convenience: the advertised tool definitions.
    #[cfg(feature = "agent")]
    pub(crate) async fn list_tools(&mut self) -> Result<Vec<Value>, RpcError> {
        let result = self.request("tools/list", json!({})).await?;
        Ok(result
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// End the session cleanly: close stdin so the child's stdio loop exits
    /// and flushes, give it a moment to leave on its own, then make sure.
    pub(crate) async fn shutdown(mut self) {
        drop(self.stdin.take());
        if tokio::time::timeout(Duration::from_secs(2), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.start_kill();
            let _ = self.child.wait().await;
        }
    }

    async fn send(&mut self, msg: &Value) -> Result<(), RpcError> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| RpcError::Transport("server stdin already closed".into()))?;
        let line = format!("{msg}\n");
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| RpcError::Transport(format!("write to server failed: {e}")))?;
        stdin
            .flush()
            .await
            .map_err(|e| RpcError::Transport(format!("flush to server failed: {e}")))
    }

    /// Read line-delimited JSON until the response with `want` arrives,
    /// skipping notifications and non-JSON log lines. Bounded by
    /// `RESPONSE_TIMEOUT` so a wedged child cannot hang the run silently.
    async fn recv_id(&mut self, want: i64) -> Result<Value, RpcError> {
        let read = async {
            loop {
                let mut line = String::new();
                let n =
                    self.reader.read_line(&mut line).await.map_err(|e| {
                        RpcError::Transport(format!("read from server failed: {e}"))
                    })?;
                if n == 0 {
                    return Err(RpcError::Transport(
                        "server closed the connection unexpectedly".into(),
                    ));
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let v: Value = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(_) => continue, // non-JSON log line on the pipe
                };
                // An error with `id: null` is the JSON-RPC parse-error shape —
                // it answers no request in particular and nothing else will,
                // so waiting past it would spin forever.
                if v.get("id").is_some_and(Value::is_null) && v.get("error").is_some() {
                    return Err(RpcError::Transport(format!(
                        "server reported a protocol error: {}",
                        v["error"]
                    )));
                }
                if v.get("id").and_then(Value::as_i64) == Some(want) {
                    return Ok(v);
                }
            }
        };
        match tokio::time::timeout(RESPONSE_TIMEOUT, read).await {
            Ok(r) => r,
            Err(_) => Err(RpcError::Transport(format!(
                "server gave no response within {}s",
                RESPONSE_TIMEOUT.as_secs()
            ))),
        }
    }
}

impl Drop for StdioClient {
    fn drop(&mut self) {
        // Backstop for early-error paths that never reach `shutdown`: closing
        // stdin ends the child's stdio loop; the kill guarantees no orphan if
        // it is past reading. tokio's reaper collects the zombie.
        drop(self.stdin.take());
        let _ = self.child.start_kill();
    }
}
