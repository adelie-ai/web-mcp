#![deny(warnings)]

// web-mcp binary. mcp-core owns the JSON-RPC protocol, framing, version
// negotiation, and the `serve` CLI (transport/host/port/socket-path, with
// `--mode` accepted as a back-compat alias). This binary contributes only the
// web-specific serve flags, builds a `WebConfig` from them, and hands mcp-core
// a `WebService` to dispatch against.

use clap::Args;
use mcp_core::{ServerConfig, TransportKind};
use web_mcp::config::{DEFAULT_NAV_TIMEOUT_MS, DEFAULT_SEARCH_URL, DEFAULT_USER_AGENT, WebConfig};
use web_mcp::service::WebService;

/// web-mcp's own `serve` flags, flattened by mcp-core alongside its
/// `CommonServeArgs` (transport/host/port/socket-path).
#[derive(Args)]
struct Local {
    /// Search endpoint for web_search (a Mojeek-compatible HTML results URL).
    #[arg(long, env = "WEB_SEARCH_URL", default_value = DEFAULT_SEARCH_URL)]
    search_url: String,
    /// User-Agent for outbound search requests.
    #[arg(long, env = "WEB_USER_AGENT", default_value = DEFAULT_USER_AGENT)]
    user_agent: String,
    /// Path to the Chrome/Chromium executable. When unset, a system install
    /// is auto-detected (google-chrome-stable, chromium, ...).
    #[arg(long, env = "WEB_CHROME_PATH")]
    chrome_path: Option<String>,
    /// Extra argument to pass to Chrome (repeatable). Example:
    /// --chrome-arg=--no-sandbox for restricted/container environments.
    #[arg(long = "chrome-arg")]
    chrome_arg: Vec<String>,
    /// Allow browsing private/loopback/link-local hosts (disables the SSRF
    /// guard). Off by default; enable only for trusted/offline use.
    #[arg(
        long,
        env = "WEB_ALLOW_PRIVATE_HOSTS",
        default_value_t = false,
        num_args = 0..=1,
        default_missing_value = "true"
    )]
    allow_private_hosts: bool,
    /// Navigation timeout in milliseconds for web_read / web_screenshot.
    #[arg(long, env = "WEB_NAV_TIMEOUT_MS", default_value_t = DEFAULT_NAV_TIMEOUT_MS)]
    nav_timeout_ms: u64,
}

#[tokio::main]
async fn main() -> mcp_core::Result<()> {
    // web-mcp speaks stdio in the live config and dropped its bespoke websocket
    // transport (mcp-core's is feature-gated and not enabled here), so only
    // stdio and unix are offered. `--mode stdio` still works as an alias.
    let config = ServerConfig::new("web-mcp", env!("CARGO_PKG_VERSION"))
        .without_websocket()
        .with_unix()
        .default_transport(TransportKind::Stdio);

    mcp_core::run::<Local, _, _, _>(config, |local| async move {
        let web_config = WebConfig {
            search_url: local.search_url,
            user_agent: local.user_agent,
            chrome_executable: local.chrome_path,
            chrome_args: local.chrome_arg,
            allow_private_hosts: local.allow_private_hosts,
            nav_timeout_ms: local.nav_timeout_ms,
        };
        Ok(WebService::with_config(web_config))
    })
    .await
}
