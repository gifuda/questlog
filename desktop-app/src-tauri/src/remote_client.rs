//! Minimal HTTP client for the m5p authority API (S5 remote mode).
//! std-only, mirrors api_server's protocol; every call has a short timeout
//! so an unreachable authority degrades into a visible error, never a hang.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(8);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Bound the connect phase: an unresponsive tunnel endpoint must fail in
/// seconds, not the OS default ~75s (this path is on the synchronous
/// command/startup path, so an unbounded connect freezes the whole UI).
/// The cap covers connect only — DNS resolution of a hostname api_base is
/// not bounded (std has no resolving timeout); deploy api_base as an IP.
/// Every resolved address is tried (connect_timeout takes a single addr —
/// taking only the first would drop the v4 fallback on AAAA-first hosts).
fn connect_bounded(host_port: &str) -> Result<TcpStream, String> {
    let addrs: Vec<SocketAddr> = host_port
        .to_socket_addrs()
        .map_err(|e| format!("api resolve {host_port}: {e}"))?
        .collect();
    let mut last_err = format!("api connect {host_port}: no addresses");
    for addr in &addrs {
        match TcpStream::connect_timeout(addr, CONNECT_TIMEOUT) {
            Ok(stream) => return Ok(stream),
            Err(e) => last_err = format!("api connect {host_port} ({addr}): {e}"),
        }
    }
    Err(last_err)
}

/// Remote API mode is active: mirror marker present AND it carries an api_base.
pub fn enabled() -> bool {
    crate::data_root::is_remote() && crate::data_root::api_base().is_some()
}

fn base() -> Result<String, String> {
    crate::data_root::api_base()
        .map(str::to_string)
        .ok_or_else(|| "remote mode has no api_base configured".to_string())
}

/// Percent-encode a query value (UTF-8 → %XX; unreserved chars pass).
pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn request(method: &str, path_and_query: &str, body: Option<&serde_json::Value>) -> Result<serde_json::Value, String> {
    let base = base()?;
    let url = base.trim_end_matches('/');
    let (host_port, _scheme) = match url.strip_prefix("http://") {
        Some(rest) => (rest, ()),
        None => return Err(format!("unsupported api_base (need http://): {url}")),
    };
    let payload = body.map(serde_json::to_vec).transpose().map_err(|e| format!("serialize: {e}"))?;
    let mut stream = connect_bounded(host_port)?;
    stream.set_read_timeout(Some(TIMEOUT)).ok();
    stream.set_write_timeout(Some(TIMEOUT)).ok();
    let mut req = format!(
        "{method} {path_and_query} HTTP/1.1\r\nHost: {host_port}\r\nConnection: close\r\nAccept-Encoding: gzip\r\n"
    );
    if let Some(p) = &payload {
        req.push_str("Content-Type: application/json\r\n");
        req.push_str(&format!("Content-Length: {}\r\n", p.len()));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).map_err(|e| format!("api write: {e}"))?;
    if let Some(p) = &payload {
        stream.write_all(p).map_err(|e| format!("api write body: {e}"))?;
    }
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).map_err(|e| format!("api read: {e}"))?;
    // Byte-level split: a gzip body is binary and from_utf8_lossy would
    // mangle it before we ever see the Content-Encoding header.
    let header_end = find_subslice(&raw, b"\r\n\r\n").ok_or("api response: no header end")?;
    let headers = String::from_utf8_lossy(&raw[..header_end]);
    let status_line = headers.lines().next().unwrap_or_default();
    let status: u16 = status_line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let gzipped = headers.lines().any(|l| {
        l.split_once(':')
            .map(|(n, v)| n.trim().eq_ignore_ascii_case("content-encoding") && v.trim().eq_ignore_ascii_case("gzip"))
            .unwrap_or(false)
    });
    let body_bytes: Vec<u8> = if gzipped {
        let mut dec = flate2::read::GzDecoder::new(&raw[header_end + 4..]);
        let mut out = Vec::new();
        dec.read_to_end(&mut out).map_err(|e| format!("api gunzip: {e}"))?;
        out
    } else {
        raw[header_end + 4..].to_vec()
    };
    let text = String::from_utf8_lossy(&body_bytes);
    let value: serde_json::Value = serde_json::from_str(text.trim_start()).map_err(|e| format!("api parse: {e}"))?;
    if status >= 400 {
        return Err(format!("api {status}: {}", value.get("error").and_then(|x| x.as_str()).unwrap_or("?")));
    }
    Ok(value)
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// GET a JSON resource from the authority API.
pub fn get_json(path_and_query: &str) -> Result<serde_json::Value, String> {
    request("GET", path_and_query, None)
}

/// POST a JSON body to the authority API and return its response.
pub fn post_json(path_and_query: &str, body: &serde_json::Value) -> Result<serde_json::Value, String> {
    request("POST", path_and_query, Some(body))
}

/// Deserialize an API payload into the local command's return type.
pub fn get_into<T: serde::de::DeserializeOwned>(path_and_query: &str) -> Result<T, String> {
    let v = get_json(path_and_query)?;
    serde_json::from_value(v).map_err(|e| format!("api shape mismatch: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_literal_resolves_to_single_addr() {
        let addrs: Vec<SocketAddr> = "127.0.0.1:15889".to_socket_addrs().unwrap().collect();
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0].port(), 15889);
    }

    #[test]
    fn refused_connect_is_bounded_error_not_hang() {
        // Grab an unused loopback port, release it, then connect — refused
        // (or any error) must come back promptly, never block.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let start = std::time::Instant::now();
        let res = connect_bounded(&format!("127.0.0.1:{port}"));
        assert!(res.is_err());
        assert!(start.elapsed() < CONNECT_TIMEOUT + Duration::from_secs(1));
    }

    #[test]
    fn unresolvable_host_is_error() {
        assert!(connect_bounded("not a host port").is_err());
    }
}
