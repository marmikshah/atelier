use atelier_core::source::{Asset, Cel};
use serde_json::Value;

/// Settings once, then each layer's complete frame descriptions.
pub(super) fn encode(asset: &Asset) -> Result<String, String> {
    let value = serde_json::to_value(asset).map_err(|e| e.to_string())?;
    let mut out = String::from("# Atelier replay. Rebuild with: atelier replay <this-file>\n");
    for key in [
        "format",
        "renderer",
        "name",
        "canvas",
        "frames",
        "palette",
        "tags",
        "reference",
    ] {
        if let Some(v) = value.get(key) {
            field(&mut out, key, v);
        }
    }
    for layer in value["layers"].as_array().ok_or("missing layers")? {
        out.push_str("\n[[layers]]\n");
        for key in ["name", "opacity", "visible", "blend"] {
            if let Some(v) = layer.get(key) {
                field(&mut out, key, v);
            }
        }
        for cel in layer
            .get("cels")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            out.push_str("\n[[layers.cels]]\n");
            cel_fields(&mut out, cel);
        }
    }
    Ok(out)
}
pub(super) fn cel_bytes(cel: &Cel) -> usize {
    let mut out = String::new();
    cel_fields(
        &mut out,
        &serde_json::to_value(cel).expect("cel serializes"),
    );
    out.len()
}
fn cel_fields(out: &mut String, cel: &Value) {
    for key in [
        "frames", "size", "at", "origin", "legend", "image", "palette",
    ] {
        if let Some(v) = cel.get(key) {
            field(out, key, v);
        }
    }
    if let Some(grid) = cel.get("grid").and_then(Value::as_str) {
        if !grid.contains("'''") && !grid.contains('\r') {
            out.push_str("grid = '''\n");
            out.push_str(grid);
            if !grid.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("'''\n");
        } else {
            field(out, "grid", &Value::String(grid.into()));
        }
    }
    if let Some(ops) = cel.get("draw").and_then(Value::as_array) {
        out.push_str("draw = [\n");
        for op in ops {
            out.push_str(&format!("  {},\n", inline(op)));
        }
        out.push_str("]\n");
    }
}
fn field(out: &mut String, key: &str, value: &Value) {
    out.push_str(&format!("{key} = {}\n", inline(value)));
}
fn quote(text: &str) -> String {
    toml_edit::Value::from(text).to_string()
}
pub(super) fn inline(value: &Value) -> String {
    match value {
        Value::String(s) => quote(s),
        // TOML integers are signed; unsigned procedural seeds remain exact as
        // decimal strings rather than being truncated or converted to floats.
        Value::Number(n) if n.as_u64().is_some_and(|v| v > i64::MAX as u64) => {
            quote(&n.to_string())
        }
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(inline).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(items) => format!(
            "{{ {} }}",
            items
                .iter()
                .map(|(k, v)| format!(
                    "{} = {}",
                    if k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                        k.clone()
                    } else {
                        quote(k)
                    },
                    inline(v)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => value.to_string(),
    }
}
