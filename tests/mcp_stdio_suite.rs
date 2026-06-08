#![deny(warnings)]

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};

// ── MCP stdio harness ─────────────────────────────────────────────────────────

struct McpStdioClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    next_id: u64,
}

impl McpStdioClient {
    fn start() -> Self {
        let exe = env!("CARGO_BIN_EXE_web-mcp");

        let mut child = Command::new(exe)
            .args(["serve", "--mode", "stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn web-mcp serve --mode stdio");

        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");

        Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        }
    }

    fn send(&mut self, obj: &Value) {
        let s = serde_json::to_string(obj).expect("serialize jsonrpc");
        self.stdin
            .write_all(s.as_bytes())
            .and_then(|_| self.stdin.write_all(b"\n"))
            .and_then(|_| self.stdin.flush())
            .expect("write jsonrpc line");
    }

    fn read_msg(&mut self) -> Value {
        let mut line = String::new();
        loop {
            line.clear();
            let n = self.stdout.read_line(&mut line).expect("read line");
            if n == 0 {
                panic!("mcp server closed stdout");
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
                return v;
            }
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;

        self.send(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));

        loop {
            let msg = self.read_msg();
            if msg.get("id").and_then(|v| v.as_u64()) != Some(id) {
                continue;
            }
            if let Some(err) = msg.get("error") {
                return Err(err.to_string());
            }
            return Ok(msg);
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({"jsonrpc":"2.0","method":method,"params":params}));
    }

    fn initialize(&mut self) {
        self.call(
            "initialize",
            json!({"protocolVersion":"2025-11-25","capabilities":{}}),
        )
        .expect("initialize");
        self.notify("initialized", json!({}));
    }

    fn tool_call(&mut self, name: &str, arguments: Value) -> Result<Value, String> {
        let resp = self.call("tools/call", json!({"name":name,"arguments":arguments}))?;
        resp.get("result")
            .cloned()
            .ok_or_else(|| format!("missing result field: {resp}"))
    }
}

impl Drop for McpStdioClient {
    fn drop(&mut self) {
        let _ = self.call("shutdown", json!({}));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Extract the structured payload from a successful tool result.
///
/// Prefers `structuredContent`; falls back to parsing the first `text` content
/// entry as JSON. (The payload is carried as a `text` block per MCP, with a
/// `structuredContent` mirror for typed clients.)
fn extract_json(tool_result: &Value) -> Value {
    assert_ne!(
        tool_result.get("isError"),
        Some(&Value::Bool(true)),
        "expected a successful tool result, got isError: {tool_result}"
    );
    if let Some(structured) = tool_result.get("structuredContent") {
        return structured.clone();
    }
    let content = tool_result
        .get("content")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("expected result.content array, got: {tool_result}"));
    for entry in content {
        if entry.get("type") == Some(&Value::String("text".to_string()))
            && let Some(text) = entry.get("text").and_then(|v| v.as_str())
            && let Ok(v) = serde_json::from_str::<Value>(text)
        {
            return v;
        }
    }
    panic!("no parseable text content entry in: {tool_result}");
}

/// Assert a tool result is a failure (`isError: true`) whose text contains
/// `needle` (case-insensitive). Tool-execution failures (bad params, blocked
/// URL, navigation error) are reported as `isError` results, not JSON-RPC
/// errors.
fn expect_tool_error_contains(result: &Value, needle: &str) {
    assert_eq!(
        result.get("isError"),
        Some(&Value::Bool(true)),
        "expected isError result, got: {result}"
    );
    let text = result
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|c| c.first())
        .and_then(|e| e.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("");
    assert!(
        text.to_lowercase().contains(&needle.to_lowercase()),
        "expected error text containing '{needle}', got: {text}"
    );
}

fn network_tests_enabled() -> bool {
    std::env::var("RUN_NETWORK_TESTS").ok().as_deref() == Some("1")
}

fn expect_err_contains<T>(res: Result<T, String>, needle: &str) {
    match res {
        Ok(_) => panic!("expected error containing '{needle}', but call succeeded"),
        Err(e) => {
            let lower = e.to_lowercase();
            assert!(
                lower.contains(&needle.to_lowercase()),
                "expected error containing '{needle}', got: {e}"
            );
        }
    }
}

// ── Protocol tests (no network) ───────────────────────────────────────────────

#[test]
fn test_initialize_response_shape() {
    let mut client = McpStdioClient::start();
    let resp = client
        .call(
            "initialize",
            json!({"protocolVersion":"2025-11-25","capabilities":{}}),
        )
        .expect("initialize");
    let result = resp.get("result").expect("result field");
    assert_eq!(
        result
            .get("serverInfo")
            .and_then(|s| s.get("name"))
            .and_then(|v| v.as_str()),
        Some("web-mcp")
    );
}

#[test]
fn test_tools_list_contains_expected_tools() {
    let mut client = McpStdioClient::start();
    client.initialize();
    let resp = client.call("tools/list", json!({})).expect("tools/list");
    // mcp-core returns `tools` as a flat array of tool objects.
    let tools = resp["result"]["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(|v| v.as_str()))
        .collect();
    for expected in ["web_search", "web_read", "web_screenshot"] {
        assert!(names.contains(&expected), "missing {expected}: {names:?}");
    }
}

#[test]
fn test_initialize_has_no_top_level_tools_key() {
    // mcp-core advertises tools via tools/list, not the initialize result; the
    // initialize result must not leak a non-standard top-level `tools` key.
    let mut client = McpStdioClient::start();
    let resp = client
        .call(
            "initialize",
            json!({"protocolVersion":"2025-06-18","capabilities":{}}),
        )
        .expect("initialize");
    assert!(
        resp["result"].get("tools").is_none(),
        "initialize must not embed a top-level tools key: {resp}"
    );
}

#[test]
fn test_tool_call_before_initialize_returns_error() {
    // tools/call before the initialize handshake is a protocol fault, surfaced
    // by mcp-core as a JSON-RPC "not initialized" error (not an isError result).
    let mut client = McpStdioClient::start();
    let result = client.tool_call("web_search", json!({"query": "rust"}));
    expect_err_contains(result, "not initialized");
}

#[test]
fn test_unknown_tool_is_tool_error_result() {
    // Per the MCP spec (and mcp-core), an unknown tool name is a tool-level
    // failure surfaced as an `isError: true` result, not a JSON-RPC error.
    let mut client = McpStdioClient::start();
    client.initialize();
    let res = client.tool_call("nope", json!({})).expect("result");
    expect_tool_error_contains(&res, "not found");
}

// ── Parameter validation / guard tests (no network) ──────────────────────────
//
// These are tool-execution failures: per the MCP spec they come back as a
// successful tools/call result carrying `isError: true`, not a JSON-RPC error.

#[test]
fn test_search_missing_query() {
    let mut client = McpStdioClient::start();
    client.initialize();
    let res = client.tool_call("web_search", json!({})).expect("result");
    expect_tool_error_contains(&res, "query");
}

#[test]
fn test_read_missing_url() {
    let mut client = McpStdioClient::start();
    client.initialize();
    let res = client.tool_call("web_read", json!({})).expect("result");
    expect_tool_error_contains(&res, "url");
}

#[test]
fn test_read_rejects_non_http_scheme() {
    let mut client = McpStdioClient::start();
    client.initialize();
    let res = client
        .tool_call("web_read", json!({"url": "file:///etc/passwd"}))
        .expect("result");
    expect_tool_error_contains(&res, "scheme");
}

#[test]
fn test_read_blocks_loopback_ssrf() {
    let mut client = McpStdioClient::start();
    client.initialize();
    // Guard must refuse before any browser launch — this returns fast.
    let res = client
        .tool_call(
            "web_read",
            json!({"url": "http://169.254.169.254/latest/meta-data/"}),
        )
        .expect("result");
    expect_tool_error_contains(&res, "private");
    let res = client
        .tool_call("web_read", json!({"url": "http://localhost:8080/"}))
        .expect("result");
    expect_tool_error_contains(&res, "local");
}

#[test]
fn test_screenshot_blocks_loopback_ssrf() {
    let mut client = McpStdioClient::start();
    client.initialize();
    let res = client
        .tool_call("web_screenshot", json!({"url": "http://127.0.0.1/"}))
        .expect("result");
    expect_tool_error_contains(&res, "private");
}

// ── Network integration tests (require RUN_NETWORK_TESTS=1) ──────────────────

#[test]
fn test_search_network() {
    if !network_tests_enabled() {
        eprintln!("Skipping network test (set RUN_NETWORK_TESTS=1 to enable)");
        return;
    }
    let mut client = McpStdioClient::start();
    client.initialize();
    let result = client
        .tool_call(
            "web_search",
            json!({"query": "rust programming language", "count": 5}),
        )
        .expect("web_search");
    let arr = extract_json(&result);
    let arr = arr.as_array().expect("array of results");
    assert!(!arr.is_empty(), "expected search results");
    assert!(arr[0].get("title").and_then(|v| v.as_str()).is_some());
    assert!(
        arr[0]
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .starts_with("http")
    );
}

#[test]
fn test_read_example_com_network() {
    if !network_tests_enabled() {
        eprintln!("Skipping network test (set RUN_NETWORK_TESTS=1 to enable)");
        return;
    }
    let mut client = McpStdioClient::start();
    client.initialize();
    let result = client
        .tool_call(
            "web_read",
            json!({"url": "https://example.com", "include_links": true}),
        )
        .expect("web_read example.com");
    let page = extract_json(&result);
    assert!(
        page["title"]
            .as_str()
            .unwrap_or("")
            .contains("Example Domain"),
        "unexpected title: {}",
        page["title"]
    );
    // Assert on the rendered body text without pinning to exact wording (the
    // example.com copy changes); "domain" is stable in the visible text.
    assert!(
        page["content"]
            .as_str()
            .unwrap_or("")
            .to_lowercase()
            .contains("domain"),
        "rendered content missing expected text: {:?}",
        page["content"]
    );
    assert!(page["links"].is_array());
}

#[test]
fn test_screenshot_example_com_network() {
    if !network_tests_enabled() {
        eprintln!("Skipping network test (set RUN_NETWORK_TESTS=1 to enable)");
        return;
    }
    let mut client = McpStdioClient::start();
    client.initialize();
    let result = client
        .tool_call("web_screenshot", json!({"url": "https://example.com"}))
        .expect("web_screenshot");
    let entry = &result["content"][0];
    assert_eq!(entry["type"], json!("image"));
    assert_eq!(entry["mimeType"], json!("image/png"));
    let data = entry["data"].as_str().expect("base64 data");
    assert!(data.len() > 100, "screenshot data implausibly small");
}
