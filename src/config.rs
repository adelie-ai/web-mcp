#![deny(warnings)]

// Runtime configuration for web-mcp: the headless-Chrome executable/arguments,
// the navigation timeout, and the SSRF policy. (There is no search endpoint:
// `web_search` was removed because keyless results pages all block automated
// access even through the real browser — see `service.rs` / `operations/mod.rs`.)

/// Default navigation timeout (milliseconds) for `web_read` / `web_screenshot`.
pub const DEFAULT_NAV_TIMEOUT_MS: u64 = 30_000;

/// Browser settings and safety policy.
#[derive(Debug, Clone)]
pub struct WebConfig {
    /// Path to the Chrome/Chromium executable. When `None`, chromiumoxide
    /// auto-detects a system install (it probes `google-chrome-stable`,
    /// `chromium`, etc.).
    pub chrome_executable: Option<String>,
    /// Extra command-line arguments passed to Chrome (e.g. `--no-sandbox` in a
    /// restricted/container environment).
    pub chrome_args: Vec<String>,
    /// When false (default), `web_read`/`web_screenshot` refuse URLs that
    /// resolve to loopback, private, link-local, or unique-local addresses.
    /// This is the SSRF guard; set true only for trusted/offline use.
    pub allow_private_hosts: bool,
    /// Navigation timeout in milliseconds.
    pub nav_timeout_ms: u64,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            chrome_executable: None,
            chrome_args: Vec::new(),
            allow_private_hosts: false,
            nav_timeout_ms: DEFAULT_NAV_TIMEOUT_MS,
        }
    }
}
