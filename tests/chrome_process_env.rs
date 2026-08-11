#![deny(warnings)]
#![cfg(unix)]

//! What the real launch path actually hands the browser process (#18).
//!
//! The allowlist itself is unit-tested beside the code. This file tests the
//! wiring: it drives the shipped binary, points it at a stub executable that
//! records the environment it is started with, and reads that record. Without
//! it, deleting the one line that applies the allowlist would leave every other
//! test green while Chrome went back to inheriting everything.
//!
//! No browser and no network are involved. The stub exits immediately, so the
//! launch fails and the tool call returns an error - which is fine, because the
//! record has already been written by then.

use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A value planted in the server's environment that no browser should see.
const PLANTED_SECRET: &str = "web-mcp-test-secret-value";

/// Run the server against a stub browser and return the environment the stub
/// was started with, as (name, value) pairs.
fn environment_handed_to_the_browser(scratch: &Path) -> Vec<(String, String)> {
    let record = scratch.join("child-environment.txt");
    let stub = scratch.join("stub-browser.sh");
    // The record path is written into the stub rather than passed through the
    // environment, because passing it through is exactly what this test proves
    // does not work.
    fs::write(
        &stub,
        format!("#!/bin/sh\nenv > '{}'\nexit 0\n", record.display()),
    )
    .expect("write the stub browser");
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).expect("make the stub runnable");

    let mut child = Command::new(env!("CARGO_BIN_EXE_web-mcp"))
        .args(["serve", "--mode", "stdio", "--allow-private-hosts"])
        .arg("--chrome-path")
        .arg(&stub)
        .env("WEB_MCP_TEST_SECRET", PLANTED_SECRET)
        .env("DISPLAY", ":99")
        .env("XDG_SESSION_TYPE", "x11")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn web-mcp serve --mode stdio");

    let mut stdin = child.stdin.take().expect("child stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("child stdout"));
    for line in [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"env-test","version":"1"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        // A loopback URL with the guard disabled: the request reaches the
        // launch without any name resolution or network traffic.
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"web_screenshot","arguments":{"url":"http://127.0.0.1:1/"}}}),
    ] {
        writeln!(stdin, "{line}").expect("write a jsonrpc line");
    }
    stdin.flush().expect("flush");

    // Read until the tool call is answered; by then the stub has run.
    let mut line = String::new();
    loop {
        line.clear();
        let read = stdout.read_line(&mut line).expect("read a line");
        assert_ne!(read, 0, "the server closed stdout before answering");
        if let Ok(msg) = serde_json::from_str::<Value>(line.trim())
            && msg.get("id").and_then(Value::as_u64) == Some(2)
        {
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();

    let recorded = fs::read_to_string(&record).expect("the stub browser recorded its environment");
    recorded
        .lines()
        .filter_map(|entry| entry.split_once('='))
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

fn scratch_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "web-mcp-chrome-env-test-{}-{name}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("create the scratch directory");
    dir
}

#[test]
fn the_browser_process_receives_no_value_from_outside_the_allowlist() {
    let scratch = scratch_dir("allowlist");
    let environment = environment_handed_to_the_browser(&scratch);

    let secret = environment
        .iter()
        .find(|(name, _)| name == "WEB_MCP_TEST_SECRET")
        .map(|(_, value)| value.as_str());
    assert_eq!(
        secret,
        Some(""),
        "a variable outside the allowlist must reach the browser with no value"
    );
    assert!(
        !environment.iter().any(|(_, value)| value == PLANTED_SECRET),
        "no part of the browser's environment may carry the planted secret"
    );
    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn the_browser_process_receives_the_display_variables() {
    let scratch = scratch_dir("display");
    let environment = environment_handed_to_the_browser(&scratch);

    for (name, expected) in [("DISPLAY", ":99"), ("XDG_SESSION_TYPE", "x11")] {
        let actual = environment
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str());
        assert_eq!(actual, Some(expected), "{name} must reach the browser");
    }
    let _ = fs::remove_dir_all(&scratch);
}
