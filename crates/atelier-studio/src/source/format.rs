use atelier_core::source::Asset;
use serde_json::Value;

/// Human-facing layout: metadata, ordered assignments, then named part tables.
/// Operations and instance records stay on one line; pixel grids stay spatial.
pub(super) fn encode(asset: &Asset) -> Result<String, String> {
    let value = serde_json::to_value(asset).map_err(|e| e.to_string())?;
    let mut out =
        String::from("# Atelier artwork source. Rebuild with: atelier replay <this-file>\n");
    for key in [
        "format",
        "renderer",
        "name",
        "canvas",
        "frames",
        "palette",
        "reference",
        "tags",
    ] {
        if let Some(v) = value.get(key) {
            field(&mut out, key, v);
        }
    }
    for key in ["layers", "cels"] {
        out.push_str(&format!("\n{key} = [\n"));
        for item in value[key].as_array().ok_or("expected array")? {
            out.push_str(&format!("  {},\n", inline(item)));
        }
        out.push_str("]\n");
    }
    if !asset.inks.is_empty() {
        out.push_str("\n[inks]\n");
        for (key, v) in value["inks"].as_object().ok_or("expected inks")? {
            field(&mut out, &quote(key), v);
        }
    }
    for (name, p) in value["parts"].as_object().ok_or("expected parts")? {
        out.push_str(&format!("\n[parts.{}]\n", quote(name)));
        for key in ["size", "origin", "legend", "image", "palette", "base"] {
            if let Some(v) = p.get(key) {
                field(&mut out, key, v);
            }
        }
        if let Some(grid) = p.get("grid").and_then(Value::as_str) {
            if !grid.contains("'''") && !grid.contains('\r') {
                out.push_str("grid = '''\n");
                out.push_str(grid);
                out.push_str("'''\n");
            } else {
                field(&mut out, "grid", &Value::String(grid.into()));
            }
        }
        for key in ["items", "draw"] {
            if let Some(array) = p.get(key).and_then(Value::as_array) {
                out.push_str(&format!("{key} = [\n"));
                for item in array {
                    out.push_str(&format!("  {},\n", inline(item)));
                }
                out.push_str("]\n");
            }
        }
    }
    Ok(out)
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
