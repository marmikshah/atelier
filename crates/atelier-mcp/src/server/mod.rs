//! atelier MCP server (rmcp). Exposes the headless document editor as MCP
//! tools over stdio or Streamable HTTP. Tools return JSON text content; studio
//! errors keep the {"error": ...} payload AND set `is_error` so MCP harnesses
//! flag the failure instead of treating it as a success.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock as Content, ListToolsResult, PaginatedRequestParams,
    ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, tool_handler};
use serde::Deserialize;
use serde_json::{Value, json};

use atelier_studio::Studio;

mod params;
mod toolsdoc;
mod transport;

pub use toolsdoc::{tools_html, tools_text};
pub use transport::{run, run_http};

use params::*;

fn j(v: Value) -> String {
    serde_json::to_string(&v).unwrap_or_else(|_| "{}".into())
}

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
        Ok(v) => CallToolResult::success(vec![Content::text(j(v))]),
        Err(e) => CallToolResult::error(vec![Content::text(j(json!({"error": e})))]),
    }
}

/// Wrap an image-producing studio result as MCP content: an inline PNG (so the
/// agent SEES the pixels in the same turn — no separate file read) plus a JSON
/// text part with the measured stats. Errors come back as a `{"error": ...}`
/// text part with `is_error` set, matching `res`.
fn img_result(r: Result<(Vec<u8>, Value), String>) -> CallToolResult {
    match r {
        Ok((png, report)) => CallToolResult::success(vec![
            Content::image(base64(&png), "image/png"),
            Content::text(j(report)),
        ]),
        Err(e) => CallToolResult::error(vec![Content::text(j(json!({"error": e})))]),
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

/// A list of `[r,g,b(,a)]` arrays -> a palette of RGBA swatches.
fn palette_list(v: &[Vec<i64>]) -> Result<Vec<[u8; 4]>, String> {
    v.iter().map(|c| rgba(c)).collect()
}

/// [r,g,b] or [r,g,b,a] -> RGBA, STRICT: exactly 3..=4 components, each
/// 0..=255 — the same shape `validate_batch_op` enforces on the batch path.
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
    mode: Option<&str>,
    cutoff: Option<u8>,
    bg: Option<&[i64]>,
) -> Result<atelier_core::document::AlphaSnap, String> {
    use atelier_core::document::AlphaSnap;
    match mode.unwrap_or("preserve") {
        "preserve" => Ok(AlphaSnap::Preserve),
        "opaque" => Ok(AlphaSnap::Opaque(cutoff.unwrap_or(128))),
        "flatten" => Ok(AlphaSnap::Flatten(match bg {
            Some(b) => rgba(b)?,
            None => [0, 0, 0, 255],
        })),
        m => Err(format!(
            "unknown alpha mode '{m}' — use preserve | opaque | flatten"
        )),
    }
}

/// Optional [x0,y0,x1,y1] -> (x0,y0,x1,y1). A region that is present but
/// shorter than 4 numbers is an agent mistake — error loudly instead of
/// silently acting on the whole canvas.
fn region(r: &Option<Vec<i32>>) -> Result<Option<(i32, i32, i32, i32)>, String> {
    match r {
        None => Ok(None),
        Some(v) if v.len() >= 4 => Ok(Some((v[0], v[1], v[2], v[3]))),
        Some(v) => Err(format!(
            "region needs 4 numbers [x0,y0,x1,y1], got {} — omit it to act on the whole canvas",
            v.len()
        )),
    }
}

/// Re-deserialize `doc_draw`'s flattened op params (plus the shared doc_id/
/// layer/frame) into a composite draw op's own typed struct (box_iso → DocBox,
/// panel → DocPanel), so those primitives ride the single `doc_draw` surface.
fn draw_op_params<T: serde::de::DeserializeOwned>(p: &DocDraw) -> Result<T, String> {
    let mut m = p.params.clone();
    m.insert("doc_id".to_string(), Value::String(p.doc_id.clone()));
    m.insert("layer".to_string(), Value::from(p.layer as u64));
    m.insert("frame".to_string(), Value::from(p.frame as u64));
    serde_json::from_value(Value::Object(m))
        .map_err(|e| format!("doc_draw op={}: bad params — {e}", p.op))
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
    result
        .content
        .iter()
        .filter_map(|c| c.as_text())
        .find_map(|t| serde_json::from_str::<Value>(&t.text).ok())
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

/// Hub-operation discriminator. Most hubs call it `op`; checkpoints predate
/// that convention and use `action`.
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
    tool: &str,
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
        Some(error) => tracing::warn!(%tool, %op, %doc, %caller, ms, %error, "tool error"),
        None if is_error_result(result) => {
            // is_error without a JSON error payload: surface what text there is.
            let text = result
                .content
                .first()
                .and_then(|c| c.as_text())
                .map(|t| t.text.clone())
                .unwrap_or_default();
            tracing::warn!(%tool, %op, %doc, %caller, ms, error = %text, "tool error");
        }
        None => tracing::info!(%tool, %op, %doc, %caller, ms, "ok"),
    }
}

// The tool surface, split by domain. Declared after the helpers above so the
// `try_res!` macro is in scope inside them.
mod tools_doc;
mod tools_draw;
mod tools_export;
mod tools_read;

// --- server ----------------------------------------------------------------

/// Optional defaults attached to one call. Context is deliberately carried by
/// the caller, never retained in the server: stdio, HTTP, CLI, and replay
/// therefore resolve identical arguments, and concurrent clients cannot change
/// one another's active document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallContext {
    /// Stable name used in logs. MCP callers may override the transport-derived
    /// identity with `session` in Atelier's namespaced request metadata.
    pub caller: String,
    pub doc_id: Option<String>,
    pub layer: Option<usize>,
    pub frame: Option<usize>,
}

impl CallContext {
    pub fn new(caller: impl Into<String>) -> Self {
        Self {
            caller: caller.into(),
            ..Self::default()
        }
    }
}

/// Namespaced MCP `_meta` entry carrying [`CallContext`] defaults.
pub const CALL_CONTEXT_META_KEY: &str = "io.github.marmikshah.atelier/context";

/// Expand only fields the target tool actually accepts. Explicit arguments
/// always win. The expanded object is what dispatch logs and journals, so
/// replay never depends on an ambient session.
fn apply_call_context(tool: &str, mut args: Value, context: &CallContext) -> Value {
    let operation = call_op(&args).map(str::to_owned);
    let op = operation.as_deref();
    let Some(obj) = args.as_object_mut() else {
        return args;
    };

    let accepts_doc = !matches!(tool, "doc_create" | "list_docs")
        && !matches!((tool, op), ("doc_palette", None | Some("generate")));
    let accepts_layer = matches!(
        tool,
        "doc_batch"
            | "doc_draw"
            | "doc_fx"
            | "doc_region"
            | "doc_paint_grid"
            | "doc_dither_ramp"
            | "doc_dump_region"
            | "doc_silhouette"
            | "doc_components"
            | "doc_frame_diff"
            | "doc_seam_report"
            | "doc_anim_audit"
            | "doc_critique"
    ) || matches!(
        (tool, op),
        ("doc_palette", Some("snap" | "swap" | "report"))
    );
    let accepts_layer_index = matches!(
        (tool, op),
        (
            "doc_layer",
            Some("set" | "move" | "insert" | "delete" | "rename" | "duplicate" | "merge_down")
        )
    );
    let accepts_frame = matches!(
        tool,
        "doc_batch"
            | "doc_draw"
            | "doc_fx"
            | "doc_region"
            | "doc_paint_grid"
            | "doc_dither_ramp"
            | "doc_dump_region"
            | "doc_silhouette"
            | "doc_components"
            | "doc_seam_report"
            | "doc_look"
            | "doc_critique"
    ) || matches!(
        (tool, op),
        ("doc_palette", Some("snap" | "swap" | "report"))
            | ("doc_ref", Some("compare" | "diff"))
            | (
                "doc_frame",
                Some("duration" | "delete" | "insert" | "duplicate" | "move")
            )
    );

    if accepts_doc
        && !obj.contains_key("doc_id")
        && let Some(doc_id) = &context.doc_id
    {
        obj.insert("doc_id".into(), json!(doc_id));
    }
    if accepts_layer
        && !obj.contains_key("layer")
        && let Some(layer) = context.layer
    {
        obj.insert("layer".into(), json!(layer));
    }
    if accepts_layer_index
        && !obj.contains_key("index")
        && let Some(layer) = context.layer
    {
        obj.insert("index".into(), json!(layer));
    }
    if accepts_frame
        && !obj.contains_key("frame")
        && let Some(frame) = context.frame
    {
        obj.insert("frame".into(), json!(frame));
    }
    args
}

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
        }
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
        tool: &str,
        args: Value,
        context: CallContext,
    ) -> Result<CallToolResult, ErrorData> {
        let args = apply_call_context(tool, args, &context);
        let caller = context.caller.as_str();
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
        let result = match self.invoke(tool, args.clone()).await {
            Ok(r) => r,
            // Protocol-level failure (unknown tool, malformed params): the
            // caller sees the error; make sure the operator does too.
            Err(e) => {
                tracing::error!(%tool, %caller, error = %e, "tool call failed (protocol error)");
                return Err(e);
            }
        };
        log_call(tool, &args, caller, &result, started.elapsed());

        if !is_error_result(&result) && journaled {
            let target = journal_target(tool, &args, &result);
            let args = journal_args(tool, args, target.as_deref());
            if let Some(id) = &target {
                self.studio().journal_append(id, tool, &args);
            }
        }
        Ok(result)
    }

    /// Invoke one tool by name: deserialize the args into that tool's own
    /// param struct and run the same handler the MCP router used to reach.
    /// The router is now only the schema registry (it advertises the surface);
    /// this match is the dispatch — one path for every transport. The
    /// count-pin and dispatch-coverage tests keep the two in lockstep.
    async fn invoke(&self, tool: &str, args: Value) -> Result<CallToolResult, ErrorData> {
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
            "doc_create" => call!(DocCreate, doc_create),
            "list_docs" => call!(ListDocs, list_docs),
            "doc_info" => call!(DocRef, doc_info),
            "delete_doc" => call!(DocRef, delete_doc),
            "doc_layer" => call!(DocLayer, doc_layer),
            "doc_frame" => call!(DocFrame, doc_frame),
            "doc_add_tag" => call!(DocAddTag, doc_add_tag),
            "doc_checkpoint" => call!(DocCheckpoint, doc_checkpoint),
            "doc_palette" => call!(DocPalette, doc_palette),
            "doc_batch" => call!(DocBatch, doc_batch),
            "doc_draw" => call!(DocDraw, doc_draw),
            "doc_fx" => call!(DocFx, doc_fx),
            "doc_region" => call!(DocRegion, doc_region),
            "doc_paint_grid" => call!(DocPaintGrid, doc_paint_grid),
            "doc_dither_ramp" => call!(DocDitherRamp, doc_dither_ramp),
            "doc_look" => call!(DocLook, doc_look),
            "doc_dump_region" => call!(DocDumpRegion, doc_dump_region),
            "doc_silhouette" => call!(DocSilhouette, doc_silhouette),
            "doc_components" => call!(DocComponents, doc_components),
            "doc_frame_diff" => call!(DocFrameDiff, doc_frame_diff),
            "doc_seam_report" => call!(DocSeamReport, doc_seam_report),
            "doc_anim_audit" => call!(DocAnimAudit, doc_anim_audit),
            "doc_critique" => call!(DocCritique, doc_critique),
            "doc_contact_sheet" => call!(DocContactSheet, doc_contact_sheet),
            "doc_ref" => call!(DocRefOp, doc_ref),
            "doc_export" => call!(DocExport, doc_export),
            _ => {
                return Err(ErrorData::invalid_params(
                    format!("unknown tool: {tool}"),
                    None,
                ));
            }
        })
    }
}

/// Tools that never change the document store. They still take a shared store
/// lock so a concurrent mutation cannot expose a half-written document.
const READ_ONLY_TOOLS: &[&str] = &[
    // the eye and the audits it reports through
    "doc_look",
    "doc_info",
    "doc_critique",
    "doc_silhouette",
    "doc_dump_region",
    "doc_components",
    "doc_anim_audit",
    "doc_frame_diff",
    "doc_seam_report",
    "doc_contact_sheet",
    // library/external output: not document-store mutations
    "list_docs",
    "doc_export",
];

/// Whether a call changes files in the document store. Kept separate from
/// journaling: checkpoints and reference setup mutate working state but are
/// session context, not deterministic recipe steps.
fn is_store_mutation(tool: &str, args: &Value) -> bool {
    if READ_ONLY_TOOLS.contains(&tool) {
        return false;
    }
    let op = call_op(args);
    !matches!(
        (tool, op),
        ("doc_ref", Some("analyze" | "compare" | "diff"))
            | ("doc_palette", Some("report"))
            | ("doc_checkpoint", Some("list"))
    ) && !matches!(
        (tool, op),
        ("doc_palette", None | Some("generate")) if args.get("set_doc").is_none()
    )
}

/// True when a successful mutation is one deterministic step in rebuilding the
/// document. A checkpoint restore replaces the journal with its snapshot;
/// recording save/restore/prune would make checkpoint ids part of the recipe.
/// Reference files are external working context, so no `doc_ref` op is
/// journaled. Unknown tools default to mutation+journal: a spurious recipe step
/// fails loudly, while omitting a real mutation silently changes the rebuild.
fn is_journaled(tool: &str, args: &Value) -> bool {
    is_store_mutation(tool, args) && !matches!(tool, "delete_doc" | "doc_checkpoint" | "doc_ref")
}

/// The document a call belongs to. `doc_create` names it in the result (the id
/// is minted there, not passed in); everything else carries `doc_id`.
fn journal_target(tool: &str, args: &Value, result: &CallToolResult) -> Option<String> {
    match tool {
        // The id is minted in the result, not passed in.
        "doc_create" => result_json(result)?
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string),
        // doc_export writes an external artifact; it does not define the
        // document's pixels, so it must NOT enter the per-document recipe —
        // replaying a rebuild would re-run the export against the author's
        // out_path (or fail-abort if that path is gone).
        "doc_export" => None,
        // op=generate locks its palette onto `set_doc`, carrying no `doc_id`;
        // without this the lock is silently dropped from the recipe and replay
        // rebuilds the document off-palette.
        "doc_palette" => args
            .get("doc_id")
            .or_else(|| args.get("set_doc"))
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => args
            .get("doc_id")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

/// Union of doc_batch's `frame` and `frames`, in order, deduped — one call
/// fixes a static layer across the whole timeline instead of N round-trips.
fn batch_targets(frame: usize, frames: Option<Vec<usize>>) -> Vec<usize> {
    let mut targets = vec![frame];
    for f in frames.into_iter().flatten() {
        if !targets.contains(&f) {
            targets.push(f);
        }
    }
    targets
}

/// `doc_create`'s minted id exists only in its result. Stamping it into the
/// recorded args lets replay remap later steps when a rerun mints a new id.
fn journal_args(tool: &str, mut args: Value, target: Option<&str>) -> Value {
    if tool == "doc_create"
        && let (Some(id), Some(obj)) = (target, args.as_object_mut())
    {
        obj.insert("doc_id".into(), json!(id));
    }
    args
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct McpCallContext {
    doc_id: Option<String>,
    layer: Option<usize>,
    frame: Option<usize>,
    session: Option<String>,
}

fn context_from_meta(
    meta: Option<&rmcp::model::Meta>,
    fallback_caller: String,
) -> Result<CallContext, ErrorData> {
    let Some(raw) = meta.and_then(|meta| meta.0.get(CALL_CONTEXT_META_KEY)) else {
        return Ok(CallContext::new(fallback_caller));
    };
    let parsed: McpCallContext = serde_json::from_value(raw.clone()).map_err(|error| {
        ErrorData::invalid_params(
            format!("invalid MCP _meta['{CALL_CONTEXT_META_KEY}']: {error}"),
            None,
        )
    })?;
    let caller = match parsed.session {
        Some(session)
            if session.is_empty()
                || session.len() > 128
                || session.chars().any(char::is_control) =>
        {
            return Err(ErrorData::invalid_params(
                "atelier call-context session must be 1..=128 bytes with no control characters",
                None,
            ));
        }
        Some(session) => session,
        None => fallback_caller,
    };
    Ok(CallContext {
        caller,
        doc_id: parsed.doc_id,
        layer: parsed.layer,
        frame: parsed.frame,
    })
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
        let tool = request.name.to_string();
        let args = request
            .arguments
            .clone()
            .map(Value::Object)
            .unwrap_or_else(|| json!({}));
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
        let call_context = context_from_meta(Some(&context.meta), caller)?;
        self.dispatch(&tool, args, call_context).await
    }

    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some(
            "Atelier is a stateless, offline pixel-art editor. Keep the id returned by \
             doc_create; use doc_batch or doc_paint_grid for a burst of marks and doc_look \
             to inspect the result. Calls may carry {doc_id,layer,frame,session} under MCP \
             _meta[\"io.github.marmikshah.atelier/context\"]; explicit arguments win and \
             resolved arguments are \
             journaled, so stdio, HTTP, CLI, and replay stay equivalent. Save a \
             doc_checkpoint before destructive edits. All 26 tools are advertised."
                .into(),
        );
        info
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PUBLIC_TOOL_NAMES: &[&str] = &[
        "delete_doc",
        "doc_add_tag",
        "doc_anim_audit",
        "doc_batch",
        "doc_checkpoint",
        "doc_components",
        "doc_contact_sheet",
        "doc_create",
        "doc_critique",
        "doc_dither_ramp",
        "doc_draw",
        "doc_dump_region",
        "doc_export",
        "doc_frame",
        "doc_frame_diff",
        "doc_fx",
        "doc_info",
        "doc_layer",
        "doc_look",
        "doc_paint_grid",
        "doc_palette",
        "doc_ref",
        "doc_region",
        "doc_seam_report",
        "doc_silhouette",
        "list_docs",
    ];

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
            PUBLIC_TOOL_NAMES.len(),
            "tool count changed — update the docs"
        );
        assert_eq!(
            Atelier::registry_tools().len(),
            PUBLIC_TOOL_NAMES.len(),
            "every tool is advertised; there is no profile filter"
        );
        let instructions = temp_atelier("info")
            .get_info()
            .instructions
            .unwrap_or_default();
        assert!(
            instructions.contains("26 tools"),
            "get_info instructions drifted from the tool count"
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
        let expected: Vec<String> = PUBLIC_TOOL_NAMES
            .iter()
            .map(|name| (*name).to_string())
            .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn the_registry_lists_without_a_studio() {
        // `atelier tools` lists the registry only; building a `Studio` for it
        // used to create ~/.atelier/documents as a side effect of `--help`-level
        // work. The router is an associated fn, so nothing here touches disk.
        assert_eq!(Atelier::registry_tools().len(), 26);
        assert!(tools_text().starts_with("atelier tools — 26 tools\n"));
        assert!(tools_html().contains("26</strong> tools"));
    }

    #[tokio::test]
    async fn dispatch_recognizes_every_advertised_tool() {
        // invoke() is a hand-written match parallel to the router's registry:
        // add a #[tool] without adding a dispatch arm and every caller hears
        // "unknown tool" — while list_tools still happily advertises it. This
        // pins the lockstep the count-pin test alone cannot see.
        let a = temp_atelier("dispatch-coverage");
        for t in Atelier::registry_tools() {
            let name = t.name.to_string();
            if let Err(e) = a.dispatch(&name, json!({}), CallContext::new("test")).await {
                assert!(
                    !e.message.contains("unknown tool"),
                    "{name} is advertised but dispatch has no arm for it"
                );
            }
        }
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
    fn every_read_only_tool_is_a_real_tool() {
        let names: Vec<String> = Atelier::tool_router()
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        for t in READ_ONLY_TOOLS {
            assert!(
                names.contains(&t.to_string()),
                "READ_ONLY_TOOLS names '{t}', which is not a tool — renamed or removed?"
            );
        }
    }

    #[test]
    fn the_eye_is_never_journaled_but_the_hand_always_is() {
        // Reads rebuild nothing; replaying them is noise.
        for t in ["doc_look", "doc_info", "doc_critique", "doc_silhouette"] {
            assert!(!is_journaled(t, &json!({"doc_id": "d"})), "{t} is a read");
        }
        assert!(
            !is_journaled("doc_export", &json!({"doc_id": "d"})),
            "export writes an artifact but does not build the document"
        );
        // Anything that marks the canvas has to be in the recipe.
        for t in ["doc_draw", "doc_batch", "doc_fx", "doc_create"] {
            assert!(
                is_journaled(t, &json!({"doc_id": "d"})),
                "{t} builds the art"
            );
        }
    }

    #[test]
    fn hub_tools_are_classified_by_op_not_by_name() {
        // Reference setup changes working state but is external context, not a
        // deterministic recipe step.
        assert!(!is_journaled("doc_ref", &json!({"op": "compare"})));
        assert!(!is_journaled("doc_ref", &json!({"op": "diff"})));
        assert!(!is_store_mutation("doc_ref", &json!({"op": "compare"})));
        assert!(is_store_mutation("doc_ref", &json!({"op": "set"})));
        assert!(!is_journaled("doc_ref", &json!({"op": "set"})));

        // Palette reports and unbound generation are reads; document-targeted
        // palette ops remain deterministic mutations.
        assert!(!is_journaled("doc_palette", &json!({"op": "report"})));
        assert!(is_journaled("doc_palette", &json!({"op": "set"})));
        assert!(!is_store_mutation(
            "doc_palette",
            &json!({"op": "generate"})
        ));
        assert!(is_journaled(
            "doc_palette",
            &json!({"op": "generate", "set_doc": "hero"})
        ));

        // Checkpoint files mutate the store, but restore replaces the live
        // journal with the checkpointed one instead of recording checkpoint ids.
        assert!(!is_journaled("doc_checkpoint", &json!({"action": "list"})));
        assert!(!is_store_mutation(
            "doc_checkpoint",
            &json!({"action": "list"})
        ));
        assert!(is_store_mutation(
            "doc_checkpoint",
            &json!({"action": "restore"})
        ));
        assert!(!is_journaled(
            "doc_checkpoint",
            &json!({"action": "restore"})
        ));
        // An unknown tool defaults to journaled: a missing mutation corrupts
        // the replay, a spurious step only wastes a line.
        assert!(is_journaled("doc_newly_added_tool", &json!({})));
    }

    #[test]
    fn doc_create_is_journaled_to_the_id_it_minted() {
        // The id is in the result, not the args — journaling by args alone
        // would file every doc_create under nothing.
        let created = CallToolResult::success(vec![Content::text(
            json!({"id": "sprite", "w": 8}).to_string(),
        )]);
        assert_eq!(
            journal_target("doc_create", &json!({"name": "sprite"}), &created).as_deref(),
            Some("sprite")
        );
        let drew = CallToolResult::success(vec![Content::text(json!({"ok": true}).to_string())]);
        assert_eq!(
            journal_target("doc_draw", &json!({"doc_id": "sprite"}), &drew).as_deref(),
            Some("sprite")
        );
    }

    #[test]
    fn batch_frames_union_keeps_order_and_dedupes() {
        assert_eq!(batch_targets(0, None), vec![0]);
        assert_eq!(batch_targets(0, Some(vec![2, 0, 1, 2])), vec![0, 2, 1]);
        assert_eq!(batch_targets(3, Some(vec![])), vec![3]);
    }

    #[test]
    fn call_context_expands_supported_fields_without_overriding_args() {
        let context = CallContext {
            caller: "session-a".into(),
            doc_id: Some("hero".into()),
            layer: Some(2),
            frame: Some(3),
        };
        assert_eq!(
            apply_call_context("doc_draw", json!({"op": "clear_cel", "frame": 7}), &context),
            json!({
                "doc_id": "hero",
                "layer": 2,
                "frame": 7,
                "op": "clear_cel"
            }),
            "explicit frame wins over the context default"
        );
        assert_eq!(
            apply_call_context("doc_palette", json!({"op": "generate"}), &context),
            json!({"op": "generate"}),
            "unbound palette generation must not mutate the active document"
        );
        assert_eq!(
            apply_call_context("list_docs", json!({}), &context),
            json!({}),
            "library calls do not acquire document/cel context"
        );
        assert_eq!(
            apply_call_context(
                "doc_layer",
                json!({"op": "rename", "name": "ink"}),
                &context
            ),
            json!({"doc_id": "hero", "op": "rename", "index": 2, "name": "ink"}),
            "the layer default maps to doc_layer's index field"
        );
        assert_eq!(
            apply_call_context("doc_layer", json!({"op": "add"}), &context),
            json!({"doc_id": "hero", "op": "add"}),
            "adding a layer has no existing-layer target"
        );
    }

    #[test]
    fn mcp_context_is_namespaced_strict_and_session_named() {
        let meta: rmcp::model::Meta = serde_json::from_value(json!({
            CALL_CONTEXT_META_KEY: {
                "doc_id": "hero",
                "layer": 1,
                "frame": 2,
                "session": "sprite-pass"
            }
        }))
        .unwrap();
        let context = context_from_meta(Some(&meta), "transport".into()).unwrap();
        assert_eq!(
            context,
            CallContext {
                caller: "sprite-pass".into(),
                doc_id: Some("hero".into()),
                layer: Some(1),
                frame: Some(2),
            }
        );

        let bad: rmcp::model::Meta = serde_json::from_value(json!({
            CALL_CONTEXT_META_KEY: {"unknown": true}
        }))
        .unwrap();
        assert!(context_from_meta(Some(&bad), "transport".into()).is_err());
    }

    #[test]
    fn store_mutations_are_classified_for_locking() {
        // Its own journal dies with the doc dir, so journaling delete is moot.
        assert!(!is_journaled("delete_doc", &json!({"doc_id": "x"})));
        assert!(is_store_mutation("delete_doc", &json!({"doc_id": "x"})));
        assert!(is_store_mutation("doc_draw", &json!({"doc_id": "x"})));
        assert!(!is_store_mutation(
            "doc_export",
            &json!({"doc_id": "x", "op": "sheet"})
        ));
        assert!(!is_store_mutation("doc_look", &json!({"doc_id": "x"})));
    }

    #[test]
    fn doc_create_journals_the_minted_id_for_replay_remapping() {
        // A collision mints `sprite-2`; without the stamp, replay could not
        // tell which recorded id the later steps' `doc_id: "sprite"` meant.
        assert_eq!(
            journal_args("doc_create", json!({"name": "sprite"}), Some("sprite-2")),
            json!({"name": "sprite", "doc_id": "sprite-2"})
        );
        // Every other tool records its args untouched.
        assert_eq!(
            journal_args("doc_draw", json!({"doc_id": "sprite"}), Some("sprite")),
            json!({"doc_id": "sprite"})
        );
    }

    #[test]
    fn replay_fidelity_edges_are_journaled_correctly() {
        let ok = CallToolResult::success(vec![Content::text(json!({"ok": true}).to_string())]);
        // doc_export writes an artifact, not document state — replaying a rebuild
        // must not re-run it, so it belongs to no document's recipe.
        assert!(!is_journaled(
            "doc_export",
            &json!({"doc_id": "hero", "op": "sheet"})
        ));
        assert_eq!(
            journal_target("doc_export", &json!({"doc_id": "hero", "op": "sheet"}), &ok),
            None,
            "export must not enter the per-document journal"
        );
        // doc_palette op=generate locks a palette onto `set_doc`, carrying no
        // `doc_id`; the recipe must still capture it or replay rebuilds off-palette.
        assert_eq!(
            journal_target(
                "doc_palette",
                &json!({"op": "generate", "set_doc": "hero"}),
                &ok
            )
            .as_deref(),
            Some("hero")
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
    fn base64_matches_known_vectors() {
        // RFC 4648 test vectors exercise the 0/1/2 trailing-byte padding cases.
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[tokio::test]
    async fn resolved_context_is_journaled_and_checkpoint_restore_rewinds_it() {
        async fn ok(atelier: &Atelier, tool: &str, args: Value, context: CallContext) -> Value {
            let result = atelier.dispatch(tool, args, context).await.unwrap();
            assert!(
                !is_error_result(&result),
                "{tool} failed: {:?}",
                result_json(&result)
            );
            result_json(&result).unwrap()
        }

        let atelier = temp_atelier("context-checkpoint");
        ok(
            &atelier,
            "doc_create",
            json!({"name": "hero", "width": 4, "height": 4}),
            CallContext::new("test"),
        )
        .await;
        let cel = || CallContext {
            caller: "test".into(),
            doc_id: Some("hero".into()),
            layer: Some(0),
            frame: Some(0),
        };
        ok(
            &atelier,
            "doc_draw",
            json!({"op": "pencil", "points": [[1, 1]], "color": [200, 0, 0]}),
            cel(),
        )
        .await;
        let saved = ok(
            &atelier,
            "doc_checkpoint",
            json!({"action": "save", "label": "red"}),
            cel(),
        )
        .await;
        let checkpoint_id = saved["saved"].as_str().unwrap();

        ok(
            &atelier,
            "doc_draw",
            json!({"op": "pencil", "points": [[1, 1]], "color": [0, 0, 200]}),
            cel(),
        )
        .await;
        assert_eq!(atelier.studio().journal("hero").unwrap().len(), 3);

        ok(
            &atelier,
            "doc_checkpoint",
            json!({"action": "restore", "checkpoint_id": checkpoint_id}),
            cel(),
        )
        .await;
        let journal = atelier.studio().journal("hero").unwrap();
        assert_eq!(
            journal.len(),
            2,
            "restore must discard post-checkpoint provenance"
        );
        assert_eq!(
            journal[1]["args"],
            json!({
                "doc_id": "hero",
                "layer": 0,
                "frame": 0,
                "op": "pencil",
                "points": [[1, 1]],
                "color": [200, 0, 0]
            }),
            "the journal stores resolved context, not ambient defaults"
        );
        assert_eq!(
            atelier
                .studio()
                .doc_dump_region("hero", 0, Some(0), Some((1, 1, 1, 1)), "hex")
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
    fn rgba_is_strict_like_the_batch_validator() {
        assert_eq!(rgba(&[255, 128, 0]).unwrap(), [255, 128, 0, 255]);
        assert_eq!(rgba(&[1, 2, 3, 4]).unwrap(), [1, 2, 3, 4]);
        // These truncated silently before (300 → 44, -1 → 255, short → 0s).
        assert!(rgba(&[255, 300, 0]).is_err());
        assert!(rgba(&[255, -1, 0]).is_err());
        assert!(rgba(&[255, 0]).is_err());
        assert!(rgba(&[1, 2, 3, 4, 5]).is_err());
    }
}
