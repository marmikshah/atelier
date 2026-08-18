//! End-to-end smoke for the Streamable HTTP MCP transport. This deliberately
//! uses only `std` HTTP so the transport contract does not add a second client
//! stack to the shipped dependency graph.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use atelier_mcp::server::Atelier;
use serde_json::{Value, json};

const PROTOCOL_VERSION: &str = "2025-06-18";
const HTTP_REQUEST_LIMIT: usize = 1024 * 1024;
static SERVER_ID: AtomicUsize = AtomicUsize::new(0);

struct Server {
    child: Child,
    addr: SocketAddr,
}

impl Server {
    fn spawn() -> Self {
        Self::spawn_with_env(&[])
    }

    fn spawn_with_env(extra_env: &[(&str, &str)]) -> Self {
        // Reserve an ephemeral loopback port, then hand it to the real binary.
        let listener = TcpListener::bind("127.0.0.1:0").expect("reserve HTTP port");
        let addr = listener.local_addr().expect("reserved address");
        drop(listener);

        let id = SERVER_ID.fetch_add(1, Ordering::Relaxed);
        let home =
            std::env::temp_dir().join(format!("atelier-http-smoke-{}-{id}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let mut command = Command::new(env!("CARGO_BIN_EXE_atelier"));
        command
            .args(["--http", &addr.to_string()])
            .env("ATELIER_HOME", home)
            .env("ATELIER_LOG", "off")
            .env_remove("ATELIER_HTTP_TOKEN")
            .env_remove("ATELIER_IMPORT_ROOT")
            .env_remove("ATELIER_EXPORT_ROOT")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (name, value) in extra_env {
            command.env(name, value);
        }
        let child = command.spawn().expect("spawn atelier HTTP server");

        let deadline = Instant::now() + Duration::from_secs(10);
        while TcpStream::connect_timeout(&addr, Duration::from_millis(100)).is_err() {
            assert!(
                Instant::now() < deadline,
                "HTTP server did not listen within 10s"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        Self { child, addr }
    }

    fn post(&self, message: &Value) -> HttpResponse {
        self.post_with_bearer(message, None)
    }

    fn post_with_bearer(&self, message: &Value, token: Option<&str>) -> HttpResponse {
        let body = message.to_string();
        let authorization = token
            .map(|token| format!("Authorization: Bearer {token}\r\n"))
            .unwrap_or_default();
        let request = format!(
            "POST /mcp HTTP/1.1\r\n\
             Host: {}\r\n\
             Content-Type: application/json\r\n\
             Accept: application/json, text/event-stream\r\n\
             MCP-Protocol-Version: {PROTOCOL_VERSION}\r\n\
             {authorization}\
             Connection: close\r\n\
             Content-Length: {}\r\n\
             \r\n\
             {body}",
            self.addr,
            body.len()
        );
        self.raw_request(request.as_bytes())
    }

    fn raw_request(&self, request: &[u8]) -> HttpResponse {
        let mut stream = TcpStream::connect_timeout(&self.addr, Duration::from_secs(5))
            .expect("connect to HTTP server");
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .expect("set read timeout");
        stream.write_all(request).expect("write HTTP request");
        stream.flush().expect("flush HTTP request");

        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).expect("read HTTP response");
        HttpResponse::parse(&raw)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct HttpResponse {
    status: u16,
    body: Vec<u8>,
}

impl HttpResponse {
    fn parse(raw: &[u8]) -> Self {
        let split = raw
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("HTTP response has headers");
        let headers = String::from_utf8_lossy(&raw[..split]);
        let status = headers
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|status| status.parse().ok())
            .expect("HTTP response has a numeric status");
        let mut body = raw[split + 4..].to_vec();
        if headers
            .lines()
            .any(|line| line.eq_ignore_ascii_case("transfer-encoding: chunked"))
        {
            body = decode_chunked(&body);
        }
        Self { status, body }
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|error| {
            panic!(
                "HTTP response was not JSON ({error}): {}",
                String::from_utf8_lossy(&self.body)
            )
        })
    }
}

fn decode_chunked(mut encoded: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::new();
    loop {
        let line_end = encoded
            .windows(2)
            .position(|window| window == b"\r\n")
            .expect("chunk has a size line");
        let size_text = std::str::from_utf8(&encoded[..line_end])
            .expect("chunk size is ASCII")
            .split(';')
            .next()
            .expect("chunk size exists");
        let size = usize::from_str_radix(size_text, 16).expect("chunk size is hexadecimal");
        encoded = &encoded[line_end + 2..];
        if size == 0 {
            break;
        }
        assert!(encoded.len() >= size + 2, "chunk body is complete");
        decoded.extend_from_slice(&encoded[..size]);
        assert_eq!(&encoded[size..size + 2], b"\r\n", "chunk ends in CRLF");
        encoded = &encoded[size + 2..];
    }
    decoded
}

fn advertised_tool_names(list: &Value) -> Vec<String> {
    let mut names: Vec<String> = list["result"]["tools"]
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

#[test]
fn http_server_handshakes_lists_and_calls_the_same_registry() {
    let server = Server::spawn();

    let init = server.post(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "http-smoke", "version": "0"}
        }
    }));
    assert_eq!(init.status, 200, "initialize response: {:?}", init.body);
    let init = init.json();
    assert!(
        init["result"]["capabilities"]["tools"].is_object(),
        "tools capability advertised: {init}"
    );
    assert_eq!(
        init["result"]["serverInfo"]["name"], "atelier",
        "server identity: {init}"
    );
    assert_eq!(
        init["result"]["serverInfo"]["version"],
        env!("CARGO_PKG_VERSION"),
        "server version: {init}"
    );

    let initialized = server.post(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    }));
    assert_eq!(initialized.status, 202);

    let list = server.post(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    }));
    assert_eq!(list.status, 200, "tools/list response: {:?}", list.body);
    let list = list.json();
    assert!(list.get("error").is_none(), "tools/list failed: {list}");
    assert_eq!(
        advertised_tool_names(&list),
        registry_tool_names(),
        "HTTP must advertise the canonical registry exactly"
    );
    assert!(
        list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|tool| tool["annotations"]["readOnlyHint"].is_boolean()),
        "all tools advertise MCP annotations: {list}"
    );

    let created = server.post(&json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "doc_new",
            "arguments": {"name": "smoke-http", "width": 8, "height": 8}
        }
    }));
    assert_eq!(
        created.status, 200,
        "tools/call response: {:?}",
        created.body
    );
    let created = created.json();
    assert!(
        created.get("error").is_none(),
        "tools/call failed: {created}"
    );
    let text = created["result"]["content"]
        .as_array()
        .and_then(|content| content.iter().find_map(|block| block.get("text")))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let created_text: Value = serde_json::from_str(text).expect("doc_new returned JSON text");
    assert_eq!(
        created["result"]["structuredContent"], created_text,
        "structured content must mirror JSON text"
    );
    let doc_id = created_text["doc_id"]
        .as_str()
        .expect("doc_new returned its opaque doc_id");
    assert!(
        atelier_studio::DocumentId::parse(doc_id).is_ok(),
        "create payload: {created_text}"
    );

    // The request works even though every POST opens a fresh TCP connection:
    // document identity lives in the explicit tool arguments, not transport
    // or process state. Metadata names the caller for logs only.
    let info = server.post(&json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {
            "_meta": {
                "io.github.marmikshah.atelier/session": "http-smoke"
            },
            "name": "doc_info",
            "arguments": {"doc_id": doc_id}
        }
    }));
    assert_eq!(
        info.status, 200,
        "explicit-id call response: {:?}",
        info.body
    );
    let info = info.json();
    assert!(info.get("error").is_none(), "context call failed: {info}");
    let text = info["result"]["content"]
        .as_array()
        .and_then(|content| content.iter().find_map(|block| block.get("text")))
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(text.contains("\"w\":8"), "explicit-id call payload: {text}");
}

#[test]
fn configured_http_token_is_required_on_every_request() {
    let server = Server::spawn_with_env(&[("ATELIER_HTTP_TOKEN", "smoke-secret")]);
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "auth-smoke", "version": "0"}
        }
    });

    assert_eq!(server.post(&initialize).status, 401);
    assert_eq!(
        server
            .post_with_bearer(&initialize, Some("wrong-secret"))
            .status,
        401
    );
    let accepted = server.post_with_bearer(&initialize, Some("smoke-secret"));
    assert_eq!(
        accepted.status, 200,
        "authorized response: {:?}",
        accepted.body
    );
}

#[test]
fn http_request_bodies_are_bounded_for_fixed_and_chunked_encoding() {
    let server = Server::spawn();
    let body = vec![b'x'; HTTP_REQUEST_LIMIT + 1];

    let mut fixed = Vec::with_capacity(body.len() + 256);
    write!(
        fixed,
        "POST /mcp HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        server.addr,
        body.len()
    )
    .unwrap();
    fixed.extend_from_slice(&body);
    assert_eq!(server.raw_request(&fixed).status, 413);

    let mut chunked = Vec::with_capacity(body.len() + 280);
    write!(
        chunked,
        "POST /mcp HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n",
        server.addr,
        body.len()
    )
    .unwrap();
    chunked.extend_from_slice(&body);
    chunked.extend_from_slice(b"\r\n0\r\n\r\n");
    assert_eq!(server.raw_request(&chunked).status, 413);
}

#[test]
fn http_external_paths_are_relative_and_rooted() {
    let root = std::env::temp_dir().join(format!("atelier-http-files-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root_text = root.to_string_lossy().into_owned();
    let server = Server::spawn_with_env(&[
        ("ATELIER_IMPORT_ROOT", &root_text),
        ("ATELIER_EXPORT_ROOT", &root_text),
    ]);

    let init = server.post(&json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": PROTOCOL_VERSION, "capabilities": {},
                   "clientInfo": {"name": "path-smoke", "version": "0"}}
    }));
    assert_eq!(init.status, 200, "initialize response: {:?}", init.body);
    let created = server
        .post(&json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "doc_new", "arguments": {
                "name": "path-smoke", "width": 2, "height": 2
            }}
        }))
        .json();
    let doc_id = created["result"]["structuredContent"]["doc_id"]
        .as_str()
        .expect("doc_new returned an id");

    let exported = server
        .post(&json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "doc_export", "arguments": {
                "doc_id": doc_id, "op": "sheet", "out_path": "sheet.png", "scale": 1
            }}
        }))
        .json();
    assert!(
        exported.get("error").is_none(),
        "relative export failed: {exported}"
    );
    assert!(root.join("sheet.png").is_file());

    let attached = server
        .post(&json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {"name": "doc_ref", "arguments": {
                "doc_id": doc_id, "op": "set", "path": "sheet.png"
            }}
        }))
        .json();
    assert!(
        attached.get("error").is_none(),
        "relative import failed: {attached}"
    );

    let escaped = server
        .post(&json!({
            "jsonrpc": "2.0", "id": 5, "method": "tools/call",
            "params": {"name": "doc_export", "arguments": {
                "doc_id": doc_id, "op": "sheet", "out_path": "../escape.png"
            }}
        }))
        .json();
    assert!(
        escaped.get("error").is_some(),
        "traversal was accepted: {escaped}"
    );
    let _ = std::fs::remove_dir_all(root);
}
