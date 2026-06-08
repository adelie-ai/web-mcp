#![deny(warnings)]

// Error types for the web-mcp crate

use thiserror::Error;

/// Main error type for the web-mcp application.
#[derive(Error, Debug)]
pub enum WebMcpError {
    /// Web operation errors (search / browse).
    #[error("Web error: {0}")]
    Web(#[from] WebError),

    /// JSON serialization/deserialization errors.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// MCP protocol errors.
    #[error("MCP protocol error: {0}")]
    Mcp(#[from] McpError),

    /// Transport layer errors.
    #[error("Transport error: {0}")]
    Transport(#[from] TransportError),

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

/// MCP protocol errors.
#[derive(Error, Debug)]
pub enum McpError {
    /// Invalid protocol version.
    #[error("Unsupported protocol version: {0}")]
    InvalidProtocolVersion(String),

    /// Invalid JSON-RPC message.
    #[error("Invalid JSON-RPC message: {0}")]
    InvalidJsonRpc(String),

    /// Tool not found.
    #[error("Tool not found: {0}")]
    ToolNotFound(String),

    /// Invalid tool parameters.
    #[error("Invalid tool parameters: {0}")]
    InvalidToolParameters(String),
}

/// Transport layer errors.
#[derive(Error, Debug)]
pub enum TransportError {
    /// WebSocket connection error.
    #[error("WebSocket connection error: {0}")]
    WebSocket(String),

    /// Invalid message format.
    #[error("Invalid message format: {0}")]
    InvalidMessage(String),

    /// Connection closed.
    #[error("Connection closed")]
    ConnectionClosed,

    /// IO error in transport.
    #[error("Transport IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Result type alias for convenience.
pub type Result<T> = std::result::Result<T, WebMcpError>;
