//! atelier-mcp: the MCP server that exposes [`atelier_studio::Studio`] as tools.
//!
//! Two transports share one tool router: stdio ([`server::run`]) for clients
//! that spawn the binary, and streamable HTTP ([`server::run_http`]) which also
//! serves the live `/gallery`, `/playground` and `/live` web views.

#![allow(clippy::too_many_arguments)]

pub mod recipe;
pub mod server;
