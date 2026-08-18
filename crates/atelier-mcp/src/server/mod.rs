//! atelier MCP server (rmcp). Exposes the headless document editor as MCP
//! tools over stdio or Streamable HTTP. Tools return JSON text content; studio
//! errors keep the {"error": ...} payload AND set `is_error` so MCP harnesses
//! flag the failure instead of treating it as a success.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock as Content, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerInfo, ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, tool_handler};
use serde_json::{Value, json};

use atelier_studio::{AlphaMode, Studio, ToolName};

mod params;
mod toolsdoc;
mod transport;

pub use toolsdoc::{tools_html, tools_text};
pub use transport::{run, run_http};

use params::*;

/// Encoded PNG bytes accepted for an inline MCP image. Base64 expands this by
/// another third, so keep the pre-encoding cap deliberately conservative.
const MAX_INLINE_PNG_BYTES: usize = 8 * 1024 * 1024;

/// Standard base64 for MCP image blocks. Kept here because inline images are
/// part of tool results; Atelier no longer exposes a second, duplicate resource
/// API for document renders.
fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(TABLE[(n >> 18 & 63) as usize] as char);
        out.push(TABLE[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Wraps a studio result as a tool result: Ok becomes a JSON text part; errors
/// carry a machine-readable {"error": ...} payload and set `is_error` so every
/// caller gets the same failure.
fn res(r: Result<Value, String>) -> CallToolResult {
    match r {
        Ok(v) => CallToolResult::structured(v),
        Err(e) => CallToolResult::structured_error(json!({"error": e})),
    }
}

/// Wrap an image-producing studio result as MCP content: an inline PNG (so the
/// agent SEES the pixels in the same turn — no separate file read) plus a JSON
/// text part with the measured stats. Errors come back as a `{"error": ...}`
/// text part with `is_error` set, matching `res`.
fn img_result(r: Result<(Vec<u8>, Value), String>) -> CallToolResult {
    match r {
        Ok((png, mut report)) if png.len() > MAX_INLINE_PNG_BYTES => {
            if report.get("path").and_then(Value::as_str).is_some() {
                if let Value::Object(fields) = &mut report {
                    fields.insert(
                        "inline_image_omitted".into(),
                        json!({
                            "bytes": png.len(),
                            "limit": MAX_INLINE_PNG_BYTES,
                            "reason": "saved image exceeds the MCP inline-image limit",
                        }),
                    );
                }
                res(Ok(report))
            } else {
                CallToolResult::structured_error(json!({
                    "error": format!(
                        "inline PNG is {} bytes; limit is {} bytes. Reduce the render size, use a tool out_path where available, or use `atelier call --image-out PATH`",
                        png.len(),
                        MAX_INLINE_PNG_BYTES,
                    ),
                    "inline_bytes": png.len(),
                    "max_inline_bytes": MAX_INLINE_PNG_BYTES,
                }))
            }
        }
        Ok((png, report)) => {
            let mut result = CallToolResult::structured(report);
            result
                .content
                .insert(0, Content::image(base64(&png), "image/png"));
            result
        }
        Err(e) => CallToolResult::structured_error(json!({"error": e})),
    }
}

/// Like `img_result`, but the PNG is optional: tools whose preview is
/// conditional (diff overlays, seam overlays) degrade to a plain text result.
fn opt_img_result(r: Result<(Option<Vec<u8>>, Value), String>) -> CallToolResult {
    match r {
        Ok((Some(png), report)) => img_result(Ok((png, report))),
        Ok((None, report)) => res(Ok(report)),
        Err(e) => res(Err(e)),
    }
}

/// Acknowledge an edit op with its TEXT report only — no inline preview image.
/// doc_look is the agent's only eye: returning a preview PNG from every edit
/// tool taxed every LLM client with image tokens (an upscaled frame is tens of
/// thousands of tokens) and undercut the deliberate see-and-fix loop. If the
/// agent needs to see the result, it calls doc_look.
fn edited(r: Result<Value, String>) -> CallToolResult {
    res(r)
}

/// Preserve a successful handler result while making a post-commit durability
/// warning visible to both structured MCP clients and text-compatible clients.
/// The mutation is already visible at this point, so this must stay a success:
/// treating it as a failure would invite a duplicate retry.
fn attach_commit_warning(mut result: CallToolResult, warning: &str) -> CallToolResult {
    let detail = json!({
        "committed": true,
        "durability": "uncertain",
        "retry": false,
        "message": warning,
    });
    let mut payload = result_json(&result).unwrap_or_else(|| json!({"ok": true}));
    match &mut payload {
        Value::Object(report) => {
            report.insert("commit_warning".into(), detail);
        }
        _ => {
            payload = json!({"result": payload, "commit_warning": detail});
        }
    }
    let encoded = payload.to_string();
    result.structured_content = Some(payload);
    if let Some(text) = result.content.iter_mut().find_map(|content| match content {
        Content::Text(text) => Some(text),
        _ => None,
    }) {
        text.text = encoded;
    } else {
        result.content.push(Content::text(encoded));
    }
    result
}

/// A list of `[r,g,b(,a)]` arrays -> a palette of RGBA swatches.
fn palette_list(v: &[Vec<i64>]) -> Result<Vec<[u8; 4]>, String> {
    v.iter().map(|c| rgba(c)).collect()
}

/// [r,g,b] or [r,g,b,a] -> RGBA, STRICT: exactly 3..=4 components, each
/// 0..=255 — the same shape the operation validator enforces for draw/FX.
/// The typed tool paths used to truncate via `as u8` (300 → 44, -1 → 255)
/// into a wrong-but-plausible colour the agent then painted with.
fn rgba(v: &[i64]) -> Result<[u8; 4], String> {
    let ok = (3..=4).contains(&v.len()) && v.iter().all(|n| (0..=255).contains(n));
    if !ok {
        return Err(format!(
            "colour must be [r,g,b] or [r,g,b,a] with 0..=255 values, got {v:?}"
        ));
    }
    Ok([
        v[0] as u8,
        v[1] as u8,
        v[2] as u8,
        if v.len() == 4 { v[3] as u8 } else { 255 },
    ])
}

/// Parse the `doc_palette op=snap` / FX alpha policy string into an `AlphaSnap`.
/// The one place the mode strings are known — an unknown mode errors here
/// instead of silently mapping to Preserve.
fn alpha_snap(
    mode: AlphaMode,
    cutoff: Option<u8>,
    bg: Option<&[i64]>,
) -> Result<atelier_core::document::AlphaSnap, String> {
    use atelier_core::document::AlphaSnap;
    match mode {
        AlphaMode::Preserve => Ok(AlphaSnap::Preserve),
        AlphaMode::Opaque => Ok(AlphaSnap::Opaque(cutoff.unwrap_or(128))),
        AlphaMode::Flatten => Ok(AlphaSnap::Flatten(match bg {
            Some(b) => rgba(b)?,
            None => [0, 0, 0, 255],
        })),
    }
}

/// Fixed-size JSON region -> the tuple used by the document APIs.
fn region(value: Option<[i32; 4]>) -> Option<(i32, i32, i32, i32)> {
    value.map(|[x0, y0, x1, y1]| (x0, y0, x1, y1))
}

/// Unwrap a parse result inside a tool method, or return it as the tool error.
macro_rules! try_res {
    ($e:expr_2021) => {
        match $e {
            Ok(v) => v,
            Err(e) => return res(Err(e)),
        }
    };
}

/// The JSON report a tool answered with. Scans for the first TEXT part rather
/// than taking `content[0]`: the image-returning tools put the PNG first and the
/// stats after it. Public so the binary's CLI/replay paths read results the
/// same way the server does.
pub fn result_json(result: &rmcp::model::CallToolResult) -> Option<Value> {
    result.structured_content.clone().or_else(|| {
        result
            .content
            .iter()
            .filter_map(|c| c.as_text())
            .find_map(|t| serde_json::from_str::<Value>(&t.text).ok())
    })
}

/// Remove schemars' Rust-type `format` annotations (`uint32`, `int64`,
/// `float`, …) everywhere in a schema. They carry no JSON Schema meaning —
/// the real constraints (`type`, `minimum`) are stated alongside — and
/// Ajv-based clients log a warning per occurrence. Standard formats
/// (`date-time`, `uri`, …) are kept.
fn strip_nonstandard_formats(schema: &mut Value) {
    match schema {
        Value::Object(obj) => {
            let drop = obj.get("format").and_then(Value::as_str).is_some_and(|f| {
                matches!(
                    f,
                    "uint8"
                        | "uint16"
                        | "uint32"
                        | "uint64"
                        | "uint128"
                        | "uint"
                        | "int8"
                        | "int16"
                        | "int32"
                        | "int64"
                        | "int128"
                        | "int"
                        | "float"
                        | "double"
                )
            });
            if drop {
                obj.remove("format");
            }
            for v in obj.values_mut() {
                strip_nonstandard_formats(v);
            }
        }
        Value::Array(items) => {
            for v in items {
                strip_nonstandard_formats(v);
            }
        }
        _ => {}
    }
}

/// Conservative MCP hints for clients that show approval UI. Hubs with both
/// read and write operations advertise the more permissive behavior; hints
/// never replace Atelier's own validation or authorization.
fn tool_annotations(tool: ToolName) -> ToolAnnotations {
    let read_only = matches!(
        tool,
        ToolName::ListDocs
            | ToolName::DocInfo
            | ToolName::DocDumpRegion
            | ToolName::DocSilhouette
            | ToolName::DocComponents
            | ToolName::DocAnimAudit
            | ToolName::DocCritique
            | ToolName::DocContactSheet
    );
    let open_world = matches!(
        tool,
        ToolName::DocLook
            | ToolName::DocFrameDiff
            | ToolName::DocSeamReport
            | ToolName::DocRef
            | ToolName::DocExport
    );
    let destructive = !read_only && !matches!(tool, ToolName::DocNew | ToolName::DocAddTag);
    let idempotent = read_only
        || matches!(
            tool,
            ToolName::DeleteDoc
                | ToolName::DocLook
                | ToolName::DocFrameDiff
                | ToolName::DocSeamReport
                | ToolName::DocExport
        );
    ToolAnnotations::new()
        .read_only(read_only)
        .destructive(destructive)
        .idempotent(idempotent)
        .open_world(open_world)
}

#[derive(Clone, Debug, Default)]
struct HttpPathRoots {
    import: Option<std::path::PathBuf>,
    export: Option<std::path::PathBuf>,
}

impl HttpPathRoots {
    fn new(import: Option<std::path::PathBuf>, export: Option<std::path::PathBuf>) -> Self {
        Self { import, export }
    }

    /// Resolve only the parameters that can touch files outside the document
    /// store. HTTP callers submit relative paths; CLI, replay, and stdio never
    /// enter this policy and retain their native filesystem behavior.
    fn rewrite(&self, tool: ToolName, args: &mut Value) -> Result<(), ErrorData> {
        match tool {
            ToolName::DocRef => rewrite_http_path(
                args,
                "path",
                self.import.as_deref(),
                "ATELIER_IMPORT_ROOT",
                true,
            ),
            ToolName::DocLook | ToolName::DocFrameDiff | ToolName::DocSeamReport => {
                rewrite_http_path(
                    args,
                    "out_path",
                    self.export.as_deref(),
                    "ATELIER_EXPORT_ROOT",
                    false,
                )
            }
            ToolName::DocExport => rewrite_http_path(
                args,
                "out_path",
                self.export.as_deref(),
                "ATELIER_EXPORT_ROOT",
                false,
            ),
            _ => Ok(()),
        }
    }
}

fn rewrite_http_path(
    args: &mut Value,
    field: &str,
    root: Option<&std::path::Path>,
    root_env: &str,
    must_exist: bool,
) -> Result<(), ErrorData> {
    let Some(slot) = args.get_mut(field) else {
        return Ok(());
    };
    if slot.is_null() {
        return Ok(());
    }
    let raw = slot.as_str().ok_or_else(|| {
        ErrorData::invalid_params(
            format!("HTTP `{field}` must be a relative path string"),
            None,
        )
    })?;
    let resolved = resolve_http_path(root, raw, root_env, must_exist)
        .map_err(|error| ErrorData::invalid_params(error, None))?;
    *slot = Value::String(resolved.to_string_lossy().into_owned());
    Ok(())
}

fn resolve_http_path(
    root: Option<&std::path::Path>,
    raw: &str,
    root_env: &str,
    must_exist: bool,
) -> Result<std::path::PathBuf, String> {
    use std::path::Component;

    let Some(root) = root else {
        return Err(format!(
            "external file paths are disabled over HTTP; configure {root_env} and pass a relative path"
        ));
    };
    let relative = std::path::Path::new(raw);
    if raw.is_empty() || relative.is_absolute() {
        return Err(format!(
            "HTTP file path must be non-empty and relative, got `{raw}`"
        ));
    }
    if relative.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(format!(
            "HTTP file path cannot be absolute or contain parent traversal, got `{raw}`"
        ));
    }

    let joined = root.join(relative);
    let resolved = if must_exist {
        std::fs::canonicalize(&joined)
            .map_err(|error| format!("cannot access HTTP input `{raw}`: {error}"))?
    } else {
        canonical_output_path(&joined)?
    };
    if !resolved.starts_with(root) {
        return Err(format!("HTTP file path escapes configured {root_env}"));
    }
    Ok(resolved)
}

/// Resolve the nearest existing output ancestor, then append the missing
/// suffix to its canonical path. Returning this path (rather than the original
/// lexical path) prevents an already-present symlink from being followed a
/// second time after validation.
fn canonical_output_path(path: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let mut current = path;
    let mut suffix = Vec::new();
    loop {
        match std::fs::symlink_metadata(current) {
            Ok(_) => {
                let mut resolved = std::fs::canonicalize(current).map_err(|error| {
                    format!("cannot resolve HTTP output `{}`: {error}", path.display())
                })?;
                for component in suffix.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = current
                    .file_name()
                    .ok_or_else(|| format!("HTTP output `{}` has no file name", path.display()))?;
                suffix.push(name.to_os_string());
                current = current.parent().ok_or_else(|| {
                    format!("HTTP output `{}` has no existing ancestor", path.display())
                })?;
            }
            Err(error) => {
                return Err(format!(
                    "cannot inspect HTTP output `{}`: {error}",
                    path.display()
                ));
            }
        }
    }
}

/// True when a result is a failure — either flagged `is_error` or carrying a
/// `{"error": ...}` text payload. Public so CLI/replay exit codes and fail-fast
/// agree with the server's journaling decision.
pub fn is_error_result(result: &rmcp::model::CallToolResult) -> bool {
    if result.is_error == Some(true) {
        return true;
    }
    result
        .content
        .first()
        .and_then(|c| c.as_text())
        .and_then(|t| serde_json::from_str::<Value>(&t.text).ok())
        .is_some_and(|v| v.get("error").is_some())
}

/// Hub-operation discriminator. Editing hubs use `op`; checkpoints use
/// `action`.
fn call_op(args: &Value) -> Option<&str> {
    args.get("op")
        .or_else(|| args.get("action"))
        .and_then(Value::as_str)
}

/// One log line per completed tool call: name, `op` variant, target document,
/// duration, and — for application errors (`{"error": ...}` payloads, which are
/// `Ok` at the protocol level and otherwise invisible) — the error text at
/// `warn`. This is the observability for the whole tool surface; keep it on
/// the single dispatch path so no tool can dodge it.
fn log_call(
    tool: ToolName,
    args: &Value,
    caller: &str,
    result: &rmcp::model::CallToolResult,
    elapsed: std::time::Duration,
) {
    let op = call_op(args).unwrap_or("-");
    let doc = journal_target(tool, args, result)
        .or_else(|| {
            args.get("doc_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "-".into());
    let ms = elapsed.as_millis();
    match result_json(result).and_then(|v| {
        v.get("error").map(|e| {
            e.as_str()
                .map(str::to_string)
                .unwrap_or_else(|| e.to_string())
        })
    }) {
        Some(error) => {
            tracing::warn!(tool = tool.as_str(), %op, %doc, %caller, ms, %error, "tool error")
        }
        None if is_error_result(result) => {
            // is_error without a JSON error payload: surface what text there is.
            let text = result
                .content
                .first()
                .and_then(|c| c.as_text())
                .map(|t| t.text.clone())
                .unwrap_or_default();
            tracing::warn!(tool = tool.as_str(), %op, %doc, %caller, ms, error = %text, "tool error");
        }
        None => tracing::info!(tool = tool.as_str(), %op, %doc, %caller, ms, "ok"),
    }
}

// The tool surface, split by domain. Declared after the helpers above so the
// `try_res!` macro is in scope inside them.
mod tools_doc;
mod tools_draw;
mod tools_export;
mod tools_read;

// --- server ----------------------------------------------------------------

/// Optional per-call log label. It never supplies tool arguments or changes
/// execution; document, layer, and frame targets live only in the JSON call.
pub const SESSION_META_KEY: &str = "io.github.marmikshah.atelier/session";

#[derive(Clone)]
pub struct Atelier {
    /// A cheap path handle, not editor state. Documents are opened from disk
    /// for each operation; the async write-order lock and advisory store lock
    /// provide the actual concurrency guarantees.
    studio: Studio,
    /// Immutable after startup and shared by cheap `Atelier` clones (notably
    /// the stateless HTTP service's per-request handlers).
    tool_router: std::sync::Arc<ToolRouter<Self>>,
    /// Held across dispatch + journal for every mutating call (an async lock,
    /// because it spans the dispatcher's await). Without it two concurrent
    /// sessions could execute A→B and journal B→A, and the recipe would
    /// silently rebuild different art.
    write_order: std::sync::Arc<tokio::sync::Mutex<()>>,
    /// Present only on the Streamable HTTP service. Its presence is the
    /// transport boundary for external path policy; direct dispatch remains
    /// unrestricted for CLI/replay and stdio callers.
    http_paths: Option<std::sync::Arc<HttpPathRoots>>,
}

impl Atelier {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self::with_studio(Studio::new())
    }

    pub fn with_studio(studio: Studio) -> Self {
        Self {
            studio,
            tool_router: std::sync::Arc::new(Self::tool_router()),
            write_order: std::sync::Arc::new(tokio::sync::Mutex::new(())),
            http_paths: None,
        }
    }

    /// Rebind the shared handler path to a transaction's isolated store while
    /// retaining the immutable registry, process-wide write ordering, and the
    /// transport policy attached to this server.
    fn with_dispatch_studio(&self, studio: Studio) -> Self {
        Self {
            studio,
            tool_router: self.tool_router.clone(),
            write_order: self.write_order.clone(),
            http_paths: self.http_paths.clone(),
        }
    }

    fn with_http_paths(
        mut self,
        import: Option<std::path::PathBuf>,
        export: Option<std::path::PathBuf>,
    ) -> Self {
        self.http_paths = Some(std::sync::Arc::new(HttpPathRoots::new(import, export)));
        self
    }

    /// The whole tool surface: the per-domain routers (`tools_doc`, `tools_draw`,
    /// `tools_read`, `tools_export`) merged into one. An associated fn, not a
    /// method — the registry builds without a `Studio` on disk behind it.
    fn tool_router() -> ToolRouter<Self> {
        Self::doc_router() + Self::draw_router() + Self::read_router() + Self::export_router()
    }

    /// Every tool the server has. There is no profile filter: the surface is
    /// small enough that hiding part of it behind a flag would cost more in
    /// confusion than it ever saved in context.
    ///
    /// Schemas are scrubbed of schemars' Rust-integer `format` markers
    /// (`uint32` & co.) — they are not JSON Schema formats, and Ajv-based
    /// clients warn on every one of them (`unknown format "uint32" ignored`).
    fn advertised_tools(&self) -> Vec<rmcp::model::Tool> {
        Self::scrub_tool_schemas(self.tool_router.list_all())
    }

    /// The advertised tool surface without a server instance: `atelier tools`
    /// lists the registry and must not build a `Studio` to do it — `Studio::new`
    /// creates `~/.atelier/documents` on disk, a side effect a listing has no
    /// business having. The router is an associated fn, so the whole registry
    /// is instance-free.
    pub fn registry_tools() -> Vec<rmcp::model::Tool> {
        Self::scrub_tool_schemas(Self::tool_router().list_all())
    }

    fn scrub_tool_schemas(tools: Vec<rmcp::model::Tool>) -> Vec<rmcp::model::Tool> {
        tools
            .into_iter()
            .map(|mut t| {
                let mut schema = Value::Object((*t.input_schema).clone());
                strip_nonstandard_formats(&mut schema);
                if let Value::Object(obj) = schema {
                    t.input_schema = std::sync::Arc::new(obj);
                }
                if let Ok(tool) = t.name.parse::<ToolName>() {
                    t.annotations = Some(tool_annotations(tool));
                }
                t
            })
            .collect()
    }

    /// The document-store handle. `Studio` contains only the store path; there
    /// is no process-local selection, clipboard, or active-document state.
    fn studio(&self) -> &Studio {
        &self.studio
    }

    /// THE one dispatch path every caller funnels through — the MCP handler
    /// (`call_tool`), and the binary's own `atelier call` / `replay`.
    /// Logging, journaling, and write ordering live here so
    /// no caller can dodge them; transport-specific work (caller identity from
    /// HTTP headers) belongs to the transport above this.
    ///
    /// Speaks the rmcp result type because the MCP transport needs it natively;
    /// CLI callers read the text part (`result_json`) and the error flag
    /// (`is_error_result`).
    pub async fn dispatch(
        &self,
        tool: ToolName,
        args: Value,
        caller: &str,
    ) -> Result<CallToolResult, ErrorData> {
        // For mutations, hold the order lock from before the dispatch until
        // after the journal write, so journal order can never diverge from
        // execution order under concurrent sessions. Reads skip it.
        let journaled = is_journaled(tool, &args);
        let store_mutation = is_store_mutation(tool, &args);
        let write_order = self.write_order.clone();
        let _order = if store_mutation {
            Some(write_order.lock().await)
        } else {
            None
        };
        // The async order lock coordinates sessions in this server; the file
        // lock also coordinates separate CLI and daemon processes. Keep it
        // through journaling so pixels and provenance commit in one order.
        let _store_lock = {
            let studio = self.studio();
            if store_mutation {
                studio.lock_store_exclusive()
            } else {
                studio.lock_store_shared()
            }
        }
        .map_err(|error| ErrorData::internal_error(error, None))?;

        let started = std::time::Instant::now();
        let result = match if store_mutation {
            self.invoke_transaction(tool, args.clone(), journaled).await
        } else {
            self.invoke(tool, args.clone()).await
        } {
            Ok(r) => r,
            // Protocol-level failure (malformed params): the caller sees the
            // error; make sure the operator does too. Unknown names are
            // rejected while constructing `ToolName`, before dispatch.
            Err(e) => {
                tracing::error!(tool = tool.as_str(), %caller, error = %e, "tool call failed (protocol error)");
                return Err(e);
            }
        };
        log_call(tool, &args, caller, &result, started.elapsed());
        Ok(result)
    }

    /// Execute a store mutation on a private document generation. Nothing is
    /// made visible until both the handler and its recipe append have
    /// succeeded; dropping the transaction rolls back application and
    /// protocol failures as well as journal failures.
    async fn invoke_transaction(
        &self,
        tool: ToolName,
        args: Value,
        journaled: bool,
    ) -> Result<CallToolResult, ErrorData> {
        if let Err(error) = self.studio().cleanup_stale_transactions() {
            return Ok(res(Err(error)));
        }

        let initial_target = mutation_target(tool, &args).map_err(|error| {
            ErrorData::invalid_params(format!("bad params for tool '{tool}': {error}"), None)
        })?;
        let transaction = match self.studio().begin_transaction(initial_target.as_deref()) {
            Ok(transaction) => transaction,
            Err(error) => return Ok(res(Err(error))),
        };
        let staged = self.with_dispatch_studio(transaction.studio().clone());
        let mut result = staged.invoke(tool, args.clone()).await?;
        if is_error_result(&result) {
            return Ok(result);
        }

        // doc_new mints its target inside the staged handler. Every other
        // mutation has already resolved its explicit document argument.
        let target = if tool == ToolName::DocNew {
            journal_target(tool, &args, &result)
        } else {
            initial_target
        };
        let Some(target) = target else {
            return Ok(res(Err(format!(
                "successful {} mutation did not identify a document",
                tool.as_str()
            ))));
        };

        if journaled {
            let recorded = journal_args(tool, args, Some(&target));
            if let Err(error) = transaction
                .studio()
                .journal_append(&target, tool, &recorded)
            {
                return Ok(res(Err(error)));
            }
        }
        match transaction.commit(&target) {
            Ok(atelier_studio::CommitOutcome::Durable) => {}
            Ok(atelier_studio::CommitOutcome::DurabilityUncertain { warning }) => {
                tracing::warn!(
                    tool = tool.as_str(),
                    doc = %target,
                    warning = %warning,
                    "mutation committed with uncertain durability; do not retry automatically"
                );
                result = attach_commit_warning(result, &warning);
            }
            Err(error) => return Ok(res(Err(error))),
        }
        Ok(result)
    }

    /// Invoke one tool by name: deserialize the args into that tool's own
    /// param struct and run the same handler the MCP router used to reach.
    /// The router is now only the schema registry (it advertises the surface);
    /// this match is the dispatch — one path for every transport. The
    /// count-pin and dispatch-coverage tests keep the two in lockstep.
    async fn invoke(&self, tool: ToolName, args: Value) -> Result<CallToolResult, ErrorData> {
        /// Deserialize `args` into the tool's param struct and call its
        /// handler, mirroring rmcp's extractor: a param mismatch is a
        /// protocol-level invalid_params, never a tool result.
        macro_rules! call {
            ($ty:ty, $handler:ident) => {{
                let p = serde_json::from_value::<$ty>(args).map_err(|e| {
                    ErrorData::invalid_params(format!("bad params for tool '{tool}': {e}"), None)
                })?;
                self.$handler(Parameters(p)).await
            }};
        }
        Ok(match tool {
            ToolName::DocNew => call!(DocNew, doc_new),
            ToolName::ListDocs => call!(ListDocs, list_docs),
            ToolName::DocInfo => call!(DocRef, doc_info),
            ToolName::DeleteDoc => call!(DocRef, delete_doc),
            ToolName::DocLayer => call!(DocLayer, doc_layer),
            ToolName::DocFrame => call!(DocFrame, doc_frame),
            ToolName::DocAddTag => call!(DocAddTag, doc_add_tag),
            ToolName::DocCheckpoint => call!(DocCheckpoint, doc_checkpoint),
            ToolName::DocPalette => call!(DocPalette, doc_palette),
            ToolName::DocDraw => call!(DocDraw, doc_draw),
            ToolName::DocFx => call!(DocFx, doc_fx),
            ToolName::DocRegion => call!(DocRegion, doc_region),
            ToolName::DocPaintGrid => call!(DocPaintGrid, doc_paint_grid),
            ToolName::DocDitherRamp => call!(DocDitherRamp, doc_dither_ramp),
            ToolName::DocLook => call!(DocLook, doc_look),
            ToolName::DocDumpRegion => call!(DocDumpRegion, doc_dump_region),
            ToolName::DocSilhouette => call!(DocSilhouette, doc_silhouette),
            ToolName::DocComponents => call!(DocComponents, doc_components),
            ToolName::DocFrameDiff => call!(DocFrameDiff, doc_frame_diff),
            ToolName::DocSeamReport => call!(DocSeamReport, doc_seam_report),
            ToolName::DocAnimAudit => call!(DocAnimAudit, doc_anim_audit),
            ToolName::DocCritique => call!(DocCritique, doc_critique),
            ToolName::DocContactSheet => call!(DocContactSheet, doc_contact_sheet),
            ToolName::DocRef => call!(DocRefOp, doc_ref),
            ToolName::DocExport => call!(DocExport, doc_export),
        })
    }
}

/// Whether a call changes files in the document store. Kept separate from
/// journaling: checkpoints and reference setup mutate working state but are
/// session context, not deterministic recipe steps.
fn is_store_mutation(tool: ToolName, args: &Value) -> bool {
    if tool.is_read_only() {
        return false;
    }
    let op = call_op(args);
    !matches!(
        (tool, op),
        (ToolName::DocRef, Some("analyze" | "compare" | "diff"))
            | (ToolName::DocPalette, Some("report"))
            | (ToolName::DocCheckpoint, Some("list"))
    ) && !matches!(
        (tool, op),
        (ToolName::DocPalette, None | Some("generate"))
            if args.get("set_doc").and_then(Value::as_str).is_none()
    )
}

/// Resolve the document generation to stage before a mutating handler runs.
/// `doc_new` deliberately has no target yet; palette generation uses
/// `set_doc` instead of the usual `doc_id`.
fn mutation_target(tool: ToolName, args: &Value) -> Result<Option<String>, String> {
    let raw = match tool {
        ToolName::DocNew => None,
        ToolName::DocPalette if matches!(call_op(args), None | Some("generate")) => {
            args.get("set_doc")
        }
        ToolName::DocPalette => args.get("doc_id"),
        _ => args.get("doc_id"),
    };
    let Some(Value::String(id)) = raw else {
        // Leave missing and wrong-typed values to the normal typed
        // deserializer so callers retain the established invalid_params
        // diagnostics. The empty staging store still makes that path safe.
        return Ok(None);
    };
    if !atelier_studio::DocumentId::is_valid(id) {
        return Err(format!(
            "invalid document id '{id}' — expected a canonical lowercase UUIDv4"
        ));
    }
    Ok(Some(id.clone()))
}

/// True when a successful mutation is one deterministic step in rebuilding the
/// document. A checkpoint restore replaces the journal with its snapshot;
/// recording save/restore/prune would make checkpoint ids part of the recipe.
/// Reference files are external working context, so no `doc_ref` op is
/// journaled. [`ToolName`] is closed, so every new tool must make its replay
/// policy explicit before it can be dispatched.
fn is_journaled(tool: ToolName, args: &Value) -> bool {
    tool.is_recipe_step() && is_store_mutation(tool, args)
}

/// The document a call belongs to. `doc_new` returns it in the result (the id
/// is minted there, not passed in); everything else carries `doc_id`.
fn journal_target(tool: ToolName, args: &Value, result: &CallToolResult) -> Option<String> {
    match tool {
        // The id is minted in the result, not passed in.
        ToolName::DocNew => result_json(result)?
            .get("doc_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        // doc_export writes an external artifact; it does not define the
        // document's pixels, so it must NOT enter the per-document recipe —
        // replaying a rebuild would re-run the export against the author's
        // out_path (or fail-abort if that path is gone).
        ToolName::DocExport => None,
        // op=generate locks its palette onto `set_doc`, carrying no `doc_id`;
        // without this the lock is silently dropped from the recipe and replay
        // rebuilds the document off-palette.
        ToolName::DocPalette => args
            .get(if matches!(call_op(args), None | Some("generate")) {
                "set_doc"
            } else {
                "doc_id"
            })
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => args
            .get("doc_id")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

/// `doc_new`'s minted id exists only in its result. Stamping it into the
/// recorded args lets replay remap later steps when a rerun mints a new id.
fn journal_args(tool: ToolName, mut args: Value, target: Option<&str>) -> Value {
    if tool == ToolName::DocNew
        && let (Some(id), Some(obj)) = (target, args.as_object_mut())
    {
        obj.insert("doc_id".into(), json!(id));
    }
    args
}

fn caller_from_meta(
    meta: Option<&rmcp::model::Meta>,
    fallback_caller: String,
) -> Result<String, ErrorData> {
    let Some(raw) = meta.and_then(|meta| meta.0.get(SESSION_META_KEY)) else {
        return Ok(fallback_caller);
    };
    let session = raw.as_str().ok_or_else(|| {
        ErrorData::invalid_params(
            format!("MCP _meta['{SESSION_META_KEY}'] must be a string"),
            None,
        )
    })?;
    if session.is_empty() || session.len() > 128 || session.chars().any(char::is_control) {
        return Err(ErrorData::invalid_params(
            "atelier session must be 1..=128 bytes with no control characters",
            None,
        ));
    }
    Ok(session.to_string())
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Atelier {
    /// Advertise the tool surface — all of it. It is small enough that hiding
    /// part of it behind a profile flag would cost more in confusion than it
    /// ever saved in context.
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.advertised_tools()))
    }

    /// Transport glue: snapshot name/args (args default to `{}` to match a
    /// recipe step's shape), work out who is calling — the one
    /// transport-specific concern — then hand off to [`Atelier::dispatch`],
    /// the single path every caller (MCP, CLI, replay) shares. Because
    /// we define it, the `#[tool_handler]` macro skips generating its own.
    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
        let tool = request
            .name
            .parse::<ToolName>()
            .map_err(|error| ErrorData::invalid_params(error, None))?;
        let mut args = request
            .arguments
            .clone()
            .map(Value::Object)
            .unwrap_or_else(|| json!({}));
        if let Some(paths) = &self.http_paths {
            paths.rewrite(tool, &mut args)?;
        }
        // Fallback caller identity for the log line: configured HTTP header,
        // then TCP peer, then `-` for stdio. A per-call `session` in the
        // namespaced MCP metadata overrides it and survives HTTP reconnects.
        let caller = {
            let parts = context.extensions.get::<axum::http::request::Parts>();
            parts
                .and_then(|p| p.headers.get("x-atelier-caller"))
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
                .or_else(|| {
                    parts
                        .and_then(|p| {
                            p.extensions
                                .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
                        })
                        .map(|ci| ci.0.to_string())
                })
                .unwrap_or_else(|| "-".into())
        };
        // rmcp moves protocol `_meta` out of the params and into the request
        // context before invoking the handler.
        let caller = caller_from_meta(Some(&context.meta), caller)?;
        self.dispatch(tool, args, &caller).await
    }

    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info = Implementation::new("atelier", env!("CARGO_PKG_VERSION"))
            .with_title("Atelier")
            .with_description("Offline, headless pixel-art editor exposed through a CLI and MCP.")
            .with_website_url(env!("CARGO_PKG_HOMEPAGE"));
        info.instructions = Some(
            "Atelier is a stateless, offline pixel-art editor. Keep the doc_id returned \
             by doc_new and pass it explicitly on every later document call. Each doc_draw \
             or doc_fx call applies exactly one operation; use doc_paint_grid for dense pixel \
             rows and doc_look to inspect the result. MCP \
             _meta may carry a log label at \"io.github.marmikshah.atelier/session\", but \
             never tool defaults. Calls are journaled with concrete arguments, so stdio, \
             HTTP, CLI, and replay stay equivalent. Save a \
             doc_checkpoint before destructive edits. All 25 tools are advertised."
                .into(),
        );
        info
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str) -> ToolName {
        name.parse().unwrap()
    }

    #[test]
    fn advertised_schemas_carry_no_rust_integer_formats() {
        // Ajv-based clients (Kimi Code, most Node MCP hosts) warn on every
        // schemars integer format: `unknown format "uint32" ignored`.
        let tools = Atelier::registry_tools();
        assert!(!tools.is_empty());
        let dump = serde_json::to_string(
            &tools
                .iter()
                .map(|t| Value::Object((*t.input_schema).clone()))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        for fmt in ["uint32", "uint64", "uint8", "int32", "int64", "double"] {
            let marker = format!("\"format\":\"{fmt}\"");
            assert!(!dump.contains(&marker), "schema still advertises {marker}");
        }
    }

    #[test]
    fn strip_formats_keeps_standard_ones_and_named_properties() {
        let mut schema = json!({
            "properties": {
                "height": {"type": "integer", "format": "uint32", "minimum": 1},
                "when": {"type": "string", "format": "date-time"},
                // A *property named* "format" (doc_export has one) must survive.
                "format": {"type": "string", "enum": ["png", "gif"]}
            }
        });
        strip_nonstandard_formats(&mut schema);
        assert_eq!(schema["properties"]["height"].get("format"), None);
        assert_eq!(schema["properties"]["when"]["format"], "date-time");
        assert_eq!(schema["properties"]["format"]["enum"][0], "png");
    }

    #[test]
    fn the_tool_surface_is_the_size_the_docs_claim() {
        let n = Atelier::tool_router().list_all().len();
        // Written into README and tools.html (regen: make docs).
        // Change the surface, update them in the same commit — this is the reminder.
        assert_eq!(
            n,
            ToolName::ALL.len(),
            "tool count changed — update the docs"
        );
        assert_eq!(
            Atelier::registry_tools().len(),
            ToolName::ALL.len(),
            "every tool is advertised; there is no profile filter"
        );
        let instructions = temp_atelier("info")
            .get_info()
            .instructions
            .unwrap_or_default();
        assert!(
            instructions.contains("25 tools"),
            "get_info instructions drifted from the tool count"
        );
    }

    #[test]
    fn advertised_tool_definitions_fit_the_context_budget() {
        const MAX_TOOL_DEFINITION_BYTES: usize = 32 * 1024;
        let bytes = serde_json::to_vec(&Atelier::registry_tools()).unwrap();
        assert!(
            bytes.len() <= MAX_TOOL_DEFINITION_BYTES,
            "serialized tool definitions are {} bytes, over the {}-byte budget",
            bytes.len(),
            MAX_TOOL_DEFINITION_BYTES
        );
    }

    #[test]
    fn every_tool_advertises_explicit_conservative_hints() {
        let tools = Atelier::registry_tools();
        for tool in &tools {
            let hints = tool
                .annotations
                .as_ref()
                .unwrap_or_else(|| panic!("{} has no annotations", tool.name));
            assert!(
                hints.read_only_hint.is_some(),
                "{} read-only hint",
                tool.name
            );
            assert!(
                hints.destructive_hint.is_some(),
                "{} destructive hint",
                tool.name
            );
            assert!(
                hints.idempotent_hint.is_some(),
                "{} idempotent hint",
                tool.name
            );
            assert!(
                hints.open_world_hint.is_some(),
                "{} open-world hint",
                tool.name
            );
        }
        let hints = |name: &str| {
            tools
                .iter()
                .find(|tool| tool.name == name)
                .unwrap()
                .annotations
                .as_ref()
                .unwrap()
        };
        assert_eq!(hints("list_docs").read_only_hint, Some(true));
        assert_eq!(hints("delete_doc").destructive_hint, Some(true));
        assert_eq!(hints("doc_export").open_world_hint, Some(true));
        assert_eq!(hints("doc_new").destructive_hint, Some(false));
    }

    #[test]
    fn initialization_identifies_atelier_not_the_mcp_framework() {
        let info = temp_atelier("identity").get_info();
        assert_eq!(info.server_info.name, "atelier");
        assert_eq!(info.server_info.title.as_deref(), Some("Atelier"));
        assert_eq!(info.server_info.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(
            info.server_info.website_url.as_deref(),
            Some(env!("CARGO_PKG_HOMEPAGE"))
        );
    }

    #[test]
    fn the_public_tool_names_are_pinned() {
        // A count alone would allow one released call to disappear while an
        // unrelated new call took its place. Keep the retained 1.8 contract
        // explicit: changing a name is a deliberate breaking API decision.
        let mut actual: Vec<String> = Atelier::registry_tools()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        actual.sort();
        let mut expected: Vec<String> = ToolName::ALL.iter().map(ToString::to_string).collect();
        expected.sort();
        assert_eq!(actual, expected);
    }

    #[test]
    fn the_registry_lists_without_a_studio() {
        // `atelier tools` lists the registry only; building a `Studio` for it
        // used to create ~/.atelier/documents as a side effect of `--help`-level
        // work. The router is an associated fn, so nothing here touches disk.
        assert_eq!(Atelier::registry_tools().len(), 25);
        assert!(tools_text().starts_with("atelier tools — 25 tools\n"));
        assert!(tools_html().contains("25</strong> tools"));
    }

    #[test]
    fn no_tool_description_points_at_a_tool_that_does_not_exist() {
        // A description is the model's only guide. Naming a tool that was
        // removed sends it to call something that will never answer — and the
        // count pin cannot see this, because the count is still right.
        let tools = Atelier::tool_router().list_all();
        let names: std::collections::HashSet<String> =
            tools.iter().map(|t| t.name.to_string()).collect();
        let mentioned = regex_lite_doc_tools(&tools);
        for (tool, referenced) in mentioned {
            assert!(
                names.contains(&referenced),
                "{tool}'s description names '{referenced}', which is not a tool"
            );
        }
    }

    /// Every `doc_*` token appearing in each tool's description AND its input
    /// schema (schemars copies doc comments into schema descriptions — a stray
    /// `///` above a param struct ships to the model just as surely as the
    /// `#[tool]` description does), paired with the tool it came from.
    /// Hand-rolled: a regex crate for one scan in one test is a dependency the
    /// tower does not need.
    fn regex_lite_doc_tools(tools: &[rmcp::model::Tool]) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for t in tools {
            let schema = serde_json::to_string(&t.input_schema).unwrap_or_default();
            let d = format!("{} {schema}", t.description.as_deref().unwrap_or(""));
            for (i, _) in d.match_indices("doc_") {
                let tail: String = d[i..]
                    .chars()
                    .take_while(|c| c.is_ascii_lowercase() || *c == '_' || c.is_ascii_digit())
                    .collect();
                // `doc_id` is a parameter name, not a tool.
                if tail == "doc_id" || tail == "doc_" {
                    continue;
                }
                out.push((t.name.to_string(), tail));
            }
        }
        out
    }

    #[test]
    fn no_param_struct_ships_a_top_level_schema_description() {
        // The `#[tool]` description is a tool's one prose surface; a doc
        // comment on the param struct rides into the schema's top-level
        // `description` behind its back (a blank line does NOT detach a `///`
        // — deleting a neighbouring struct can silently re-home its comment).
        for t in Atelier::tool_router().list_all() {
            assert!(
                !t.input_schema.contains_key("description"),
                "{}'s param struct carries a doc comment — the model would see \
                 it as a second, competing description: {:?}",
                t.name,
                t.input_schema.get("description")
            );
        }
    }

    #[test]
    fn the_shipped_skills_name_only_real_tools() {
        // The shipped skills (crates/atelier/skills) are the workflow guidance we
        // install for users; they name tools verbatim. Delete a tool and they
        // rot silently — the same drift as a stale description, one crate over.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../atelier/skills");
        let Ok(entries) = std::fs::read_dir(&root) else {
            return; // skills are not present in a packaged crate; nothing to check
        };
        let names: std::collections::HashSet<String> = Atelier::tool_router()
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        let mut checked = 0;
        for e in entries.filter_map(Result::ok) {
            let skill = e.path();
            if skill.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let Ok(body) = std::fs::read_to_string(&skill) else {
                continue;
            };
            checked += 1;
            for (i, _) in body.match_indices("doc_") {
                let tool: String = body[i..]
                    .chars()
                    .take_while(|c| c.is_ascii_lowercase() || *c == '_' || c.is_ascii_digit())
                    .collect();
                if tool == "doc_id" || tool == "doc_" {
                    continue;
                }
                assert!(
                    names.contains(&tool),
                    "{} names '{tool}', which is not a tool",
                    skill.display()
                );
            }
        }
        assert!(
            checked >= 3,
            "expected the three shipped skills, saw {checked}"
        );
    }

    #[test]
    fn the_eye_is_never_journaled_but_the_hand_always_is() {
        // Reads rebuild nothing; replaying them is noise.
        for t in ["doc_look", "doc_info", "doc_critique", "doc_silhouette"] {
            assert!(
                !is_journaled(tool(t), &json!({"doc_id": "d"})),
                "{t} is a read"
            );
        }
        assert!(
            !is_journaled(ToolName::DocExport, &json!({"doc_id": "d"})),
            "export writes an artifact but does not build the document"
        );
        // Anything that marks the canvas has to be in the recipe.
        for t in ["doc_draw", "doc_fx", "doc_new"] {
            assert!(
                is_journaled(tool(t), &json!({"doc_id": "d"})),
                "{t} builds the art"
            );
        }
    }

    #[test]
    fn hub_tools_are_classified_by_op_not_by_name() {
        // Reference setup changes working state but is external context, not a
        // deterministic recipe step.
        assert!(!is_journaled(ToolName::DocRef, &json!({"op": "compare"})));
        assert!(!is_journaled(ToolName::DocRef, &json!({"op": "diff"})));
        assert!(!is_store_mutation(
            ToolName::DocRef,
            &json!({"op": "compare"})
        ));
        assert!(is_store_mutation(ToolName::DocRef, &json!({"op": "set"})));
        assert!(!is_journaled(ToolName::DocRef, &json!({"op": "set"})));

        // Palette reports and unbound generation are reads; document-targeted
        // palette ops remain deterministic mutations.
        assert!(!is_journaled(
            ToolName::DocPalette,
            &json!({"op": "report"})
        ));
        assert!(is_journaled(ToolName::DocPalette, &json!({"op": "set"})));
        assert!(!is_store_mutation(
            ToolName::DocPalette,
            &json!({"op": "generate"})
        ));
        assert!(!is_store_mutation(
            ToolName::DocPalette,
            &json!({"op": "generate", "set_doc": null})
        ));
        assert!(is_journaled(
            ToolName::DocPalette,
            &json!({"op": "generate", "set_doc": "hero"})
        ));

        // Checkpoint files mutate the store, but restore replaces the live
        // journal with the checkpointed one instead of recording checkpoint ids.
        assert!(!is_journaled(
            ToolName::DocCheckpoint,
            &json!({"action": "list"})
        ));
        assert!(!is_store_mutation(
            ToolName::DocCheckpoint,
            &json!({"action": "list"})
        ));
        assert!(is_store_mutation(
            ToolName::DocCheckpoint,
            &json!({"action": "restore"})
        ));
        assert!(!is_journaled(
            ToolName::DocCheckpoint,
            &json!({"action": "restore"})
        ));
    }

    #[test]
    fn doc_new_is_journaled_to_the_id_it_minted() {
        // The id is in the result, not the args — journaling by args alone
        // would file every doc_new under nothing.
        let doc_id = "550e8400-e29b-41d4-a716-446655440000";
        let created = CallToolResult::success(vec![Content::text(
            json!({"doc_id": doc_id, "w": 8}).to_string(),
        )]);
        assert_eq!(
            journal_target(ToolName::DocNew, &json!({"name": "sprite"}), &created).as_deref(),
            Some(doc_id)
        );
        let drew = CallToolResult::success(vec![Content::text(json!({"ok": true}).to_string())]);
        assert_eq!(
            journal_target(ToolName::DocDraw, &json!({"doc_id": doc_id}), &drew).as_deref(),
            Some(doc_id)
        );
    }

    #[test]
    fn mcp_session_is_namespaced_and_never_carries_tool_defaults() {
        let meta: rmcp::model::Meta = serde_json::from_value(json!({
            SESSION_META_KEY: "sprite-pass"
        }))
        .unwrap();
        assert_eq!(
            caller_from_meta(Some(&meta), "transport".into()).unwrap(),
            "sprite-pass"
        );

        let bad: rmcp::model::Meta = serde_json::from_value(json!({
            SESSION_META_KEY: {"doc_id": "550e8400-e29b-41d4-a716-446655440000"}
        }))
        .unwrap();
        assert!(caller_from_meta(Some(&bad), "transport".into()).is_err());
    }

    #[test]
    fn store_mutations_are_classified_for_locking() {
        // Its own journal dies with the doc dir, so journaling delete is moot.
        assert!(!is_journaled(ToolName::DeleteDoc, &json!({"doc_id": "x"})));
        assert!(is_store_mutation(
            ToolName::DeleteDoc,
            &json!({"doc_id": "x"})
        ));
        assert!(is_store_mutation(
            ToolName::DocDraw,
            &json!({"doc_id": "x"})
        ));
        assert!(!is_store_mutation(
            ToolName::DocExport,
            &json!({"doc_id": "x", "op": "sheet"})
        ));
        assert!(!is_store_mutation(
            ToolName::DocLook,
            &json!({"doc_id": "x"})
        ));
    }

    #[test]
    fn doc_new_journals_the_minted_id_for_replay_remapping() {
        let doc_id = "550e8400-e29b-41d4-a716-446655440000";
        assert_eq!(
            journal_args(ToolName::DocNew, json!({"name": "sprite"}), Some(doc_id)),
            json!({"name": "sprite", "doc_id": doc_id})
        );
        // Every other tool records its args untouched.
        assert_eq!(
            journal_args(ToolName::DocDraw, json!({"doc_id": doc_id}), Some(doc_id)),
            json!({"doc_id": doc_id})
        );
    }

    #[test]
    fn replay_fidelity_edges_are_journaled_correctly() {
        let ok = CallToolResult::success(vec![Content::text(json!({"ok": true}).to_string())]);
        // doc_export writes an artifact, not document state — replaying a rebuild
        // must not re-run it, so it belongs to no document's recipe.
        assert!(!is_journaled(
            ToolName::DocExport,
            &json!({"doc_id": "hero", "op": "sheet"})
        ));
        assert_eq!(
            journal_target(
                ToolName::DocExport,
                &json!({"doc_id": "hero", "op": "sheet"}),
                &ok
            ),
            None,
            "export must not enter the per-document journal"
        );
        // doc_palette op=generate locks a palette onto `set_doc`, carrying no
        // `doc_id`; the recipe must still capture it or replay rebuilds off-palette.
        assert_eq!(
            journal_target(
                ToolName::DocPalette,
                &json!({"op": "generate", "set_doc": "hero"}),
                &ok
            )
            .as_deref(),
            Some("hero")
        );

        // `doc_id` is a valid parameter for the other palette ops, but it is
        // irrelevant to generate. If both are present, the staged generation
        // and journal must still follow the document the handler actually
        // edits (`set_doc`).
        let doc_id = "550e8400-e29b-41d4-a716-446655440000";
        let set_doc = "6ba7b810-9dad-41d1-80b4-00c04fd430c8";
        let args = json!({"op": "generate", "doc_id": doc_id, "set_doc": set_doc});
        assert_eq!(
            mutation_target(ToolName::DocPalette, &args)
                .unwrap()
                .as_deref(),
            Some(set_doc)
        );
        assert_eq!(
            journal_target(ToolName::DocPalette, &args, &ok).as_deref(),
            Some(set_doc)
        );
    }

    #[test]
    fn result_json_reads_past_a_leading_image() {
        // img_result puts the PNG first and the stats after it; taking
        // content[0] would miss the report entirely.
        let looked = CallToolResult::success(vec![
            Content::image("ZmFrZQ==".to_string(), "image/png".to_string()),
            Content::text(json!({"doc_id": "x"}).to_string()),
        ]);
        assert_eq!(
            result_json(&looked).unwrap().get("doc_id").unwrap(),
            &json!("x")
        );
    }

    #[test]
    fn json_results_have_structured_and_text_compatible_forms() {
        let payload = json!({"ok": true, "count": 3});
        let result = res(Ok(payload.clone()));
        assert_eq!(result.structured_content, Some(payload.clone()));
        let text = result
            .content
            .iter()
            .find_map(|content| content.as_text())
            .unwrap();
        assert_eq!(serde_json::from_str::<Value>(&text.text).unwrap(), payload);

        let visual = img_result(Ok((vec![1, 2, 3], json!({"pixels": 1}))));
        assert_eq!(visual.structured_content, Some(json!({"pixels": 1})));
        assert!(matches!(visual.content[0], Content::Image(_)));

        let oversized = vec![0; MAX_INLINE_PNG_BYTES + 1];
        let rejected = img_result(Ok((oversized.clone(), json!({"pixels": 1}))));
        assert_eq!(rejected.is_error, Some(true));
        assert_eq!(
            rejected.structured_content.as_ref().unwrap()["inline_bytes"],
            oversized.len()
        );
        assert!(
            !rejected
                .content
                .iter()
                .any(|content| matches!(content, Content::Image(_)))
        );

        let saved = img_result(Ok((
            oversized,
            json!({"path": "/tmp/render.png", "pixels": 1}),
        )));
        assert_ne!(saved.is_error, Some(true));
        assert!(saved.structured_content.as_ref().unwrap()["inline_image_omitted"].is_object());
        assert!(
            !saved
                .content
                .iter()
                .any(|content| matches!(content, Content::Image(_)))
        );

        let error = res(Err("bad input".into()));
        assert_eq!(error.is_error, Some(true));
        assert_eq!(
            error.structured_content,
            Some(json!({"error": "bad input"}))
        );

        let warned = attach_commit_warning(res(Ok(json!({"ok": true}))), "committed; do not retry");
        assert!(!is_error_result(&warned));
        let warned_payload = result_json(&warned).unwrap();
        assert_eq!(warned_payload["commit_warning"]["committed"], true);
        assert_eq!(warned_payload["commit_warning"]["retry"], false);
        let compatible = warned
            .content
            .iter()
            .find_map(|content| content.as_text())
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&compatible.text).unwrap(),
            warned_payload
        );
    }

    #[test]
    fn http_paths_are_disabled_or_confined_to_configured_roots() {
        let root =
            std::env::temp_dir().join(format!("atelier-http-path-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let import = root.join("import");
        let export = root.join("export");
        std::fs::create_dir_all(&import).unwrap();
        std::fs::create_dir_all(&export).unwrap();
        std::fs::write(import.join("sample.png"), b"not decoded in this test").unwrap();
        let import = std::fs::canonicalize(import).unwrap();
        let export = std::fs::canonicalize(export).unwrap();
        let paths = HttpPathRoots::new(Some(import.clone()), Some(export.clone()));

        let mut input = json!({"path": "sample.png"});
        paths.rewrite(ToolName::DocRef, &mut input).unwrap();
        assert_eq!(input["path"], json!(import.join("sample.png")));

        let mut output = json!({"out_path": "nested/sheet.png"});
        paths.rewrite(ToolName::DocExport, &mut output).unwrap();
        assert_eq!(output["out_path"], json!(export.join("nested/sheet.png")));

        for forbidden in ["../outside.png", "/tmp/outside.png"] {
            let mut args = json!({"out_path": forbidden});
            assert!(paths.rewrite(ToolName::DocExport, &mut args).is_err());
        }
        let mut disabled = json!({"out_path": "sheet.png"});
        assert!(
            HttpPathRoots::default()
                .rewrite(ToolName::DocExport, &mut disabled)
                .unwrap_err()
                .message
                .contains("ATELIER_EXPORT_ROOT")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn http_output_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let root =
            std::env::temp_dir().join(format!("atelier-http-symlink-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let export = root.join("export");
        let outside = root.join("outside");
        std::fs::create_dir_all(&export).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        symlink(&outside, export.join("redirect")).unwrap();
        let export = std::fs::canonicalize(export).unwrap();
        let paths = HttpPathRoots::new(None, Some(export));
        let mut args = json!({"out_path": "redirect/escaped.png"});
        assert!(paths.rewrite(ToolName::DocExport, &mut args).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn base64_matches_known_vectors() {
        // RFC 4648 test vectors exercise the 0/1/2 trailing-byte padding cases.
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[tokio::test]
    async fn explicit_targets_are_journaled_and_checkpoint_restore_rewinds_them() {
        async fn ok(atelier: &Atelier, tool: &str, args: Value) -> Value {
            let tool = tool.parse::<ToolName>().unwrap();
            let result = atelier.dispatch(tool, args, "test").await.unwrap();
            assert!(
                !is_error_result(&result),
                "{tool} failed: {:?}",
                result_json(&result)
            );
            result_json(&result).unwrap()
        }

        let atelier = temp_atelier("context-checkpoint");
        let created = ok(
            &atelier,
            "doc_new",
            json!({"name": "hero", "width": 4, "height": 4}),
        )
        .await;
        let doc_id = created["doc_id"].as_str().unwrap();
        ok(
            &atelier,
            "doc_draw",
            json!({"doc_id": doc_id, "layer": 0, "frame": 0, "op": "pencil",
                   "points": [[1, 1]], "color": [200, 0, 0]}),
        )
        .await;
        let saved = ok(
            &atelier,
            "doc_checkpoint",
            json!({"doc_id": doc_id, "action": "save", "label": "red"}),
        )
        .await;
        let checkpoint_id = saved["saved"].as_str().unwrap();

        ok(
            &atelier,
            "doc_draw",
            json!({"doc_id": doc_id, "layer": 0, "frame": 0, "op": "pencil",
                   "points": [[1, 1]], "color": [0, 0, 200]}),
        )
        .await;
        assert_eq!(atelier.studio().journal(doc_id).unwrap().len(), 3);

        ok(
            &atelier,
            "doc_checkpoint",
            json!({"doc_id": doc_id, "action": "restore", "checkpoint_id": checkpoint_id}),
        )
        .await;
        let journal = atelier.studio().journal(doc_id).unwrap();
        assert_eq!(
            journal.len(),
            2,
            "restore must discard post-checkpoint provenance"
        );
        assert_eq!(
            Value::Object(journal[1].args.clone()),
            json!({
                "doc_id": doc_id,
                "layer": 0,
                "frame": 0,
                "op": "pencil",
                "points": [[1, 1]],
                "color": [200, 0, 0]
            }),
            "the journal stores the explicit target"
        );
        assert_eq!(
            atelier
                .studio()
                .doc_dump_region(
                    doc_id,
                    0,
                    Some(0),
                    Some((1, 1, 1, 1)),
                    atelier_studio::DumpMode::Hex,
                )
                .unwrap()["rows"][0],
            "#c80000"
        );
    }

    /// Build an Atelier whose studio is rooted at a throwaway temp dir.
    fn temp_atelier(tag: &str) -> Atelier {
        let dir = std::env::temp_dir().join(format!("atelier-srv-test-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        let studio = Studio::with_docs_dir(dir);
        Atelier::with_studio(studio)
    }

    fn transactions_empty(documents: &std::path::Path) -> bool {
        let transactions = documents.join(".transactions");
        !transactions.exists()
            || std::fs::read_dir(transactions).is_ok_and(|mut entries| entries.next().is_none())
    }

    fn pixel_hex(atelier: &Atelier, doc_id: &str, x: i32, y: i32) -> String {
        atelier
            .studio()
            .doc_dump_region(
                doc_id,
                0,
                Some(0),
                Some((x, y, x, y)),
                atelier_studio::DumpMode::Hex,
            )
            .unwrap()["rows"][0]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[tokio::test]
    async fn mutation_commit_publishes_pixels_and_recipe_together() {
        let documents = std::env::temp_dir().join(format!(
            "atelier-srv-test-transaction-commit-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&documents);
        let atelier = Atelier::with_studio(Studio::with_docs_dir(documents.clone()));

        let created = atelier
            .dispatch(
                ToolName::DocNew,
                json!({"name": "atomic", "width": 2, "height": 2}),
                "test",
            )
            .await
            .unwrap();
        let created = result_json(&created).unwrap();
        let doc_id = created["doc_id"].as_str().unwrap();
        assert_eq!(atelier.studio().journal(doc_id).unwrap().len(), 1);

        let drew = atelier
            .dispatch(
                ToolName::DocDraw,
                json!({
                    "doc_id": doc_id,
                    "layer": 0,
                    "frame": 0,
                    "op": "pencil",
                    "points": [[1, 1]],
                    "color": [12, 34, 56, 255]
                }),
                "test",
            )
            .await
            .unwrap();
        assert!(!is_error_result(&drew));
        assert_eq!(pixel_hex(&atelier, doc_id, 1, 1), "#0c2238");
        assert_eq!(atelier.studio().journal(doc_id).unwrap().len(), 2);
        assert!(transactions_empty(&documents));
        let _ = std::fs::remove_dir_all(documents);
    }

    #[tokio::test]
    async fn handler_and_protocol_failures_drop_the_staged_generation() {
        let documents = std::env::temp_dir().join(format!(
            "atelier-srv-test-transaction-protocol-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&documents);
        let atelier = Atelier::with_studio(Studio::with_docs_dir(documents.clone()));
        let created = atelier
            .dispatch(
                ToolName::DocNew,
                json!({"name": "rollback", "width": 2, "height": 2}),
                "test",
            )
            .await
            .unwrap();
        let created = result_json(&created).unwrap();
        let doc_id = created["doc_id"].as_str().unwrap();

        let handler_failed = atelier
            .dispatch(
                ToolName::DocDraw,
                json!({
                    "doc_id": doc_id,
                    "layer": 0,
                    "frame": 0,
                    "op": "pencil",
                    "points": [[0, 0]],
                    "color": [999, 0, 0, 255]
                }),
                "test",
            )
            .await
            .unwrap();
        assert!(is_error_result(&handler_failed));
        assert_eq!(pixel_hex(&atelier, doc_id, 0, 0), ".");
        assert_eq!(atelier.studio().journal(doc_id).unwrap().len(), 1);
        assert!(transactions_empty(&documents));

        // `op` is required, so deserialization fails after the target has
        // already been staged but before any generation can be published.
        let failed = atelier
            .dispatch(
                ToolName::DocDraw,
                json!({"doc_id": doc_id, "layer": 0, "frame": 0}),
                "test",
            )
            .await;
        assert!(failed.is_err());
        assert_eq!(pixel_hex(&atelier, doc_id, 0, 0), ".");
        assert_eq!(atelier.studio().journal(doc_id).unwrap().len(), 1);
        assert!(transactions_empty(&documents));

        let invalid_id = atelier
            .dispatch(
                ToolName::DocDraw,
                json!({
                    "doc_id": "../not-a-document",
                    "layer": 0,
                    "frame": 0,
                    "op": "clear_cel"
                }),
                "test",
            )
            .await
            .unwrap_err();
        assert!(invalid_id.message.contains("invalid document id"));
        assert!(transactions_empty(&documents));
        let _ = std::fs::remove_dir_all(documents);
    }

    #[tokio::test]
    async fn journal_failure_rolls_back_a_successful_handler() {
        let documents = std::env::temp_dir().join(format!(
            "atelier-srv-test-transaction-journal-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&documents);
        let atelier = Atelier::with_studio(Studio::with_docs_dir(documents.clone()));
        let created = atelier
            .dispatch(
                ToolName::DocNew,
                json!({"name": "rollback", "width": 2, "height": 2}),
                "test",
            )
            .await
            .unwrap();
        let created = result_json(&created).unwrap();
        let doc_id = created["doc_id"].as_str().unwrap();

        // A directory at the journal path deterministically makes append fail
        // even when tests run as root. The staged draw itself succeeds, but it
        // must never replace the live generation without provenance.
        let journal = documents.join(doc_id).join(atelier_studio::JOURNAL_FILE);
        std::fs::remove_file(&journal).unwrap();
        std::fs::create_dir(&journal).unwrap();
        let failed = atelier
            .dispatch(
                ToolName::DocDraw,
                json!({
                    "doc_id": doc_id,
                    "layer": 0,
                    "frame": 0,
                    "op": "pencil",
                    "points": [[0, 0]],
                    "color": [255, 0, 0, 255]
                }),
                "test",
            )
            .await
            .unwrap();
        assert!(is_error_result(&failed));
        let error = result_json(&failed).unwrap()["error"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(error.contains("journal"), "got: {error}");
        assert_eq!(pixel_hex(&atelier, doc_id, 0, 0), ".");
        assert!(transactions_empty(&documents));
        let _ = std::fs::remove_dir_all(documents);
    }

    /// doc_look is the agent's only eye: an edit op acknowledges with text only.
    /// Returning a preview PNG per edit costs LLM clients tens of thousands of
    /// image tokens a call — and telling the model it got a preview when it
    /// didn't suppresses the doc_look it should make. Pin both halves of the
    /// contract: edits carry no image, visual tools still do.
    #[test]
    fn edit_results_carry_no_image_but_visual_results_do() {
        let has_image = |r: &CallToolResult| {
            r.content
                .iter()
                .any(|c| matches!(c, rmcp::model::ContentBlock::Image(_)))
        };

        let edit = edited(Ok(json!({"ok": true})));
        assert!(
            !has_image(&edit),
            "edit ops must return text only — doc_look is the agent's eye"
        );

        // A 1x1 PNG stands in for any rendered frame.
        let png = vec![0x89, 0x50, 0x4E, 0x47];
        let visual = img_result(Ok((png, json!({"ok": true}))));
        assert!(
            has_image(&visual),
            "visual tools must still return the frame"
        );
    }
}

#[cfg(test)]
mod hardening_tests {
    use super::*;

    #[test]
    fn rgba_rejects_values_that_cannot_be_rgba() {
        assert_eq!(rgba(&[255, 128, 0]).unwrap(), [255, 128, 0, 255]);
        assert_eq!(rgba(&[1, 2, 3, 4]).unwrap(), [1, 2, 3, 4]);
        // These truncated silently before (300 → 44, -1 → 255, short → 0s).
        assert!(rgba(&[255, 300, 0]).is_err());
        assert!(rgba(&[255, -1, 0]).is_err());
        assert!(rgba(&[255, 0]).is_err());
        assert!(rgba(&[1, 2, 3, 4, 5]).is_err());
    }
}
