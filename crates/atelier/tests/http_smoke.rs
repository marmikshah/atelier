//! End-to-end smoke for the Streamable HTTP MCP transport. This deliberately
//! uses only `std` HTTP so the transport contract does not add a second client
//! stack to the shipped dependency graph.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use atelier_mcp::server::Atelier;
use serde_json::{Value, json};

const PROTOCOL_VERSION: &str = "2025-06-18";

struct Server {
    child: Child,
    addr: SocketAddr,
}

impl Server {
    fn spawn() -> Self {
        // Reserve an ephemeral loopback port, then hand it to the real binary.
        let listener = TcpListener::bind("127.0.0.1:0").expect("reserve HTTP port");
        let addr = listener.local_addr().expect("reserved address");
        drop(listener);

        let home = std::env::temp_dir().join(format!("atelier-http-smoke-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let child = Command::new(env!("CARGO_BIN_EXE_atelier"))
            .args(["--http", &addr.to_string()])
            .env("ATELIER_HOME", home)
            .env("ATELIER_LOG", "off")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn atelier HTTP server");

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
        let body = message.to_string();
        let mut stream = TcpStream::connect_timeout(&self.addr, Duration::from_secs(5))
            .expect("connect to HTTP server");
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .expect("set read timeout");
        write!(
            stream,
            "POST /mcp HTTP/1.1\r\n\
             Host: {}\r\n\
             Content-Type: application/json\r\n\
             Accept: application/json, text/event-stream\r\n\
             MCP-Protocol-Version: {PROTOCOL_VERSION}\r\n\
             Connection: close\r\n\
             Content-Length: {}\r\n\
             \r\n\
             {body}",
            self.addr,
            body.len()
        )
        .expect("write HTTP request");
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
    let created: Value = serde_json::from_str(text).expect("doc_new returned JSON text");
    let doc_id = created["doc_id"]
        .as_str()
        .expect("doc_new returned its opaque doc_id");
    assert!(doc_id.starts_with("d_"), "create payload: {created}");

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
