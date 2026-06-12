#![deny(warnings)]

// Web operation implementations.
//
// - `browser` — a persistent headless-Chrome instance driven over CDP for
//   rendering pages (`web_read`) and capturing screenshots (`web_screenshot`).
//
// There is no `search` module: keyless search-engine results pages all block
// automated/datacenter access (403 / CAPTCHA / "anomaly" challenge) even when
// fetched through the real headless browser, so `web_search` was removed.
// Discovery now goes through `web_read` pointed at a search-engine results URL.

pub mod browser;
