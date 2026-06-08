#![deny(warnings)]

use crate::error::{Result, TransportError};
use tokio::io::{self, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Stdin, Stdout};

/// Upper bound on a single `Content-Length`-framed message, in bytes.
///
/// Why: the body length is peer-supplied and we allocate it up front, so an
/// unbounded value (`Content-Length: 999999999999`) would let a peer trigger a
/// multi-GB allocation and OOM the process. 16 MiB is far larger than any
/// legitimate JSON-RPC message yet small enough to refuse before allocating.
const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StdioFraming {
    /// Detect framing based on the first successfully read message.
    Auto,
    /// Newline-delimited JSON messages (legacy/simple mode).
    Newline,
    /// JSON-RPC/LSP style framing: `Content-Length: N\r\n\r\n<json bytes>`
    ContentLength,
}

fn trim_crlf(s: &str) -> &str {
    s.trim_end_matches(&['\r', '\n'][..])
}

fn parse_content_length_header(line: &str) -> Option<usize> {
    let line = trim_crlf(line).trim();
    let (name, value) = line.split_once(':')?;
    if !name.trim().eq_ignore_ascii_case("content-length") {
        return None;
    }
    let value = value.trim();
    value.parse::<usize>().ok()
}

/// Parse a `Content-Length` header and enforce [`MAX_MESSAGE_BYTES`].
///
/// Returns the validated body length, or a [`TransportError::InvalidMessage`]
/// when the header is absent/malformed or the length exceeds the cap. The cap
/// is enforced *before* any buffer of that size is allocated, so a hostile
/// `Content-Length` cannot trigger an out-of-memory allocation.
fn checked_content_length(first: &str) -> std::result::Result<usize, TransportError> {
    let content_length = parse_content_length_header(first).ok_or_else(|| {
        TransportError::InvalidMessage(format!("Expected Content-Length header, got: {}", first))
    })?;
    if content_length > MAX_MESSAGE_BYTES {
        return Err(TransportError::InvalidMessage(format!(
            "Content-Length {} exceeds maximum of {} bytes",
            content_length, MAX_MESSAGE_BYTES
        )));
    }
    Ok(content_length)
}

/// STDIN/STDOUT transport for MCP
pub struct StdioTransportHandler {
    stdin: BufReader<Stdin>,
    stdout: Stdout,
    framing: StdioFraming,
}

impl Default for StdioTransportHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl StdioTransportHandler {
    /// Create a new STDIN/STDOUT transport handler
    pub fn new() -> Self {
        Self {
            stdin: BufReader::new(io::stdin()),
            stdout: io::stdout(),
            framing: StdioFraming::Auto,
        }
    }

    /// Read a JSON-RPC message from stdin.
    pub async fn read_message(&mut self) -> Result<String> {
        match self.framing {
            StdioFraming::Auto => self.read_message_auto().await,
            StdioFraming::Newline => self.read_message_newline().await,
            StdioFraming::ContentLength => self.read_message_content_length().await,
        }
    }

    /// Write a JSON-RPC message to stdout, using the detected framing mode.
    pub async fn write_message(&mut self, message: &str) -> Result<()> {
        match self.framing {
            StdioFraming::ContentLength => self.write_message_content_length(message).await,
            StdioFraming::Auto | StdioFraming::Newline => self.write_message_newline(message).await,
        }
    }

    async fn write_message_newline(&mut self, message: &str) -> Result<()> {
        self.stdout
            .write_all(message.as_bytes())
            .await
            .map_err(TransportError::Io)?;
        self.stdout
            .write_all(b"\n")
            .await
            .map_err(TransportError::Io)?;
        self.stdout.flush().await.map_err(TransportError::Io)?;
        Ok(())
    }

    async fn write_message_content_length(&mut self, message: &str) -> Result<()> {
        let bytes = message.as_bytes();
        let header = format!("Content-Length: {}\r\n\r\n", bytes.len());
        self.stdout
            .write_all(header.as_bytes())
            .await
            .map_err(TransportError::Io)?;
        self.stdout
            .write_all(bytes)
            .await
            .map_err(TransportError::Io)?;
        self.stdout.flush().await.map_err(TransportError::Io)?;
        Ok(())
    }

    async fn read_message_newline(&mut self) -> Result<String> {
        let mut line = String::new();
        let n = self
            .stdin
            .read_line(&mut line)
            .await
            .map_err(TransportError::Io)?;
        if n == 0 {
            return Err(TransportError::ConnectionClosed.into());
        }
        Ok(trim_crlf(&line).to_string())
    }

    async fn read_message_auto(&mut self) -> Result<String> {
        loop {
            let mut line = String::new();
            let n = self
                .stdin
                .read_line(&mut line)
                .await
                .map_err(TransportError::Io)?;
            if n == 0 {
                return Err(TransportError::ConnectionClosed.into());
            }

            let line_trimmed = trim_crlf(&line);
            if line_trimmed.trim().is_empty() {
                continue;
            }

            if parse_content_length_header(line_trimmed).is_some() {
                self.framing = StdioFraming::ContentLength;
                return self
                    .read_message_content_length_with_first_line(line_trimmed)
                    .await;
            }

            self.framing = StdioFraming::Newline;
            return Ok(line_trimmed.to_string());
        }
    }

    async fn read_message_content_length(&mut self) -> Result<String> {
        let mut first = String::new();
        let n = self
            .stdin
            .read_line(&mut first)
            .await
            .map_err(TransportError::Io)?;
        if n == 0 {
            return Err(TransportError::ConnectionClosed.into());
        }
        let first = trim_crlf(&first);
        self.read_message_content_length_with_first_line(first)
            .await
    }

    async fn read_message_content_length_with_first_line(&mut self, first: &str) -> Result<String> {
        // Parses and bounds the length before we allocate (OOM guard).
        let content_length = checked_content_length(first)?;

        loop {
            let mut header_line = String::new();
            let n = self
                .stdin
                .read_line(&mut header_line)
                .await
                .map_err(TransportError::Io)?;
            if n == 0 {
                return Err(TransportError::ConnectionClosed.into());
            }
            let header_line = trim_crlf(&header_line);
            if header_line.is_empty() {
                break;
            }
        }

        let mut buf = vec![0u8; content_length];
        self.stdin
            .read_exact(&mut buf)
            .await
            .map_err(TransportError::Io)?;

        let s = String::from_utf8(buf).map_err(|e| {
            TransportError::InvalidMessage(format!("Invalid UTF-8 in JSON-RPC message: {}", e))
        })?;
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_content_length_accepts_normal_frame() {
        assert_eq!(
            checked_content_length("Content-Length: 42").expect("within cap"),
            42
        );
        // At the cap exactly is allowed.
        let at_cap = format!("Content-Length: {}", MAX_MESSAGE_BYTES);
        assert_eq!(
            checked_content_length(&at_cap).expect("at cap"),
            MAX_MESSAGE_BYTES
        );
    }

    #[test]
    fn checked_content_length_rejects_oversized_frame() {
        // A hostile length must be refused *before* any buffer is allocated.
        let huge = format!("Content-Length: {}", MAX_MESSAGE_BYTES + 1);
        let err = checked_content_length(&huge).expect_err("over cap must error");
        assert!(
            matches!(err, TransportError::InvalidMessage(_)),
            "expected InvalidMessage, got {err:?}"
        );

        let absurd = "Content-Length: 999999999999";
        assert!(checked_content_length(absurd).is_err());
    }

    #[test]
    fn checked_content_length_rejects_missing_header() {
        assert!(checked_content_length("Not-A-Header: 5").is_err());
    }
}
