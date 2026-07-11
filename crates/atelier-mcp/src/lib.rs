//! atelier-mcp: the MCP server that exposes [`atelier_studio::Studio`] as tools.
//!
//! Two transports share one tool router: stdio ([`server::run`]) for clients
//! that spawn the binary, and streamable HTTP ([`server::run_http`]) mounted at
//! `/mcp`.

#![allow(clippy::too_many_arguments)]

pub mod recipe;
pub mod server;
