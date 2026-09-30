//! Structured replay: document settings, ordered layers, frame cels and drawing data.
//! File access lives in Studio; this compiler has no store or protocol dependency.
mod evaluate;
mod model;
mod validate;
pub use evaluate::compile;
pub use model::{Asset, Cel, Layer, PixelData, Pixels, Step, color, hex};
pub use validate::MAX_SOURCE_BYTES;
#[cfg(test)]
mod tests;
