#![deny(warnings)]

// Web search via Mojeek's HTML results page.
//
// Mojeek (https://www.mojeek.com/search?q=…) is an independent search engine
// that returns a stable, server-rendered HTML results page and tolerates
// programmatic requests without an API key. Each result is a `<li>` containing
// an `<a class="title" href="…">` (a direct destination URL, no redirector)
// and a `<p class="s">` snippet.
//
// This endpoint has no official API contract, so the parser is defensive and
// the markup selectors are the documented failure point if Mojeek changes their
// HTML. The endpoint is configurable for self-hosted or alternative engines.

use crate::config::WebConfig;
use crate::error::{Result, WebError};
use scraper::{Html, Selector};
use serde_json::{Value, json};

/// Upper bound on results we return in one call.
const MAX_COUNT: usize = 25;

/// Run a search and return up to `count` results as `{title, url, snippet}`.
pub async fn search(
    client: &reqwest::Client,
    config: &WebConfig,
    query: &str,
    count: usize,
) -> Result<Value> {
    let count = count.clamp(1, MAX_COUNT);

    let resp = client
        .get(&config.search_url)
        .query(&[("q", query)])
        .header(reqwest::header::USER_AGENT, &config.user_agent)
        .header(reqwest::header::ACCEPT_LANGUAGE, "en-US,en;q=0.9")
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        return Err(
            WebError::SearchFailed(format!("search backend returned HTTP {}", status)).into(),
        );
    }
    let body = resp.text().await?;

    let results = parse_results(&body, count);
    if results.is_empty() {
        return Err(WebError::SearchFailed(format!("no results for query: {}", query)).into());
    }
    Ok(Value::Array(results))
}

/// Parse the Mojeek HTML results page into result objects. Pure and
/// deterministic so it can be unit-tested against a saved fixture.
fn parse_results(html: &str, limit: usize) -> Vec<Value> {
    let doc = Html::parse_document(html);
    // Selectors are constant, valid CSS; parsing them cannot fail at runtime.
    let item_sel = Selector::parse("ul.results-standard li").expect("static selector");
    let title_sel = Selector::parse("a.title").expect("static selector");
    let snippet_sel = Selector::parse("p.s").expect("static selector");

    let mut out = Vec::new();
    for li in doc.select(&item_sel) {
        let Some(link) = li.select(&title_sel).next() else {
            continue;
        };
        let title = collapse_ws(&link.text().collect::<String>());
        let url = link.value().attr("href").unwrap_or("").to_string();
        if title.is_empty() || !(url.starts_with("http://") || url.starts_with("https://")) {
            continue;
        }
        let snippet = li
            .select(&snippet_sel)
            .next()
            .map(|s| collapse_ws(&s.text().collect::<String>()))
            .filter(|s| !s.is_empty());

        out.push(json!({ "title": title, "url": url, "snippet": snippet }));
        if out.len() >= limit {
            break;
        }
    }
    out
}

/// Collapse runs of whitespace (incl. newlines from the markup) into single
/// spaces and trim.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"
    <html><body>
      <ul class="results-standard">
        <li class="r1">
          <a title="https://rust-lang.org/" href="https://rust-lang.org/" class="ob">
            <p class="i"><span class="url">https://rust-lang.org/</span></p>
          </a>
          <h2><a class="title" title="https://rust-lang.org/" href="https://rust-lang.org/">Rust   Programming Language</a></h2>
          <p class="s">In 2018, the <strong>Rust</strong> community improved
          the <strong>programming</strong> experience.</p>
          <p class="more"><a href="/search?q=site:rust-lang.org">See more &raquo;</a></p>
        </li>
        <li class="r2">
          <h2><a class="title" href="https://doc.rust-lang.org/book/">The Rust Book</a></h2>
          <p class="s">The official book.</p>
        </li>
        <li class="r3">
          <h2><a class="title" href="/internal/relative">Should be skipped</a></h2>
        </li>
      </ul>
    </body></html>
    "#;

    #[test]
    fn parses_titles_urls_and_snippets() {
        let results = parse_results(FIXTURE, 10);
        assert_eq!(
            results.len(),
            2,
            "relative URL must be skipped: {results:?}"
        );
        assert_eq!(results[0]["title"], json!("Rust Programming Language"));
        assert_eq!(results[0]["url"], json!("https://rust-lang.org/"));
        assert_eq!(
            results[0]["snippet"],
            json!("In 2018, the Rust community improved the programming experience.")
        );
        assert_eq!(results[1]["url"], json!("https://doc.rust-lang.org/book/"));
    }

    #[test]
    fn respects_limit() {
        assert_eq!(parse_results(FIXTURE, 1).len(), 1);
    }

    #[test]
    fn empty_html_yields_no_results() {
        assert!(parse_results("<html><body></body></html>", 10).is_empty());
    }
}
