//! Minimal HTTP client for the m5p authority API (S5 remote mode).
//! std-only, mirrors api_server's protocol; every call has a short timeout
//! so an unreachable authority degrades into a visible error, never a hang.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(8);

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
    let mut stream = TcpStream::connect(host_port).map_err(|e| format!("api connect {host_port}: {e}"))?;
    stream.set_read_timeout(Some(TIMEOUT)).ok();
    stream.set_write_timeout(Some(TIMEOUT)).ok();
    let mut req = format!(
        "{method} {path_and_query} HTTP/1.1\r\nHost: {host_port}\r\nConnection: close\r\n"
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
    let text = String::from_utf8_lossy(&raw);
    let body_start = text.find("\r\n\r\n").ok_or("api response: no header end")?;
    let status_line = text.lines().next().unwrap_or_default();
    let status: u16 = status_line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let resp_body = &text[body_start + 4..];
    let value: serde_json::Value = serde_json::from_str(resp_body.trim_start()).map_err(|e| format!("api parse: {e}"))?;
    if status >= 400 {
        return Err(format!("api {status}: {}", value.get("error").and_then(|x| x.as_str()).unwrap_or("?")));
    }
    Ok(value)
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
