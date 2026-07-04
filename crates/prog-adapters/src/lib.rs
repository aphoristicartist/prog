//! Adapter boundary for HTTP, CLI, and MCP sources.
//!
//! Issue #1 only establishes the workspace. Concrete adapters land in the
//! HTTP, CLI, and MCP roadmap issues.

pub mod cli;
pub mod http;

pub const ADAPTERS_PLACEHOLDER: &str = "prog-adapters";
