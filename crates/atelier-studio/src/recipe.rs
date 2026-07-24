//! Replay recipes: the readable JSON form, legacy JSON Lines, and compact
//! JSONL v2.
//!
//! The compact form is still append-safe JSON Lines. Its first line is a
//! versioned header; every later completed line is either one call or one
//! context update. Repeated document/layer/frame fields ride a small context,
//! and common `doc_batch` operations use documented positional tuples. Unknown
//! operation shapes remain ordinary JSON objects, so adding an editor operation
//! never makes an older compact encoder lossy.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const COMPACT_VERSION: u32 = 2;
const JOURNAL_NAME: &str = "journal";
const JOURNAL_DESCRIPTION: &str = "recorded from the document's journal";
const RECORDING_NAME: &str = "recording";
const RECORDING_DESCRIPTION: &str = "recorded from a live Atelier session";

/// A replay recipe: a named, described sequence of tool calls.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Recipe {
    pub name: String,
    pub description: String,
    pub steps: Vec<Step>,
}

/// One scripted tool call.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Step {
    pub tool: String,
    #[serde(default = "empty_obj")]
    pub args: Value,
    /// Optional human note, echoed alongside the step for context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

fn empty_obj() -> Value {
    json!({})
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Context {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    doc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    layer: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    frame: Option<usize>,
}

impl Context {
    fn is_empty(&self) -> bool {
        self.doc.is_none() && self.layer.is_none() && self.frame.is_none()
    }

    fn apply(&mut self, update: &Context) {
        if let Some(doc) = &update.doc {
            self.doc = Some(doc.clone());
        }
        if let Some(layer) = update.layer {
            self.layer = Some(layer);
        }
        if let Some(frame) = update.frame {
            self.frame = Some(frame);
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CompactHeader {
    v: u32,
    name: String,
    description: String,
    #[serde(default, skip_serializing_if = "Context::is_empty")]
    defaults: Context,
}

/// One compact JSONL line after the header.
///
/// `doc`/`layer`/`frame` update the sticky context before the action. `at`
/// names exactly which context fields were removed from a generic call's args:
/// d = doc_id, l = layer, f = frame. A batch always uses all three. `use` is a
/// context-only line supported for hand-authored recipes; the encoder attaches
/// updates to actions to save a line.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CompactLine {
    #[serde(rename = "use", default, skip_serializing_if = "Option::is_none")]
    use_context: Option<Context>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    doc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    layer: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    frame: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    call: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    args: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    batch: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    frames: Option<Vec<usize>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

impl CompactLine {
    fn context_update(&self) -> Context {
        Context {
            doc: self.doc.clone(),
            layer: self.layer,
            frame: self.frame,
        }
    }

    fn has_action_fields(&self) -> bool {
        self.doc.is_some()
            || self.layer.is_some()
            || self.frame.is_some()
            || self.call.is_some()
            || self.at.is_some()
            || self.args.is_some()
            || self.batch.is_some()
            || self.frames.is_some()
            || self.note.is_some()
    }
}

/// Stateful compact-line encoder. The session recorder keeps one for the life
/// of a recording; a document journal uses a fresh doc-defaulted encoder for
/// each append so every line remains independently meaningful across CLI
/// processes.
#[doc(hidden)]
#[derive(Clone, Debug)]
pub struct CompactEncoder {
    context: Context,
}

impl CompactEncoder {
    pub fn recording() -> Self {
        Self {
            context: Context::default(),
        }
    }

    fn with_defaults(defaults: Context) -> Self {
        Self { context: defaults }
    }

    pub fn recording_header(&self) -> Result<String, String> {
        encode_header(RECORDING_NAME, RECORDING_DESCRIPTION, &self.context)
    }

    pub fn encode(&mut self, step: &Step) -> Result<String, String> {
        let line = self.compact_line(step)?;
        serde_json::to_string(&line).map_err(|error| format!("cannot encode compact step: {error}"))
    }

    fn compact_line(&mut self, step: &Step) -> Result<CompactLine, String> {
        let mut args = step.args.clone();
        let (at, update) = self.extract_context(&mut args);

        if step.tool == "doc_batch"
            && at == "dlf"
            && let Some((ops, frames)) = take_batch(&args)
        {
            return Ok(CompactLine {
                doc: update.doc,
                layer: update.layer,
                frame: update.frame,
                batch: Some(ops.iter().map(compact_batch_op).collect()),
                frames,
                note: step.note.clone(),
                ..CompactLine::default()
            });
        }

        let args = if args.as_object().is_some_and(serde_json::Map::is_empty) {
            None
        } else {
            Some(args)
        };
        Ok(CompactLine {
            doc: update.doc,
            layer: update.layer,
            frame: update.frame,
            call: Some(step.tool.clone()),
            at: (!at.is_empty()).then_some(at),
            args,
            note: step.note.clone(),
            ..CompactLine::default()
        })
    }

    /// Remove typed context fields from `args`, record exactly which fields were
    /// removed, and attach only context values that changed.
    fn extract_context(&mut self, args: &mut Value) -> (String, Context) {
        let Some(object) = args.as_object_mut() else {
            return (String::new(), Context::default());
        };
        let mut at = String::new();
        let mut update = Context::default();

        if let Some(doc) = object
            .get("doc_id")
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            object.remove("doc_id");
            at.push('d');
            if self.context.doc.as_deref() != Some(doc.as_str()) {
                self.context.doc = Some(doc.clone());
                update.doc = Some(doc);
            }
        }
        if let Some(layer) = object
            .get("layer")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
        {
            object.remove("layer");
            at.push('l');
            if self.context.layer != Some(layer) {
                self.context.layer = Some(layer);
                update.layer = Some(layer);
            }
        }
        if let Some(frame) = object
            .get("frame")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
        {
            object.remove("frame");
            at.push('f');
            if self.context.frame != Some(frame) {
                self.context.frame = Some(frame);
                update.frame = Some(frame);
            }
        }
        (at, update)
    }
}

/// Encode one per-document journal append. The header fixes the document
/// default; layer/frame updates stay on the line because separate CLI calls do
/// not share encoder memory.
pub(crate) fn compact_journal_record(
    id: &str,
    tool: &str,
    args: Value,
) -> Result<(String, String), String> {
    let defaults = Context {
        doc: Some(id.to_string()),
        ..Context::default()
    };
    let header = encode_header(JOURNAL_NAME, JOURNAL_DESCRIPTION, &defaults)?;
    let mut encoder = CompactEncoder::with_defaults(defaults);
    let line = encoder.encode(&Step {
        tool: tool.to_string(),
        args,
        note: None,
    })?;
    Ok((header, line))
}

fn encode_header(name: &str, description: &str, defaults: &Context) -> Result<String, String> {
    serde_json::to_string(&CompactHeader {
        v: COMPACT_VERSION,
        name: name.to_string(),
        description: description.to_string(),
        defaults: defaults.clone(),
    })
    .map_err(|error| format!("cannot encode compact recipe header: {error}"))
}

/// Validate the complete v2 header contract before a journal appends to it.
pub(crate) fn valid_compact_header(line: &str) -> bool {
    serde_json::from_str::<CompactHeader>(line).is_ok_and(|header| header.v == COMPACT_VERSION)
}

/// True when the source is legacy JSON Lines rather than one authored recipe
/// object or compact JSONL v2.
fn looks_like_jsonl(src: &str) -> bool {
    first_value(src).is_some_and(|value| value.get("tool").is_some())
}

fn first_value(src: &str) -> Option<Value> {
    src.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .and_then(|line| serde_json::from_str(line).ok())
}

fn compact_version(src: &str) -> Option<u64> {
    let first = first_value(src)?;
    if first.get("steps").is_some() || first.get("tool").is_some() {
        return None;
    }
    first.get("v").and_then(Value::as_u64)
}

impl Recipe {
    /// Parse any supported recipe shape, selected by content instead of the
    /// filename:
    ///
    /// - compact JSONL v2 — a `{v:2,…}` header plus context/call lines;
    /// - legacy JSONL — one `{tool,args}` call per line;
    /// - authored JSON — `{name,description,steps}`.
    pub fn parse(src: &str) -> Result<Recipe, String> {
        let recipe = if let Some(version) = compact_version(src) {
            if version != u64::from(COMPACT_VERSION) {
                return Err(format!(
                    "unsupported compact recipe version {version}; this Atelier supports version {COMPACT_VERSION}"
                ));
            }
            Self::parse_compact_jsonl(src)?
        } else if looks_like_jsonl(src) {
            Self::parse_jsonl(src)?
        } else {
            serde_json::from_str(src).map_err(|error| {
                format!(
                    "invalid recipe JSON: {error} — expected \
                     {{name, description, steps:[{{tool, args}}]}}, legacy JSON Lines of \
                     {{tool, args}}, or compact JSONL v{COMPACT_VERSION}"
                )
            })?
        };
        if recipe.steps.is_empty() {
            return Err("recipe has no steps — add at least one tool call".into());
        }
        Ok(recipe)
    }

    /// Deterministically encode this recipe as compact JSONL v2.
    pub fn to_compact_jsonl(&self) -> Result<String, String> {
        let defaults = first_context(&self.steps);
        let mut encoder = CompactEncoder::with_defaults(defaults.clone());
        let mut output = encode_header(&self.name, &self.description, &defaults)?;
        output.push('\n');
        for step in &self.steps {
            output.push_str(&encoder.encode(step)?);
            output.push('\n');
        }
        Ok(output)
    }

    /// Expand to the established, readable authored JSON shape.
    pub fn to_pretty_json(&self) -> Result<String, String> {
        let mut output = serde_json::to_string_pretty(self)
            .map_err(|error| format!("cannot encode recipe JSON: {error}"))?;
        output.push('\n');
        Ok(output)
    }

    /// Name the source shape for `atelier recipe stats`.
    pub fn source_format(src: &str) -> &'static str {
        if compact_version(src).is_some() {
            "compact-jsonl-v2"
        } else if looks_like_jsonl(src) {
            "legacy-jsonl"
        } else {
            "authored-json"
        }
    }

    /// Total operations nested inside `doc_batch` steps.
    pub fn batch_ops(&self) -> usize {
        self.steps
            .iter()
            .filter(|step| step.tool == "doc_batch")
            .filter_map(|step| step.args.get("ops").and_then(Value::as_array))
            .map(Vec::len)
            .sum()
    }

    /// Batch operations eligible for a positional v2 tuple. Everything else
    /// remains a lossless object.
    pub fn tuple_ops(&self) -> usize {
        self.steps
            .iter()
            .filter(|step| step.tool == "doc_batch")
            .filter_map(|step| step.args.get("ops").and_then(Value::as_array))
            .flatten()
            .filter(|op| op.is_object() && compact_batch_op(op).is_array())
            .count()
    }

    /// Parse legacy JSON Lines. A syntactically torn final line is dropped, but
    /// a complete malformed step is an error even when it is last.
    fn parse_jsonl(src: &str) -> Result<Recipe, String> {
        let nonempty = nonempty_lines(src);
        let last = nonempty.len().saturating_sub(1);
        let mut steps = Vec::new();
        for (index, (number, line)) in nonempty.iter().enumerate() {
            if let Err(error) = serde_json::from_str::<Value>(line) {
                if index == last {
                    warn_torn_final(*number);
                    break;
                }
                return Err(format!(
                    "line {}: {error} — expected {{tool, args}}",
                    number + 1
                ));
            }
            let step = serde_json::from_str::<Step>(line).map_err(|error| {
                format!("line {}: {error} — expected {{tool, args}}", number + 1)
            })?;
            steps.push(step);
        }
        Ok(Recipe {
            name: JOURNAL_NAME.into(),
            description: JOURNAL_DESCRIPTION.into(),
            steps,
        })
    }

    fn parse_compact_jsonl(src: &str) -> Result<Recipe, String> {
        let nonempty = nonempty_lines(src);
        let Some((header_number, header_line)) = nonempty.first() else {
            return Err("compact recipe is empty".into());
        };
        let header: CompactHeader = serde_json::from_str(header_line).map_err(|error| {
            format!(
                "line {}: invalid compact header: {error}",
                header_number + 1
            )
        })?;
        if header.v != COMPACT_VERSION {
            return Err(format!(
                "unsupported compact recipe version {}; this Atelier supports version {COMPACT_VERSION}",
                header.v
            ));
        }

        let mut context = header.defaults;
        let mut steps = Vec::new();
        let action_lines = &nonempty[1..];
        let last = action_lines.len().saturating_sub(1);
        for (index, (number, line)) in action_lines.iter().enumerate() {
            if let Err(error) = serde_json::from_str::<Value>(line) {
                if index == last {
                    warn_torn_final(*number);
                    break;
                }
                return Err(format!(
                    "line {}: invalid compact JSON: {error}",
                    number + 1
                ));
            }
            let compact: CompactLine = serde_json::from_str(line)
                .map_err(|error| format!("line {}: invalid compact step: {error}", number + 1))?;
            if let Some(update) = &compact.use_context {
                if compact.has_action_fields() {
                    return Err(format!(
                        "line {}: `use` must be a context-only line",
                        number + 1
                    ));
                }
                if update.is_empty() {
                    return Err(format!("line {}: `use` cannot be empty", number + 1));
                }
                context.apply(update);
                continue;
            }

            context.apply(&compact.context_update());
            match (&compact.call, &compact.batch) {
                (Some(tool), None) => {
                    if compact.frames.is_some() {
                        return Err(format!(
                            "line {}: `frames` belongs only to a batch",
                            number + 1
                        ));
                    }
                    if tool.is_empty() {
                        return Err(format!("line {}: `call` cannot be empty", number + 1));
                    }
                    let mut args = compact.args.clone().unwrap_or_else(empty_obj);
                    inject_context(
                        &mut args,
                        compact.at.as_deref().unwrap_or(""),
                        &context,
                        *number,
                    )?;
                    steps.push(Step {
                        tool: tool.clone(),
                        args,
                        note: compact.note.clone(),
                    });
                }
                (None, Some(batch)) => {
                    if compact.at.is_some() || compact.args.is_some() {
                        return Err(format!(
                            "line {}: a batch cannot also carry `at` or `args`",
                            number + 1
                        ));
                    }
                    let doc = context
                        .doc
                        .clone()
                        .ok_or_else(|| format!("line {}: batch has no doc context", number + 1))?;
                    let layer = context.layer.ok_or_else(|| {
                        format!("line {}: batch has no layer context", number + 1)
                    })?;
                    let frame = context.frame.ok_or_else(|| {
                        format!("line {}: batch has no frame context", number + 1)
                    })?;
                    let mut args = serde_json::Map::new();
                    args.insert("doc_id".into(), json!(doc));
                    args.insert("layer".into(), json!(layer));
                    args.insert("frame".into(), json!(frame));
                    args.insert(
                        "ops".into(),
                        Value::Array(
                            batch
                                .iter()
                                .enumerate()
                                .map(|(op_index, op)| {
                                    expand_batch_op(op).map_err(|error| {
                                        format!(
                                            "line {}, batch op {}: {error}",
                                            number + 1,
                                            op_index + 1
                                        )
                                    })
                                })
                                .collect::<Result<Vec<_>, _>>()?,
                        ),
                    );
                    if let Some(frames) = &compact.frames {
                        args.insert("frames".into(), json!(frames));
                    }
                    steps.push(Step {
                        tool: "doc_batch".into(),
                        args: Value::Object(args),
                        note: compact.note.clone(),
                    });
                }
                (Some(_), Some(_)) => {
                    return Err(format!(
                        "line {}: choose exactly one of `call` or `batch`",
                        number + 1
                    ));
                }
                (None, None) => {
                    return Err(format!(
                        "line {}: expected `call`, `batch`, or `use`",
                        number + 1
                    ));
                }
            }
        }

        Ok(Recipe {
            name: header.name,
            description: header.description,
            steps,
        })
    }
}

fn nonempty_lines(src: &str) -> Vec<(usize, &str)> {
    src.lines()
        .enumerate()
        .map(|(number, line)| (number, line.trim()))
        .filter(|(_, line)| !line.is_empty())
        .collect()
}

fn warn_torn_final(number: usize) {
    eprintln!(
        "recipe: dropped a partial final line (line {}) — the last recorded \
         step is missing from this replay",
        number + 1
    );
}

fn first_context(steps: &[Step]) -> Context {
    let mut context = Context::default();
    for step in steps {
        let Some(args) = step.args.as_object() else {
            continue;
        };
        if context.doc.is_none() {
            context.doc = args
                .get("doc_id")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        if context.layer.is_none() {
            context.layer = args
                .get("layer")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok());
        }
        if context.frame.is_none() {
            context.frame = args
                .get("frame")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok());
        }
        if context.doc.is_some() && context.layer.is_some() && context.frame.is_some() {
            break;
        }
    }
    context
}

fn inject_context(
    args: &mut Value,
    at: &str,
    context: &Context,
    line_number: usize,
) -> Result<(), String> {
    if at.is_empty() {
        return Ok(());
    }
    let mut seen = String::new();
    for key in at.chars() {
        if !matches!(key, 'd' | 'l' | 'f') {
            return Err(format!(
                "line {}: `at` accepts only d, l, and f",
                line_number + 1
            ));
        }
        if seen.contains(key) {
            return Err(format!("line {}: `at` repeats '{key}'", line_number + 1));
        }
        seen.push(key);
    }
    let object = args.as_object_mut().ok_or_else(|| {
        format!(
            "line {}: a call using `at` must have object args",
            line_number + 1
        )
    })?;
    for key in at.chars() {
        let (name, value) = match key {
            'd' => (
                "doc_id",
                context
                    .doc
                    .as_ref()
                    .map(|value| json!(value))
                    .ok_or_else(|| format!("line {}: `at` needs doc context", line_number + 1))?,
            ),
            'l' => (
                "layer",
                context
                    .layer
                    .map(|value| json!(value))
                    .ok_or_else(|| format!("line {}: `at` needs layer context", line_number + 1))?,
            ),
            'f' => (
                "frame",
                context
                    .frame
                    .map(|value| json!(value))
                    .ok_or_else(|| format!("line {}: `at` needs frame context", line_number + 1))?,
            ),
            _ => unreachable!("mask was validated"),
        };
        if object.insert(name.into(), value).is_some() {
            return Err(format!(
                "line {}: `{name}` appears in both args and `at`",
                line_number + 1
            ));
        }
    }
    Ok(())
}

/// Pull the two non-context fields out of a canonical `doc_batch` argument
/// object. Any extra/malformed field keeps the whole step in generic-call form.
fn take_batch(args: &Value) -> Option<(&Vec<Value>, Option<Vec<usize>>)> {
    let object = args.as_object()?;
    if object.len() > 2
        || object
            .keys()
            .any(|key| !matches!(key.as_str(), "ops" | "frames"))
    {
        return None;
    }
    let ops = object.get("ops")?.as_array()?;
    if !ops.iter().all(Value::is_object) {
        return None;
    }
    let frames = match object.get("frames") {
        None => None,
        Some(Value::Array(values)) => Some(
            values
                .iter()
                .map(|value| value.as_u64().and_then(|value| usize::try_from(value).ok()))
                .collect::<Option<Vec<_>>>()?,
        ),
        Some(_) => return None,
    };
    Some((ops, frames))
}

struct TupleSpec {
    op: &'static str,
    fields: &'static [&'static str],
}

/// Tuple field order is part of compact JSONL v2's on-disk contract.
const TUPLE_SPECS: &[TupleSpec] = &[
    TupleSpec {
        op: "line",
        fields: &["x0", "y0", "x1", "y1", "color", "size"],
    },
    TupleSpec {
        op: "line",
        fields: &["x0", "y0", "x1", "y1", "color"],
    },
    TupleSpec {
        op: "rect",
        fields: &["x0", "y0", "x1", "y1", "color", "fill"],
    },
    TupleSpec {
        op: "rect",
        fields: &["x0", "y0", "x1", "y1", "color"],
    },
    TupleSpec {
        op: "ellipse",
        fields: &["cx", "cy", "rx", "ry", "color", "fill"],
    },
    TupleSpec {
        op: "polyline",
        fields: &["points", "color", "size"],
    },
    TupleSpec {
        op: "pencil",
        fields: &["points", "color"],
    },
    TupleSpec {
        op: "clear_cel",
        fields: &[],
    },
    TupleSpec {
        op: "fill_cel",
        fields: &["color"],
    },
    TupleSpec {
        op: "glow",
        fields: &["radius", "intensity", "color", "mode"],
    },
];

fn compact_batch_op(value: &Value) -> Value {
    let Some(object) = value.as_object() else {
        return value.clone();
    };
    let Some(op) = object.get("op").and_then(Value::as_str) else {
        return value.clone();
    };
    for spec in TUPLE_SPECS.iter().filter(|spec| spec.op == op) {
        if object.len() != spec.fields.len() + 1
            || !spec.fields.iter().all(|field| object.contains_key(*field))
        {
            continue;
        }
        let mut tuple = vec![json!(op)];
        let mut supported = true;
        for field in spec.fields {
            match compact_tuple_field(field, &object[*field]) {
                Some(value) => tuple.push(value),
                None => {
                    supported = false;
                    break;
                }
            }
        }
        if supported {
            return Value::Array(tuple);
        }
    }
    value.clone()
}

fn compact_tuple_field(field: &str, value: &Value) -> Option<Value> {
    match field {
        "color" => color_to_hex(value).map(Value::String),
        "points" => flatten_points(value).map(Value::Array),
        "fill" => value.as_bool().map(|value| json!(u8::from(value))),
        _ => Some(value.clone()),
    }
}

fn expand_batch_op(value: &Value) -> Result<Value, String> {
    if value.is_object() {
        return Ok(value.clone());
    }
    let tuple = value
        .as_array()
        .ok_or("batch operation must be an object or tuple array")?;
    let op = tuple
        .first()
        .and_then(Value::as_str)
        .ok_or("tuple must start with an operation name")?;
    let spec = TUPLE_SPECS
        .iter()
        .find(|spec| spec.op == op && tuple.len() == spec.fields.len() + 1)
        .ok_or_else(|| {
            let lengths = TUPLE_SPECS
                .iter()
                .filter(|spec| spec.op == op)
                .map(|spec| spec.fields.len().to_string())
                .collect::<Vec<_>>();
            if lengths.is_empty() {
                format!("unknown tuple operation '{op}'; use an object for unsupported shapes")
            } else {
                format!(
                    "tuple '{op}' has {} value(s); expected {}",
                    tuple.len().saturating_sub(1),
                    lengths.join(" or ")
                )
            }
        })?;
    let mut object = serde_json::Map::new();
    object.insert("op".into(), json!(op));
    for (field, value) in spec.fields.iter().zip(&tuple[1..]) {
        object.insert((*field).into(), expand_tuple_field(field, value)?);
    }
    Ok(Value::Object(object))
}

fn expand_tuple_field(field: &str, value: &Value) -> Result<Value, String> {
    match field {
        "color" => hex_to_color(value),
        "points" => expand_points(value),
        "fill" => match value {
            Value::Bool(_) => Ok(value.clone()),
            Value::Number(number) if number.as_u64() == Some(0) => Ok(json!(false)),
            Value::Number(number) if number.as_u64() == Some(1) => Ok(json!(true)),
            _ => Err("tuple fill must be 0, 1, or a boolean".into()),
        },
        _ => Ok(value.clone()),
    }
}

fn color_to_hex(value: &Value) -> Option<String> {
    let channels = value.as_array()?;
    if !matches!(channels.len(), 3 | 4) {
        return None;
    }
    let channels = channels
        .iter()
        .map(|channel| channel.as_u64().and_then(|value| u8::try_from(value).ok()))
        .collect::<Option<Vec<_>>>()?;
    let mut output = String::from("#");
    for channel in channels {
        use std::fmt::Write as _;
        write!(&mut output, "{channel:02x}").ok()?;
    }
    Some(output)
}

fn hex_to_color(value: &Value) -> Result<Value, String> {
    if value.is_array() {
        return Ok(value.clone());
    }
    let source = value
        .as_str()
        .and_then(|value| value.strip_prefix('#'))
        .ok_or("tuple color must be #rrggbb, #rrggbbaa, or an array")?;
    if !matches!(source.len(), 6 | 8) || !source.is_ascii() {
        return Err("tuple color must be #rrggbb or #rrggbbaa".into());
    }
    let mut channels = Vec::with_capacity(source.len() / 2);
    for index in (0..source.len()).step_by(2) {
        channels.push(
            u8::from_str_radix(&source[index..index + 2], 16)
                .map_err(|_| "tuple color contains a non-hex channel")?,
        );
    }
    Ok(json!(channels))
}

fn flatten_points(value: &Value) -> Option<Vec<Value>> {
    let points = value.as_array()?;
    let mut flat = Vec::with_capacity(points.len() * 2);
    for point in points {
        let pair = point.as_array()?;
        if pair.len() != 2 || !pair.iter().all(Value::is_number) {
            return None;
        }
        flat.extend(pair.iter().cloned());
    }
    Some(flat)
}

fn expand_points(value: &Value) -> Result<Value, String> {
    let flat = value
        .as_array()
        .ok_or("tuple points must be a flat array")?;
    if flat.len() % 2 != 0 || !flat.iter().all(Value::is_number) {
        return Err("tuple points must contain x,y number pairs".into());
    }
    Ok(Value::Array(
        flat.chunks_exact(2)
            .map(|pair| Value::Array(pair.to_vec()))
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_authored_object_form() {
        let recipe = Recipe::parse(
            r#"{"name":"n","description":"d","steps":[{"tool":"doc_create","args":{"name":"x"}}]}"#,
        )
        .unwrap();
        assert_eq!(recipe.name, "n");
        assert_eq!(recipe.steps[0].tool, "doc_create");
    }

    #[test]
    fn an_authored_recipe_with_an_extra_v_field_is_not_misdetected_as_compact() {
        let recipe = Recipe::parse(
            r#"{"v":2,"name":"n","description":"d","steps":[{"tool":"doc_create","args":{}}]}"#,
        )
        .unwrap();
        assert_eq!(recipe.steps[0].tool, "doc_create");
        assert_eq!(
            Recipe::source_format(&recipe.to_pretty_json().unwrap()),
            "authored-json"
        );
    }

    #[test]
    fn reads_a_legacy_document_journal() {
        let recipe = Recipe::parse(
            "{\"tool\":\"doc_create\",\"args\":{\"name\":\"x\"}}\n\
             \n\
             {\"tool\":\"doc_draw\",\"args\":{\"op\":\"rect\"}}\n",
        )
        .unwrap();
        assert_eq!(recipe.steps.len(), 2);
        assert_eq!(recipe.steps[1].args["op"], "rect");
    }

    #[test]
    fn compact_round_trip_preserves_metadata_calls_args_and_notes() {
        let original = Recipe {
            name: "hero".into(),
            description: "a reversible test".into(),
            steps: vec![
                Step {
                    tool: "doc_create".into(),
                    args: json!({"name":"hero","width":16,"height":16}),
                    note: Some("create".into()),
                },
                Step {
                    tool: "doc_batch".into(),
                    args: json!({
                        "doc_id":"hero","layer":0,"frame":0,
                        "frames":[1, 2],
                        "ops":[
                            {"op":"line","x0":1,"y0":2,"x1":3,"y1":4,"color":[255,128,0,255],"size":1},
                            {"op":"pencil","points":[[1,2],[3,4]],"color":[1,2,3]},
                            {"op":"future","amount":7}
                        ]
                    }),
                    note: Some("paint".into()),
                },
                Step {
                    tool: "doc_info".into(),
                    args: json!({"doc_id":"hero"}),
                    note: None,
                },
            ],
        };
        let compact = original.to_compact_jsonl().unwrap();
        assert!(compact.contains(r##""#ff8000ff""##));
        assert!(compact.contains(r#"["line",1,2,3,4"#));
        assert!(compact.contains(r#"{"amount":7,"op":"future"}"#));
        assert_eq!(Recipe::parse(&compact).unwrap(), original);
        assert_eq!(
            Recipe::parse(&compact).unwrap().to_compact_jsonl().unwrap(),
            compact,
            "compact encoding is canonical"
        );
    }

    #[test]
    fn hand_authored_use_and_context_masks_expand_exactly() {
        let source = r##"{"v":2,"name":"n","description":"d"}
{"use":{"doc":"hero","layer":2,"frame":3}}
{"call":"doc_draw","at":"dlf","args":{"op":"clear_cel"}}
{"frame":4,"batch":[["fill_cel","#010203"]]}
"##;
        let recipe = Recipe::parse(source).unwrap();
        assert_eq!(
            recipe.steps[0].args,
            json!({"doc_id":"hero","layer":2,"frame":3,"op":"clear_cel"})
        );
        assert_eq!(
            recipe.steps[1].args,
            json!({
                "doc_id":"hero","layer":2,"frame":4,
                "ops":[{"op":"fill_cel","color":[1,2,3]}]
            })
        );
    }

    #[test]
    fn compact_errors_name_the_line_and_tuple() {
        let bad_mask = r#"{"v":2,"name":"n","description":"d","defaults":{"doc":"x"}}
{"call":"doc_info","at":"x"}
"#;
        assert!(Recipe::parse(bad_mask).unwrap_err().contains("line 2"));

        let bad_tuple = r#"{"v":2,"name":"n","description":"d","defaults":{"doc":"x","layer":0,"frame":0}}
{"batch":[["line",1,2]]}
"#;
        let error = Recipe::parse(bad_tuple).unwrap_err();
        assert!(error.contains("batch op 1"), "got: {error}");
        assert!(error.contains("expected 6 or 5"), "got: {error}");
    }

    #[test]
    fn each_legacy_shape_reports_its_own_error() {
        let error = Recipe::parse("{ not json").unwrap_err();
        assert!(error.contains("invalid recipe JSON"), "got: {error}");
        let error = Recipe::parse("{\"tool\":\"doc_create\"}\n{\"tool\":\ntorn\n").unwrap_err();
        assert!(error.contains("line 2"), "got: {error}");
    }

    #[test]
    fn a_torn_final_line_is_tolerated_but_a_complete_bad_step_is_not() {
        let recipe =
            Recipe::parse("{\"tool\":\"doc_create\",\"args\":{}}\n{\"tool\":\"doc_dr").unwrap();
        assert_eq!(recipe.steps.len(), 1);
        assert!(Recipe::parse("{\"tool\":\"doc_create\",\"args\":{}}\n{}").is_err());

        let compact = "{\"v\":2,\"name\":\"n\",\"description\":\"d\"}\n\
                       {\"call\":\"doc_create\",\"args\":{}}\n\
                       {\"call\":\"doc_dr";
        assert_eq!(Recipe::parse(compact).unwrap().steps.len(), 1);
    }

    #[test]
    fn an_empty_recipe_is_an_error_in_every_shape() {
        assert!(Recipe::parse(r#"{"name":"n","description":"d","steps":[]}"#).is_err());
        assert!(Recipe::parse("\n\n").is_err());
        assert!(Recipe::parse("{\"v\":2,\"name\":\"n\",\"description\":\"d\"}\n").is_err());
    }

    #[test]
    fn the_large_example_is_lossless_and_at_least_half_the_size() {
        let source = include_str!("../../../docs/examples/kamehameha.json");
        let recipe = Recipe::parse(source).unwrap();
        let compact = recipe.to_compact_jsonl().unwrap();
        assert_eq!(Recipe::parse(&compact).unwrap(), recipe);
        assert!(
            compact.len() * 2 <= source.len(),
            "{} -> {} bytes ({:.1}% reduction)",
            source.len(),
            compact.len(),
            100.0 * (source.len() - compact.len()) as f64 / source.len() as f64
        );
        assert_eq!(recipe.batch_ops(), 320);
        assert_eq!(recipe.tuple_ops(), 320);
    }

    #[test]
    fn journal_record_is_self_contained_relative_to_its_header() {
        let (header, line) = compact_journal_record(
            "hero",
            "doc_draw",
            json!({"doc_id":"hero","layer":2,"frame":3,"op":"fill_cel","color":[1,2,3]}),
        )
        .unwrap();
        let recipe = Recipe::parse(&format!("{header}\n{line}\n")).unwrap();
        assert_eq!(recipe.steps.len(), 1);
        assert_eq!(recipe.steps[0].args["doc_id"], "hero");
        assert_eq!(recipe.steps[0].args["layer"], 2);
        assert_eq!(recipe.steps[0].args["frame"], 3);
    }
}
