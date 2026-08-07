#![deny(warnings)]

// Acceptance tests for the telemetry web-mcp inherits from mcp-core's `run`:
// the stdio transport keeps stdout clean at any log level, and a page URL
// never reaches an INFO line (D10, the level contract).
//
// Each test spawns the real binary. Only a real process proves what reaches
// file descriptor 1 and what the installed subscriber really writes to
// stderr; an in-process capturing layer only proves what a test told a layer
// to do.

use serde_json::{Value, json};
use std::io::Write;
use std::process::{Child, Command, Output, Stdio};

/// A URL `Url::parse` rejects outright (no scheme, no colon), so the guard's
/// `WebError::InvalidParameters` message quotes it back whole. This drives
/// the failure path without any network or browser launch, and doubles as
/// the sentinel a test greps for.
const MARKER_URL: &str = "MARKER-9f3d1c2a-no-such-scheme";

fn spawn_with_log_level(level: &str) -> Child {
    let exe = env!("CARGO_BIN_EXE_web-mcp");
    Command::new(exe)
        .args(["serve", "--mode", "stdio"])
        .env("RUST_LOG", level)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn web-mcp serve --mode stdio")
}

fn run_requests(level: &str, requests: &[Value]) -> Output {
    let mut child = spawn_with_log_level(level);
    {
        let stdin = child.stdin.as_mut().expect("child has a piped stdin");
        for request in requests {
            writeln!(stdin, "{request}").expect("write jsonrpc line");
        }
    }
    drop(child.stdin.take());
    child.wait_with_output().expect("child must exit")
}

/// The level word `tracing_subscriber`'s default console formatter writes as
/// the second whitespace-separated token, right after the timestamp. Reading
/// it this way (rather than a substring search for "INFO") does not confuse
/// a level word for content that happens to contain the same letters.
fn line_level(line: &str) -> Option<&str> {
    line.split_whitespace()
        .nth(1)
        .filter(|token| matches!(*token, "ERROR" | "WARN" | "INFO" | "DEBUG" | "TRACE"))
}

#[test]
fn stdout_carries_only_jsonrpc_at_trace_level() {
    let requests = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"web_read","arguments":{"url":MARKER_URL}}}),
        json!({"jsonrpc":"2.0","id":4,"method":"shutdown","params":{}}),
    ];
    let output = run_requests("trace", &requests);
    assert!(
        output.status.success(),
        "web-mcp must exit cleanly, otherwise an empty stdout proves nothing: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    let mut replies = 0;
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let value: Value = serde_json::from_str(line).unwrap_or_else(|e| {
            panic!("every stdout line must be JSON-RPC, but {line:?} is not: {e}")
        });
        assert_eq!(
            value.get("jsonrpc").and_then(Value::as_str),
            Some("2.0"),
            "every stdout line must carry the JSON-RPC envelope: {line:?}"
        );
        replies += 1;
    }
    assert_eq!(
        replies, 4,
        "expected one reply per request that carried an id"
    );

    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    assert!(
        stderr.contains("INFO") || stderr.contains("DEBUG") || stderr.contains("TRACE"),
        "at RUST_LOG=trace the subscriber must be installed and log to stderr; stderr was: \
         {stderr:?}"
    );
}

/// AC: no URL reaches an INFO (or higher) line, for either browsing tool, and
/// the failure path is what is driven here — the guard's own error message is
/// where a URL most naturally ends up embedded in text meant for a person.
#[test]
fn no_url_reaches_an_info_line_on_the_failure_path() {
    let requests = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"web_read","arguments":{"url":MARKER_URL}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"web_screenshot","arguments":{"url":MARKER_URL}}}),
        json!({"jsonrpc":"2.0","id":4,"method":"shutdown","params":{}}),
    ];
    let output = run_requests("trace", &requests);
    assert!(
        output.status.success(),
        "web-mcp must exit cleanly: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8");
    let mut saw_marker_at_debug = false;
    for line in stderr.lines() {
        if !line.contains(MARKER_URL) {
            continue;
        }
        let level = line_level(line);
        assert!(
            matches!(level, Some("DEBUG") | Some("TRACE")),
            "the URL reached a line at level {level:?}, at or above INFO: {line:?}"
        );
        if level == Some("DEBUG") {
            saw_marker_at_debug = true;
        }
    }
    assert!(
        saw_marker_at_debug,
        "the URL must still be reachable at DEBUG, or this test cannot tell a real fix from \
         a line that was simply deleted; stderr was: {stderr:?}"
    );
}
