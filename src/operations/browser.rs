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

/// Chrome flags web-mcp always applies to blunt the automation fingerprints that
/// trip false-positive bot challenges (e.g. Cloudflare "suspicious activity from
/// your network"). `--disable-blink-features=AutomationControlled` stops Chrome
/// advertising `navigator.webdriver`; combined with `--headless=new` (which uses
/// an ordinary Chrome user-agent, not "HeadlessChrome") the browser presents like
/// a normal one. Why only this: it removes the obvious tells a real user's
/// browser never shows — it is deliberately NOT an attempt to defeat TLS/JA3 or
/// behavioural fingerprinting, which no flag can, and a determined bot-management
/// challenge may still block.
const ANTI_AUTOMATION_ARGS: &[&str] = &["--disable-blink-features=AutomationControlled"];

/// The full Chrome argument list for a launch: the always-on anti-automation
/// flags first, then the operator's configured `chrome_args` (so config can add
/// container flags like `--no-sandbox` or override behaviour).
fn chrome_launch_args(user_args: &[String]) -> Vec<String> {
    ANTI_AUTOMATION_ARGS
        .iter()
        .map(|s| (*s).to_string())
        .chain(user_args.iter().cloned())
        .collect()
}

/// The only variables web-mcp passes from its own environment into the Chrome
/// child. Everything else is hidden from it - see [`chrome_env`].
///
/// The list is short on purpose. Each entry is here because Chrome needs it to
/// reach the host session, to start at all, to render in the right language, or
/// to reach the network the way the operator configured it. Nothing on it
/// carries a credential.
const CHROME_ENV_ALLOWLIST: &[&str] = &[
    // Reach the host session. An X11 session needs the display address and the
    // authorization cookie that goes with it; a Wayland session needs the
    // socket name and the runtime directory that holds the socket. The session
    // type tells Chrome which of the two to prefer.
    "DISPLAY",
    "XAUTHORITY",
    "WAYLAND_DISPLAY",
    "XDG_RUNTIME_DIR",
    "XDG_SESSION_TYPE",
    // Start at all. Chrome resolves its helper programs on PATH, derives its
    // cache and crash-report paths from HOME, writes scratch files under
    // TMPDIR, and finds its own shared libraries through LD_LIBRARY_PATH when
    // it is not a system install.
    "PATH",
    "HOME",
    "TMPDIR",
    "LD_LIBRARY_PATH",
    // Render in the operator's language and report the operator's local time to
    // page scripts.
    "LANG",
    "LC_ALL",
    "TZ",
    // Reach the network through the operator's proxy. Both spellings, because
    // different libraries read different ones.
    "HTTP_PROXY",
    "http_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "NO_PROXY",
    "no_proxy",
];

/// Build the environment for the Chrome child out of `parent`, this process's
/// own environment.
///
/// A name on [`CHROME_ENV_ALLOWLIST`] keeps its value. Every other name is
/// mapped to an empty value.
///
/// Why map rather than drop: a child process inherits its parent's whole
/// environment, and chromiumoxide gives no way to clear it - the entries it
/// takes are applied on top of what the child already inherited. Setting a name
/// to an empty value is therefore how web-mcp removes it. The name still
/// reaches Chrome; the value does not, and the value is what leaks. This
/// matters most when web-mcp is hosted in-process inside a desktop client,
/// where this process's environment is the client's own - API keys, tokens, and
/// the session-bus address that fronts the desktop credential store.
///
/// A variable the parent does not have is not invented, because an empty value
/// is not the same as an absent one: an empty `DISPLAY` would make Chrome try
/// to open a display that is not there.
///
/// One limit, stated plainly: a variable whose name is not valid UTF-8 cannot be
/// named in the `String` map chromiumoxide takes, so it passes through
/// inherited. An allowlisted variable whose value is not valid UTF-8 is left
/// inherited too, rather than forwarded through a lossy conversion that would
/// corrupt it.
fn chrome_env<I>(parent: I) -> Vec<(String, String)>
where
    I: IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
{
    parent
        .into_iter()
        .filter_map(|(name, value)| {
            let name = name.into_string().ok()?;
            if CHROME_ENV_ALLOWLIST.contains(&name.as_str()) {
                value.into_string().ok().map(|value| (name, value))
            } else {
                Some((name, String::new()))
            }
        })
        .collect()
}

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
        for arg in chrome_launch_args(&self.config.chrome_args) {
            builder = builder.arg(arg);
        }
        builder = builder.envs(chrome_env(std::env::vars_os()));
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
        log_navigation_start(url);
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

/// Log that navigation to `url` is starting: web-mcp's one outbound network
/// call, made through the browser rather than a direct HTTP client, so this
/// is both "the outbound HTTP request" and "the browser navigation" the
/// level contract asks a server to log.
///
/// A page URL is a tool argument — content, never an id — so it stays at
/// DEBUG and is never attached to a span (a span field would leave the
/// process with `otel` on regardless of level). Kept as its own function so
/// a test can drive it directly, without a real browser or network.
fn log_navigation_start(url: &Url) {
    tracing::debug!(url = %url, "navigating");
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
    use std::ffi::OsString;

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

    #[test]
    fn log_navigation_start_puts_the_url_at_debug_only() {
        use std::collections::BTreeMap;
        use std::sync::{Arc, Mutex};
        use tracing::field::{Field, Visit};
        use tracing_subscriber::Layer;
        use tracing_subscriber::layer::{Context, SubscriberExt};

        type LoggedEvent = (tracing::Level, BTreeMap<String, String>);

        #[derive(Clone, Default)]
        struct Capture(Arc<Mutex<Vec<LoggedEvent>>>);

        struct Collector<'a>(&'a mut BTreeMap<String, String>);
        impl Visit for Collector<'_> {
            fn record_str(&mut self, field: &Field, value: &str) {
                self.0.insert(field.name().to_string(), value.to_string());
            }
            fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                self.0
                    .insert(field.name().to_string(), format!("{value:?}"));
            }
        }

        impl<S: tracing::Subscriber> Layer<S> for Capture {
            fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
                let mut fields = BTreeMap::new();
                event.record(&mut Collector(&mut fields));
                self.0
                    .lock()
                    .expect("capture lock is only held to push one record")
                    .push((*event.metadata().level(), fields));
            }
        }

        let url: Url = "https://example.com/MARKER-log-nav-9f3d1c"
            .parse()
            .expect("a valid url");
        let capture = Capture::default();
        let subscriber = tracing_subscriber::registry().with(capture.clone());
        tracing::subscriber::with_default(subscriber, || {
            log_navigation_start(&url);
        });

        let events = capture
            .0
            .lock()
            .expect("capture lock is only held to push one record");
        assert_eq!(
            events.len(),
            1,
            "navigating must log exactly one event: {events:?}"
        );
        let (level, fields) = &events[0];
        assert_eq!(
            *level,
            tracing::Level::DEBUG,
            "navigation start must log at DEBUG, so it stays off the INFO band"
        );
        assert_eq!(
            fields.get("url").map(String::as_str),
            Some(url.as_str()),
            "the event must carry the url that was navigated to"
        );
    }

    /// The environment a desktop session hands a process it starts, plus one
    /// value a host process might hold that Chrome has no reason to see.
    fn desktop_session_env() -> Vec<(OsString, OsString)> {
        [
            ("DISPLAY", ":0"),
            ("XAUTHORITY", "/run/user/1000/xauth"),
            ("WAYLAND_DISPLAY", "wayland-0"),
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
            ("XDG_SESSION_TYPE", "wayland"),
            ("PATH", "/usr/bin:/bin"),
            ("HOME", "/home/user"),
            ("LANG", "en_GB.UTF-8"),
            ("ANTHROPIC_API_KEY", "a-real-secret"),
            ("DATABASE_URL", "postgres://user:pass@db/adele"),
            ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1000/bus"),
        ]
        .into_iter()
        .map(|(name, value)| (OsString::from(name), OsString::from(value)))
        .collect()
    }

    fn value_of<'a>(env: &'a [(String, String)], name: &str) -> Option<&'a str> {
        env.iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn chrome_env_forwards_the_display_variables() {
        // A browser that must reach the host session needs the X11 display and
        // its cookie, or the Wayland socket and the runtime directory holding
        // it. Without these a headed browser has no session to draw into.
        let env = chrome_env(desktop_session_env());
        assert_eq!(value_of(&env, "DISPLAY"), Some(":0"));
        assert_eq!(value_of(&env, "XAUTHORITY"), Some("/run/user/1000/xauth"));
        assert_eq!(value_of(&env, "WAYLAND_DISPLAY"), Some("wayland-0"));
        assert_eq!(value_of(&env, "XDG_RUNTIME_DIR"), Some("/run/user/1000"));
        assert_eq!(value_of(&env, "XDG_SESSION_TYPE"), Some("wayland"));
    }

    #[test]
    fn chrome_env_forwards_what_chrome_needs_to_start() {
        let env = chrome_env(desktop_session_env());
        assert_eq!(value_of(&env, "PATH"), Some("/usr/bin:/bin"));
        assert_eq!(value_of(&env, "HOME"), Some("/home/user"));
        assert_eq!(value_of(&env, "LANG"), Some("en_GB.UTF-8"));
    }

    #[test]
    fn chrome_env_keeps_no_value_from_outside_the_allowlist() {
        // web-mcp can be hosted in-process inside a desktop client, so this
        // process's environment can hold that client's own secrets. None of
        // those values may travel into the browser it spawns.
        let env = chrome_env(desktop_session_env());
        for hidden in [
            "ANTHROPIC_API_KEY",
            "DATABASE_URL",
            "DBUS_SESSION_BUS_ADDRESS",
        ] {
            assert_eq!(
                value_of(&env, hidden),
                Some(""),
                "{hidden} must reach Chrome with no value"
            );
        }
    }

    #[test]
    fn chrome_env_forwards_a_value_only_for_an_allowlisted_name() {
        // The general property behind the two tests above: every entry either
        // names an allowlisted variable and carries its value, or carries
        // nothing at all.
        let parent = desktop_session_env();
        let env = chrome_env(parent.clone());
        for (name, value) in &env {
            if value.is_empty() {
                continue;
            }
            assert!(
                CHROME_ENV_ALLOWLIST.contains(&name.as_str()),
                "{name} carries a value but is not on the allowlist"
            );
            let original = parent
                .iter()
                .find(|(key, _)| key == name.as_str())
                .expect("the name came from the parent environment");
            assert_eq!(value.as_str(), original.1.to_string_lossy());
        }
    }

    #[test]
    fn chrome_env_does_not_invent_a_variable_the_parent_lacks() {
        // A headless container has no display. web-mcp must not hand Chrome an
        // empty DISPLAY there, because an empty value is not the same as no
        // value: Chrome would try to open it and fail.
        let env = chrome_env([(OsString::from("PATH"), OsString::from("/usr/bin"))]);
        assert_eq!(value_of(&env, "DISPLAY"), None);
        assert_eq!(value_of(&env, "WAYLAND_DISPLAY"), None);
    }

    const AUTOMATION_FLAG: &str = "--disable-blink-features=AutomationControlled";

    #[test]
    fn launch_args_always_disable_automation_control() {
        // The flag that stops Chrome advertising navigator.webdriver is applied
        // even with no operator args — that's what blunts the false-positive bot
        // challenges (Cloudflare "suspicious activity").
        let args = chrome_launch_args(&[]);
        assert_eq!(args, vec![AUTOMATION_FLAG.to_string()]);
    }

    #[test]
    fn launch_args_prepend_defaults_before_user_args() {
        let user = vec![
            "--no-sandbox".to_string(),
            "--disable-dev-shm-usage".to_string(),
        ];
        let args = chrome_launch_args(&user);
        assert!(
            args.contains(&AUTOMATION_FLAG.to_string()),
            "anti-automation flag is present"
        );
        assert!(
            args.contains(&"--no-sandbox".to_string()),
            "operator container flags are preserved"
        );
        let auto = args.iter().position(|a| a == AUTOMATION_FLAG).unwrap();
        let sandbox = args.iter().position(|a| a == "--no-sandbox").unwrap();
        assert!(auto < sandbox, "defaults come before operator args");
    }
}
