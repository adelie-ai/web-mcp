# web-mcp

A small, fast Rust **MCP server** (plus library) that gives **LLM agents** two
web capabilities behind one tool surface:

- **Web search** — a keyless query → ranked `{title, url, snippet}` results.
- **Read-only browsing** — open a URL in **headless Chrome** (full JavaScript
  rendering, via the Chrome DevTools Protocol), and return the rendered text,
  the page's links, or a PNG screenshot.

No API keys are required.

## Tools

| Tool | Purpose |
| --- | --- |
| `web_search` | Search the web and return ranked results (`title`, `url`, `snippet`). |
| `web_read` | Open a URL in headless Chrome and return rendered `text` (default) or `html`, optionally with the page's outbound links. Content is character-capped (`max_chars`, default 50k) with a `truncated` flag. |
| `web_screenshot` | Open a URL in headless Chrome and return a PNG screenshot (viewport or `full_page`) as an MCP image. |

Search and read results are returned as a `type: "json"` content entry;
screenshots as a `type: "image"` (base64 PNG). See
[`docs/result_shapes.md`](docs/result_shapes.md).

## Safety: SSRF guard

A headless browser that fetches any URL is a server-side request forgery (SSRF)
risk. Before navigating, `web_read`/`web_screenshot`:

- require an `http`/`https` scheme, and
- resolve the host and **refuse** loopback, private, link-local, and
  unique-local addresses (e.g. `127.0.0.1`, `10.0.0.0/8`, `192.168.0.0/16`, the
  `169.254.169.254` cloud-metadata address).

This is on by default. `--allow-private-hosts` (env `WEB_ALLOW_PRIVATE_HOSTS`)
disables it for trusted/offline use.

## Search backend

The default backend is **[Mojeek](https://www.mojeek.com/)** — an independent
search engine (its own crawler) with a stable, server-renderable HTML results
page and no key requirement. It was chosen over DuckDuckGo's HTML endpoint,
which serves an "anomaly" bot-challenge to automated/non-residential clients.
The endpoint is configurable (`--search-url`), though the HTML parser is
Mojeek-specific.

## Build & run

Requires a Rust toolchain (pinned in `rust-toolchain.toml`) and a Chrome /
Chromium install (auto-detected; `google-chrome-stable`, `chromium`, …).

```bash
cargo build --release

# stdio transport (recommended for local/editor usage)
./target/release/web-mcp serve --mode stdio

# WebSocket transport (recommended for hosted services)
./target/release/web-mcp serve --mode websocket --host 0.0.0.0 --port 8080
```

### Configuration

| Flag | Env var | Default |
| --- | --- | --- |
| `--search-url` | `WEB_SEARCH_URL` | `https://www.mojeek.com/search` |
| `--user-agent` | `WEB_USER_AGENT` | a desktop Chrome UA |
| `--chrome-path` | `WEB_CHROME_PATH` | auto-detected |
| `--chrome-arg` (repeatable) | — | none (e.g. `--chrome-arg=--no-sandbox`) |
| `--allow-private-hosts` | `WEB_ALLOW_PRIVATE_HOSTS` | `false` |
| `--nav-timeout-ms` | `WEB_NAV_TIMEOUT_MS` | `30000` |

A single headless Chrome instance is launched lazily on first browse and reused
for the life of the process (each request gets its own tab); it is relaunched
automatically if it dies.

## Architecture

Mirrors the sibling MCP servers in this monorepo (`fileio-mcp`, `geocode-mcp`,
`openstreetmap-mcp`):

- `src/main.rs` — CLI entrypoint, JSON-RPC loop, stdio + WebSocket transports.
- `src/server.rs` — MCP lifecycle (`initialize` / `tools/list` / `tools/call`).
- `src/tools.rs` — tool schemas, argument parsing, dispatch.
- `src/config.rs` — search endpoint, UA, Chrome, and SSRF policy.
- `src/url_guard.rs` — the SSRF guard.
- `src/operations/search.rs` — Mojeek HTML search.
- `src/operations/browser.rs` — the persistent headless-Chrome manager.
- `src/error.rs` — structured error types (`thiserror`).

## Testing

```bash
cargo test                       # unit + protocol/validation/SSRF tests (no network)
just test-network                # additionally launch Chrome and hit the live web
```

Network- and browser-dependent integration tests are gated behind
`RUN_NETWORK_TESTS=1` so the default suite is deterministic and offline.

## License

Apache-2.0. See `LICENSE-APACHE` and `NOTICE`.
