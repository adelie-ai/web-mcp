#![deny(warnings)]
#![recursion_limit = "256"]

// Library crate for web-mcp. Protocol/transport/CLI dispatch is provided by
// `mcp-core`; this crate supplies the domain: config, the SSRF guard, the web
// operations (search + headless-Chrome browsing), and the `McpService` impl.

pub mod config;
pub mod error;
pub mod operations;
pub mod service;
pub mod url_guard;
