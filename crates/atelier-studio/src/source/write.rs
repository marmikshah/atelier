//! Render readable source text, preserving unchanged comments and formatting.
use super::syntax::{self, Node};
use atelier_core::source::{Asset, PixelData, Step, color, hex};
use serde_json::{Map, Value};

fn scalar(value: &Value) -> String {
    match value {
        Value::String(s) => syntax::quote(s),
        Value::Bool(b) => format!("#{b}"),
        Value::Null => "#null".into(),
        _ => value.to_string(),
    }
}
fn colors(values: &[String]) -> String {
    values
        .iter()
        .map(|s| syntax::quote(s))
        .collect::<Vec<_>>()
        .join(" ")
}
fn args<T: ToString>(values: &[T]) -> String {
    values
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}
fn color_value(key: &str, v: &Value) -> Value {
    if matches!(key, "color" | "colorize")
        && let Some(a) = v.as_array()
        && matches!(a.len(), 3 | 4)
    {
        let mut c = [0, 0, 0, 255];
        for (i, n) in a.iter().enumerate() {
            c[i] = n.as_u64().unwrap_or(0) as u8;
        }
        return Value::String(hex(&c));
    }
    v.clone()
}
fn fields_text(out: &mut String, object: &Map<String, Value>, depth: usize, skip: &str) {
    for (key, v) in object {
        if key == skip {
            continue;
        }
        let v = color_value(key, v);
        if !v.is_array() && !v.is_object() {
            out.push_str(&format!(" {key}={}", scalar(&v)));
        }
    }
    let complex: Vec<_> = object
        .iter()
        .filter(|(k, v)| {
            *k != skip && {
                let v = color_value(k, v);
                v.is_array() || v.is_object()
            }
        })
        .collect();
    if complex.is_empty() {
        out.push('\n');
        return;
    }
    out.push_str(" {\n");
    for (key, v) in complex {
        array_text(out, key, v, depth + 1);
    }
    out.push_str(&format!("{}}}\n", "  ".repeat(depth)));
}
fn array_text(out: &mut String, key: &str, v: &Value, depth: usize) {
    let pad = "  ".repeat(depth);
    match v {
        Value::Array(items) if items.iter().all(|v| !v.is_array() && !v.is_object()) => out
            .push_str(&format!(
                "{pad}{key} {}\n",
                items.iter().map(scalar).collect::<Vec<_>>().join(" ")
            )),
        Value::Array(items) => {
            out.push_str(&format!("{pad}{key} {{\n"));
            let name = match key {
                "points" => "point",
                "stops" => "stop",
                _ => "item",
            };
            for item in items {
                match item {
                    Value::Object(obj) => {
                        out.push_str(&format!("{pad}  {name}"));
                        fields_text(out, obj, depth + 1, "");
                    }
                    _ => array_text(out, name, item, depth + 1),
                }
            }
            out.push_str(&format!("{pad}}}\n"));
        }
        Value::Object(obj) => {
            out.push_str(&format!("{pad}{key}"));
            fields_text(out, obj, depth, "");
        }
        _ => out.push_str(&format!("{pad}{key} {}\n", scalar(v))),
    }
}
pub(super) fn encode(asset: &Asset) -> Result<String, String> {
    asset.validate()?;
    let mut out = format!(
        "atelier {} format=1 renderer=1\ncanvas {}\nframes {}\n",
        syntax::quote(&asset.name),
        args(&asset.canvas),
        args(&asset.frames)
    );
    if !asset.palette.is_empty() {
        out.push_str(&format!("palette {}\n", colors(&asset.palette)));
    }
    if let Some(r) = &asset.reference {
        out.push_str(&format!("reference {}\n", syntax::quote(r)));
    }
    for tag in &asset.tags {
        let v = serde_json::to_value(tag).map_err(|e| e.to_string())?;
        out.push_str(&format!("tag {}", syntax::quote(&tag.name)));
        fields_text(
            &mut out,
            v.as_object().ok_or("tag must be object")?,
            0,
            "name",
        );
    }
    for layer in &asset.layers {
        out.push_str(&format!("\nlayer {}", syntax::quote(&layer.name)));
        if layer.opacity != 255 {
            out.push_str(&format!(" opacity={}", layer.opacity));
        }
        if !layer.visible {
            out.push_str(" visible=#false");
        }
        if layer.blend != atelier_core::raster::Blend::Normal {
            out.push_str(&format!(
                " blend={}",
                scalar(&serde_json::to_value(layer.blend).map_err(|e| e.to_string())?)
            ));
        }
        out.push_str(" {\n");
        for cel in &layer.cels {
            out.push_str(&format!("  frame {} {{\n", args(&cel.frames)));
            if let Some(size) = cel.size {
                out.push_str(&format!("    size {}\n", args(&size)));
            }
            if cel.at != [0, 0] {
                out.push_str(&format!("    at {}\n", args(&cel.at)));
            }
            let mut active = &asset.palette;
            for step in &cel.steps {
                match step {
                    Step::Draw { op, palette } => {
                        if active != palette {
                            out.push_str(&format!("    palette {}\n", colors(palette)));
                            active = palette;
                        }
                        out.push_str(&format!(
                            "    {}",
                            op["op"].as_str().ok_or("operation missing name")?
                        ));
                        fields_text(
                            &mut out,
                            op.as_object().ok_or("invalid operation")?,
                            2,
                            "op",
                        );
                    }
                    Step::Pixels(p) => {
                        out.push_str("    pixels");
                        if p.origin != [0, 0] {
                            out.push_str(&format!(" x={} y={}", p.origin[0], p.origin[1]));
                        }
                        out.push_str(" {\n      legend {\n");
                        for (symbol, c) in &p.legend {
                            out.push_str(&format!(
                                "        color {} {}\n",
                                syntax::quote(symbol),
                                syntax::quote(c)
                            ));
                        }
                        out.push_str("      }\n");
                        for d in &p.data {
                            match d {
                                PixelData::Grid(text) => {
                                    out.push_str("      grid \"\"\"\n");
                                    for row in text.lines() {
                                        let row = row.trim_end_matches(['.', ' ']);
                                        if !row.is_empty() {
                                            out.push_str("        ");
                                            out.push_str(row);
                                        }
                                        out.push('\n');
                                    }
                                    out.push_str("        \"\"\"\n");
                                }
                                PixelData::Row { runs, repeat } => {
                                    out.push_str("      row");
                                    for (s, n) in runs {
                                        out.push_str(&format!(" {} {n}", syntax::quote(s)));
                                    }
                                    if *repeat != 1 {
                                        out.push_str(&format!(" repeat={repeat}"));
                                    }
                                    out.push('\n');
                                }
                                PixelData::Span { x, y, text, rows } => {
                                    out.push_str(&format!(
                                        "      span {x} {y} {}",
                                        syntax::quote(text)
                                    ));
                                    if *rows != 1 {
                                        out.push_str(&format!(" rows={rows}"));
                                    }
                                    out.push('\n');
                                }
                            }
                        }
                        out.push_str("    }\n");
                    }
                }
            }
            out.push_str("  }\n");
        }
        out.push_str("}\n");
    }
    Ok(out)
}
/// Keep the spelling, comments and whitespace of unchanged source nodes.
/// A source edit never triggers a wholesale pretty-print of other artwork.
pub(super) fn preserve(previous: &str, canonical: &str) -> Result<String, String> {
    let mut before = syntax::parse(previous)?;
    let mut after = syntax::parse(canonical)?;
    normalize_colours(&mut before);
    normalize_colours(&mut after);
    let mut out = merge(&before, &after, previous, canonical);
    out.push_str(&previous[before.last().map_or(0, |n| n.end)..]);
    Ok(out)
}
fn normalize_colours(nodes: &mut [Node]) {
    fn normalize(v: &mut Value) {
        if let Some(s) = v.as_str()
            && let Ok(c) = color(s)
        {
            *v = Value::String(hex(&c));
        }
    }
    for n in nodes {
        for key in ["color", "colorize"] {
            if let Some(v) = n.props.get_mut(key) {
                normalize(v);
            }
        }
        if n.name == "palette" {
            for v in &mut n.args {
                normalize(v);
            }
        }
        if n.name == "color"
            && let Some(v) = n.args.get_mut(1)
        {
            normalize(v);
        }
        normalize_colours(&mut n.children);
    }
}
fn merge(old: &[Node], new: &[Node], before: &str, after: &str) -> String {
    let mut out = String::new();
    let mut used = vec![false; old.len()];
    for n in new {
        let index = old
            .iter()
            .enumerate()
            .find(|(i, o)| !used[*i] && o.same(n))
            .map(|(i, _)| i)
            .or_else(|| {
                old.iter()
                    .enumerate()
                    .find(|(i, o)| {
                        !used[*i]
                            && o.name == n.name
                            && (n.name != "frame" || o.args.iter().any(|f| n.args.contains(f)))
                    })
                    .map(|(i, _)| i)
            });
        if let Some(i) = index {
            used[i] = true;
            let o = &old[i];
            if o.same(n) {
                out.push_str(&before[o.start..o.end]);
                continue;
            }
            if let (Some((ob, oe)), Some((nb, ne))) = (o.body, n.body) {
                if o.header(n) {
                    out.push_str(&before[o.start..ob]);
                } else {
                    out.push_str(&after[n.start..nb]);
                }
                out.push_str(&merge(&o.children, &n.children, before, after));
                let last = o.children.last().map_or(ob, |c| c.end);
                out.push_str(&before[last..oe]);
                if o.header(n) {
                    out.push_str(&before[oe..o.end]);
                } else {
                    out.push_str(&after[ne..n.end]);
                }
                continue;
            }
        }
        out.push_str(&after[n.start..n.end]);
    }
    out
}
