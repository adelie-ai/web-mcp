#![deny(warnings)]

// Headless-Chrome browsing over the Chrome DevTools Protocol (CDP).
//
// A single Chrome instance is launched lazily on first use and kept alive for
// the life of the process (launching Chrome costs ~hundreds of ms, so we don't
// want to pay it per request). Each `web_read` / `web_screenshot` opens its own
// blank tab, navigates, extracts, and closes the tab. The browser lock is held
// only long enough to create the tab — the returned `Page` is independent, so
// concurrent requests don't serialize on each other's navigation.
//
// If Chrome dies (crash, OOM, external kill), the next request transparently
// relaunches it.

use crate::config::WebConfig;
use crate::error::{Result, WebError, WebMcpError};
use crate::url_guard::UrlGuard;
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::page::ScreenshotParams;
use chromiumoxide::{Browser, BrowserConfig, Page};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use url::Url;

/// Monotonic launch counter, used to give every Chrome launch a unique
/// `--user-data-dir`. chromiumoxide otherwise reuses a single fixed profile
/// directory, whose `SingletonLock` collides when more than one instance runs.
static LAUNCH_SEQ: AtomicU64 = AtomicU64::new(0);

/// JS that collects every absolute http(s) link with its visible text.
const LINKS_JS: &str = "Array.from(document.querySelectorAll('a[href]'))\
.map(a => ({ href: a.href, text: (a.innerText || '').trim() }))\
.filter(l => l.href.startsWith('http'))";

/// JS that returns the page's rendered, human-visible text.
const INNER_TEXT_JS: &str = "document.body ? document.body.innerText : ''";

/// A live browser plus the background task pumping its CDP event stream.
struct Live {
    browser: Browser,
    handler: JoinHandle<()>,
}

/// Owns the persistent headless-Chrome instance and serves page operations.
pub struct BrowserManager {
    config: Arc<WebConfig>,
    /// SSRF guard, re-applied to the *final* URL after redirects: Chrome does
    /// its own DNS resolution and follows redirects unchecked, so a public URL
    /// that 3xx-redirects to a private/metadata host would otherwise slip past
    /// the pre-navigation guard. See `navigate`.
    guard: UrlGuard,
    inner: Mutex<Option<Live>>,
}

/// Shape of a single link as returned by [`LINKS_JS`].
#[derive(Debug, Deserialize, Default)]
struct LinkJs {
    href: String,
    #[serde(default)]
    text: String,
}

impl BrowserManager {
    /// Create a manager. No browser is launched until the first request.
    pub fn new(config: Arc<WebConfig>) -> Self {
        let guard = UrlGuard::new(config.allow_private_hosts);
        Self {
            config,
            guard,
            inner: Mutex::new(None),
        }
    }

    /// Launch a fresh headless Chrome and spawn its event-handler task.
    async fn launch(&self) -> Result<Live> {
        // Unique profile dir per launch avoids the singleton-lock collision
        // that occurs when chromiumoxide's default fixed profile is shared
        // across processes (or across a relaunch after a crash).
        let seq = LAUNCH_SEQ.fetch_add(1, Ordering::Relaxed);
        let data_dir =
            std::env::temp_dir().join(format!("web-mcp-chrome-{}-{}", std::process::id(), seq));

        let mut builder = BrowserConfig::builder()
            .new_headless_mode()
            .user_data_dir(&data_dir);
        if let Some(exe) = &self.config.chrome_executable {
            builder = builder.chrome_executable(exe);
        }
        for arg in &self.config.chrome_args {
            builder = builder.arg(arg.as_str());
        }
        let cfg = builder.build().map_err(WebError::Navigation)?;

        let (browser, mut handler) = Browser::launch(cfg).await?;
        // Drive the CDP event stream until the browser closes. We don't act on
        // individual events; we just need the stream pumped for the connection
        // to function.
        let handler = tokio::spawn(async move {
            while let Some(event) = handler.next().await {
                if event.is_err() {
                    break;
                }
            }
        });
        Ok(Live { browser, handler })
    }

    /// Open a fresh blank tab, (re)launching the browser if it has died. Holds
    /// the lock only to create the tab.
    async fn new_tab(&self) -> Result<Page> {
        let mut guard = self.inner.lock().await;
        let dead = guard
            .as_ref()
            .map(|l| l.handler.is_finished())
            .unwrap_or(true);
        if dead {
            if let Some(old) = guard.take() {
                old.handler.abort();
            }
            *guard = Some(self.launch().await?);
        }
        let live = guard.as_ref().expect("browser ensured present above");
        let page = live.browser.new_page("about:blank").await?;
        Ok(page)
    }

    /// Navigate `page` to `url`, bounded by the configured navigation timeout,
    /// then re-validate the landed-on URL against the SSRF guard.
    ///
    /// The pre-navigation guard only vets the URL the caller supplied. Chrome
    /// follows redirects and re-resolves DNS itself, so a public origin that
    /// 3xx-redirects to an internal/metadata host must be caught here, after the
    /// fact, by re-checking `page.url()`.
    async fn navigate(&self, page: &Page, url: &Url) -> Result<()> {
        let dur = Duration::from_millis(self.config.nav_timeout_ms);
        let nav = async {
            page.goto(url.as_str()).await?;
            page.wait_for_navigation().await?;
            Ok::<(), WebMcpError>(())
        };
        match tokio::time::timeout(dur, nav).await {
            Ok(res) => res?,
            Err(_) => {
                return Err(WebError::Timeout(format!(
                    "navigation to {} exceeded {} ms",
                    url, self.config.nav_timeout_ms
                ))
                .into());
            }
        }

        // Re-apply the guard to wherever we actually landed (post-redirect).
        if let Some(final_url) = page.url().await?
            && final_url != url.as_str()
        {
            self.guard.check(&final_url).await?;
        }
        Ok(())
    }

    /// Navigate to `url` and extract its content.
    ///
    /// `format` is `"text"` (rendered innerText, default) or `"html"` (full
    /// serialized DOM). `include_links` adds an array of `{href, text}`.
    /// `max_chars` truncates `content` (0 = no limit).
    pub async fn read(
        &self,
        url: &Url,
        format: &str,
        include_links: bool,
        max_chars: usize,
    ) -> Result<Value> {
        let page = self.new_tab().await?;
        let result = self
            .read_on_page(&page, url, format, include_links, max_chars)
            .await;
        // Best-effort tab cleanup; a failure here must not mask the result.
        let _ = page.close().await;
        result
    }

    async fn read_on_page(
        &self,
        page: &Page,
        url: &Url,
        format: &str,
        include_links: bool,
        max_chars: usize,
    ) -> Result<Value> {
        self.navigate(page, url).await?;

        let title = page.get_title().await?.unwrap_or_default();
        let is_html = format.eq_ignore_ascii_case("html");
        let raw = if is_html {
            page.content().await?
        } else {
            page.evaluate(INNER_TEXT_JS)
                .await?
                .into_value::<String>()
                .unwrap_or_default()
        };
        let (content, truncated) = truncate(raw, max_chars);

        let final_url = page.url().await?.unwrap_or_else(|| url.to_string());
        let mut obj = json!({
            "url": final_url,
            "title": title,
            "format": if is_html { "html" } else { "text" },
            "content": content,
            "truncated": truncated,
        });

        if include_links {
            let links: Vec<LinkJs> = page
                .evaluate(LINKS_JS)
                .await?
                .into_value()
                .unwrap_or_default();
            let links: Vec<Value> = links
                .into_iter()
                .map(|l| json!({ "href": l.href, "text": l.text }))
                .collect();
            obj["links"] = Value::Array(links);
        }

        Ok(obj)
    }

    /// Navigate to `url` and capture a PNG screenshot, returning raw PNG bytes.
    pub async fn screenshot(&self, url: &Url, full_page: bool) -> Result<Vec<u8>> {
        let page = self.new_tab().await?;
        let result = self.screenshot_on_page(&page, url, full_page).await;
        let _ = page.close().await;
        result
    }

    async fn screenshot_on_page(&self, page: &Page, url: &Url, full_page: bool) -> Result<Vec<u8>> {
        self.navigate(page, url).await?;
        let params = ScreenshotParams::builder()
            .format(CaptureScreenshotFormat::Png)
            .full_page(full_page)
            .build();
        let bytes = page.screenshot(params).await?;
        Ok(bytes)
    }
}

/// Truncate `s` to at most `max_chars` characters (0 = unlimited). Returns the
/// possibly-truncated string and whether truncation happened.
fn truncate(s: String, max_chars: usize) -> (String, bool) {
    if max_chars == 0 {
        return (s, false);
    }
    match s.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => {
            let mut t = s;
            t.truncate(byte_idx);
            (t, true)
        }
        None => (s, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_respects_char_boundaries() {
        let (t, cut) = truncate("héllo wörld".to_string(), 5);
        assert!(cut);
        assert_eq!(t, "héllo");
    }

    #[test]
    fn truncate_zero_is_unlimited() {
        let (t, cut) = truncate("abc".to_string(), 0);
        assert!(!cut);
        assert_eq!(t, "abc");
    }

    #[test]
    fn truncate_shorter_than_limit_is_untouched() {
        let (t, cut) = truncate("abc".to_string(), 100);
        assert!(!cut);
        assert_eq!(t, "abc");
    }
}
