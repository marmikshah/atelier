//! atelier: a headless pixel-art editor exposed as an MCP server.
//!
//! A document is an ordered stack of layers over a timeline of frames; cels are
//! the layer-by-frame RGBA images that hold the actual pixels. Agents paint cels
//! with drawing primitives, render a PNG to *look* at the canvas, iterate on what
//! they see, and export the result (PNG / spritesheet / GIF).

// Drawing/region ops are inherently coordinate-heavy (layer, frame, x0..y1,
// colour, …); the argument-count lint fights the domain here.
#![allow(clippy::too_many_arguments)]

pub mod document;
pub mod raster;
pub mod replay;
pub mod server;
pub mod service;
pub mod studio;
