# web-mcp

A small, fast Rust **MCP server** (plus library) that gives **LLM agents**
read-only web access behind a small tool surface:

- **Read-only browsing** — open a URL in **headless Chrome** (full JavaScript
  rendering, via the Chrome DevTools Protocol), and return the rendered text,
  the page's links, or a PNG screenshot.

No API keys are required.

## Tools

| Tool | Purpose |
| --- | --- |
| `web_read` | Open a URL in headless Chrome and return rendered `text` (default) or `html`, optionally with the page's outbound links. Content is character-capped (`max_chars`, default 50k) with a `truncated` flag. |
| `web_screenshot` | Open a URL in headless Chrome and return a PNG screenshot (viewport or `full_page`) as an MCP image. |

Read results are returned as a `type: "json"` content entry; screenshots as a
`type: "image"` (base64 PNG). See
[`docs/result_shapes.md`](docs/result_shapes.md).

### No `web_search` tool — discovery via `web_read`

This server intentionally exposes **no** `web_search` tool. Every keyless
search-engine results page (Mojeek, DuckDuckGo, Brave, Ecosia, public SearXNG,
…) blocks automated/datacenter access with a `403`, a CAPTCHA, or an "anomaly"
challenge — *even when fetched through the real headless browser* — so a
built-in search tool would fail unpredictably depending on the host's IP
reputation. Rather than ship a tool that often errors, discovery is done with
`web_read` itself: point it at a search engine's results URL (e.g.
`https://www.bing.com/search?q=YOUR+QUERY` or
`https://duckduckgo.com/html/?q=YOUR+QUERY`) with `include_links=true`, then
follow the result links. The `web_read` tool description tells the model how.

## Safety: SSRF guard

A headless browser that fetches any URL is a server-side request forgery (SSRF)
risk. Before navigating, `web_read`/`web_screenshot`:

- require an `http`/`https` scheme, and
- resolve the host and **refuse** loopback, private, link-local, and
  unique-local addresses (e.g. `127.0.0.1`, `10.0.0.0/8`, `192.168.0.0/16`, the
  `169.254.169.254` cloud-metadata address).

This is on by default. `--allow-private-hosts` (env `WEB_ALLOW_PRIVATE_HOSTS`)
disables it for trusted/offline use.

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
| `--chrome-path` | `WEB_CHROME_PATH` | auto-detected |
| `--chrome-arg` (repeatable) | — | none (e.g. `--chrome-arg=--no-sandbox`) |
| `--allow-private-hosts` | `WEB_ALLOW_PRIVATE_HOSTS` | `false` |
| `--nav-timeout-ms` | `WEB_NAV_TIMEOUT_MS` | `30000` |

A single headless Chrome instance is launched lazily on first browse and reused
for the life of the process (each request gets its own tab); it is relaunched
automatically if it dies.

## Logging

`mcp-core`'s `run` installs the process subscriber; this crate calls nothing
to get it. Logs go to stderr, never stdout — the stdio transport frames
JSON-RPC on stdout, and one log line there would corrupt the protocol stream.
`RUST_LOG` sets the level (default `info`); see `mcp-core`'s own README for
the full level contract, the request/tool-call spans, and the standard
`OTEL_*` environment variables.

What this server adds on top of what it inherits:

- A `debug!` line each time it navigates the browser to a URL — the one
  outbound network call this server makes. A page URL is a tool argument, so
  it stays at DEBUG and is never attached to a span; `RUST_LOG=debug` is what
  it takes to see it.
- `web.upstream_failures`, a counter labelled `tool` and `reason`
  (`navigation`, `timeout`, or `browser`), for a failure reaching outward. A
  blocked URL or a bad parameter is a decline, not a fault, and is not
  counted here.
- `mcp-core` already records a tool-call counter and a latency histogram by
  tool and outcome (`mcp.tools.call`, `mcp.tools.call.duration`); this server
  does not duplicate them.

### The `otel` feature

Off by default. A pure passthrough — `web-mcp -> mcp-core -> adelie-telemetry`
— so this crate takes no direct dependency on `adelie-telemetry` or on any
opentelemetry crate. With the feature off, `cargo tree` resolves no
opentelemetry crate at all.

```bash
cargo build --features otel
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 ./target/debug/web-mcp serve --mode stdio
```

## Architecture

Protocol/transport/CLI plumbing comes from the shared `mcp-core` crate; this
crate supplies the web-specific pieces:

- `src/main.rs` — CLI entrypoint; builds `WebConfig` and hands `mcp-core` a `WebService`.
- `src/service.rs` — the `McpService` impl: tool schemas, argument parsing, dispatch.
- `src/config.rs` — Chrome settings, navigation timeout, and SSRF policy.
- `src/url_guard.rs` — the SSRF guard.
- `src/operations/browser.rs` — the persistent headless-Chrome manager.
- `src/error.rs` — structured error types (`thiserror`).

## Testing

```bash
just check                       # default features: fmt, lint, build, test
just check-otel                  # the same, built with --features otel
just test-network                # additionally launch Chrome and hit the live web
```

Network- and browser-dependent integration tests are gated behind
`RUN_NETWORK_TESTS=1` so the default suite is deterministic and offline. The
`tests/telemetry_*.rs` files are the telemetry acceptance suite: that stdout
carries only JSON-RPC at `RUST_LOG=trace`, that no page URL reaches an INFO
line or a span field, and that a default build resolves no opentelemetry
crate.

## License

Apache-2.0. See `LICENSE-APACHE` and `NOTICE`.
