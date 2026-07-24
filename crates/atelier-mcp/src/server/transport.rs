//! Transports: how bytes reach the server. Stdio is the default (one client,
//! one process); Streamable HTTP serves many clients off one shared studio.
//! The tool surface below this layer neither knows nor cares which is active.

use rmcp::ServiceExt;

use atelier_studio::Studio;

use super::Atelier;
use super::recorder::Recorder;

/// Run over stdio (default transport). `record` enables session recording to a
/// recipe at that path.
pub async fn run(record: Option<std::path::PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    let mut atelier = Atelier::new();
    if let Some(path) = record {
        tracing::info!(path = %path.display(), "session recording on");
        atelier = atelier.with_recording(path);
    }
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        transport = "stdio",
        "atelier MCP server starting"
    );
    let service = atelier.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    tracing::info!("atelier MCP server stopped (client closed stdio)");
    Ok(())
}

/// Run as a networked MCP server over Streamable HTTP at `addr`, mounted at
/// `/mcp`. Stateless: no sessions, every POST self-contained; one shared studio
/// backs all clients (writes serialised by its Mutex).
/// `allowed_hosts` extends the loopback default for LAN/remote `Host` validation
/// (DNS-rebinding guard); pass the host(s) clients will use.
pub async fn run_http(
    addr: &str,
    allowed_hosts: Vec<String>,
    record: Option<std::path::PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::never::NeverSessionManager,
    };

    // Shared studio across all HTTP sessions.
    let studio = std::sync::Arc::new(std::sync::Mutex::new(Studio::new()));
    // One recorder shared across sessions so every call lands in one recipe.
    if let Some(path) = &record {
        tracing::info!(path = %path.display(), "session recording on");
    }
    let recorder = record.map(Recorder::new);
    let mut config = StreamableHttpServerConfig::default();
    for h in allowed_hosts {
        if !config.allowed_hosts.contains(&h) {
            config.allowed_hosts.push(h);
        }
    }
    // Stateless by design. The server keeps no per-session state — documents
    // load and save from disk on every call, and the studio, write-order lock,
    // and recorder are all process-global — so a session id would name
    // nothing. Running stateful anyway meant sessions could *die* (idle
    // eviction, daemon restart), and clients that idled through the timeout
    // came back to "Session not found" and gave up. With no sessions there is
    // nothing to lose: every POST is self-contained, a daemon restart is
    // invisible, and plain JSON responses replace SSE framing (nothing here
    // streams server→client).
    config.stateful_mode = false;
    config.json_response = true;
    // One write-order lock shared by every session, like the studio itself —
    // per-session locks would order nothing.
    let write_order = std::sync::Arc::new(tokio::sync::Mutex::new(()));
    let factory = {
        let studio = studio.clone();
        let recorder = recorder.clone();
        move || {
            let mut atelier = Atelier::with_studio(studio.clone());
            atelier.recorder = recorder.clone();
            atelier.write_order = write_order.clone();
            Ok(atelier)
        }
    };
    let service: StreamableHttpService<Atelier, NeverSessionManager> =
        StreamableHttpService::new(factory, Default::default(), config);

    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .inspect_err(|e| tracing::error!(%addr, error = %e, "cannot bind HTTP listener"))?;
    let local = listener.local_addr()?;
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        endpoint = %format!("http://{local}/mcp"),
        "atelier MCP server listening"
    );
    // with_connect_info stamps each request with the TCP peer address, which
    // the per-call log uses as the default caller identity.
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("shutdown signal received");
    })
    .await?;
    tracing::info!("atelier MCP server stopped");
    Ok(())
}
