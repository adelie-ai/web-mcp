# web-mcp Result Shapes

Search and read tools return their payload as a single `type: "json"` content
entry; screenshots return a `type: "image"` entry:

```json
{ "content": [ { "type": "json",  "value": <payload> } ] }
{ "content": [ { "type": "image", "data": "<base64 png>", "mimeType": "image/png" } ] }
```

On failure (no results, blocked URL, navigation error, bad parameters) the tool
returns a JSON-RPC error instead of a result.

## `web_search` → array of Result

```json
[
  {
    "title": "Rust Programming Language",
    "url": "https://www.rust-lang.org/",
    "snippet": "A language empowering everyone to build reliable and efficient software."
  }
]
```

Ordered by the search engine's relevance ranking. `snippet` may be `null`.

## `web_read` → Page

```json
{
  "url": "https://example.com/",
  "title": "Example Domain",
  "format": "text",
  "content": "Example Domain\n\nThis domain is for use in documentation examples…",
  "truncated": false,
  "links": [
    { "href": "https://www.iana.org/domains/example", "text": "Learn more" }
  ]
}
```

- `format` echoes the requested mode: `"text"` (rendered `innerText`, default)
  or `"html"` (full serialized DOM).
- `content` is capped at the requested `max_chars` (default 50000; 0 = no
  limit). `truncated` is `true` when content was cut.
- `links` is present only when `include_links: true` — every absolute http(s)
  link on the page as `{href, text}`.
- `url` is the final URL after any redirects.

## `web_screenshot` → image

A `type: "image"` content entry with base64-encoded PNG bytes and
`mimeType: "image/png"`. `full_page: true` captures the entire scrollable page;
the default captures just the viewport.
