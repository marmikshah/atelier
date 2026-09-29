//! Retained artwork: named parts, exact pixel resources and ordered composition.
//!
//! Parsing and file access live in Studio. This module accepts decoded resources
//! and builds a document without a store, a journal, or a protocol server.

mod evaluate;
mod model;
mod validate;

pub use evaluate::compile;
pub use model::{Asset, Cel, Instance, Layer, Part, Placement, color, hex};
pub use validate::{MAX_PARTS, MAX_SOURCE_BYTES};

#[cfg(test)]
mod tests;
