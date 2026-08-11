#![deny(warnings)]

// Runtime configuration for web-mcp: the headless-Chrome executable/arguments,
// the navigation timeout, the screenshot directory, and the SSRF policy. (There
// is no search endpoint: `web_search` was removed because keyless results pages
// all block automated access even through the real browser — see `service.rs` /
// `operations/mod.rs`.)

use std::path::PathBuf;

/// Default navigation timeout (milliseconds) for `web_read` / `web_screenshot`.
pub const DEFAULT_NAV_TIMEOUT_MS: u64 = 30_000;

/// Directory name `web_screenshot` writes under, inside the per-user cache
/// directory [`default_screenshot_dir`] settles on.
const SCREENSHOT_DIR_NAME: &str = "web-mcp/screenshots";

/// Where `web_screenshot` writes when the operator names no directory, or
/// `None` when there is nowhere safe to choose.
///
/// The choice is a per-user cache directory: `$XDG_CACHE_HOME`, else
/// `$HOME/.cache`. There is deliberately no fall back to the system temp
/// directory. That directory is shared with every other user on the machine, so
/// a fixed name inside it is one any of them can create first - as a symbolic
/// link to a directory of their own, which web-mcp would then resolve and treat
/// as its boundary, handing them every screenshot. A default nobody can trust
/// is worse than no default: with `None`, `save_as` is refused and says to set
/// `--screenshot-dir`, and screenshots still come back inline.
pub fn default_screenshot_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|dir| dir.is_absolute())
                .map(|home| home.join(".cache"))
        })?;
    Some(base.join(SCREENSHOT_DIR_NAME))
}

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
    /// The one directory `web_screenshot` may write a `save_as` file into, or
    /// `None` when none is configured and none can be defaulted safely - in
    /// which case `save_as` is refused. Every caller-supplied path is resolved
    /// inside it; see [`ScreenshotDir`](crate::screenshot::ScreenshotDir).
    pub screenshot_dir: Option<PathBuf>,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            chrome_executable: None,
            chrome_args: Vec::new(),
            allow_private_hosts: false,
            nav_timeout_ms: DEFAULT_NAV_TIMEOUT_MS,
            screenshot_dir: default_screenshot_dir(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_screenshot_dir_is_absolute_and_named_for_web_mcp() {
        // Every environment this runs in has HOME, so a directory is chosen.
        let dir = default_screenshot_dir().expect("a per-user cache directory");
        assert!(dir.is_absolute(), "{} should be absolute", dir.display());
        assert!(
            dir.ends_with("web-mcp/screenshots"),
            "{} should be web-mcp's own directory",
            dir.display()
        );
    }

    #[test]
    fn default_screenshot_dir_never_chooses_the_shared_temp_directory() {
        // The shared temp directory is the one place a default must not land:
        // another user can create the name first and point it at themselves.
        // With no per-user directory to choose, there is no default at all.
        if let Some(dir) = default_screenshot_dir() {
            assert!(
                !dir.starts_with(std::env::temp_dir()),
                "{} must not be under the shared temp directory",
                dir.display()
            );
        }
    }
}
