//! atelier MCP server (rmcp). Exposes the headless document editor as MCP
//! tools over stdio or Streamable HTTP. Tools return JSON text content; studio
//! errors keep the {"error": ...} payload AND set `is_error` so MCP harnesses
//! flag the failure instead of treating it as a success.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock as Content, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResult, Resource,
    ResourceContents, ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, tool_handler};
use serde_json::{Value, json};

use atelier_studio::Studio;

mod params;
mod recorder;
mod resources;
mod toolsdoc;
mod transport;

pub use toolsdoc::{tools_html, tools_text};
pub use transport::{run, run_http};

use params::*;
use recorder::Recorder;
use resources::{RESOURCE_RENDER_SCALE, ResourceTarget, base64, parse_resource_uri};

fn j(v: Value) -> String {
    serde_json::to_string(&v).unwrap_or_else(|_| "{}".into())
}

/// Wraps a studio result as a tool result: Ok becomes a JSON text part; errors
/// keep the {"error": ...} payload (replay and older clients sniff it) and set
/// `is_error` so the harness surfaces the failure.
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
    let op = args.get("op").and_then(Value::as_str).unwrap_or("-");
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

#[derive(Clone)]
pub struct Atelier {
    /// Shared so concurrent HTTP sessions serialise document file writes.
    studio: std::sync::Arc<std::sync::Mutex<Studio>>,
    tool_router: ToolRouter<Self>,
    /// Optional session recorder; when set, each tool call is logged to a recipe.
    recorder: Option<Recorder>,
    /// Held across dispatch + journal for every mutating call (an async lock,
    /// because it spans the dispatcher's await). The studio mutex serialises
    /// the mutations themselves, but it is released between the mutation and
    /// its `journal_append` — without this outer lock two concurrent sessions
    /// could execute A→B and journal B→A, and the recipe would silently
    /// rebuild different art.
    write_order: std::sync::Arc<tokio::sync::Mutex<()>>,
}

impl Atelier {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self::with_studio(std::sync::Arc::new(std::sync::Mutex::new(Studio::new())))
    }

    pub fn with_studio(studio: std::sync::Arc<std::sync::Mutex<Studio>>) -> Self {
        Self {
            studio,
            tool_router: Self::tool_router(),
            recorder: None,
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

    /// Enable session recording: every tool call is appended to a recipe at `path`.
    pub fn with_recording(mut self, path: std::path::PathBuf) -> Self {
        self.recorder = Some(Recorder::new(path));
        self
    }

    /// The shared studio.
    ///
    /// Recovers from a poisoned lock instead of propagating the panic: one bad
    /// tool call used to take the whole server down with it, since every later
    /// call re-panicked on the poison. The studio keeps almost no in-memory
    /// state — documents load and save per op — so the guarded data is not
    /// meaningfully corrupted by a panic mid-op, and a live server that answers
    /// the next call beats one that is bricked until restart.
    fn studio(&self) -> std::sync::MutexGuard<'_, Studio> {
        self.studio
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// THE one dispatch path every caller funnels through — the MCP handler
    /// (`call_tool`), and the binary's own `atelier call` / `replay`.
    /// Logging, journaling, write ordering and session recording live here so
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
        caller: &str,
    ) -> Result<CallToolResult, ErrorData> {
        let recorder = self.recorder.clone();
        // For mutations, hold the order lock from before the dispatch until
        // after the journal write, so journal order can never diverge from
        // execution order under concurrent sessions. Reads skip it.
        let journaled = is_journaled(tool, &args);
        let recorded = is_recorded(tool, &args);
        let write_order = self.write_order.clone();
        let _order = if recorded {
            Some(write_order.lock().await)
        } else {
            None
        };

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

        // A read rebuilds nothing, so it belongs in neither recipe. Both
        // recorders answer to the same question — what did it take to make
        // this? — so they share the one classifier.
        if !is_error_result(&result) && recorded {
            let target = journal_target(tool, &args, &result);
            // A successful paste consumed the clipboard as it stands now (the
            // order lock is still held, so nothing changed it since).
            let clipboard = if tool == "doc_region"
                && args.get("op").and_then(Value::as_str) == Some("paste")
                && args.get("clipboard").is_none()
            {
                self.studio()
                    .clipboard_pixels()
                    .map(|(w, h, b)| (w, h, b.to_vec()))
            } else {
                None
            };
            let args = recorded_args(tool, args, target.as_deref(), clipboard);
            // The document's own journal: on by default, so every document is a
            // replayable recipe without anyone having to know to ask first.
            if journaled && let Some(id) = &target {
                self.studio().journal_append_compact(id, tool, &args);
            }
            // The session recorder (--record) stays opt-in and cross-document:
            // it captures a whole sitting, which per-document journals cannot
            // express (that is also why it gets delete_doc and the journal
            // does not).
            if let Some(recorder) = recorder {
                recorder.record(tool, args);
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
            "doc_clear_cel" => call!(DocCel, doc_clear_cel),
            "doc_checkpoint" => call!(DocCheckpoint, doc_checkpoint),
            "doc_palette" => call!(DocPalette, doc_palette),
            "doc_batch" => call!(DocBatch, doc_batch),
            "doc_draw" => call!(DocDraw, doc_draw),
            "doc_fx" => call!(DocFx, doc_fx),
            "doc_region" => call!(DocRegion, doc_region),
            "doc_select" => call!(DocSelect, doc_select),
            "doc_paint_grid" => call!(DocPaintGrid, doc_paint_grid),
            "doc_dither_ramp" => call!(DocDitherRamp, doc_dither_ramp),
            "doc_tile" => call!(DocTile, doc_tile),
            "doc_look" => call!(DocLook, doc_look),
            "doc_dump_region" => call!(DocDumpRegion, doc_dump_region),
            "doc_silhouette" => call!(DocSilhouette, doc_silhouette),
            "doc_slice" => call!(DocSlice, doc_slice),
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

    /// Enumerate every browsable resource: per document, its structure JSON and a
    /// frame-0 PNG render, each with a human-readable name.
    fn list_resource_specs(&self) -> Vec<Resource> {
        let docs = self.studio().list_docs();
        let mut out = Vec::new();
        for d in docs["documents"].as_array().into_iter().flatten() {
            let Some(id) = d["id"].as_str() else { continue };
            let name = d["name"].as_str().unwrap_or(id);
            out.push(
                Resource::new(format!("atelier://doc/{id}"), format!("{name} (structure)"))
                    .with_description("Document structure: layers, frames, cels, tags.")
                    .with_mime_type("application/json"),
            );
            out.push(
                Resource::new(
                    format!("atelier://doc/{id}/render"),
                    format!("{name} (render)"),
                )
                .with_description(format!(
                    "Frame 0 flattened to a PNG at scale {RESOURCE_RENDER_SCALE}."
                ))
                .with_mime_type("image/png"),
            );
        }
        out
    }

    /// Resolve a parsed [`ResourceTarget`] to its contents (structure JSON text or
    /// a PNG blob). Studio errors surface as `resource_not_found`.
    fn fetch_resource(
        &self,
        uri: &str,
        target: ResourceTarget,
    ) -> Result<ResourceContents, String> {
        match target {
            ResourceTarget::Structure(id) => {
                let v = self.studio().doc_info(&id)?;
                Ok(ResourceContents::text(j(v), uri).with_mime_type("application/json"))
            }
            ResourceTarget::Render(id) => {
                let png = self
                    .studio()
                    .render_png_bytes(&id, 0, RESOURCE_RENDER_SCALE)?;
                Ok(ResourceContents::blob(base64(&png), uri).with_mime_type("image/png"))
            }
        }
    }
}

/// Tools that only LOOK: they read a document and never change it, so replaying
/// them rebuilds nothing. (Some can write a PREVIEW artifact via `out_path` —
/// doc_look, doc_frame_diff, doc_seam_report — but previews are not the art,
/// and re-running them against a moved out_path would be worse than skipping.)
///
/// This is an allowlist, and the default is deliberately the other way: an
/// unlisted tool gets journaled. A stray read in a recipe replays as a harmless
/// no-op, but a mutation missing from a recipe silently produces different art —
/// so when in doubt, record. Anything added here must be genuinely inert.
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
    // library-level: not part of any one document's provenance
    "list_docs",
    "delete_doc",
];

/// True when this call is part of how the document got made, and so belongs in
/// its journal.
///
/// The hub tools are mixed — `doc_ref op=compare` is the see-and-fix loop's eye
/// while `op=import` paints a guide layer — so the op decides, not the tool. The
/// read ops are also the ones an agent calls most, which is exactly the noise a
/// recipe should not carry.
fn is_journaled(tool: &str, args: &Value) -> bool {
    if READ_ONLY_TOOLS.contains(&tool) {
        return false;
    }
    let op = args.get("op").and_then(Value::as_str);
    !matches!(
        (tool, op),
        ("doc_ref", Some("analyze" | "compare" | "diff"))
            | ("doc_palette", Some("report"))
            | ("doc_checkpoint", Some("list"))
    )
}

/// What the session recorder (`--record`) captures: everything the journal
/// does, plus `delete_doc`. Deleting is not part of any one document's
/// provenance (the whole dir is gone), but a cross-document sitting that
/// creates x, deletes it, and creates x again replays as x + x-2 unless the
/// delete is in the recording.
fn is_recorded(tool: &str, args: &Value) -> bool {
    tool == "delete_doc" || is_journaled(tool, args)
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
        // out_path (or fail-abort if that path is gone). The session recorder
        // still captures it (it keys off `is_journaled`, not this).
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

/// The args a call is recorded with, enriched so the recording is
/// self-contained:
///
/// - `doc_create`'s minted id exists only in its result; stamping it into the
///   recorded args lets `atelier replay` remap every later step's ids when a
///   re-run mints a different one (replay strips it before sending).
/// - a paste's pixels exist only in the process clipboard, which may have been
///   filled from another document — the per-document journal cannot express
///   that, so the pixels ride along (base64 RGBA) and replay pastes them
///   directly.
fn recorded_args(
    tool: &str,
    mut args: Value,
    target: Option<&str>,
    clipboard: Option<(u32, u32, Vec<u8>)>,
) -> Value {
    if tool == "doc_create"
        && let (Some(id), Some(obj)) = (target, args.as_object_mut())
    {
        obj.insert("doc_id".into(), json!(id));
    }
    if tool == "doc_region"
        && args.get("op").and_then(Value::as_str) == Some("paste")
        && let (Some((w, h, buf)), Some(obj)) = (clipboard, args.as_object_mut())
    {
        obj.insert(
            "clipboard".into(),
            json!({"w": w, "h": h, "data": base64(&buf)}),
        );
    }
    args
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
        // Caller identity for the log line. `X-Atelier-Caller` header when the
        // client chose a name; otherwise the TCP peer address — each client
        // process holds its own keep-alive connection, so the port separates
        // two sessions of the *same* client (e.g. two editor windows) with no
        // config at all. stdio has one caller by definition and logs `-`.
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
        self.dispatch(&tool, args, &caller).await
    }

    /// List the browsable resources: structure JSON + frame-0 render per document.
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::with_all_items(
            self.list_resource_specs(),
        ))
    }

    /// Read one resource by URI. Unknown URIs and missing documents become a
    /// `resource_not_found` error rather than a panic.
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        let target = parse_resource_uri(&request.uri).ok_or_else(|| {
            ErrorData::resource_not_found(format!("unknown resource uri: {}", request.uri), None)
        })?;
        let contents = self
            .fetch_resource(&request.uri, target)
            .map_err(|e| ErrorData::resource_not_found(e, None))?;
        Ok(ReadResourceResult::new(vec![contents]))
    }

    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .build();
        info.instructions = Some(
            "atelier: the pixel-art studio you can see — a headless editor where every \
             mark is a tool call and doc_look hands the frame back as an image. doc_create a \
             layered/animated document, then paint cels with doc_draw (one op: \
             line/rect/ellipse/fill/stroke/text/…) or doc_batch (many ops in one call). LOOK with doc_look \
             after every burst of edits — it returns the frame as an INLINE image plus \
             value stats (pass doc_look out_path when you also need the PNG written to a file). \
             Recreating from a reference image? doc_ref op=set FIRST, doc_ref op=analyze \
             to plan (subject palette + silhouette), optionally doc_ref op=import onto a \
             guide layer, then doc_ref op=compare after EVERY pass — it scores silhouette \
             IoU and per-cell colour ΔE against the reference so likeness is measured, \
             not remembered. \
             Audit before exporting: doc_critique (failure modes), doc_palette op=report, \
             doc_silhouette. Animate by duplicating frames (doc_frame op=add copy_from) and \
             repainting only what moves — there is no pose interpolation; doc_frame_diff and \
             doc_anim_audit check what changed and whether the timing reads. doc_checkpoint \
             save before risky ops (quantize, palette snap) — restore rolls back. Export with \
             doc_export (op=sheet|anim|tileset) / op=all. list_docs browses the library. \
             30 tools, all of them advertised — there is no profile to switch."
                .into(),
        );
        info
    }
}

#[cfg(test)]
mod tests {
    use super::resources::base64_decode;
    use super::*;

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
        assert_eq!(n, 30, "tool count changed — update the docs");
        assert_eq!(
            Atelier::registry_tools().len(),
            30,
            "every tool is advertised; there is no profile filter"
        );
        let instructions = temp_atelier("info")
            .get_info()
            .instructions
            .unwrap_or_default();
        assert!(
            instructions.contains("30 tools"),
            "get_info instructions drifted from the tool count"
        );
    }

    #[test]
    fn the_registry_lists_without_a_studio() {
        // `atelier tools` lists the registry only; building a `Studio` for it
        // used to create ~/.atelier/documents as a side effect of `--help`-level
        // work. The router is an associated fn, so nothing here touches disk.
        assert_eq!(Atelier::registry_tools().len(), 30);
        assert!(tools_text().starts_with("atelier tools — 30 tools\n"));
        assert!(tools_html().contains("30</strong> tools"));
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
            if let Err(e) = a.dispatch(&name, json!({}), "test").await {
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
                "READ_ONLY_TOOLS names '{t}', which is not a tool — renamed or removed? \
                 A stale entry here silently drops a real call from every journal."
            );
        }
    }

    #[test]
    fn the_eye_is_never_journaled_but_the_hand_always_is() {
        // Reads rebuild nothing; replaying them is noise.
        for t in ["doc_look", "doc_info", "doc_critique", "doc_silhouette"] {
            assert!(!is_journaled(t, &json!({"doc_id": "d"})), "{t} is a read");
        }
        // Anything that marks the canvas has to be in the recipe.
        for t in [
            "doc_draw",
            "doc_batch",
            "doc_fx",
            "doc_create",
            "doc_export",
        ] {
            assert!(
                is_journaled(t, &json!({"doc_id": "d"})),
                "{t} builds the art"
            );
        }
    }

    #[test]
    fn hub_tools_are_classified_by_op_not_by_name() {
        // doc_ref is both the eye (compare) and the hand (import).
        assert!(!is_journaled("doc_ref", &json!({"op": "compare"})));
        assert!(!is_journaled("doc_ref", &json!({"op": "diff"})));
        assert!(is_journaled("doc_ref", &json!({"op": "import"})));
        assert!(is_journaled("doc_ref", &json!({"op": "set"})));
        // Same split inside doc_palette and doc_checkpoint.
        assert!(!is_journaled("doc_palette", &json!({"op": "report"})));
        assert!(is_journaled("doc_palette", &json!({"op": "set"})));
        assert!(!is_journaled("doc_checkpoint", &json!({"op": "list"})));
        assert!(is_journaled("doc_checkpoint", &json!({"op": "restore"})));
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
    fn delete_doc_is_recorded_but_never_journaled() {
        // Its own journal dies with the doc dir, so journaling it is moot —
        // but a cross-document sitting that creates x, deletes it and creates
        // x again replays as x + x-2 unless the recording keeps the delete.
        assert!(!is_journaled("delete_doc", &json!({"doc_id": "x"})));
        assert!(is_recorded("delete_doc", &json!({"doc_id": "x"})));
        // Everything the journal keeps, the recorder keeps too.
        assert!(is_recorded("doc_draw", &json!({"doc_id": "x"})));
        assert!(!is_recorded("doc_look", &json!({"doc_id": "x"})));
    }

    #[test]
    fn a_reused_recording_path_is_truncated_not_appended() {
        let path = std::env::temp_dir().join("atelier-rec-truncate.jsonl");
        std::fs::write(&path, "{\"tool\":\"doc_create\",\"args\":{}}\n").unwrap();
        let rec = Recorder::new(path.clone());
        rec.record("doc_draw", json!({"doc_id": "x"}));
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text.lines().count(),
            2,
            "old sitting must not survive: {text}"
        );
        assert!(text.lines().next().unwrap().contains("\"v\":2"));
        assert!(text.contains("doc_draw"));
    }

    #[test]
    fn doc_create_records_the_minted_id_for_replay_remapping() {
        // A collision mints `sprite-2`; without the stamp, replay could not
        // tell which recorded id the later steps' `doc_id: "sprite"` meant.
        assert_eq!(
            recorded_args(
                "doc_create",
                json!({"name": "sprite"}),
                Some("sprite-2"),
                None
            ),
            json!({"name": "sprite", "doc_id": "sprite-2"})
        );
        // Every other tool records its args untouched.
        assert_eq!(
            recorded_args(
                "doc_draw",
                json!({"doc_id": "sprite"}),
                Some("sprite"),
                None
            ),
            json!({"doc_id": "sprite"})
        );
    }

    #[test]
    fn a_journaled_paste_embeds_its_pixels() {
        // The clipboard may have been filled from ANOTHER document; without
        // the pixels in the step, replaying this document's journal alone
        // fails with "clipboard is empty".
        let pixels = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
        let stamped = recorded_args(
            "doc_region",
            json!({"doc_id": "x", "op": "paste", "x": 1, "y": 2}),
            Some("x"),
            Some((2, 1, pixels.clone())),
        );
        let cb = &stamped["clipboard"];
        assert_eq!(cb["w"], 2);
        assert_eq!(cb["h"], 1);
        assert_eq!(base64_decode(cb["data"].as_str().unwrap()).unwrap(), pixels);
        // Non-paste region ops carry nothing extra.
        let copy = recorded_args(
            "doc_region",
            json!({"doc_id": "x", "op": "copy"}),
            Some("x"),
            None,
        );
        assert!(copy.get("clipboard").is_none());
    }

    #[test]
    fn replay_fidelity_edges_are_journaled_correctly() {
        let ok = CallToolResult::success(vec![Content::text(json!({"ok": true}).to_string())]);
        // doc_export writes an artifact, not document state — replaying a rebuild
        // must not re-run it, so it belongs to no document's recipe.
        assert!(is_journaled(
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
    fn recorder_appends_a_replayable_jsonl_recipe() {
        let path = std::env::temp_dir().join("atelier-rec-roundtrip.jsonl");
        let _ = std::fs::remove_file(&path);
        let rec = Recorder::new(path.clone());

        rec.record("doc_create", json!({"name": "x", "width": 8, "height": 8}));
        rec.record("doc_draw", json!({"doc_id": "x", "op": "rect"}));

        // Whatever the recorder leaves on disk must parse through replay's own
        // parser — the two halves share this format or neither works.
        let src = std::fs::read_to_string(&path).expect("recipe file written");
        assert_eq!(
            src.lines().count(),
            3,
            "one header plus one appended line per call"
        );
        let recipe = crate::recipe::Recipe::parse(&src).expect("recipe parses");
        assert_eq!(recipe.steps.len(), 2);
        assert_eq!(recipe.steps[0].tool, "doc_create");
        assert_eq!(recipe.steps[0].args["width"], 8);
        assert_eq!(recipe.steps[1].args["op"], "rect");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn recorder_skips_failed_steps() {
        let path = std::env::temp_dir().join("atelier-rec-skip.json");
        let _ = std::fs::remove_file(&path);
        let rec = Recorder::new(path.clone());

        // Mirror call_tool: record only when the result is not an error payload.
        let ok = rmcp::model::CallToolResult::success(vec![rmcp::model::ContentBlock::text(j(
            json!({"id": "x"}),
        ))]);
        let err = rmcp::model::CallToolResult::success(vec![rmcp::model::ContentBlock::text(j(
            json!({"error": "no such doc"}),
        ))]);
        assert!(!is_error_result(&ok));
        assert!(is_error_result(&err));
        if !is_error_result(&ok) {
            rec.record("doc_create", json!({"name": "x"}));
        }
        if !is_error_result(&err) {
            rec.record("doc_info", json!({"doc_id": "nope"}));
        }

        let src = std::fs::read_to_string(&path).expect("recipe file written");
        let recipe = crate::recipe::Recipe::parse(&src).expect("recipe parses");
        assert_eq!(recipe.steps.len(), 1);
        assert_eq!(recipe.steps[0].tool, "doc_create");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn recorder_creates_missing_parent_dir() {
        let base = std::env::temp_dir().join(format!("atelier-rec-nested-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let path = base.join("a").join("b").join("recipe.json");
        let rec = Recorder::new(path.clone()); // must create a/b/
        rec.record("doc_create", json!({"name": "x"}));
        assert!(path.exists(), "recipe written into freshly created dirs");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn resource_uri_parses_and_rejects() {
        assert_eq!(
            parse_resource_uri("atelier://doc/hero"),
            Some(ResourceTarget::Structure("hero".into()))
        );
        assert_eq!(
            parse_resource_uri("atelier://doc/hero/render"),
            Some(ResourceTarget::Render("hero".into()))
        );
        // Unknown scheme, empty id, extra segments, and bare prefixes -> None.
        assert_eq!(parse_resource_uri("file:///hero"), None);
        assert_eq!(parse_resource_uri("atelier://doc/"), None);
        assert_eq!(parse_resource_uri("atelier://doc//render"), None);
        assert_eq!(parse_resource_uri("atelier://doc/hero/extra"), None);
        assert_eq!(parse_resource_uri("atelier://doc/hero/render/x"), None);
    }

    #[test]
    fn base64_decode_round_trips_and_rejects_garbage() {
        for input in [&b""[..], b"f", b"fo", b"foo", b"foob", b"\x00\xff\x10\x80"] {
            assert_eq!(base64_decode(&base64(input)).unwrap(), input);
        }
        assert!(base64_decode("a!!!").is_err());
        assert!(base64_decode("a").is_err(), "dangling single char");
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

    /// Build an Atelier whose studio is rooted at a throwaway temp dir.
    fn temp_atelier(tag: &str) -> Atelier {
        let dir = std::env::temp_dir().join(format!("atelier-srv-test-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        let studio = Studio::with_docs_dir(dir);
        Atelier::with_studio(std::sync::Arc::new(std::sync::Mutex::new(studio)))
    }

    #[test]
    fn fetch_returns_structure_json_and_png_blob() {
        let a = temp_atelier("fetch");
        a.studio().doc_create("Hero", 8, 8).unwrap();

        // Structure resource: JSON text carrying the document's dimensions.
        let s = a
            .fetch_resource(
                "atelier://doc/hero",
                ResourceTarget::Structure("hero".into()),
            )
            .expect("structure fetched");
        match s {
            ResourceContents::TextResourceContents {
                text, mime_type, ..
            } => {
                assert_eq!(mime_type.as_deref(), Some("application/json"));
                let v: Value = serde_json::from_str(&text).expect("valid json");
                assert_eq!(v["w"], 8);
                assert_eq!(v["id"], "hero");
            }
            other => panic!("expected text contents, got {other:?}"),
        }

        // Render resource: a base64 PNG blob (verify the magic bytes decode out).
        let r = a
            .fetch_resource(
                "atelier://doc/hero/render",
                ResourceTarget::Render("hero".into()),
            )
            .expect("render fetched");
        match r {
            ResourceContents::BlobResourceContents {
                blob, mime_type, ..
            } => {
                assert_eq!(mime_type.as_deref(), Some("image/png"));
                // base64 of a PNG always starts with the "iVBOR" signature.
                assert!(blob.starts_with("iVBOR"), "not a PNG blob: {}", &blob[..8]);
            }
            other => panic!("expected blob contents, got {other:?}"),
        }
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

    /// A tool that promises an inline preview it no longer returns lies to the
    /// model: it may skip doc_look believing it already saw the art. No edit
    /// tool's description may advertise one.

    #[test]
    fn list_resource_specs_pairs_each_doc() {
        let a = temp_atelier("list");
        a.studio().doc_create("Hero", 8, 8).unwrap();
        let specs = a.list_resource_specs();
        let uris: Vec<&str> = specs.iter().map(|r| r.uri.as_str()).collect();
        assert!(uris.contains(&"atelier://doc/hero"));
        assert!(uris.contains(&"atelier://doc/hero/render"));
        // Both carry their advertised mime types.
        for r in &specs {
            assert!(r.mime_type.is_some());
        }
    }

    #[test]
    fn fetch_unknown_doc_errors() {
        let a = temp_atelier("missing");
        let err = a
            .fetch_resource(
                "atelier://doc/ghost",
                ResourceTarget::Structure("ghost".into()),
            )
            .unwrap_err();
        assert!(err.contains("no document"), "got: {err}");
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
