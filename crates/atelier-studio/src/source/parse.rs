//! Domain schema and encoding. Drawing parameters go straight to the existing
//! strict operation validator. Unknown or duplicate source fields are errors.
use super::syntax::{self, Node};
use atelier_core::source::{Asset, Cel, Layer, PixelData, Pixels, Step, color, hex};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

fn strings(args: &[Value]) -> Result<Vec<String>, String> {
    args.iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or("expected quoted text".into())
        })
        .collect()
}
fn parse_palette(args: &[Value]) -> Result<Vec<String>, String> {
    strings(args)?
        .iter()
        .map(|s| color(s).map(|c| hex(&c)))
        .collect()
}
fn number<T: TryFrom<u64>>(v: &Value) -> Result<T, String> {
    v.as_u64()
        .and_then(|n| n.try_into().ok())
        .ok_or("unsigned integer out of range".into())
}
fn pair(n: &Node) -> Result<[u32; 2], String> {
    plain(n, 2)?;
    Ok([number(&n.args[0])?, number(&n.args[1])?])
}
fn plain(n: &Node, count: usize) -> Result<(), String> {
    if n.args.len() != count || !n.props.is_empty() || n.body.is_some() {
        Err(format!("'{}' expects {count} arguments", n.name))
    } else {
        Ok(())
    }
}
fn fields(n: &Node, args: Option<usize>, keys: &[&str], block: bool) -> Result<(), String> {
    if args.is_some_and(|count| n.args.len() != count)
        || n.body.is_some() != block
        || n.props.keys().any(|k| !keys.contains(&k.as_str()))
    {
        return Err(format!(
            "invalid arguments, properties or block on '{}'",
            n.name
        ));
    }
    Ok(())
}
fn unique<'a>(seen: &mut BTreeMap<&'a str, ()>, n: &'a Node) -> Result<(), String> {
    if seen.insert(n.name.as_str(), ()).is_some() {
        return Err(format!("duplicate '{}'", n.name));
    }
    Ok(())
}
pub(super) fn decode(text: &str) -> Result<Asset, String> {
    let nodes = syntax::parse(text)?;
    let mut a = Asset {
        format: 0,
        renderer: 0,
        name: String::new(),
        canvas: [0, 0],
        frames: vec![100],
        palette: vec![],
        tags: vec![],
        reference: None,
        layers: vec![],
    };
    let mut seen = BTreeMap::new();
    for n in &nodes {
        if !matches!(n.name.as_str(), "layer" | "tag") {
            unique(&mut seen, n)?;
        }
        match n.name.as_str() {
            "atelier" => {
                fields(n, Some(1), &["format", "renderer"], false)?;
                a.name = strings(&n.args)?[0].clone();
                a.format = number(n.props.get("format").ok_or("missing format")?)?;
                a.renderer = number(n.props.get("renderer").ok_or("missing renderer")?)?;
            }
            "canvas" => a.canvas = pair(n)?,
            "frames" => {
                fields(n, None, &[], false)?;
                a.frames = n.args.iter().map(number).collect::<Result<_, _>>()?;
            }
            "palette" => {
                fields(n, None, &[], false)?;
                a.palette = parse_palette(&n.args)?;
            }
            "reference" => {
                plain(n, 1)?;
                a.reference = Some(strings(&n.args)?[0].clone());
            }
            "tag" => {
                fields(n, Some(1), &["from", "to", "direction"], false)?;
                let mut v: Map<_, _> = n.props.clone().into_iter().collect();
                v.insert("name".into(), n.args[0].clone());
                a.tags
                    .push(serde_json::from_value(Value::Object(v)).map_err(|e| e.to_string())?);
            }
            "layer" => a.layers.push(layer(n, &a.palette)?),
            _ => return Err(format!("unknown document node '{}'", n.name)),
        }
    }
    if !seen.contains_key("atelier") || !seen.contains_key("canvas") {
        return Err("source requires atelier and canvas nodes".into());
    }
    // Palette is document state even if it was declared after a layer. Require
    // settings first so an operation's authoring palette is never ambiguous.
    let first_layer = nodes
        .iter()
        .position(|n| n.name == "layer")
        .unwrap_or(nodes.len());
    if nodes[first_layer..]
        .iter()
        .any(|n| !matches!(n.name.as_str(), "layer" | "tag"))
    {
        return Err("document settings must precede layers".into());
    }
    a.validate()?;
    Ok(a)
}
fn layer(n: &Node, palette: &[String]) -> Result<Layer, String> {
    fields(n, Some(1), &["opacity", "visible", "blend"], true)?;
    let mut props: Map<_, _> = n.props.clone().into_iter().collect();
    props.insert("name".into(), n.args[0].clone());
    let mut l: Layer = serde_json::from_value(Value::Object(props)).map_err(|e| e.to_string())?;
    for n in &n.children {
        if n.name != "frame" {
            return Err(format!("unknown layer node '{}'", n.name));
        }
        fields(n, None, &[], true)?;
        let mut c = Cel {
            frames: n.args.iter().map(number).collect::<Result<_, _>>()?,
            ..Cel::default()
        };
        let mut seen = BTreeMap::new();
        let mut active = palette.to_vec();
        for f in &n.children {
            match f.name.as_str() {
                "size" => {
                    unique(&mut seen, f)?;
                    c.size = Some(pair(f)?);
                }
                "at" => {
                    unique(&mut seen, f)?;
                    plain(f, 2)?;
                    c.at = serde_json::from_value(json!(f.args)).map_err(|e| e.to_string())?;
                }
                "palette" => {
                    fields(f, None, &[], false)?;
                    active = parse_palette(&f.args)?;
                }
                "pixels" => c.steps.push(Step::Pixels(pixels(f)?)),
                _ => c.steps.push(Step::Draw {
                    op: operation(f)?,
                    palette: active.clone(),
                }),
            }
        }
        l.cels.push(c);
    }
    Ok(l)
}
fn pixels(n: &Node) -> Result<Pixels, String> {
    fields(n, Some(0), &["x", "y"], true)?;
    let mut p = Pixels {
        origin: [
            n.props.get("x").map(number).transpose()?.unwrap_or(0),
            n.props.get("y").map(number).transpose()?.unwrap_or(0),
        ],
        ..Pixels::default()
    };
    let mut saw_legend = false;
    for n in &n.children {
        match n.name.as_str() {
            "legend" => {
                fields(n, Some(0), &[], true)?;
                if saw_legend {
                    return Err("duplicate legend".into());
                }
                saw_legend = true;
                for c in &n.children {
                    if c.name != "color" {
                        return Err("legend requires color nodes".into());
                    }
                    plain(c, 2)?;
                    let values = strings(&c.args)?;
                    if p.legend
                        .insert(values[0].clone(), hex(&color(&values[1])?))
                        .is_some()
                    {
                        return Err("duplicate legend symbol".into());
                    }
                }
            }
            "grid" => {
                plain(n, 1)?;
                p.data.push(PixelData::Grid(strings(&n.args)?[0].clone()));
            }
            "row" => {
                fields(n, None, &["repeat"], false)?;
                if n.args.is_empty() || !n.args.len().is_multiple_of(2) {
                    return Err("row expects symbol/count pairs".into());
                }
                let runs = n
                    .args
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|r| {
                        Ok((
                            r[0].as_str().ok_or("row symbol must be quoted")?.to_owned(),
                            number(&r[1])?,
                        ))
                    })
                    .collect::<Result<_, String>>()?;
                p.data.push(PixelData::Row {
                    runs,
                    repeat: n.props.get("repeat").map(number).transpose()?.unwrap_or(1),
                });
            }
            "span" => {
                fields(n, Some(3), &["rows"], false)?;
                p.data.push(PixelData::Span {
                    x: number(&n.args[0])?,
                    y: number(&n.args[1])?,
                    text: n.args[2]
                        .as_str()
                        .ok_or("span requires quoted pixels")?
                        .to_owned(),
                    rows: n.props.get("rows").map(number).transpose()?.unwrap_or(1),
                });
            }
            _ => return Err(format!("unknown pixel node '{}'", n.name)),
        }
    }
    Ok(p)
}
fn value(n: &Node) -> Result<Value, String> {
    if n.body.is_some() {
        if !n.args.is_empty() || !n.props.is_empty() {
            return Err("array block cannot have arguments or properties".into());
        }
        n.children
            .iter()
            .map(|item| {
                if !matches!(item.name.as_str(), "item" | "point" | "stop") {
                    return Err("array requires item, point or stop nodes".into());
                }
                if !item.props.is_empty() {
                    if !item.args.is_empty() {
                        return Err("object item cannot have arguments".into());
                    }
                    let mut obj: Map<_, _> = item.props.clone().into_iter().collect();
                    for field in &item.children {
                        if obj.insert(field.name.clone(), value(field)?).is_some() {
                            return Err("duplicate object field".into());
                        }
                    }
                    Ok(Value::Object(obj))
                } else if item.body.is_some() {
                    value(item)
                } else {
                    Ok(Value::Array(item.args.clone()))
                }
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array)
    } else if !n.props.is_empty() {
        if !n.args.is_empty() {
            return Err("object field cannot have arguments".into());
        }
        Ok(Value::Object(n.props.clone().into_iter().collect()))
    } else {
        Ok(Value::Array(n.args.clone()))
    }
}
fn operation(n: &Node) -> Result<Value, String> {
    if !n.args.is_empty() {
        return Err(format!("operation '{}' takes named properties", n.name));
    }
    let mut op: Map<_, _> = n.props.clone().into_iter().collect();
    if op.contains_key("op") {
        return Err("operation name is the node name".into());
    }
    op.insert("op".into(), Value::String(n.name.clone()));
    for f in &n.children {
        if op.insert(f.name.clone(), value(f)?).is_some() {
            return Err(format!("duplicate operation field '{}'", f.name));
        }
    }
    for key in ["color", "colorize"] {
        if let Some(Value::String(c)) = op.get(key) {
            op.insert(key.into(), json!(color(c)?));
        }
    }
    if let Some(Value::Array(stops)) = op.get_mut("stops") {
        for stop in stops {
            if let Some(Value::String(c)) = stop.get("color") {
                stop["color"] = json!(color(c)?);
            }
        }
    }
    if let Some(Value::Array(colors)) = op.get_mut("colors") {
        for c in colors {
            if let Value::String(text) = c {
                *c = json!(color(text)?);
            }
        }
    }
    Ok(Value::Object(op))
}
