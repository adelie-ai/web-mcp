#![deny(warnings)]

// web-mcp binary. mcp-core owns the JSON-RPC protocol, framing, version
// negotiation, and the `serve` CLI (transport/host/port/socket-path, with
// `--mode` accepted as a back-compat alias). This binary contributes only the
// web-specific serve flags, builds a `WebConfig` from them, and hands mcp-core
// a `WebService` to dispatch against.

use clap::Args;
use web_mcp::config::{DEFAULT_NAV_TIMEOUT_MS, WebConfig};
use web_mcp::{WebService, server_config};

/// web-mcp's own `serve` flags, flattened by mcp-core alongside its
/// `CommonServeArgs` (transport/host/port/socket-path).
#[derive(Args)]
struct Local {
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
    // stdio and unix are offered. `--mode stdio` still works as an alias. The
    // config - identity, transports, and the model-facing `instructions` blurb -
    // is built by `server_config` in the library so it stays unit-testable.
    let config = server_config();

    mcp_core::run::<Local, _, _, _>(config, |local| async move {
        // The zero-config default is `web_mcp::build_service()` (used for
        // in-process hosting); the binary layers its serve flags/env onto the
        // same `WebService::with_config` constructor, so both share one
        // construction path with no default drift.
        let web_config = WebConfig {
            chrome_executable: local.chrome_path,
            chrome_args: local.chrome_arg,
            allow_private_hosts: local.allow_private_hosts,
            nav_timeout_ms: local.nav_timeout_ms,
        };
        Ok(WebService::with_config(web_config))
    })
    .await
}
