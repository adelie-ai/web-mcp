# web-mcp Result Shapes

`web_read` returns its JSON payload as a single `type: "text"` content entry
(the payload serialized to a string) plus a `structuredContent` mirror for typed
clients. `web_screenshot` returns the same JSON shape when it is given
`save_as`, and a `type: "image"` entry when it is not:

```json
{ "content": [ { "type": "text",  "text": "<json string>" } ], "structuredContent": <payload> }
{ "content": [ { "type": "image", "data": "<base64 png>", "mimeType": "image/png" } ] }
```

> There is no `web_search` tool — keyless results pages block automated access
> even via the headless browser, so discovery is done by pointing `web_read` at
> a search-engine results URL with `include_links: true`. See the README.

On a tool-execution failure (blocked URL, navigation error, bad parameters) the
tool returns a **successful** `tools/call` result with `isError: true` and the
message in a `text` content entry:

```json
{ "content": [ { "type": "text", "text": "<error message>" } ], "isError": true }
```

Protocol-level faults (unknown tool, server not initialized, malformed
request) are still reported as JSON-RPC errors.

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

## `web_screenshot` → saved file, or image

With `save_as`, the PNG is written to a file inside the server's screenshot
directory and the reply carries metadata only:

```json
{
  "path": "/home/<user>/.cache/web-mcp/screenshots/example-home.png",
  "bytes": 245760,
  "width": 1920,
  "height": 1080
}
```

- `path` is the absolute path of the written file.
- `bytes` is its size on disk.
- `width` and `height` come from the PNG header. Both are omitted if that header
  cannot be read.

Without `save_as`, the reply is a `type: "image"` content entry with
base64-encoded PNG bytes and `mimeType: "image/png"`.

`full_page: true` captures the entire scrollable page in either form; the
default captures just the viewport.

A `save_as` outside the screenshot directory is refused as a tool error, the
same as a blocked URL. See the README for the rules.
