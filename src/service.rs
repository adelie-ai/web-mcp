#![deny(warnings)]

// The `McpService` implementation: web_read / web_screenshot.
//
// mcp-core owns the JSON-RPC protocol, framing, version negotiation, and the
// `serve` CLI. This module only describes the tools and executes them, holding
// the long-lived state (headless-Chrome handle, SSRF guard) that the tools need.
//
// There is intentionally no `web_search`: every keyless search-engine results
// page blocks automated/datacenter access (403 / CAPTCHA / "anomaly" challenge)
// even through the real headless browser, so a `web_search` tool here would
// always fail. Discovery is instead done by pointing `web_read` at a search
// engine's results URL — see the `web_read` tool description.

use crate::config::WebConfig;
use crate::error::{WebError, WebMcpError};
use crate::operations::browser::BrowserManager;
use crate::url_guard::UrlGuard;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use mcp_core::{CallError, Content, McpService, ToolDef, ToolReply, async_trait};
use serde_json::{Value, json};
use std::sync::Arc;

/// Default cap on extracted page text, in characters.
const DEFAULT_MAX_CHARS: u64 = 50_000;

/// The web-mcp service: owns the browser manager and SSRF guard, and implements
/// [`McpService`] for mcp-core to dispatch against. All web access goes through
/// the headless browser.
pub struct WebService {
    browser: BrowserManager,
    guard: UrlGuard,
}

impl WebService {
    /// Create a service using default configuration.
    pub fn new() -> Self {
        Self::with_config(WebConfig::default())
    }

    /// Create a service with a specific configuration.
    pub fn with_config(config: WebConfig) -> Self {
        let config = Arc::new(config);
        let guard = UrlGuard::new(config.allow_private_hosts);
        let browser = BrowserManager::new(Arc::clone(&config));
        Self { browser, guard }
    }

    async fn execute_read(&self, args: &Value) -> Result<ToolReply, WebMcpError> {
        let raw_url = require_str(args, "url")?;
        let url = self.guard.check(raw_url).await?;
        let format = get_str(args, "format").unwrap_or("text");
        let include_links = get_bool(args, "include_links").unwrap_or(false);
        let max_chars = get_u64(args, "max_chars").unwrap_or(DEFAULT_MAX_CHARS) as usize;

        let result = self
            .browser
            .read(&url, format, include_links, max_chars)
            .await?;
        Ok(ToolReply::json(&result)?)
    }

    async fn execute_screenshot(&self, args: &Value) -> Result<ToolReply, WebMcpError> {
        let raw_url = require_str(args, "url")?;
        let url = self.guard.check(raw_url).await?;
        let full_page = get_bool(args, "full_page").unwrap_or(false);

        let png = self.browser.screenshot(&url, full_page).await?;
        Ok(image_reply(&png))
    }
}

impl Default for WebService {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl McpService for WebService {
    fn tools(&self) -> Vec<ToolDef> {
        vec![
            ToolDef::new(
                "web_read",
                "Open a URL in a headless browser (full JavaScript rendering) and return the page's content. Use 'text' format for the rendered, human-readable text (best for reading/summarizing) or 'html' for the raw rendered DOM. Set include_links=true to get the page's outbound links as {href, text}.\n\nDISCOVERY (there is no separate search tool): to find pages, point this tool at a search engine's results URL with include_links=true, then read the page's text and follow the relevant result links. Build the URL by URL-encoding your query (spaces as '+'), e.g. 'https://www.bing.com/search?q=YOUR+QUERY' or 'https://duckduckgo.com/html/?q=YOUR+QUERY'. If one engine returns a bot-challenge / few links (some block automated access), try another engine or read the result page's visible text for leads, then web_read the destination URLs you find. Prefer navigating directly to a known authoritative URL when you already know one.\n\nRefuses non-http(s) URLs and, by default, private/loopback/link-local hosts.",
                json!({
                    "type": "object",
                    "properties": {
                        "url": {
                            "type": "string",
                            "description": "The absolute http(s) URL to open. To read an article: 'https://example.com/article'. To search: a results URL like 'https://duckduckgo.com/html/?q=rust+async+runtime' (then use include_links to harvest result links)."
                        },
                        "format": {
                            "type": "string",
                            "description": "Output format: 'text' (rendered visible text, default) or 'html' (full rendered DOM).",
                            "enum": ["text", "html"]
                        },
                        "include_links": {
                            "type": "boolean",
                            "description": "If true, also return the page's outbound links as {href, text}. Default: false. Set this when reading a search-engine results page so you can follow the result links."
                        },
                        "max_chars": {
                            "type": "number",
                            "description": "Truncate returned content to this many characters (0 = no limit). Default: 50000. A 'truncated' flag indicates if content was cut."
                        }
                    },
                    "required": ["url"]
                }),
            ),
            ToolDef::new(
                "web_screenshot",
                "Open a URL in a headless browser and capture a PNG screenshot, returned as an image. Use to see how a page looks or to capture visual content that text extraction misses. Same URL safety rules as web_read.",
                json!({
                    "type": "object",
                    "properties": {
                        "url": {
                            "type": "string",
                            "description": "The absolute http(s) URL to capture. Example: 'https://example.com'."
                        },
                        "full_page": {
                            "type": "boolean",
                            "description": "If true, capture the entire scrollable page; if false (default), capture just the viewport."
                        }
                    },
                    "required": ["url"]
                }),
            ),
        ]
    }

    /// Execute a tool call.
    ///
    /// Bad arguments, a guard-blocked URL, or a navigation failure are
    /// tool-level failures: they come back as `CallError::Tool`, which mcp-core
    /// surfaces as `isError: true` content the model can react to — not as a
    /// JSON-RPC protocol error. An unknown tool name is likewise a
    /// `CallError::Tool` per the MCP spec.
    async fn call_tool(&self, name: &str, arguments: &Value) -> Result<ToolReply, CallError> {
        let outcome = match name {
            "web_read" => self.execute_read(arguments).await,
            "web_screenshot" => self.execute_screenshot(arguments).await,
            other => return Err(CallError::tool(format!("Tool not found: {other}"))),
        };
        outcome.map_err(|e| CallError::tool(e.to_string()))
    }
}

/// Require a non-empty string argument.
fn require_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, WebMcpError> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            WebError::InvalidParameters(format!("Missing required parameter: {key}")).into()
        })
}

/// Optional string argument.
fn get_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

/// Optional boolean argument.
fn get_bool(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(Value::as_bool)
}

/// Optional unsigned-integer argument, accepting JSON numbers and numeric strings.
fn get_u64(args: &Value, key: &str) -> Option<u64> {
    let v = args.get(key)?;
    v.as_u64()
        .or_else(|| v.as_f64().map(|f| f.max(0.0) as u64))
        .or_else(|| v.as_str()?.parse::<u64>().ok())
}

/// Build a [`ToolReply`] carrying PNG bytes as an MCP `image` content block
/// (base64-encoded). MCP's text/structured helpers don't cover images, so this
/// uses the raw-content escape hatch.
fn image_reply(png: &[u8]) -> ToolReply {
    let block = json!({
        "type": "image",
        "data": BASE64.encode(png),
        "mimeType": "image/png",
    });
    ToolReply::text("").with_content(vec![Content::Raw(block)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn require_str_rejects_empty_and_missing() {
        assert!(require_str(&json!({ "url": "" }), "url").is_err());
        assert!(require_str(&json!({}), "url").is_err());
        assert_eq!(require_str(&json!({ "url": "x" }), "url").unwrap(), "x");
    }

    #[test]
    fn tools_exposes_browser_tools_and_no_search() {
        let names: Vec<String> = WebService::new()
            .tools()
            .into_iter()
            .map(|t| t.name)
            .collect();
        for expected in ["web_read", "web_screenshot"] {
            assert!(names.iter().any(|n| n == expected), "missing {expected}");
        }
        // web_search was removed: keyless results pages all block automation, so
        // a search tool here would always fail. Discovery goes via web_read.
        assert!(
            !names.iter().any(|n| n == "web_search"),
            "web_search must not be exposed"
        );
    }

    #[test]
    fn image_reply_is_base64_png() {
        let reply = image_reply(&[0x89, 0x50, 0x4e, 0x47]);
        assert!(!reply.is_error);
        assert_eq!(reply.content.len(), 1);
        // The PNG is carried as a raw MCP `image` content block.
        let Content::Raw(block) = &reply.content[0] else {
            panic!("expected a raw image content block");
        };
        assert_eq!(block["type"], json!("image"));
        assert_eq!(block["mimeType"], json!("image/png"));
        assert_eq!(block["data"], json!("iVBORw=="));
    }

    #[test]
    fn json_reply_uses_text_content_and_structured() {
        let payload = json!({ "a": 1, "b": ["x", "y"] });
        let reply = ToolReply::json(&payload).expect("json reply");
        assert!(!reply.is_error);
        // Standard MCP text content, not the bespoke "json" type.
        let Content::Text(text) = &reply.content[0] else {
            panic!("expected a text content block");
        };
        assert_eq!(
            serde_json::from_str::<Value>(text).expect("text is valid json"),
            payload
        );
        // Typed clients get the structured form.
        assert_eq!(reply.structured_content, Some(payload));
    }

    #[tokio::test]
    async fn unknown_tool_is_tool_error() {
        let svc = WebService::new();
        match svc.call_tool("does_not_exist", &json!({})).await {
            Err(CallError::Tool(msg)) => assert!(msg.to_lowercase().contains("not found"), "{msg}"),
            other => panic!("expected CallError::Tool, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn blocked_url_is_tool_error() {
        // A guard-blocked URL is a tool-level failure: call_tool returns
        // CallError::Tool (mcp-core renders it as isError content), never a
        // protocol error. No browser/network is reached because the guard
        // refuses localhost before any launch.
        let svc = WebService::new();
        match svc
            .call_tool("web_read", &json!({ "url": "http://127.0.0.1/" }))
            .await
        {
            Err(CallError::Tool(msg)) => {
                let lower = msg.to_lowercase();
                assert!(
                    lower.contains("private") || lower.contains("refused"),
                    "{msg}"
                );
            }
            other => panic!("expected CallError::Tool, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn missing_required_param_is_tool_error() {
        let svc = WebService::new();
        match svc.call_tool("web_read", &json!({})).await {
            Err(CallError::Tool(_)) => {}
            other => panic!("expected CallError::Tool, got {other:?}"),
        }
    }
}
