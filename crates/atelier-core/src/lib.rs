//! atelier-core: the headless pixel-art document model.
//!
//! A [`document::Document`] is an ordered stack of layers over a timeline of
//! frames; cels are the layer-by-frame RGBA images that hold the actual pixels.
//! [`raster`] holds the pixel-level primitives (blending, colour spaces, line/
//! stroke rasterisation, easing/noise) the document and studio layers compose.
//!
//! This crate has no MCP, async, or server dependencies — it is the functional
//! core that `atelier-studio` operates on and `atelier-mcp` exposes.

// Drawing/region ops are inherently coordinate-heavy (layer, frame, x0..y1,
// colour, …); the argument-count lint fights the domain here.
#![allow(clippy::too_many_arguments)]

pub mod document;
pub mod raster;
