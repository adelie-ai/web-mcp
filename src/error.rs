#![deny(warnings)]

// Domain error types for the web-mcp crate.
//
// Protocol/transport concerns (JSON-RPC dispatch, framing, version negotiation)
// now live in `mcp-core`; these errors describe only what can go wrong inside
// the web operations (search, browse) and the SSRF guard. The service layer
// maps them onto `mcp_core::CallError` / `ToolReply` at the boundary.

use thiserror::Error;

/// Error type for the web operations (search / browse) and SSRF guard.
#[derive(Error, Debug)]
pub enum WebMcpError {
    /// Web operation errors (search / browse).
    #[error("Web error: {0}")]
    Web(#[from] WebError),

    /// JSON serialization/deserialization errors.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// IO errors.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// HTTP client errors (search backend).
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// Headless-browser (Chrome DevTools Protocol) errors.
    #[error("Browser error: {0}")]
    Browser(#[from] chromiumoxide::error::CdpError),
}

/// Web operation errors.
///
/// Structured so callers can distinguish a refused (guard-blocked) URL from a
/// genuine navigation failure or an empty search.
#[derive(Error, Debug)]
pub enum WebError {
    /// The search backend returned an error or unparseable response.
    #[error("Search failed: {0}")]
    SearchFailed(String),

    /// A URL was refused by the SSRF guard (private/loopback/link-local host).
    #[error("Refused to fetch URL: {0}")]
    Blocked(String),

    /// The caller supplied invalid parameters.
    #[error("Invalid parameters: {0}")]
    InvalidParameters(String),

    /// Navigation or content extraction failed.
    #[error("Navigation failed: {0}")]
    Navigation(String),

    /// An operation exceeded its time budget.
    #[error("Timed out: {0}")]
    Timeout(String),
}

/// Result type alias for convenience.
pub type Result<T> = std::result::Result<T, WebMcpError>;
