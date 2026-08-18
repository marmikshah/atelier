//! Transports: how bytes reach the server. Stdio is the default (one client,
//! one process); Streamable HTTP serves many clients off one shared studio.
//! The tool surface below this layer neither knows nor cares which is active.

use rmcp::ServiceExt;

use super::Atelier;

/// Tool arguments are JSON and never contain uploaded image bytes. One MiB is
/// ample for dense paint grids and long point lists while bounding both fixed
/// and chunked request bodies before rmcp collects them in memory.
const MAX_HTTP_REQUEST_BYTES: usize = 1024 * 1024;
/// Bound slow uploads and the number of request bodies/handlers resident at
/// once. Saturation fails fast instead of building an unbounded task queue.
const HTTP_BODY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const MAX_HTTP_IN_FLIGHT: usize = 64;

fn invalid_config(message: impl Into<String>) -> Box<dyn std::error::Error> {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message.into()).into()
}

fn http_token_from_env() -> Result<Option<String>, Box<dyn std::error::Error>> {
    let Some(raw) = std::env::var_os("ATELIER_HTTP_TOKEN") else {
        return Ok(None);
    };
    let token = raw
        .into_string()
        .map_err(|_| invalid_config("ATELIER_HTTP_TOKEN must be valid UTF-8"))?;
    validate_http_token(&token)?;
    Ok(Some(token))
}

fn validate_http_token(token: &str) -> Result<(), Box<dyn std::error::Error>> {
    if token.is_empty() || token.chars().any(char::is_whitespace) {
        return Err(invalid_config(
            "ATELIER_HTTP_TOKEN must be non-empty and contain no whitespace",
        ));
    }
    Ok(())
}

fn http_root_from_env(
    name: &str,
) -> Result<Option<std::path::PathBuf>, Box<dyn std::error::Error>> {
    let Some(raw) = std::env::var_os(name) else {
        return Ok(None);
    };
    if raw.is_empty() {
        return Err(invalid_config(format!("{name} cannot be empty")));
    }
    let path = std::path::PathBuf::from(raw);
    let canonical = std::fs::canonicalize(&path).map_err(|error| {
        invalid_config(format!(
            "cannot resolve configured {name} {}: {error}",
            path.display()
        ))
    })?;
    if !canonical.is_dir() {
        return Err(invalid_config(format!(
            "configured {name} is not a directory: {}",
            canonical.display()
        )));
    }
    Ok(Some(canonical))
}

fn validate_bind_access(
    addresses: &[std::net::SocketAddr],
    token: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    if addresses.is_empty() {
        return Err(invalid_config("HTTP bind address resolved to no endpoints"));
    }
    if addresses.iter().any(|address| !address.ip().is_loopback()) && token.is_none() {
        return Err(invalid_config(
            "refusing non-loopback HTTP bind without ATELIER_HTTP_TOKEN",
        ));
    }
    Ok(())
}

async fn limit_http_body(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let (parts, body) = request.into_parts();
    match tokio::time::timeout(
        HTTP_BODY_TIMEOUT,
        axum::body::to_bytes(body, MAX_HTTP_REQUEST_BYTES),
    )
    .await
    {
        Ok(Ok(bytes)) => {
            let request = axum::http::Request::from_parts(parts, axum::body::Body::from(bytes));
            next.run(request).await
        }
        Ok(Err(_)) => axum::http::StatusCode::PAYLOAD_TOO_LARGE.into_response(),
        Err(_) => axum::http::StatusCode::REQUEST_TIMEOUT.into_response(),
    }
}

/// Run over stdio (default transport).
pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let atelier = Atelier::new();
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
/// path handle backs all clients. Mutations are ordered by the shared async
/// lock and the document store's advisory file lock.
/// `allowed_hosts` extends the loopback default for LAN/remote `Host` validation
/// (DNS-rebinding guard); pass the host(s) clients will use. Non-loopback binds
/// require `ATELIER_HTTP_TOKEN`. When that variable is set, every request must
/// send `Authorization: Bearer <token>`.
pub async fn run_http(
    addr: &str,
    allowed_hosts: Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    use axum::http::header::{AUTHORIZATION, HeaderValue, WWW_AUTHENTICATE};
    use axum::response::IntoResponse;
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::never::NeverSessionManager,
    };

    let token = http_token_from_env()?;
    let import_root = http_root_from_env("ATELIER_IMPORT_ROOT")?;
    let export_root = http_root_from_env("ATELIER_EXPORT_ROOT")?;
    // Validate the address the OS actually bound. Resolving first and binding
    // the hostname a second time would leave a DNS-change window between the
    // security decision and the listener creation.
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .inspect_err(|e| tracing::error!(%addr, error = %e, "cannot bind HTTP listener"))?;
    let local = listener.local_addr()?;
    validate_bind_access(&[local], token.as_deref())?;

    // Build the immutable tool router once. Each stateless request gets a cheap
    // clone sharing that router and the single write-order lock; `Studio`
    // itself is only a cloneable document-store path.
    let atelier = Atelier::new().with_http_paths(import_root, export_root);
    let mut config = StreamableHttpServerConfig::default();
    for h in allowed_hosts {
        if !config.allowed_hosts.contains(&h) {
            config.allowed_hosts.push(h);
        }
    }
    // Stateless by design. The server keeps no per-session state — documents
    // load and save from disk on every call, and the studio and write-order
    // lock are process-global — so a session id would name nothing. Running
    // stateful anyway meant sessions could *die* (idle
    // eviction, daemon restart), and clients that idled through the timeout
    // came back to "Session not found" and gave up. With no sessions there is
    // nothing to lose: every POST is self-contained, a daemon restart is
    // invisible, and plain JSON responses replace SSE framing (nothing here
    // streams server→client).
    config.stateful_mode = false;
    config.json_response = true;
    let factory = move || Ok(atelier.clone());
    let service: StreamableHttpService<Atelier, NeverSessionManager> =
        StreamableHttpService::new(factory, Default::default(), config);

    // rmcp accepts a raw Tower service and collects request bodies itself, so
    // Axum's extractor-only default limit does not apply. Collect once with a
    // hard ceiling and pass a bounded in-memory body to rmcp.
    let permits = std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_HTTP_IN_FLIGHT));
    let router = axum::Router::new()
        .nest_service("/mcp", service)
        .layer(axum::middleware::from_fn(limit_http_body))
        .layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let permits = permits.clone();
                async move {
                    use axum::response::IntoResponse;

                    let Ok(_permit) = permits.try_acquire_owned() else {
                        let mut response =
                            axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
                        response.headers_mut().insert(
                            axum::http::header::RETRY_AFTER,
                            axum::http::HeaderValue::from_static("1"),
                        );
                        return response;
                    };
                    next.run(request).await
                }
            },
        ));
    let router = if let Some(token) = &token {
        let expected = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| invalid_config("ATELIER_HTTP_TOKEN is not a valid bearer token"))?;
        router.layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let expected = expected.clone();
                async move {
                    if request.headers().get(AUTHORIZATION) == Some(&expected) {
                        next.run(request).await
                    } else {
                        let mut response = axum::http::StatusCode::UNAUTHORIZED.into_response();
                        response
                            .headers_mut()
                            .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
                        response
                    }
                }
            },
        ))
    } else {
        router
    };
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        endpoint = %format!("http://{local}/mcp"),
        authentication = if token.is_some() { "bearer" } else { "none (loopback only)" },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_loopback_bind_requires_a_token() {
        let loopback = ["127.0.0.1:8765".parse().unwrap()];
        let wildcard = ["0.0.0.0:8765".parse().unwrap()];
        assert!(validate_bind_access(&loopback, None).is_ok());
        assert!(validate_bind_access(&wildcard, Some("secret")).is_ok());
        assert!(
            validate_bind_access(&wildcard, None)
                .unwrap_err()
                .to_string()
                .contains("ATELIER_HTTP_TOKEN")
        );
    }

    #[test]
    fn bearer_token_must_be_non_empty_without_whitespace() {
        assert!(validate_http_token("opaque-token_123").is_ok());
        for invalid in ["", "two words", "line\nbreak", " trailing"] {
            assert!(
                validate_http_token(invalid).is_err(),
                "accepted {invalid:?}"
            );
        }
    }
}
