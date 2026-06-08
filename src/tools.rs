#![deny(warnings)]

// Tool registry and MCP tool definitions.

use crate::config::WebConfig;
use crate::error::{McpError, Result};
use crate::operations::{browser::BrowserManager, search};
use crate::url_guard::UrlGuard;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::Value;
use std::sync::Arc;

/// Default number of search results.
const DEFAULT_SEARCH_COUNT: u64 = 8;
/// Default cap on extracted page text, in characters.
const DEFAULT_MAX_CHARS: u64 = 50_000;
/// Request timeout for the search HTTP call. Bounds the worst case so a hung
/// search backend can't stall a tool turn indefinitely.
const SEARCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Tool registry: owns the HTTP client, browser manager, SSRF guard, and config
/// and dispatches MCP tool calls.
pub struct ToolRegistry {
    client: reqwest::Client,
    config: Arc<WebConfig>,
    browser: BrowserManager,
    guard: UrlGuard,
}

impl ToolRegistry {
    /// Create a registry using default configuration.
    pub fn new() -> Self {
        Self::with_config(WebConfig::default())
    }

    /// Create a registry with a specific configuration.
    pub fn with_config(config: WebConfig) -> Self {
        let config = Arc::new(config);
        let guard = UrlGuard::new(config.allow_private_hosts);
        let browser = BrowserManager::new(Arc::clone(&config));
        // A bounded-timeout client so a slow/hung search backend can't wedge a
        // turn. `build` only fails if the TLS backend can't initialize; fall
        // back to the default client in that (effectively impossible) case.
        let client = reqwest::Client::builder()
            .timeout(SEARCH_TIMEOUT)
            .build()
            .unwrap_or_default();
        Self {
            client,
            config,
            browser,
            guard,
        }
    }

    /// Get all tools in MCP format.
    pub fn list_tools(&self) -> Value {
        serde_json::json!([
            {
                "name": "web_search",
                "description": "Search the web and return ranked results as a list of {title, url, snippet}. Use this to discover relevant pages; follow up with web_read to fetch a result's content.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "The search query. Plain keywords work best, e.g. 'rust async runtime comparison'."
                        },
                        "count": {
                            "type": "number",
                            "description": "Maximum number of results to return. Range: 1-25 (default: 8)."
                        }
                    },
                    "required": ["query"]
                }
            },
            {
                "name": "web_read",
                "description": "Open a URL in a headless browser (full JavaScript rendering) and return the page's content. Use 'text' format for the rendered, human-readable text (best for reading/summarizing) or 'html' for the raw rendered DOM. Optionally include the page's outbound links. Refuses non-http(s) URLs and, by default, private/loopback/link-local hosts.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "url": {
                            "type": "string",
                            "description": "The absolute http(s) URL to open. Example: 'https://example.com/article'."
                        },
                        "format": {
                            "type": "string",
                            "description": "Output format: 'text' (rendered visible text, default) or 'html' (full rendered DOM).",
                            "enum": ["text", "html"]
                        },
                        "include_links": {
                            "type": "boolean",
                            "description": "If true, also return the page's outbound links as {href, text}. Default: false."
                        },
                        "max_chars": {
                            "type": "number",
                            "description": "Truncate returned content to this many characters (0 = no limit). Default: 50000. A 'truncated' flag indicates if content was cut."
                        }
                    },
                    "required": ["url"]
                }
            },
            {
                "name": "web_screenshot",
                "description": "Open a URL in a headless browser and capture a PNG screenshot, returned as an image. Use to see how a page looks or to capture visual content that text extraction misses. Same URL safety rules as web_read.",
                "inputSchema": {
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
                }
            }
        ])
    }

    /// Execute a tool call by name with the given arguments.
    ///
    /// An unknown tool is a protocol fault and returns `Err` (the caller maps it
    /// to a JSON-RPC error). A failure *within* a known tool (search returned
    /// nothing, navigation timed out, URL blocked, bad arguments) is a tool
    /// result with `isError: true` per the MCP spec — the model should see it as
    /// tool output, not a transport error.
    pub async fn execute_tool(&self, tool_name: &str, arguments: &Value) -> Result<Value> {
        let outcome = match tool_name {
            "web_search" => self.execute_search(arguments).await,
            "web_read" => self.execute_read(arguments).await,
            "web_screenshot" => self.execute_screenshot(arguments).await,
            _ => return Err(McpError::ToolNotFound(tool_name.to_string()).into()),
        };
        Ok(outcome.unwrap_or_else(|e| mcp_tool_error(&e.to_string())))
    }

    async fn execute_search(&self, args: &Value) -> Result<Value> {
        let query = require_str(args, "query")?;
        let count = get_u64(args, "count").unwrap_or(DEFAULT_SEARCH_COUNT) as usize;

        let result = search::search(&self.client, &self.config, query, count).await?;
        Ok(mcp_tool_result_json(result))
    }

    async fn execute_read(&self, args: &Value) -> Result<Value> {
        let raw_url = require_str(args, "url")?;
        let url = self.guard.check(raw_url).await?;
        let format = get_str(args, "format").unwrap_or("text");
        let include_links = get_bool(args, "include_links").unwrap_or(false);
        let max_chars = get_u64(args, "max_chars").unwrap_or(DEFAULT_MAX_CHARS) as usize;

        let result = self
            .browser
            .read(&url, format, include_links, max_chars)
            .await?;
        Ok(mcp_tool_result_json(result))
    }

    async fn execute_screenshot(&self, args: &Value) -> Result<Value> {
        let raw_url = require_str(args, "url")?;
        let url = self.guard.check(raw_url).await?;
        let full_page = get_bool(args, "full_page").unwrap_or(false);

        let png = self.browser.screenshot(&url, full_page).await?;
        Ok(mcp_tool_result_image(&png))
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Require a non-empty string argument.
fn require_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            McpError::InvalidToolParameters(format!("Missing required parameter: {}", key)).into()
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

/// Wrap a JSON value in the MCP tool-result envelope.
///
/// MCP's content union has no `json` member, so the payload is serialized into a
/// `text` block (readable by every client) and also surfaced as
/// `structuredContent` for clients that consume typed tool output.
fn mcp_tool_result_json(value: Value) -> Value {
    let text = serde_json::to_string(&value).unwrap_or_else(|_| "null".to_string());
    serde_json::json!({
        "content": [ { "type": "text", "text": text } ],
        "structuredContent": value,
    })
}

/// Build a failed tool-result (`isError: true`) carrying the error text. Tool
/// execution failures are reported this way rather than as JSON-RPC errors.
fn mcp_tool_error(message: &str) -> Value {
    serde_json::json!({
        "content": [ { "type": "text", "text": message } ],
        "isError": true,
    })
}

/// Wrap PNG bytes in the MCP image content envelope (base64-encoded).
fn mcp_tool_result_image(png: &[u8]) -> Value {
    serde_json::json!({
        "content": [ {
            "type": "image",
            "data": BASE64.encode(png),
            "mimeType": "image/png",
        } ]
    })
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
    fn list_tools_exposes_three_tools() {
        let names: Vec<String> = ToolRegistry::new()
            .list_tools()
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str).map(str::to_string))
            .collect();
        for expected in ["web_search", "web_read", "web_screenshot"] {
            assert!(names.iter().any(|n| n == expected), "missing {expected}");
        }
    }

    #[test]
    fn image_envelope_is_base64_png() {
        let env = mcp_tool_result_image(&[0x89, 0x50, 0x4e, 0x47]);
        let entry = &env["content"][0];
        assert_eq!(entry["type"], json!("image"));
        assert_eq!(entry["mimeType"], json!("image/png"));
        assert_eq!(entry["data"], json!("iVBORw=="));
    }

    #[test]
    fn json_envelope_uses_text_content_and_structured() {
        let payload = json!({ "a": 1, "b": ["x", "y"] });
        let env = mcp_tool_result_json(payload.clone());
        let entry = &env["content"][0];
        // Standard MCP content type, not the bespoke "json".
        assert_eq!(entry["type"], json!("text"));
        let text = entry["text"].as_str().expect("text content");
        assert_eq!(
            serde_json::from_str::<Value>(text).expect("text is valid json"),
            payload
        );
        // Typed clients get the structured form.
        assert_eq!(env["structuredContent"], payload);
        assert!(env.get("isError").is_none());
    }

    #[test]
    fn error_envelope_sets_is_error_flag() {
        let env = mcp_tool_error("navigation timed out");
        assert_eq!(env["isError"], json!(true));
        assert_eq!(env["content"][0]["type"], json!("text"));
        assert_eq!(env["content"][0]["text"], json!("navigation timed out"));
    }

    #[tokio::test]
    async fn unknown_tool_is_protocol_error() {
        let reg = ToolRegistry::new();
        let res = reg.execute_tool("does_not_exist", &json!({})).await;
        assert!(res.is_err(), "unknown tool must surface as Err (JSON-RPC)");
    }

    #[tokio::test]
    async fn blocked_url_is_tool_error_result_not_err() {
        // A guard-blocked URL is a tool-level failure: execute_tool returns Ok
        // with an isError result, never an Err. No browser/network is reached
        // because the guard refuses localhost before any launch.
        let reg = ToolRegistry::new();
        let res = reg
            .execute_tool("web_read", &json!({ "url": "http://127.0.0.1/" }))
            .await
            .expect("blocked URL is a tool result, not a protocol error");
        assert_eq!(res["isError"], json!(true));
        let text = res["content"][0]["text"]
            .as_str()
            .expect("error text")
            .to_lowercase();
        assert!(
            text.contains("private") || text.contains("refused"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn missing_required_param_is_tool_error_result() {
        let reg = ToolRegistry::new();
        let res = reg
            .execute_tool("web_read", &json!({}))
            .await
            .expect("missing param is a tool result");
        assert_eq!(res["isError"], json!(true));
    }
}
