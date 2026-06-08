#![deny(warnings)]

// Runtime configuration for web-mcp: the search backend endpoint, the HTTP
// User-Agent, the headless-Chrome executable/arguments, and the SSRF policy.

/// Default search endpoint used by `web_search`.
///
/// Why Mojeek: it is an independent search engine (its own crawler) with a
/// stable, server-renderable HTML results page and no API key requirement. It
/// tolerates programmatic requests, unlike DuckDuckGo's HTML endpoint which
/// serves an "anomaly" bot-challenge to non-residential / automated clients.
/// The endpoint is configurable, so an operator can point this at a different
/// engine or a self-hosted instance (the HTML parser is Mojeek-specific).
pub const DEFAULT_SEARCH_URL: &str = "https://www.mojeek.com/search";

/// Default User-Agent for search HTTP requests. A conventional desktop browser
/// UA, since some engines vary output by client.
pub const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

/// Default navigation timeout (milliseconds) for `web_read` / `web_screenshot`.
pub const DEFAULT_NAV_TIMEOUT_MS: u64 = 30_000;

/// Endpoints, identification, browser settings, and safety policy.
#[derive(Debug, Clone)]
pub struct WebConfig {
    /// Search endpoint for `web_search` (a Mojeek-compatible HTML results URL).
    pub search_url: String,
    /// User-Agent for outbound search HTTP requests.
    pub user_agent: String,
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
            search_url: DEFAULT_SEARCH_URL.to_string(),
            user_agent: DEFAULT_USER_AGENT.to_string(),
            chrome_executable: None,
            chrome_args: Vec::new(),
            allow_private_hosts: false,
            nav_timeout_ms: DEFAULT_NAV_TIMEOUT_MS,
        }
    }
}
