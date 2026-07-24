//! Compatibility re-export for the recipe contract and compact codec.
//!
//! Recipe persistence belongs beside the Studio store. Keeping the established
//! `atelier_mcp::recipe::{Recipe, Step}` path avoids breaking embedders while
//! the MCP recorder shares the lower-layer encoder.

pub(crate) use atelier_studio::recipe::CompactEncoder;
pub use atelier_studio::recipe::{Recipe, Step};
