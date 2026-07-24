// --- resources -------------------------------------------------------------

/// Scale used when rendering the `render` resource (matches the doc_look default).
pub(crate) const RESOURCE_RENDER_SCALE: u32 = 4;

/// A parsed `atelier://` resource URI: which document, and which view of it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ResourceTarget {
    /// `atelier://doc/<id>` — the document structure JSON.
    Structure(String),
    /// `atelier://doc/<id>/render` — frame 0 PNG render (blob).
    Render(String),
}

/// Parse an `atelier://doc/<id>` or `.../render` URI into a [`ResourceTarget`].
/// Returns None for any other scheme/shape (the caller maps that to an error).
pub(crate) fn parse_resource_uri(uri: &str) -> Option<ResourceTarget> {
    let rest = uri.strip_prefix("atelier://doc/")?;
    match rest.strip_suffix("/render") {
        Some(id) if !id.is_empty() => Some(ResourceTarget::Render(id.to_string())),
        Some(_) => None, // "atelier://doc//render"
        None if !rest.is_empty() && !rest.contains('/') => {
            Some(ResourceTarget::Structure(rest.to_string()))
        }
        None => None, // empty id, or extra path segments
    }
}

/// Standard base64 (no line wrapping) — the blob field wants pre-encoded text and
/// we'd rather not pull in a crate for ~15 lines.
pub(crate) fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18 & 63) as usize] as char);
        out.push(T[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}
