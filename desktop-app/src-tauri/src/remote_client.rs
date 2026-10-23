//! Minimal HTTP client for the m5p authority API (S5 remote mode).
//! std-only, mirrors api_server's protocol; every call has a short timeout
//! so an unreachable authority degrades into a visible error, never a hang.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(8);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const TASK_TIMEOUT: Duration = Duration::from_secs(5);
const TASK_MAX_HEADER_BYTES: usize = 16 * 1024;
const TASK_MAX_BODY_BYTES: usize = 16 * 1024;

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
    request_with_read_cap(method, path_and_query, body, TIMEOUT)
}

fn request_with_read_cap(method: &str, path_and_query: &str, body: Option<&serde_json::Value>, io_timeout: Duration) -> Result<serde_json::Value, String> {
    let base = base()?;
    let url = base.trim_end_matches('/');
    let (host_port, _scheme) = match url.strip_prefix("http://") {
        Some(rest) => (rest, ()),
        None => return Err(format!("unsupported api_base (need http://): {url}")),
    };
    let payload = body.map(serde_json::to_vec).transpose().map_err(|e| format!("serialize: {e}"))?;
    let mut stream = connect_bounded(host_port)?;
    stream.set_read_timeout(Some(io_timeout)).ok();
    stream.set_write_timeout(Some(io_timeout)).ok();
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

/// GET with a shorter read/write cap — for the health probe, which must
/// report "offline" faster than the data plane's 8s.
pub fn get_json_timeout(path_and_query: &str, cap: Duration) -> Result<serde_json::Value, String> {
    request_with_read_cap("GET", path_and_query, None, cap)
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

/// Dedicated bounded client for the task-maintenance route. It deliberately
/// avoids the general read_to_end path used by larger legacy API payloads.
pub fn get_task_maintenance() -> serde_json::Value {
    match base().map_err(|_| ()).and_then(|value| fetch_task_maintenance_from(&value, TASK_TIMEOUT)) {
        Ok(value) => value,
        Err(()) => crate::task_maintenance::unknown(),
    }
}

fn fetch_task_maintenance_from(api_base: &str, timeout: Duration) -> Result<serde_json::Value, ()> {
    let endpoint = api_base.strip_prefix("http://").ok_or(())?.trim_end_matches('/');
    if endpoint.contains('/') || endpoint.contains('?') || endpoint.contains('#') {
        return Err(());
    }
    let address: SocketAddr = endpoint.parse().map_err(|_| ())?;
    if !address.ip().is_loopback() || address.port() == 0 {
        return Err(());
    }
    let deadline = Instant::now() + timeout;
    let remaining = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()).ok_or(())?;
    let mut stream = TcpStream::connect_timeout(&address, remaining).map_err(|_| ())?;
    set_remaining_timeouts(&stream, deadline)?;
    stream.write_all(b"GET /api/task-maintenance HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nAccept-Encoding: gzip\r\n\r\n")
        .map_err(|_| ())?;
    let (status, encoding, body) = read_task_response(&mut stream, deadline)?;
    if status != 200 {
        return Err(());
    }
    let decoded = match encoding.as_deref() {
        None | Some("identity") => body,
        Some(value) if value.eq_ignore_ascii_case("gzip") => {
            let decoder = flate2::read::GzDecoder::new(body.as_slice());
            let mut decoded = Vec::new();
            decoder.take((TASK_MAX_BODY_BYTES + 1) as u64).read_to_end(&mut decoded).map_err(|_| ())?;
            if decoded.len() > TASK_MAX_BODY_BYTES {
                return Err(());
            }
            decoded
        }
        _ => return Err(()),
    };
    crate::task_maintenance::validate_snapshot(&decoded, chrono::Utc::now()).map_err(|_| ())
}

fn set_remaining_timeouts(stream: &TcpStream, deadline: Instant) -> Result<(), ()> {
    let remaining = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()).ok_or(())?;
    stream.set_read_timeout(Some(remaining)).map_err(|_| ())?;
    stream.set_write_timeout(Some(remaining)).map_err(|_| ())?;
    Ok(())
}

fn read_task_response(stream: &mut TcpStream, deadline: Instant) -> Result<(u16, Option<String>, Vec<u8>), ()> {
    let mut raw = Vec::with_capacity(2048);
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(end) = find_subslice(&raw, b"\r\n\r\n") {
            if end > TASK_MAX_HEADER_BYTES { return Err(()); }
            break end;
        }
        if raw.len() > TASK_MAX_HEADER_BYTES { return Err(()); }
        set_remaining_timeouts(stream, deadline)?;
        let allowed = (TASK_MAX_HEADER_BYTES + 4).saturating_sub(raw.len()).min(chunk.len());
        let read = stream.read(&mut chunk[..allowed]).map_err(|_| ())?;
        if read == 0 { return Err(()); }
        raw.extend_from_slice(&chunk[..read]);
    };
    let header = std::str::from_utf8(&raw[..header_end]).map_err(|_| ())?;
    let mut lines = header.split("\r\n");
    let status: u16 = lines.next().and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse().ok()).ok_or(())?;
    let mut content_length = None;
    let mut encoding = None;
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(())?;
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            if content_length.is_some() { return Err(()); }
            content_length = Some(value.parse::<usize>().map_err(|_| ())?);
        } else if name.eq_ignore_ascii_case("content-encoding") {
            if encoding.is_some() { return Err(()); }
            encoding = Some(value.to_ascii_lowercase());
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(());
        }
    }
    let content_length = content_length.ok_or(())?;
    if content_length > TASK_MAX_BODY_BYTES { return Err(()); }
    let body_start = header_end + 4;
    let mut body = raw[body_start..].to_vec();
    if body.len() > content_length { return Err(()); }
    while body.len() < content_length {
        set_remaining_timeouts(stream, deadline)?;
        let allowed = (content_length - body.len()).min(chunk.len());
        let read = stream.read(&mut chunk[..allowed]).map_err(|_| ())?;
        if read == 0 { return Err(()); }
        body.extend_from_slice(&chunk[..read]);
    }
    Ok((status, encoding, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread::JoinHandle;

    fn serve_task_response(status: u16, headers: String, body: Vec<u8>, delay: Duration) -> (String, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            if !delay.is_zero() { std::thread::sleep(delay); }
            let response_header = format!("HTTP/1.1 {status} Test\r\n{headers}\r\nConnection: close\r\n\r\n");
            let _ = stream.write_all(response_header.as_bytes());
            let _ = stream.write_all(&body);
        });
        (format!("http://{address}"), server)
    }

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

    #[test]
    fn task_maintenance_reads_only_loopback_and_accepts_bounded_json() {
        let body = serde_json::to_vec(&crate::task_maintenance::unknown()).unwrap();
        let (base, server) = serve_task_response(200, format!("Content-Length: {}\r\nContent-Type: application/json", body.len()), body, Duration::ZERO);
        let result = fetch_task_maintenance_from(&base, Duration::from_secs(2)).unwrap();
        assert_eq!(result["state"], "UNKNOWN");
        server.join().unwrap();
        assert!(fetch_task_maintenance_from("http://example.invalid:15889", Duration::from_secs(1)).is_err());
    }

    #[test]
    fn task_maintenance_rejects_oversized_headers_and_body() {
        let body = serde_json::to_vec(&crate::task_maintenance::unknown()).unwrap();
        let huge_header = format!("Content-Length: {}\r\nX-Pad: {}", body.len(), "x".repeat(TASK_MAX_HEADER_BYTES));
        let (base, server) = serve_task_response(200, huge_header, body.clone(), Duration::ZERO);
        assert!(fetch_task_maintenance_from(&base, Duration::from_secs(2)).is_err());
        server.join().unwrap();

        let (base, server) = serve_task_response(200, format!("Content-Length: {}", TASK_MAX_BODY_BYTES + 1), vec![], Duration::ZERO);
        assert!(fetch_task_maintenance_from(&base, Duration::from_secs(2)).is_err());
        server.join().unwrap();
    }

    #[test]
    fn task_maintenance_rejects_slow_response_and_gzip_expansion() {
        let body = serde_json::to_vec(&crate::task_maintenance::unknown()).unwrap();
        let (base, server) = serve_task_response(200, format!("Content-Length: {}", body.len()), body, Duration::from_millis(250));
        let started = Instant::now();
        assert!(fetch_task_maintenance_from(&base, Duration::from_millis(80)).is_err());
        assert!(started.elapsed() < Duration::from_millis(500));
        server.join().unwrap();

        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&vec![b'x'; TASK_MAX_BODY_BYTES * 8]).unwrap();
        let compressed = encoder.finish().unwrap();
        let (base, server) = serve_task_response(200,
            format!("Content-Length: {}\r\nContent-Encoding: gzip", compressed.len()), compressed, Duration::ZERO);
        assert!(fetch_task_maintenance_from(&base, Duration::from_secs(2)).is_err());
        server.join().unwrap();
    }
}
