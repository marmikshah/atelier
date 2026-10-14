//! The per-document recipe: a readable text journal of what happened to a
//! document, written so that a replay never depends on how a tool behaves.
//!
//! ```text
//! atelier 5 48080aae-72aa-4f03-94ec-70b69e2791b0 0.1.0
//! new name=potion-flask width=32 height=32
//! palette set colors=#1a1630,#f0b07a,#5ad8f8
//! layer rename index=0 name="glass back"
//! legend A=#1a1630 B=#5ad8f8
//! "glass back" @0 grid = 12,2 8x4
//!     .AAAAAA.
//!     ABBBBBBA
//!     ABBBBBBA
//!     .AAAAAA.
//! "glass back" @0 erase pencil = 12,2 13,2 .
//! "glass back" @1 shift dx=1 wrap=true
//! "glass back" @1 erase pencil ~
//! ```
//!
//! The first line names the format, the document, and the Atelier version
//! that created the file. Every other line is one of:
//!
//! - a **legend** line binding one-letter glyphs to colours for the pixel
//!   rows below it; bindings persist and a later line may rebind a glyph;
//! - a **call**: a tool without its `doc_` prefix, its `op` as a bare word,
//!   then `key=value` arguments; `doc_id` is implied by the header. These are
//!   the operations whose result is exact by definition: document, palette,
//!   layer, frame, tag, font and rescale calls, and the `shift`, `flip` and
//!   `rotate` effects on one cel, written after a scope;
//! - an **effect**: a scope `<layer> @<frame>`, the call that was made in
//!   words, then `=` and the pixels it left behind, either `x,y WxH` and that
//!   many rows below it, or `x0,y0 x1,y1 C` for a block of one colour. `~`
//!   in place of the `=` part marks a call that changed nothing.
//!
//! A layer is named by its name, JSON-quoted when it is not a plain word, or
//! `#N` by index when its name is not unique. In a pixel row `.` is a
//! transparent pixel and every other character is a legend glyph; a block
//! with more colours than there are glyphs is written in `hex` rows of
//! `#rrggbb` tokens instead. Argument values are written the plainest way
//! that reads back exactly: numbers, `true`, `false`, `null`, plain words and
//! `#` colours as is; a list of them joined by commas; a list of such lists
//! joined by spaces; a map as `key:value` pairs; anything else as JSON.
//! Blank lines and lines starting with `#` are ignored.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::{Studio, ToolName};

/// The recipe format number written in the header line.
pub const JOURNAL_SCHEMA: u32 = 5;

pub const MAX_JOURNAL_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_JOURNAL_ENTRIES: usize = 100_000;

/// One RGBA pixel; alpha zero is transparent and written as `.`.
pub type Pixel = [u8; 4];

pub const TRANSPARENT: Pixel = [0, 0, 0, 0];

/// The glyphs a legend can bind, in the order they are handed out.
const GLYPHS: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// Pixel rows are indented this much.
const ROW_INDENT: &str = "    ";

/// Which layer a cel statement names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayerRef {
    /// The layer's name, unique among the document's layers at that point.
    Name(String),
    /// The layer's index, used when its name is shared.
    Index(usize),
}

/// The cel a statement applies to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    pub layer: LayerRef,
    pub frame: usize,
}

/// The pixels an effect left behind inside the rectangle it changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// Nothing changed: written `~`.
    None,
    /// One colour over an inclusive rectangle: written `x0,y0 x1,y1 C`.
    Solid {
        x0: u32,
        y0: u32,
        x1: u32,
        y1: u32,
        color: Pixel,
    },
    /// The block at `x,y`, one row per pixel row, every row the same width.
    Rows {
        x: u32,
        y: u32,
        rows: Vec<Vec<Pixel>>,
    },
}

/// One recorded statement, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Statement {
    /// A document-level call; `args` carries the full `doc_id`, which the
    /// file states once in its header.
    Call {
        tool: ToolName,
        args: Map<String, Value>,
    },
    /// An exact operation on one cel: `shift`, `flip` or `rotate` with the
    /// parameters it was given, less the layer and frame the scope names.
    CelCall {
        scope: Scope,
        op: String,
        args: Map<String, Value>,
    },
    /// What a call did to one cel, annotated with the call.
    Effect {
        scope: Scope,
        annotation: String,
        change: Change,
    },
}

impl Statement {
    pub fn call(tool: ToolName, args: Map<String, Value>) -> Self {
        Self::Call { tool, args }
    }

    /// The tool a statement was recorded for, where there is one.
    pub fn tool(&self) -> Option<ToolName> {
        match self {
            Self::Call { tool, .. } => Some(*tool),
            _ => None,
        }
    }
}

/// Tools whose calls are recorded as effects, never as calls.
pub fn is_pixel_tool(tool: ToolName) -> bool {
    matches!(
        tool,
        ToolName::DocDraw
            | ToolName::DocFx
            | ToolName::DocPaintGrid
            | ToolName::DocRegion
            | ToolName::DocDitherRamp
    )
}

/// The `doc_fx` operations recorded as cel calls rather than pixels: they
/// move pixels without computing any.
pub fn is_exact_fx(op: &str) -> bool {
    matches!(op, "shift" | "flip" | "rotate")
}

/// `#rrggbb`, or `#rrggbbaa` when the pixel is translucent.
pub fn hex_colour(pixel: Pixel) -> String {
    if pixel[3] == 255 {
        format!("#{:02x}{:02x}{:02x}", pixel[0], pixel[1], pixel[2])
    } else {
        format!(
            "#{:02x}{:02x}{:02x}{:02x}",
            pixel[0], pixel[1], pixel[2], pixel[3]
        )
    }
}

/// Read `#rrggbb` or `#rrggbbaa`.
pub fn parse_colour(text: &str) -> Option<Pixel> {
    let digits = text.strip_prefix('#')?;
    if !matches!(digits.len(), 6 | 8) || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).ok();
    Some([
        channel(0)?,
        channel(2)?,
        channel(4)?,
        if digits.len() == 8 { channel(6)? } else { 255 },
    ])
}

/// The running glyph table: which letter stands for which colour in the pixel
/// rows written so far. The writer binds glyphs as blocks need them and the
/// reader follows the same `legend` lines.
#[derive(Clone, Debug, Default)]
pub struct Legend {
    /// Bound glyphs in binding order, oldest first.
    bound: Vec<(char, Pixel)>,
    by_colour: HashMap<Pixel, char>,
}

impl Legend {
    /// How many distinct colours one block of rows can carry.
    pub const CAPACITY: usize = 62;

    pub fn glyph(&self, colour: Pixel) -> Option<char> {
        self.by_colour.get(&colour).copied()
    }

    pub fn colour(&self, glyph: char) -> Option<Pixel> {
        self.bound
            .iter()
            .find(|(bound, _)| *bound == glyph)
            .map(|(_, colour)| *colour)
    }

    /// Bind `glyph` to `colour`, replacing any earlier binding of either.
    fn bind(&mut self, glyph: char, colour: Pixel) {
        if let Some(previous) = self.by_colour.remove(&colour) {
            self.bound.retain(|(bound, _)| *bound != previous);
        }
        if let Some(position) = self.bound.iter().position(|(bound, _)| *bound == glyph) {
            let (_, previous) = self.bound.remove(position);
            self.by_colour.remove(&previous);
        }
        self.bound.push((glyph, colour));
        self.by_colour.insert(colour, glyph);
    }

    /// Make every colour in `needed` bound, handing out free glyphs first and
    /// then rebinding the glyphs bound longest ago to colours the block does
    /// not use. Returns the `legend` line that records the new bindings, or
    /// `None` when every colour was bound already. `needed` must hold at most
    /// [`Self::CAPACITY`] colours.
    pub fn provide(&mut self, needed: &[Pixel]) -> Result<Option<String>, String> {
        let mut distinct: Vec<Pixel> = Vec::new();
        for colour in needed {
            if colour[3] != 0 && !distinct.contains(colour) {
                distinct.push(*colour);
            }
        }
        if distinct.len() > Self::CAPACITY {
            return Err(format!(
                "a block needs {} colours; the legend holds {}",
                distinct.len(),
                Self::CAPACITY
            ));
        }
        let mut added = Vec::new();
        let unbound: Vec<Pixel> = distinct
            .iter()
            .copied()
            .filter(|c| self.glyph(*c).is_none())
            .collect();
        for colour in &unbound {
            let glyph = match GLYPHS.chars().find(|g| self.colour(*g).is_none()) {
                Some(free) => free,
                None => {
                    // Every glyph is taken: reuse the oldest one this block
                    // does not need.
                    self.bound
                        .iter()
                        .find(|(_, bound)| !distinct.contains(bound))
                        .map(|(glyph, _)| *glyph)
                        .ok_or("the legend has no glyph to spare")?
                }
            };
            self.bind(glyph, *colour);
            added.push((glyph, *colour));
        }
        if added.is_empty() {
            return Ok(None);
        }
        let pairs: Vec<String> = added
            .iter()
            .map(|(glyph, colour)| format!("{glyph}={}", hex_colour(*colour)))
            .collect();
        Ok(Some(format!("legend {}\n", pairs.join(" "))))
    }

    /// Apply one `legend` line.
    fn read(&mut self, line: &str) -> Result<(), String> {
        let bindings = line
            .strip_prefix("legend")
            .ok_or("not a legend line")?
            .trim_start_matches(' ');
        if bindings.is_empty() {
            return Err("a legend line binds at least one glyph".into());
        }
        for pair in bindings.split(' ') {
            let (glyph, colour) = pair
                .split_once('=')
                .ok_or_else(|| format!("'{pair}' is not a glyph=colour binding"))?;
            let mut chars = glyph.chars();
            let glyph = match (chars.next(), chars.next()) {
                (Some(glyph), None) if GLYPHS.contains(glyph) => glyph,
                _ => return Err(format!("'{glyph}' is not a legend glyph")),
            };
            let colour = parse_colour(colour)
                .filter(|colour| colour[3] != 0)
                .ok_or_else(|| format!("'{colour}' is not an opaque or translucent colour"))?;
            self.bind(glyph, colour);
        }
        Ok(())
    }
}

/// The header line for a new recipe, newline-terminated.
pub fn encode_header(id: &str) -> String {
    format!(
        "atelier {JOURNAL_SCHEMA} {id} {}\n",
        env!("CARGO_PKG_VERSION")
    )
}

// -- argument values ----------------------------------------------------------

fn is_plain_key(key: &str) -> bool {
    let mut bytes = key.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// A string that reads unambiguously without quotes: a word or a `#` colour.
fn is_bare_word(text: &str) -> bool {
    let mut bytes = text.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || matches!(first, b'_' | b'#'))
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        && !matches!(text, "true" | "false" | "null")
}

/// A layer name that can stand at the start of a line unquoted. It must not
/// start like an index (`#`), a comment or a quote, and must not contain the
/// characters that end or shape a line.
fn is_bare_layer_name(text: &str) -> bool {
    let mut bytes = text.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

/// A map key that can sit before a `:` in a comma-joined map.
fn is_map_key(key: &str) -> bool {
    !key.is_empty()
        && !key.starts_with(['"', '[', '{'])
        && key
            .chars()
            .all(|c| !c.is_whitespace() && !c.is_control() && !matches!(c, ':' | ','))
}

/// A scalar written as is: a number, `true`, `false`, `null`, or a bare word.
fn atom(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => is_bare_word(text).then(|| text.clone()),
        Value::Array(_) | Value::Object(_) => None,
        scalar => serde_json::to_string(scalar).ok(),
    }
}

/// A list of two or more atoms as `a,b`: the inner form of a nested list.
fn atom_list(value: &Value) -> Option<String> {
    let items = value.as_array().filter(|items| items.len() >= 2)?;
    let atoms = items.iter().map(atom).collect::<Option<Vec<_>>>()?;
    Some(atoms.join(","))
}

/// A point list token: `x,y` with no map separator, so it can follow another
/// token on the line without being taken for an argument.
fn is_pair_token(token: &str) -> bool {
    token.contains(',') && !token.contains([':', '='])
}

/// The plainest spelling of `value` that [`parse_value`] reads back exactly.
fn encode_value(value: &Value) -> Result<String, String> {
    let plain = match value {
        // Two or more lists of atoms, space-separated: `7,33 17,33 18,33`.
        Value::Array(items) if items.len() >= 2 && items.iter().all(|item| item.is_array()) => {
            items
                .iter()
                .map(atom_list)
                .collect::<Option<Vec<_>>>()
                .filter(|lists| lists.iter().all(|list| is_pair_token(list)))
                .map(|lists| lists.join(" "))
        }
        Value::Array(_) => atom_list(value),
        Value::Object(map) if !map.is_empty() => map
            .iter()
            .map(|(key, value)| {
                atom(value)
                    .filter(|_| is_map_key(key))
                    .map(|value| format!("{key}:{value}"))
            })
            .collect::<Option<Vec<_>>>()
            .map(|pairs| pairs.join(",")),
        scalar => atom(scalar),
    };
    match plain {
        Some(plain) => Ok(plain),
        None => serde_json::to_string(value).map_err(|error| error.to_string()),
    }
}

fn parse_atom(word: &str) -> Result<Value, String> {
    match word {
        "" => Err("a value is missing".into()),
        "true" => Ok(Value::Bool(true)),
        "false" => Ok(Value::Bool(false)),
        "null" => Ok(Value::Null),
        word if word.starts_with(|c: char| c.is_ascii_digit() || c == '-') => {
            match serde_json::from_str(word) {
                Ok(number @ Value::Number(_)) => Ok(number),
                _ => Err(format!("'{word}' is not a number")),
            }
        }
        word if is_bare_word(word) => Ok(Value::String(word.to_string())),
        word => Err(format!("'{word}' must be quoted")),
    }
}

/// Read a value written without JSON brackets or quotes.
fn parse_value(word: &str) -> Result<Value, String> {
    if word.contains(':') {
        let mut map = Map::new();
        for pair in word.split(',') {
            let (key, value) = pair
                .split_once(':')
                .filter(|(key, _)| is_map_key(key))
                .ok_or_else(|| format!("'{pair}' is not a key:value pair"))?;
            if map.insert(key.to_string(), parse_atom(value)?).is_some() {
                return Err(format!("map key '{key}' is given twice"));
            }
        }
        Ok(Value::Object(map))
    } else if word.contains(',') {
        word.split(',').map(parse_atom).collect()
    } else {
        parse_atom(word)
    }
}

/// `key=value` arguments after a call's name, `op` lifted to a bare word.
fn encode_args(args: &Map<String, Value>) -> Result<String, String> {
    if !args.keys().all(|key| is_plain_key(key)) {
        // An argument name the line grammar cannot carry: keep the call exact.
        return serde_json::to_string(args)
            .map(|json| format!(" {json}"))
            .map_err(|error| error.to_string());
    }
    let mut out = String::new();
    let op = args
        .get("op")
        .and_then(Value::as_str)
        .filter(|op| is_bare_word(op));
    if let Some(op) = op {
        out.push(' ');
        out.push_str(op);
    }
    let mut keys: Vec<&String> = args
        .keys()
        .filter(|key| op.is_none() || *key != "op")
        .collect();
    keys.sort_by(|a, b| arg_rank(a).cmp(&arg_rank(b)).then_with(|| a.cmp(b)));
    for key in keys {
        out.push(' ');
        out.push_str(key);
        out.push('=');
        out.push_str(&encode_value(&args[key])?);
    }
    Ok(out)
}

/// Arguments written first, in this order, so every line leads with where it
/// applies. The rest follow alphabetically.
fn arg_rank(key: &str) -> (usize, usize) {
    const LEADING: [&str; 10] = [
        "op", "layer", "frame", "frame_to", "index", "name", "x", "y", "width", "height",
    ];
    match LEADING.iter().position(|leading| *leading == key) {
        Some(position) => (0, position),
        None => (1, 0),
    }
}

/// Parse `[op] key=value...` or a JSON object.
fn parse_args(mut rest: &str, allow_op: bool) -> Result<Map<String, Value>, String> {
    rest = rest.trim_start_matches(' ');
    if rest.starts_with('{') {
        return serde_json::from_str(rest).map_err(|error| error.to_string());
    }
    let mut args = Map::new();
    let mut first = true;
    while !rest.is_empty() {
        let word = rest.split(' ').next().unwrap_or("");
        if !word.contains('=') {
            if first && allow_op && is_bare_word(word) {
                args.insert("op".into(), Value::String(word.to_string()));
                rest = rest[word.len()..].trim_start_matches(' ');
                first = false;
                continue;
            }
            return Err(format!("expected key=value, found '{rest}'"));
        }
        first = false;
        let Some((key, after)) = rest.split_once('=') else {
            unreachable!("the first token holds an '='");
        };
        if !is_plain_key(key) {
            return Err(format!("'{key}' is not an argument name"));
        }
        let (value, used) = if after.starts_with(['"', '[', '{']) {
            let mut stream = serde_json::Deserializer::from_str(after).into_iter::<Value>();
            let value = stream
                .next()
                .ok_or_else(|| format!("'{key}=' has no value"))?
                .map_err(|error| format!("{key}: {error}"))?;
            (value, stream.byte_offset())
        } else {
            let word = after.split(' ').next().unwrap_or("");
            let mut lists = vec![parse_value(word).map_err(|error| format!("{key}: {error}"))?];
            let mut used = word.len();
            // A point list continues over the following `x,y` tokens.
            if is_pair_token(word) {
                for token in after[used..].split(' ').skip(1) {
                    if !is_pair_token(token) {
                        break;
                    }
                    lists.push(parse_value(token).map_err(|error| format!("{key}: {error}"))?);
                    used += 1 + token.len();
                }
            }
            let value = match lists.len() {
                1 => lists.remove(0),
                _ => Value::Array(lists),
            };
            (value, used)
        };
        let remainder = &after[used..];
        if !remainder.is_empty() && !remainder.starts_with(' ') {
            return Err(format!("{key}: unexpected text after the value"));
        }
        if args.insert(key.to_string(), value).is_some() {
            return Err(format!("argument '{key}' is given twice"));
        }
        rest = remainder.trim_start_matches(' ');
    }
    Ok(args)
}

// -- scopes -------------------------------------------------------------------

fn encode_scope(scope: &Scope) -> String {
    let layer = match &scope.layer {
        LayerRef::Index(index) => format!("#{index}"),
        LayerRef::Name(name) if is_bare_layer_name(name) => name.clone(),
        LayerRef::Name(name) => serde_json::to_string(name).unwrap_or_default(),
    };
    format!("{layer} @{}", scope.frame)
}

/// Read a scope at the start of a line, returning it with what follows, or
/// `None` when the line does not start with one.
fn parse_scope(line: &str) -> Result<Option<(Scope, &str)>, String> {
    let (layer, rest) = if line.starts_with('"') {
        let mut stream = serde_json::Deserializer::from_str(line).into_iter::<Value>();
        let name = match stream.next() {
            Some(Ok(Value::String(name))) => name,
            _ => return Ok(None),
        };
        (LayerRef::Name(name), &line[stream.byte_offset()..])
    } else {
        let word = line.split(' ').next().unwrap_or("");
        let layer = if let Some(index) = word.strip_prefix('#') {
            match index.parse::<usize>() {
                Ok(index) => LayerRef::Index(index),
                Err(_) => return Ok(None),
            }
        } else if is_bare_layer_name(word) {
            LayerRef::Name(word.to_string())
        } else {
            return Ok(None);
        };
        (layer, &line[word.len()..])
    };
    let Some(rest) = rest.strip_prefix(" @") else {
        return Ok(None);
    };
    let (frame, rest) = rest.split_once(' ').unwrap_or((rest, ""));
    let frame = frame
        .parse::<usize>()
        .map_err(|_| format!("'@{frame}' is not a frame number"))?;
    Ok(Some((Scope { layer, frame }, rest.trim_start_matches(' '))))
}

// -- encoding -----------------------------------------------------------------

/// Write `statements` after the recipe's existing text, binding legend glyphs
/// from `legend` as the pixel rows need them.
pub fn encode_statements(
    statements: &[Statement],
    legend: &mut Legend,
    id: &str,
) -> Result<String, String> {
    let mut out = String::new();
    for statement in statements {
        match statement {
            Statement::Call { tool, args } => {
                let mut args = args.clone();
                match args.get("doc_id") {
                    Some(Value::String(target)) if target == id => {
                        args.remove("doc_id");
                    }
                    None => {
                        args.insert("doc_id".into(), Value::Null);
                    }
                    Some(other) => {
                        return Err(format!(
                            "refusing to journal a call for another document: doc_id {other}"
                        ));
                    }
                }
                if is_pixel_tool(*tool) {
                    return Err(format!("'{tool}' is recorded as an effect, not a call"));
                }
                let name = tool
                    .as_str()
                    .strip_prefix("doc_")
                    .ok_or_else(|| format!("'{tool}' is not a recipe step"))?;
                out.push_str(name);
                out.push_str(&encode_args(&args)?);
                out.push('\n');
            }
            Statement::CelCall { scope, op, args } => {
                if !is_exact_fx(op) {
                    return Err(format!("'{op}' is recorded as an effect, not a cel call"));
                }
                let mut args = args.clone();
                for key in ["doc_id", "layer", "frame", "frame_to", "op"] {
                    args.remove(key);
                }
                out.push_str(&encode_scope(scope));
                out.push(' ');
                out.push_str(op);
                out.push_str(&encode_args(&args)?);
                out.push('\n');
            }
            Statement::Effect {
                scope,
                annotation,
                change,
            } => {
                let annotation = clean_annotation(annotation);
                let mut line = encode_scope(scope);
                if !annotation.is_empty() {
                    line.push(' ');
                    line.push_str(&annotation);
                }
                match change {
                    Change::None => {
                        out.push_str(&line);
                        out.push_str(" ~\n");
                    }
                    Change::Solid {
                        x0,
                        y0,
                        x1,
                        y1,
                        color,
                    } => {
                        if x1 < x0 || y1 < y0 {
                            return Err("a solid block needs ordered corners".into());
                        }
                        let colour = if color[3] == 0 {
                            ".".to_string()
                        } else {
                            hex_colour(*color)
                        };
                        out.push_str(&line);
                        out.push_str(&format!(" = {x0},{y0} {x1},{y1} {colour}\n"));
                    }
                    Change::Rows { x, y, rows } => {
                        let width = rows.first().map(Vec::len).unwrap_or(0);
                        if width == 0 || rows.iter().any(|row| row.len() != width) {
                            return Err("a pixel block needs rows of one non-zero width".into());
                        }
                        let colours: Vec<Pixel> = rows.iter().flatten().copied().collect();
                        let distinct = colours
                            .iter()
                            .filter(|c| c[3] != 0)
                            .collect::<std::collections::HashSet<_>>()
                            .len();
                        if distinct > Legend::CAPACITY {
                            out.push_str(&line);
                            out.push_str(&format!(" = {x},{y} {width}x{} hex\n", rows.len()));
                            for row in rows {
                                out.push_str(ROW_INDENT);
                                let tokens: Vec<String> = row
                                    .iter()
                                    .map(|pixel| {
                                        if pixel[3] == 0 {
                                            ".".to_string()
                                        } else {
                                            hex_colour(*pixel)
                                        }
                                    })
                                    .collect();
                                out.push_str(&tokens.join(" "));
                                out.push('\n');
                            }
                        } else {
                            if let Some(binding) = legend.provide(&colours)? {
                                out.push_str(&binding);
                            }
                            out.push_str(&line);
                            out.push_str(&format!(" = {x},{y} {width}x{}\n", rows.len()));
                            for row in rows {
                                out.push_str(ROW_INDENT);
                                for pixel in row {
                                    out.push(if pixel[3] == 0 {
                                        '.'
                                    } else {
                                        legend.glyph(*pixel).ok_or("legend lost a colour")?
                                    });
                                }
                                out.push('\n');
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(out)
}

/// The annotation is for people and the reader skips it, so it may hold any
/// words that cannot be taken for the rest of the line: no `=`, no `~`, no
/// line breaks, single spaces.
fn clean_annotation(annotation: &str) -> String {
    annotation
        .split_whitespace()
        .filter(|word| !word.contains(['=', '~']))
        .collect::<Vec<_>>()
        .join(" ")
}

// -- parsing ------------------------------------------------------------------

/// Completed statements and whether a crash left an incomplete final append.
#[derive(Debug)]
pub struct ParsedJournal {
    pub statements: Vec<Statement>,
    pub torn_tail: bool,
    /// Whether the file already starts with its header line.
    pub headed: bool,
    /// The glyph bindings in force at the end of the file.
    pub legend: Legend,
}

/// Validate the per-document journal contract. An absent/empty journal means
/// "no recipe"; a non-empty one is a complete, self-identifying rebuild.
pub fn validate_journal(statements: &[Statement]) -> Result<(), String> {
    let Some(first) = statements.first() else {
        return Ok(());
    };
    let Statement::Call {
        tool: ToolName::DocNew,
        args,
    } = first
    else {
        return Err("journal must start with doc_new".into());
    };
    let recorded_id = args
        .get("doc_id")
        .and_then(Value::as_str)
        .filter(|id| Studio::valid_id(id))
        .ok_or("journal doc_new requires a valid args.doc_id stamp")?;
    for (index, statement) in statements.iter().enumerate().skip(1) {
        let Statement::Call { tool, args } = statement else {
            continue;
        };
        if *tool == ToolName::DocNew {
            return Err("journal may contain exactly one doc_new".into());
        }
        if !tool.is_recipe_step() {
            return Err(format!(
                "journal line {} uses non-recipe tool '{tool}'",
                index + 1
            ));
        }
        let mut targets = Vec::new();
        for key in ["doc_id", "set_doc"] {
            if let Some(value) = args.get(key) {
                targets.push(value.as_str().ok_or_else(|| {
                    format!("journal line {} ({tool}) has a non-string {key}", index + 1)
                })?);
            }
        }
        if targets.is_empty() {
            return Err(format!(
                "journal line {} ({tool}) has no document target",
                index + 1
            ));
        }
        if targets.iter().any(|target| *target != recorded_id) {
            return Err(format!(
                "journal line {} ({tool}) targets a document other than '{recorded_id}'",
                index + 1
            ));
        }
    }
    Ok(())
}

/// Parse a recipe for store reads and external replay. An empty recipe has no
/// statements. Only an incomplete final record is tolerated; malformed
/// complete records and other formats are rejected.
pub fn parse_journal(source: &str) -> Result<ParsedJournal, String> {
    parse_with_limits(source, MAX_JOURNAL_BYTES, MAX_JOURNAL_ENTRIES)
}

fn parse_header(line: &str) -> Result<String, String> {
    let unsupported = |found: &dyn std::fmt::Display| {
        format!("unsupported journal format {found} (this build supports {JOURNAL_SCHEMA})")
    };
    // A line of an earlier JSON format is a version mismatch, not corruption.
    if let Ok(Value::Object(object)) = serde_json::from_str::<Value>(line)
        && let Some(schema) = object.get("schema")
    {
        return Err(unsupported(schema));
    }
    let mut words = line.split(' ');
    if words.next() != Some("atelier") {
        return Err("a recipe starts with an 'atelier <format> <doc-id> <version>' line".into());
    }
    let format = words.next().ok_or("the header has no format number")?;
    if format.parse::<u32>().ok() != Some(JOURNAL_SCHEMA) {
        if format.parse::<u32>().is_ok() {
            return Err(unsupported(&format));
        }
        return Err(format!("'{format}' is not a format number"));
    }
    let id = words.next().ok_or("the header has no document id")?;
    if !Studio::valid_id(id) {
        return Err("the header requires a valid document id".into());
    }
    match (words.next(), words.next()) {
        (Some(version), None) if !version.is_empty() => Ok(id.to_string()),
        _ => Err("the header ends with one Atelier version".into()),
    }
}

/// The shape of an effect's `=` part: a block of rows to read next, or a
/// complete change.
enum EffectTail {
    Rows {
        x: u32,
        y: u32,
        width: usize,
        height: usize,
        hex: bool,
    },
    Done(Change),
}

fn parse_point(token: &str) -> Result<(u32, u32), String> {
    let (x, y) = token
        .split_once(',')
        .ok_or_else(|| format!("'{token}' is not an x,y point"))?;
    match (x.parse::<u32>(), y.parse::<u32>()) {
        (Ok(x), Ok(y)) => Ok((x, y)),
        _ => Err(format!("'{token}' is not an x,y point")),
    }
}

fn parse_effect_tail(tail: &str) -> Result<EffectTail, String> {
    let tokens: Vec<&str> = tail.split(' ').filter(|t| !t.is_empty()).collect();
    match tokens.as_slice() {
        [origin, size] | [origin, size, "hex"] if size.contains('x') => {
            let (x, y) = parse_point(origin)?;
            let (width, height) = size
                .split_once('x')
                .and_then(|(w, h)| Some((w.parse::<usize>().ok()?, h.parse::<usize>().ok()?)))
                .filter(|(w, h)| *w > 0 && *h > 0)
                .ok_or_else(|| format!("'{size}' is not a WxH block size"))?;
            Ok(EffectTail::Rows {
                x,
                y,
                width,
                height,
                hex: tokens.len() == 3,
            })
        }
        [from, to, colour] => {
            let (x0, y0) = parse_point(from)?;
            let (x1, y1) = parse_point(to)?;
            if x1 < x0 || y1 < y0 {
                return Err(format!("'{from} {to}' is not an ordered rectangle"));
            }
            let color = if *colour == "." {
                TRANSPARENT
            } else {
                parse_colour(colour)
                    .filter(|c| c[3] != 0)
                    .ok_or_else(|| format!("'{colour}' is not a colour"))?
            };
            Ok(EffectTail::Done(Change::Solid {
                x0,
                y0,
                x1,
                y1,
                color,
            }))
        }
        _ => Err(format!(
            "'= {tail}' must be 'x,y WxH', 'x,y WxH hex' or 'x0,y0 x1,y1 colour'"
        )),
    }
}

fn parse_row(row: &str, width: usize, hex: bool, legend: &Legend) -> Result<Vec<Pixel>, String> {
    let row = row
        .strip_prefix(ROW_INDENT)
        .ok_or("a pixel row is indented by four spaces")?;
    let pixels: Result<Vec<Pixel>, String> = if hex {
        row.split(' ')
            .map(|token| {
                if token == "." {
                    Ok(TRANSPARENT)
                } else {
                    parse_colour(token)
                        .filter(|c| c[3] != 0)
                        .ok_or_else(|| format!("'{token}' is not a colour"))
                }
            })
            .collect()
    } else {
        row.chars()
            .map(|glyph| {
                if glyph == '.' {
                    Ok(TRANSPARENT)
                } else {
                    legend
                        .colour(glyph)
                        .ok_or_else(|| format!("glyph '{glyph}' is not in the legend"))
                }
            })
            .collect()
    };
    let pixels = pixels?;
    if pixels.len() != width {
        return Err(format!(
            "a pixel row has {} pixels; the block is {width} wide",
            pixels.len()
        ));
    }
    Ok(pixels)
}

/// A line after its scope: an effect or a cel call.
fn parse_cel_statement(
    scope: Scope,
    rest: &str,
) -> Result<(Statement, Option<EffectTail>), String> {
    if let Some(annotation) = rest.strip_suffix(" ~").or((rest == "~").then_some("")) {
        return Ok((
            Statement::Effect {
                scope,
                annotation: annotation.to_string(),
                change: Change::None,
            },
            None,
        ));
    }
    if let Some((annotation, tail)) = rest.rsplit_once(" = ") {
        let tail = parse_effect_tail(tail)?;
        let statement = Statement::Effect {
            scope,
            annotation: annotation.to_string(),
            change: Change::None,
        };
        return Ok((statement, Some(tail)));
    }
    if let Some(tail) = rest.strip_prefix("= ") {
        let tail = parse_effect_tail(tail)?;
        let statement = Statement::Effect {
            scope,
            annotation: String::new(),
            change: Change::None,
        };
        return Ok((statement, Some(tail)));
    }
    let (op, args) = rest.split_once(' ').unwrap_or((rest, ""));
    if !is_exact_fx(op) {
        return Err(format!(
            "'{op}' is not a cel call (shift, flip or rotate) or an effect with '=' or '~'"
        ));
    }
    let args = parse_args(args, false)?;
    for key in ["doc_id", "layer", "frame", "frame_to", "op"] {
        if args.contains_key(key) {
            return Err(format!("a cel call must not carry '{key}'"));
        }
    }
    Ok((
        Statement::CelCall {
            scope,
            op: op.to_string(),
            args,
        },
        None,
    ))
}

/// Restore the document id the header implies.
fn expand_args(mut args: Map<String, Value>, id: &str) -> Result<Map<String, Value>, String> {
    match args.get("doc_id") {
        None => {
            args.insert("doc_id".into(), Value::String(id.to_string()));
        }
        Some(Value::Null) => {
            args.remove("doc_id");
        }
        Some(_) => return Err("a recipe line must not repeat doc_id".into()),
    }
    Ok(args)
}

fn parse_call(line: &str, id: &str) -> Result<Statement, String> {
    let (tool, rest) = line.split_once(' ').unwrap_or((line, ""));
    let tool: ToolName = serde_json::from_value(Value::String(format!("doc_{tool}")))
        .map_err(|_| format!("unknown tool '{tool}'"))?;
    if is_pixel_tool(tool) {
        return Err(format!(
            "'{}' is recorded as an effect on a cel, not as a call",
            tool.as_str().trim_start_matches("doc_")
        ));
    }
    let args = parse_args(rest, true)?;
    let args = expand_args(args, id)?;
    Ok(Statement::Call { tool, args })
}

fn parse_with_limits(
    source: &str,
    max_bytes: u64,
    max_entries: usize,
) -> Result<ParsedJournal, String> {
    if source.len() as u64 > max_bytes {
        return Err(format!(
            "journal is {} bytes, over the {max_bytes}-byte replay limit",
            source.len()
        ));
    }
    let mut lines: Vec<&str> = source
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    let is_blank = |line: &str| line.trim().is_empty();
    // Every complete line ends in a newline, so an unterminated final line is
    // what a crash left behind even when it happens to read as a shorter
    // statement.
    let mut torn_tail = lines.pop().is_some_and(|last| !last.is_empty());
    let located = |index: usize, error: String| {
        format!(
            "journal line {}: {error} — expected a header, then one statement per line",
            index + 1
        )
    };

    let mut statements = Vec::new();
    let mut legend = Legend::default();
    let mut header: Option<String> = None;
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        if is_blank(line) {
            index += 1;
            continue;
        }
        // A `#` line is a note, unless it is a `#N @F` scope naming a layer
        // by index.
        let scope = parse_scope(line).map_err(|error| located(index, error))?;
        if scope.is_none() && line.starts_with('#') {
            index += 1;
            continue;
        }
        let Some(id) = &header else {
            match parse_header(line) {
                Ok(id) => header = Some(id),
                Err(error) if error.starts_with("unsupported journal format") => {
                    return Err(error);
                }
                Err(error) => return Err(located(index, error)),
            }
            index += 1;
            continue;
        };
        if line.starts_with(ROW_INDENT) {
            return Err(located(index, "a pixel row outside a block".into()));
        }
        let statement_index = index;
        index += 1;
        // The scope was read first, so a layer named `legend` keeps its lines.
        if scope.is_none() && (line.starts_with("legend ") || line == "legend") {
            legend
                .read(line)
                .map_err(|error| located(statement_index, error))?;
            continue;
        }
        let statement = match scope {
            Some((scope, rest)) => {
                let (mut statement, tail) = parse_cel_statement(scope, rest)
                    .map_err(|error| located(statement_index, error))?;
                if let Some(EffectTail::Rows {
                    x,
                    y,
                    width,
                    height,
                    hex,
                }) = tail
                {
                    let mut rows = Vec::with_capacity(height.min(lines.len()));
                    let mut complete = true;
                    for _ in 0..height {
                        // A block that stops at the end of the file was cut by a crash.
                        let Some(row) = lines.get(index) else {
                            complete = false;
                            break;
                        };
                        rows.push(
                            parse_row(row, width, hex, &legend)
                                .map_err(|error| located(index, error))?,
                        );
                        index += 1;
                    }
                    if !complete {
                        torn_tail = true;
                        break;
                    }
                    if let Statement::Effect { change, .. } = &mut statement {
                        *change = Change::Rows { x, y, rows };
                    }
                } else if let (Some(EffectTail::Done(done)), Statement::Effect { change, .. }) =
                    (tail, &mut statement)
                {
                    *change = done;
                }
                statement
            }
            None => parse_call(line, id).map_err(|error| located(statement_index, error))?,
        };
        if statements.len() >= max_entries {
            return Err(format!(
                "journal line {}: recipe has more than {max_entries} entries",
                statement_index + 1
            ));
        }
        statements.push(statement);
    }
    validate_journal(&statements)?;
    Ok(ParsedJournal {
        statements,
        torn_tail,
        headed: header.is_some(),
        legend,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ID: &str = "550e8400-e29b-41d4-a716-446655440000";
    const OTHER: &str = "123e4567-e89b-42d3-a456-426614174000";

    fn header() -> String {
        format!("atelier 5 {ID} 0.1.0\n")
    }

    const NEW: &str = "new name=x width=4 height=4\n";

    fn call(tool: ToolName, args: Value) -> Statement {
        Statement::call(tool, serde_json::from_value(args).unwrap())
    }

    fn scope(layer: &str, frame: usize) -> Scope {
        Scope {
            layer: LayerRef::Name(layer.into()),
            frame,
        }
    }

    fn encode(statements: &[Statement]) -> String {
        encode_statements(statements, &mut Legend::default(), ID).unwrap()
    }

    const K: Pixel = [0x1a, 0x16, 0x30, 255];
    const C: Pixel = [0x5a, 0xd8, 0xf8, 255];

    #[test]
    fn reads_a_recipe() {
        let recipe = parse_journal(&format!(
            "# a note\n{}{NEW}\n# another\nlayer rename index=0 name=ink\n\
             legend A=#1a1630\nink @0 pencil #1a1630 = 1,2 3x1\n    A.A\n\
             ink @0 erase pencil ~\nink @1 shift dx=1 wrap=true\n",
            header()
        ))
        .unwrap();
        assert_eq!(
            recipe.statements.len(),
            5,
            "blank, # and legend lines are not statements"
        );
        assert_eq!(
            recipe.statements[0],
            call(
                ToolName::DocNew,
                json!({"doc_id": ID, "name": "x", "width": 4, "height": 4})
            )
        );
        assert_eq!(
            recipe.statements[1],
            call(
                ToolName::DocLayer,
                json!({"doc_id": ID, "op": "rename", "index": 0, "name": "ink"})
            )
        );
        assert_eq!(
            recipe.statements[2],
            Statement::Effect {
                scope: scope("ink", 0),
                annotation: "pencil #1a1630".into(),
                change: Change::Rows {
                    x: 1,
                    y: 2,
                    rows: vec![vec![K, TRANSPARENT, K]]
                }
            }
        );
        assert_eq!(
            recipe.statements[3],
            Statement::Effect {
                scope: scope("ink", 0),
                annotation: "erase pencil".into(),
                change: Change::None
            }
        );
        assert_eq!(
            recipe.statements[4],
            Statement::CelCall {
                scope: scope("ink", 1),
                op: "shift".into(),
                args: serde_json::from_value(json!({"dx": 1, "wrap": true})).unwrap()
            }
        );
        assert_eq!(recipe.legend.colour('A'), Some(K));
        assert!(recipe.headed && !recipe.torn_tail);
    }

    #[test]
    fn calls_lead_with_their_op_and_where_they_apply() {
        let layer = call(
            ToolName::DocLayer,
            json!({"doc_id": ID, "name": "glass back", "op": "rename", "index": 0}),
        );
        assert_eq!(
            encode(&[layer]),
            "layer rename index=0 name=\"glass back\"\n"
        );
        let palette = call(
            ToolName::DocPalette,
            json!({"doc_id": ID, "op": "set", "colors": ["#1a1630", "#f0b07a"]}),
        );
        assert_eq!(encode(&[palette]), "palette set colors=#1a1630,#f0b07a\n");
        let frame = call(
            ToolName::DocFrame,
            json!({"doc_id": ID, "op": "add", "copy_from": 1, "count": 2}),
        );
        assert_eq!(encode(&[frame]), "frame add copy_from=1 count=2\n");
        // An op that is not a bare word stays a keyed argument.
        let odd = call(ToolName::DocPalette, json!({"doc_id": ID, "op": "a b"}));
        assert_eq!(encode(&[odd]), "palette op=\"a b\"\n");
    }

    #[test]
    fn effects_carry_their_pixels_and_scopes_name_layers() {
        let statements = [
            Statement::Effect {
                scope: scope("ink", 0),
                annotation: "grid".into(),
                change: Change::Rows {
                    x: 12,
                    y: 2,
                    rows: vec![
                        vec![TRANSPARENT, K, K],
                        vec![K, C, K],
                        vec![TRANSPARENT, K, TRANSPARENT],
                    ],
                },
            },
            Statement::Effect {
                scope: Scope {
                    layer: LayerRef::Name("glass back".into()),
                    frame: 3,
                },
                annotation: "line #5ad8f8 0,0 3,3 size 2".into(),
                change: Change::Rows {
                    x: 0,
                    y: 0,
                    rows: vec![vec![C, [0x5a, 0xd8, 0xf8, 0x80]]],
                },
            },
            Statement::Effect {
                scope: Scope {
                    layer: LayerRef::Index(2),
                    frame: 0,
                },
                annotation: "erase rect".into(),
                change: Change::Solid {
                    x0: 1,
                    y0: 1,
                    x1: 4,
                    y1: 2,
                    color: TRANSPARENT,
                },
            },
            Statement::Effect {
                scope: scope("ink", 0),
                annotation: "rect #1a1630 fill".into(),
                change: Change::Solid {
                    x0: 0,
                    y0: 0,
                    x1: 3,
                    y1: 3,
                    color: K,
                },
            },
            Statement::Effect {
                scope: scope("ink", 0),
                annotation: "erase pencil".into(),
                change: Change::None,
            },
            Statement::CelCall {
                scope: scope("ink", 2),
                op: "flip".into(),
                args: serde_json::from_value(json!({"horizontal": true})).unwrap(),
            },
        ];
        let written = encode(&statements);
        assert_eq!(
            written,
            "legend A=#1a1630 B=#5ad8f8\n\
             ink @0 grid = 12,2 3x3\n    .AA\n    ABA\n    .A.\n\
             legend C=#5ad8f880\n\
             \"glass back\" @3 line #5ad8f8 0,0 3,3 size 2 = 0,0 2x1\n    BC\n\
             #2 @0 erase rect = 1,1 4,2 .\n\
             ink @0 rect #1a1630 fill = 0,0 3,3 #1a1630\n\
             ink @0 erase pencil ~\n\
             ink @2 flip horizontal=true\n"
        );
        let read = parse_journal(&format!("{}{NEW}{written}", header())).unwrap();
        assert_eq!(&read.statements[1..], &statements);
    }

    #[test]
    fn legend_glyphs_persist_and_are_rebound_when_exhausted() {
        let mut legend = Legend::default();
        let colours: Vec<Pixel> = (0..70u8).map(|n| [n, 0, 0, 255]).collect();
        let first = legend.provide(&colours[..62]).unwrap().unwrap();
        assert!(first.starts_with("legend A=#000000 B=#010000"));
        assert!(first.ends_with(" 9=#3d0000\n"));
        assert_eq!(
            legend.provide(&colours[..62]).unwrap(),
            None,
            "already bound"
        );
        // Two more colours evict the two bound longest ago.
        let more = legend.provide(&colours[61..64]).unwrap().unwrap();
        assert_eq!(more, "legend A=#3e0000 B=#3f0000\n");
        assert_eq!(legend.colour('A'), Some([62, 0, 0, 255]));
        assert_eq!(legend.glyph([0, 0, 0, 255]), None);
        assert!(
            legend
                .provide(&colours[..63])
                .unwrap_err()
                .contains("63 colours"),
            "a block over capacity is refused, not truncated"
        );
        // A legend line in a file rebinds the same way.
        let mut read = Legend::default();
        read.read("legend A=#000000 B=#010000").unwrap();
        read.read("legend A=#3e0000").unwrap();
        assert_eq!(read.colour('A'), Some([62, 0, 0, 255]));
        assert_eq!(read.colour('B'), Some([1, 0, 0, 255]));
        assert!(read.read("legend").is_err());
        assert!(read.read("legend ?=#000000").is_err());
        assert!(
            read.read("legend A=#00000000").is_err(),
            "transparent is always '.'"
        );
    }

    #[test]
    fn blocks_with_too_many_colours_are_written_in_hex() {
        let row: Vec<Pixel> = (0..63u8).map(|n| [n, 1, 2, 255]).collect();
        let effect = Statement::Effect {
            scope: scope("ink", 0),
            annotation: "gradient".into(),
            change: Change::Rows {
                x: 0,
                y: 0,
                rows: vec![row.clone()],
            },
        };
        let written = encode(std::slice::from_ref(&effect));
        assert!(written.starts_with("ink @0 gradient = 0,0 63x1 hex\n    #000102 #010102 "));
        assert!(!written.contains("legend"));
        let read = parse_journal(&format!("{}{NEW}{written}", header())).unwrap();
        assert_eq!(read.statements[1], effect);
    }

    #[test]
    fn values_lists_and_maps_are_written_plainly() {
        let line = |tool, args| encode(&[call(tool, args)]);
        assert_eq!(
            line(
                ToolName::DocPalette,
                json!({"doc_id": ID, "op": "swap", "from": ["#1a1630"], "to": [[1, 2, 3, 4]]})
            ),
            "palette swap from=[\"#1a1630\"] to=[[1,2,3,4]]\n"
        );
        assert_eq!(
            line(
                ToolName::DocAddTag,
                json!({"doc_id": ID, "name": "walk", "from": 0, "to": 7, "direction": "forward"})
            ),
            "add_tag name=walk direction=forward from=0 to=7\n"
        );
        // Anything that would not read back exactly stays JSON.
        for (value, written) in [
            (json!(["one"]), "[\"one\"]"),
            (json!([]), "[]"),
            (json!({}), "{}"),
            (json!([[1, 2], [3, 4]]), "1,2 3,4"),
            (json!([[1, 2]]), "[[1,2]]"),
            (json!([[1], [2]]), "[[1],[2]]"),
            (json!([[1, 2], [3]]), "[[1,2],[3]]"),
            (json!([[1, 2], 3]), "[[1,2],3]"),
            (json!([["a", "b"], ["c", "d"]]), "a,b c,d"),
            (json!([[[1, 2], [3, 4]]]), "[[[1,2],[3,4]]]"),
            (json!(["a", "b c"]), "[\"a\",\"b c\"]"),
            (json!(["1", 2]), "[\"1\",2]"),
            (json!({"a:b": 1}), "{\"a:b\":1}"),
            (json!({"K": [1, 2, 3]}), "{\"K\":[1,2,3]}"),
            (json!("a,b"), "\"a,b\""),
            (json!("K:1"), "\"K:1\""),
            (json!("#"), "#"),
            (json!([true, null, "x", 1]), "true,null,x,1"),
        ] {
            assert_eq!(encode_value(&value).unwrap(), written);
            let read = parse_journal(&format!("{}{NEW}palette v={written}\n", header())).unwrap();
            let Statement::Call { args, .. } = &read.statements[1] else {
                panic!("not a call");
            };
            assert_eq!(args["v"], value, "{written}");
        }
    }

    #[test]
    fn every_statement_round_trips_exactly() {
        let statements = [
            call(
                ToolName::DocNew,
                json!({"doc_id": ID, "name": "x", "width": 4, "height": 4}),
            ),
            // Words that would read as other JSON types keep their quotes.
            call(
                ToolName::DocLayer,
                json!({"doc_id": ID, "op": "rename", "index": 0, "name": "true"}),
            ),
            call(
                ToolName::DocLayer,
                json!({"doc_id": ID, "op": "add", "name": "a \"quoted\" = name\nwith a break é"}),
            ),
            call(
                ToolName::DocLayer,
                json!({"doc_id": ID, "op": "add", "name": "12"}),
            ),
            call(
                ToolName::DocLayer,
                json!({"doc_id": ID, "op": "add", "name": ""}),
            ),
            // No document id of its own.
            call(
                ToolName::DocPalette,
                json!({"op": "generate", "set_doc": ID, "base": [1, 2, 3]}),
            ),
            call(
                ToolName::DocPalette,
                json!({"doc_id": ID, "op": "swap", "from": [[1, 2, 3], "#040506"], "to": ["#000000", "#ffffff"]}),
            ),
            // An argument name the line grammar cannot carry.
            call(
                ToolName::DocPalette,
                json!({"doc_id": ID, "op": "set", "odd key=1": [1]}),
            ),
            // Layers whose names need quoting, or are shared, in scopes.
            Statement::Effect {
                scope: Scope {
                    layer: LayerRef::Name("a \"quoted\" @0 name".into()),
                    frame: 0,
                },
                annotation: "text #ffffff 1,1".into(),
                change: Change::Rows {
                    x: 0,
                    y: 0,
                    rows: vec![vec![[255, 255, 255, 255]]],
                },
            },
            Statement::Effect {
                scope: Scope {
                    layer: LayerRef::Name("#5".into()),
                    frame: 1,
                },
                annotation: String::new(),
                change: Change::None,
            },
            Statement::Effect {
                scope: Scope {
                    layer: LayerRef::Name("legend".into()),
                    frame: 1,
                },
                annotation: "wipe".into(),
                change: Change::Solid {
                    x0: 0,
                    y0: 0,
                    x1: 3,
                    y1: 3,
                    color: TRANSPARENT,
                },
            },
            Statement::Effect {
                scope: Scope {
                    layer: LayerRef::Name("layer".into()),
                    frame: 0,
                },
                annotation: "pencil".into(),
                change: Change::Rows {
                    x: 2,
                    y: 3,
                    rows: vec![vec![[1, 2, 3, 4]], vec![TRANSPARENT]],
                },
            },
            Statement::CelCall {
                scope: Scope {
                    layer: LayerRef::Index(0),
                    frame: 9,
                },
                op: "rotate".into(),
                args: serde_json::from_value(json!({"turns": 3})).unwrap(),
            },
        ];
        let mut source = header();
        source.push_str(&encode(&statements));
        assert!(!source[header().len()..].contains(ID) || source.contains("set_doc"));
        let recipe = parse_journal(&source).unwrap();
        assert_eq!(recipe.statements, statements, "{source}");
        assert!(
            encode_statements(
                &[call(ToolName::DocLayer, json!({"doc_id": OTHER}))],
                &mut Legend::default(),
                ID
            )
            .is_err(),
            "a call for another document is never written"
        );
        assert!(
            encode_statements(
                &[call(ToolName::DocDraw, json!({"doc_id": ID, "op": "rect"}))],
                &mut Legend::default(),
                ID
            )
            .unwrap_err()
            .contains("effect"),
            "pixel tools are never calls"
        );
    }

    #[test]
    fn annotations_are_kept_harmless() {
        let effect = Statement::Effect {
            scope: scope("ink", 0),
            annotation: "text #ffffff 1,1 \"a = b ~\" size=2\n next".into(),
            change: Change::None,
        };
        assert_eq!(encode(&[effect]), "ink @0 text #ffffff 1,1 \"a b next ~\n");
    }

    #[test]
    fn a_torn_final_record_preserves_completed_steps() {
        let effect = "ink @0 grid = 0,0 2x3\n    A.\n    .A\n    AA\n";
        for torn in [
            "la",
            "layer rename name=\"#10",
            "palette set colors=[1,2],[3",
            // A cut that still reads as a shorter statement is not mistaken for one.
            "layer add name=x",
            "# a note",
            "  ",
            &effect[..effect.len() - 8],
            // Cut exactly between two rows of a block: the file still ends in a newline.
            &effect[..effect.len() - 7],
            &effect[..effect.len() - 14],
            "legend B=#010000",
        ] {
            let recipe = parse_journal(&format!(
                "{}{NEW}legend A=#1a1630\nink @0 erase pencil ~\n{torn}",
                header()
            ))
            .unwrap();
            assert_eq!(recipe.statements.len(), 2, "{torn:?}");
            assert!(recipe.torn_tail, "{torn:?}");
        }
        let only_header = parse_journal("atelier 5 550e84").unwrap();
        assert!(only_header.torn_tail && !only_header.headed);
    }

    #[test]
    fn corruption_and_other_formats_are_rejected() {
        let bad = |body: &str| parse_journal(&format!("{}{NEW}{body}", header())).unwrap_err();
        assert!(bad("layer op=\ntorn\n").contains("line 3"));
        assert!(bad("layer rename name=\"#10\n").contains("line 3"));
        assert!(bad("not_a_tool op=rect\n").contains("unknown tool"));
        assert!(bad("layer rename op=line\n").contains("given twice"));
        assert!(bad("layer rename x=1y=2\n").contains("line 3"));
        assert!(bad("layer rename name=\"a\"b\n").contains("unexpected text"));
        assert!(bad("layer rename stray stray\n").contains("key=value"));
        assert!(bad("doc_layer add\n").contains("unknown tool"));
        assert!(bad("layer add x=1,,2\n").contains("value is missing"));
        assert!(bad("layer add x=1,b:2\n").contains("key:value"));
        assert!(bad("layer add x=a:1,a:2\n").contains("given twice"));
        assert!(bad("layer add x=12abc\n").contains("not a number"));
        assert!(bad("layer add x=a,b*c\n").contains("must be quoted"));
        assert!(bad("draw op=rect layer=0 frame=0\n").contains("effect"));
        assert!(bad("paint_grid layer=0\n").contains("effect"));
        assert!(bad(&format!("layer doc_id=\"{ID}\"\n")).contains("must not repeat doc_id"));
        // Cel statements.
        assert!(bad("ink @0 blur radius=1\n").contains("not a cel call"));
        assert!(bad("ink @0 shift layer=1\n").contains("must not carry 'layer'"));
        assert!(bad("ink @x shift dx=1\n").contains("not a frame number"));
        assert!(bad("ink @0 grid = 0,0 2x1\n    AB\n").contains("not in the legend"));
        assert!(bad("ink @0 grid = 0,0 2x1\n  ..\n").contains("four spaces"));
        assert!(bad("legend A=#1a1630\nink @0 grid = 0,0 2x1\n    A\n").contains("2 wide"));
        assert!(bad("ink @0 grid = 0,0 0x1\n").contains("WxH"));
        assert!(bad("ink @0 grid = 3,3 1,1 .\n").contains("ordered rectangle"));
        assert!(bad("ink @0 grid = 0,0 1,1 #00000000\n").contains("not a colour"));
        assert!(bad("ink @0 grid = 0,0\n").contains("must be"));
        assert!(bad("    AB\n").contains("outside a block"));
        assert!(
            bad("legend A=#1a1630\nink @0 grid = 0,0 1x1 hex\n    A\n").contains("not a colour")
        );
        // A `#` line is a note unless it is a `#N @F` scope.
        let noted =
            parse_journal(&format!("{}{NEW}#x @0 shift\n# 2 @0 shift\n", header())).unwrap();
        assert_eq!(noted.statements.len(), 1);
        assert!(bad("#2 @0 blur\n").contains("not a cel call"));
        assert!(parse_journal("\n\n").unwrap().statements.is_empty());
        assert!(parse_journal(NEW).unwrap_err().contains("line 1"));
        assert!(parse_journal("atelier 5 d_0000000000000000 0.1.0\n").is_err());
        assert!(parse_journal(&format!("atelier 5 {ID}\n")).is_err());
        // Earlier and later formats are version mismatches, named as such.
        for (source, number) in [
            (format!("atelier 6 {ID} 9.0.0\n"), "6"),
            (
                format!("atelier 4 {ID} 0.1.0\nnew name=x width=4 height=4\n"),
                "4",
            ),
            (
                format!("atelier 3 {ID} 0.1.0\ndoc_new name=x width=4 height=4\n"),
                "3",
            ),
            (
                format!("{{\"schema\":2,\"doc_id\":\"{ID}\",\"atelier\":\"0.1.0\"}}\n"),
                "2",
            ),
        ] {
            let error = parse_journal(&source).unwrap_err();
            assert!(
                error.contains(&format!("unsupported journal format {number}")),
                "{error}"
            );
        }
    }

    #[test]
    fn the_contract_is_one_doc_new_first_targeting_the_header_document() {
        let bad = |body: &str| parse_journal(&format!("{}{body}", header())).unwrap_err();
        assert!(bad("layer add name=x\n").contains("start with doc_new"));
        assert!(bad(&format!("{NEW}{NEW}")).contains("exactly one doc_new"));
        assert!(
            bad(&format!(
                "{NEW}palette generate doc_id=null set_doc=\"{OTHER}\"\n"
            ))
            .contains("targets a document other than")
        );
        assert!(
            bad(&format!("{NEW}palette generate doc_id=null\n")).contains("no document target")
        );
    }

    #[test]
    fn encoded_size_is_checked_before_parsing() {
        let error = parse_with_limits("not-a-recipe", 4, MAX_JOURNAL_ENTRIES).unwrap_err();
        assert!(error.contains("over the 4-byte replay limit"), "{error}");
        assert!(
            !error.contains("line 1"),
            "size must be the first check: {error}"
        );
    }

    #[test]
    fn entry_count_is_bounded_with_the_overflowing_line_reported() {
        let source = format!("{}{NEW}layer add name=x\n", header());
        let error = parse_with_limits(&source, MAX_JOURNAL_BYTES, 1).unwrap_err();
        assert!(error.contains("line 3"), "{error}");
        assert!(error.contains("more than 1 entries"), "{error}");
    }
}
