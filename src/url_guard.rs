#![deny(warnings)]

// SSRF guard for the browsing tools.
//
// A headless browser that will fetch any URL an LLM hands it is a classic
// server-side request forgery (SSRF) vector: it could be steered at
// `http://localhost:…`, a private LAN host, or a cloud metadata endpoint
// (`169.254.169.254`). Before navigating, we parse the URL, require an
// http(s) scheme, resolve the host, and refuse any address in a
// loopback/private/link-local/unique-local range — unless the operator has
// explicitly opted into private hosts.
//
// This is best-effort: a hostname could resolve to a public IP here and a
// private one when Chrome later resolves it (DNS rebinding). It nonetheless
// blocks the overwhelmingly common cases (literal private IPs, `localhost`,
// and names that already resolve internally).

use crate::error::{Result, WebError};
use std::net::IpAddr;
use url::Url;

/// Validates outbound browsing URLs against the SSRF policy.
#[derive(Debug, Clone)]
pub struct UrlGuard {
    allow_private: bool,
}

impl UrlGuard {
    /// Create a guard. When `allow_private` is true the guard only enforces the
    /// scheme check and lets private/loopback hosts through.
    pub fn new(allow_private: bool) -> Self {
        Self { allow_private }
    }

    /// Parse and validate `raw`. Returns the normalized [`Url`] on success, or a
    /// [`WebError::Blocked`] / [`WebError::InvalidParameters`] otherwise.
    pub async fn check(&self, raw: &str) -> Result<Url> {
        let url = Url::parse(raw)
            .map_err(|e| WebError::InvalidParameters(format!("invalid URL '{}': {}", raw, e)))?;

        match url.scheme() {
            "http" | "https" => {}
            other => {
                return Err(WebError::InvalidParameters(format!(
                    "unsupported URL scheme '{}': only http and https are allowed",
                    other
                ))
                .into());
            }
        }

        let host = url
            .host_str()
            .ok_or_else(|| WebError::InvalidParameters(format!("URL has no host: {}", raw)))?;

        if self.allow_private {
            return Ok(url);
        }

        // Literal IP host: check directly, no DNS.
        if let Ok(ip) = host.parse::<IpAddr>() {
            if ip_is_blocked(ip) {
                return Err(WebError::Blocked(format!(
                    "host {} is a private/loopback/link-local address",
                    ip
                ))
                .into());
            }
            return Ok(url);
        }

        // `localhost` and friends never legitimately point outward.
        let lower = host.to_ascii_lowercase();
        if lower == "localhost" || lower.ends_with(".localhost") {
            return Err(WebError::Blocked(format!("host '{}' is local", host)).into());
        }

        // Resolve the name and reject if ANY resolved address is internal.
        let port = url.port_or_known_default().unwrap_or(443);
        let addrs = tokio::net::lookup_host((host, port)).await.map_err(|e| {
            WebError::Navigation(format!("could not resolve host '{}': {}", host, e))
        })?;
        let mut saw_any = false;
        for addr in addrs {
            saw_any = true;
            if ip_is_blocked(addr.ip()) {
                return Err(WebError::Blocked(format!(
                    "host '{}' resolves to internal address {}",
                    host,
                    addr.ip()
                ))
                .into());
            }
        }
        if !saw_any {
            return Err(WebError::Navigation(format!("host '{}' did not resolve", host)).into());
        }

        Ok(url)
    }
}

/// True if `ip` is in a range a browsing tool must not reach: loopback,
/// unspecified, private, link-local, or IPv6 unique-local.
fn ip_is_blocked(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                // 100.64.0.0/10 carrier-grade NAT (shared address space).
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 0x40)
        }
        IpAddr::V6(v6) => {
            // An IPv4-mapped address (::ffff:a.b.c.d) must be judged by its v4 value.
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return ip_is_blocked(IpAddr::V4(mapped));
            }
            let seg = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                // fe80::/10 link-local
                || (seg[0] & 0xffc0) == 0xfe80
                // fc00::/7 unique-local
                || (seg[0] & 0xfe00) == 0xfc00
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_internal_v4() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.5.5",
            "192.168.0.1",
            "169.254.169.254", // cloud metadata
            "0.0.0.0",
            "100.64.0.1",
        ] {
            assert!(ip_is_blocked(ip.parse().unwrap()), "{ip} should be blocked");
        }
    }

    #[test]
    fn allows_public_v4() {
        for ip in ["1.1.1.1", "8.8.8.8", "93.184.216.34"] {
            assert!(
                !ip_is_blocked(ip.parse().unwrap()),
                "{ip} should be allowed"
            );
        }
    }

    #[test]
    fn blocks_internal_v6() {
        for ip in ["::1", "fe80::1", "fc00::1", "::ffff:127.0.0.1", "::"] {
            assert!(ip_is_blocked(ip.parse().unwrap()), "{ip} should be blocked");
        }
        assert!(!ip_is_blocked("2606:4700:4700::1111".parse().unwrap()));
    }

    #[tokio::test]
    async fn rejects_non_http_scheme() {
        let guard = UrlGuard::new(false);
        assert!(guard.check(" file:///etc/passwd").await.is_err());
        assert!(guard.check("ftp://example.com").await.is_err());
    }

    #[tokio::test]
    async fn rejects_literal_localhost() {
        let guard = UrlGuard::new(false);
        assert!(guard.check("http://127.0.0.1:8080/").await.is_err());
        assert!(guard.check("http://localhost/").await.is_err());
    }

    #[tokio::test]
    async fn allow_private_bypasses_ip_check() {
        let guard = UrlGuard::new(true);
        assert!(guard.check("http://127.0.0.1:8080/").await.is_ok());
        // scheme is still enforced
        assert!(guard.check("file:///etc/passwd").await.is_err());
    }
}
