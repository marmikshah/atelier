use crate::document::{DocMeta, FrameMeta, LayerMeta, TagMeta};
use crate::raster::Blend;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::borrow::Cow;
use std::collections::BTreeMap;

/// A replay describes the current document, grouped by layer and frame.
/// Drawing operations reuse the renderer's existing parameter schemas.
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<TagMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    pub layers: Vec<Layer>,
}
fn frames() -> Vec<u32> {
    vec![100]
}
fn first() -> Vec<usize> {
    vec![0]
}
fn opaque() -> u8 {
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
fn is_opaque(v: &u8) -> bool {
    *v == 255
}
fn is_true(v: &bool) -> bool {
    *v
}
fn normal(v: &Blend) -> bool {
    *v == Blend::Normal
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    pub name: String,
    #[serde(default = "opaque", skip_serializing_if = "is_opaque")]
    pub opacity: u8,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub visible: bool,
    #[serde(default, skip_serializing_if = "normal")]
    pub blend: Blend,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cels: Vec<Cel>,
}

/// One independent cel or several identical frames. A grid/image supplies
/// optional starting pixels; `draw` contains only operations still needed to
/// reproduce this cel. No UUIDs, API envelopes, or editing history are stored.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cel {
    #[serde(default = "first")]
    pub frames: Vec<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<[u32; 2]>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub at: [i32; 2],
    #[serde(default, skip_serializing_if = "is_origin")]
    pub origin: [u32; 2],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grid: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub legend: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub draw: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub palette: Option<Vec<String>>,
}
impl Cel {
    pub fn size(&self, canvas: [u32; 2]) -> [u32; 2] {
        self.size.unwrap_or(canvas)
    }
}
impl Asset {
    pub fn cels(&self) -> impl Iterator<Item = &Cel> {
        self.layers.iter().flat_map(|l| &l.cels)
    }
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
