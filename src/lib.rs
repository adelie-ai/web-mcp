#![deny(warnings)]
#![recursion_limit = "256"]

// Library crate for web-mcp. Protocol/transport/CLI dispatch is provided by
// `mcp-core`; this crate supplies the domain: config, the SSRF guard, the web
// operations (headless-Chrome browsing), and the `McpService` impl.

pub mod config;
pub mod error;
pub mod operations;
pub mod service;
pub mod url_guard;

#[cfg(test)]
mod build_service_tests {
    use super::*;
    use mcp_core::{CallError, McpService};
    use serde_json::json;

    #[test]
    fn build_service_exposes_default_browser_tools() {
        // build_service() yields the same tool surface as the binary's zero-flag
        // default: the two browser tools, and deliberately no web_search.
        let names: Vec<String> = build_service()
            .tools()
            .into_iter()
            .map(|t| t.name)
            .collect();
        for expected in ["web_read", "web_screenshot"] {
            assert!(names.iter().any(|n| n == expected), "missing {expected}");
        }
        assert!(
            !names.iter().any(|n| n == "web_search"),
            "web_search must not be exposed"
        );
    }

    #[tokio::test]
    async fn build_service_defaults_keep_ssrf_guard_on() {
        // The built-in default is the safe one: private/loopback hosts are
        // refused (SSRF guard on), exactly as `web-mcp serve` with no flags. No
        // browser launches - the guard refuses before any navigation.
        let svc = build_service();
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

    #[test]
    fn server_config_reachable_at_crate_root() {
        // server_config() is reachable from the crate root and keeps the web-mcp
        // identity plus a non-empty model-facing instructions blurb.
        let cfg = server_config();
        assert_eq!(cfg.name, "web-mcp");
        let instructions = cfg.instructions.expect("advertises MCP instructions");
        assert!(!instructions.trim().is_empty());
    }
}
