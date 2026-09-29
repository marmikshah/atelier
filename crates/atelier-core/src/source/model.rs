use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::borrow::Cow;

use crate::document::{DocMeta, FrameMeta, LayerMeta, TagMeta};
use crate::raster::Blend;

/// The on-disk source contract. Unlike the document's working UUID, part and
/// layer names are stable identities chosen by the author.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Asset {
    pub format: u32,
    pub renderer: u32,
    pub name: String,
    pub canvas: [u32; 2],
    #[serde(default = "frames")]
    pub frames: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub palette: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inks: BTreeMap<String, String>,
    pub layers: Vec<Layer>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<TagMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(default)]
    pub parts: BTreeMap<String, Part>,
    #[serde(default)]
    pub cels: Vec<Cel>,
}

fn frames() -> Vec<u32> {
    vec![100]
}
pub(super) fn one() -> u32 {
    1
}
pub(super) fn opaque() -> u8 {
    255
}
fn yes() -> bool {
    true
}
fn is_zero(v: &[i32; 2]) -> bool {
    *v == [0, 0]
}
fn is_origin(v: &[u32; 2]) -> bool {
    *v == [0, 0]
}
fn is_one(v: &u32) -> bool {
    *v == 1
}
fn is_opaque(v: &u8) -> bool {
    *v == 255
}
fn is_false(v: &bool) -> bool {
    !v
}
fn is_true(v: &bool) -> bool {
    *v
}
fn no_turn(v: &u8) -> bool {
    *v == 0
}
fn normal(v: &Blend) -> bool {
    *v == Blend::Normal
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    pub id: String,
    pub name: String,
    #[serde(default = "opaque", skip_serializing_if = "is_opaque")]
    pub opacity: u8,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub visible: bool,
    #[serde(default, skip_serializing_if = "normal")]
    pub blend: Blend,
}

/// Exactly one of `grid`, `image`, `items`, or `draw` defines a part. Explicit
/// logical size remains fixed when an image or grid is cropped inside it.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Part {
    pub size: [u32; 2],
    #[serde(default, skip_serializing_if = "is_origin")]
    pub origin: [u32; 2],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grid: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub legend: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<Instance>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draw: Option<Vec<Value>>,
    /// Optional initial pixels for a sequence of existing core drawing ops.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// A procedure can preserve its palette at the time it was authored.
    /// Omitted inherits the asset palette; an empty array means unlocked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub palette: Option<Vec<String>>,
}

impl Part {
    pub fn kind(&self) -> &'static str {
        if self.grid.is_some() {
            "grid"
        } else if self.image.is_some() {
            "image"
        } else if self.items.is_some() {
            "group"
        } else {
            "draw"
        }
    }

    pub fn dependencies(&self) -> Vec<&str> {
        self.base
            .iter()
            .map(String::as_str)
            .chain(self.items.iter().flatten().map(|i| i.part.as_str()))
            .collect()
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Placement {
    /// Copy all RGBA tuples, including transparent pixels and transparent RGB.
    #[default]
    Replace,
    /// Standard Atelier source-over compositing with opacity and blend.
    Over,
}

fn replace(v: &Placement) -> bool {
    *v == Placement::Replace
}

/// Integer transformations are applied about the logical rectangle: flips,
/// clockwise quarter turns, then scale, then translation. No resampling.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Instance {
    pub part: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub at: [i32; 2],
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub scale: u32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub flip_x: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub flip_y: bool,
    #[serde(default, skip_serializing_if = "no_turn")]
    pub turns: u8,
    #[serde(default, skip_serializing_if = "replace")]
    pub mode: Placement,
    #[serde(default = "opaque", skip_serializing_if = "is_opaque")]
    pub opacity: u8,
    #[serde(default, skip_serializing_if = "normal")]
    pub blend: Blend,
    /// Rebind named inks for this instance, without merging equal-colour roles.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bindings: BTreeMap<String, String>,
}

impl Instance {
    pub fn new(part: impl Into<String>) -> Self {
        Self {
            part: part.into(),
            at: [0, 0],
            scale: 1,
            flip_x: false,
            flip_y: false,
            turns: 0,
            mode: Placement::Replace,
            opacity: 255,
            blend: Blend::Normal,
            bindings: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cel {
    pub layer: String,
    pub frames: Vec<usize>,
    pub part: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub at: [i32; 2],
}

impl Asset {
    pub fn metadata(&self) -> Result<DocMeta, String> {
        Ok(DocMeta {
            format_version: 1,
            name: self.name.clone(),
            w: self.canvas[0],
            h: self.canvas[1],
            palette: self
                .palette
                .iter()
                .map(|s| color(s))
                .collect::<Result<_, _>>()?,
            layers: self
                .layers
                .iter()
                .map(|l| LayerMeta {
                    name: l.name.clone(),
                    opacity: l.opacity,
                    visible: l.visible,
                    blend: l.blend,
                })
                .collect(),
            frames: self
                .frames
                .iter()
                .map(|ms| FrameMeta { duration_ms: *ms })
                .collect(),
            tags: self.tags.clone(),
            cels: vec![],
            reference: self.reference.as_ref().map(|_| "reference.png".into()),
        })
    }

    pub fn ink(&self, name: &str, bindings: &BTreeMap<String, String>) -> Result<[u8; 4], String> {
        // Bindings are one level, not aliases that can form another graph.
        let value = bindings.get(name).map_or(name, String::as_str);
        color(self.inks.get(value).map_or(value, String::as_str))
            .map_err(|_| format!("unknown ink or invalid colour '{value}'"))
    }
}

/// Parse exact RGB/RGBA hex. No colour quantisation or alpha normalisation.
pub fn color(text: &str) -> Result<[u8; 4], String> {
    let hex = text
        .strip_prefix('#')
        .ok_or_else(|| format!("expected #RRGGBB or #RRGGBBAA: '{text}'"))?;
    if !matches!(hex.len(), 6 | 8) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("expected #RRGGBB or #RRGGBBAA: '{text}'"));
    }
    let mut rgba = [0, 0, 0, 255];
    for (i, slot) in rgba.iter_mut().enumerate().take(hex.len() / 2) {
        *slot = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).map_err(|e| e.to_string())?;
    }
    Ok(rgba)
}

pub fn hex(c: &[u8; 4]) -> String {
    if c[3] == 255 {
        format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
    } else {
        format!("#{:02x}{:02x}{:02x}{:02x}", c[0], c[1], c[2], c[3])
    }
}

/// TOML has signed integers. A quoted decimal seed represents the rest of the
/// existing renderer's u64 range without changing the drawing-operation API.
pub(super) fn drawing_op(op: &Value) -> Result<Cow<'_, Value>, String> {
    let Some(Value::String(seed)) = op.get("seed") else {
        return Ok(Cow::Borrowed(op));
    };
    if seed.is_empty() || !seed.bytes().all(|c| c.is_ascii_digit()) {
        return Err("seed must be an unsigned decimal integer".into());
    }
    let seed: u64 = seed.parse().map_err(|_| "seed exceeds the u64 range")?;
    let mut resolved = op.clone();
    resolved["seed"] = Value::from(seed);
    Ok(Cow::Owned(resolved))
}
