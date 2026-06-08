#![deny(warnings)]

// Web operation implementations.
//
// - `search`  — DuckDuckGo HTML endpoint, parsed server-side.
// - `browser` — a persistent headless-Chrome instance driven over CDP for
//   rendering pages (`web_read`) and capturing screenshots (`web_screenshot`).

pub mod browser;
pub mod search;
