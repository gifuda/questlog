//! Minimal HTTP API for the headless runtime (S3).
//!
//! Serves the remote read + control surface that the M4 GUI consumes from
//! S5 on, plus the remote kill-switch entry. std-only (no framework): a
//! thread-per-connection HTTP/1.1 server with close semantics — single
//! user on a tailnet, the ACL is the auth boundary (sync-server :5888
//! precedent). Default bind is localhost so the surface stays inert until
//! S4 points it at the tailnet interface.
//!
//! All writes go through the same in-process store locks as the GUI
//! command layer — single-writer discipline is unchanged.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 1024 * 1024;

static STARTED: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

pub fn serve(bind: &str) -> Result<(), String> {
    let listener = TcpListener::bind(bind).map_err(|e| format!("bind {bind}: {e}"))?;
    let _ = STARTED.set(std::time::Instant::now());
    log::info!("[api] listening on http://{bind}");
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                std::thread::spawn(move || handle(s));
            }
            Err(e) => log::warn!("[api] accept error: {e}"),
        }
    }
    Ok(())
}

struct Request {
    method: String,
    path: String,
    query: Vec<(String, String)>,
    content_type: Option<String>,
    body: Vec<u8>,
}

fn handle(mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
    let Some(req) = read_request(&mut stream) else {
        return;
    };
    // POST endpoints are JSON-only: this also blocks browser-origin
    // simple-request forgeries (sendBeacon/form posts are text/plain or
    // urlencoded and get rejected without ever reaching a route; a JSON
    // cross-origin fetch needs a CORS preflight this server never grants).
    let (code, body) = if req.method == "POST"
        && !req
            .content_type
            .as_deref()
            .unwrap_or("")
            .contains("application/json")
    {
        (415, serde_json::json!({ "error": "Content-Type must be application/json" }))
    } else {
        route(&req)
    };
    let payload = serde_json::to_vec(&body).unwrap_or_else(|_| b"{}".to_vec());
    let status = match code {
        200 => "200 OK",
        400 => "400 Bad Request",
        404 => "404 Not Found",
        405 => "405 Method Not Allowed",
        415 => "415 Unsupported Media Type",
        _ => "500 Internal Server Error",
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    let _ = stream.write_all(&payload);
}

/// Read one request (headers + Content-Length body). Returns None on
/// malformed input; the connection is simply dropped.
fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];
    // Read until end of headers.
    let header_end = loop {
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            break pos;
        }
        if buf.len() > MAX_HEADER_BYTES {
            return None;
        }
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let header_str = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = header_str.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();

    let mut content_length = 0usize;
    let mut content_type: Option<String> = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            } else if name.trim().eq_ignore_ascii_case("content-type") {
                content_type = Some(value.trim().to_string());
            }
        }
    }
    if content_length > MAX_BODY_BYTES {
        return None;
    }
    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(content_length);

    let (path, query_str) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };
    let query = query_str
        .split('&')
        .filter(|s| !s.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((k, v)) => (url_decode(k), url_decode(v)),
            None => (url_decode(pair), String::new()),
        })
        .collect();
    Some(Request {
        method,
        path,
        query,
        content_type,
        body,
    })
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                match std::str::from_utf8(&bytes[i + 1..i + 3])
                    .ok()
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                {
                    Some(b) => {
                        out.push(b);
                        i += 3;
                    }
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn query_get(req: &Request, key: &str) -> Option<String> {
    req.query
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
}

/// Registry allowlist for path-taking read endpoints — the API must not
/// become an arbitrary-path file reader (audit M7).
fn project_allowed(project_path: &str) -> bool {
    crate::discovery::discover_projects()
        .iter()
        .any(|(_, path)| path.to_string_lossy() == project_path)
}

fn ok(v: serde_json::Value) -> (u16, serde_json::Value) {
    (200, v)
}
fn err(status: u16, msg: impl Into<String>) -> (u16, serde_json::Value) {
    (status, serde_json::json!({ "error": msg.into() }))
}
fn map_store<T>(r: Result<T, String>) -> (u16, serde_json::Value)
where
    T: serde::Serialize,
{
    match r {
        Ok(v) => ok(serde_json::to_value(v).unwrap_or_default()),
        Err(e) => err(409, e),
    }
}

fn parse_body(req: &Request) -> Result<serde_json::Value, (u16, serde_json::Value)> {
    if req.body.is_empty() {
        return Err(err(400, "empty body"));
    }
    serde_json::from_slice(&req.body).map_err(|e| err(400, format!("bad JSON body: {e}")))
}

// ── S5: body-field extraction helpers for the write routes ──

/// Optional string field.
fn s<'a>(v: &'a serde_json::Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(|x| x.as_str())
}
/// Optional owned string field.
fn os(v: &serde_json::Value, k: &str) -> Option<String> {
    s(v, k).map(|x| x.to_string())
}
/// Optional boolean field.
fn ob(v: &serde_json::Value, k: &str) -> Option<bool> {
    v.get(k).and_then(|x| x.as_bool())
}
/// Optional string-array field.
fn ovec(v: &serde_json::Value, k: &str) -> Option<Vec<String>> {
    v.get(k)
        .and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
}
/// Required string field — 400 with the exact missing key name.
fn need<'a>(v: &'a serde_json::Value, k: &str) -> Result<&'a str, (u16, serde_json::Value)> {
    s(v, k).ok_or_else(|| err(400, format!("body needs '{k}'")))
}

fn route(req: &Request) -> (u16, serde_json::Value) {
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/health") => ok(serde_json::json!({
            "ok": true,
            "pid": std::process::id(),
            "uptime_secs": STARTED.get().map(|t| t.elapsed().as_secs()).unwrap_or(0),
            "runner_running": crate::runner::is_running(),
            "runner": crate::runner::runtime_status(),
            "remote_mode": crate::data_root::is_remote(),
            "todo_schedule": crate::runner_config::get_todo_schedule(),
        })),
        ("GET", "/api/projects") => {
            let projects: Vec<serde_json::Value> = crate::discovery::discover_projects()
                .into_iter()
                .map(|(name, path)| serde_json::json!({ "name": name, "path": path }))
                .collect();
            ok(serde_json::json!({ "projects": projects }))
        }
        ("GET", "/api/todos") => match query_get(req, "project") {
            Some(p) if project_allowed(&p) => {
                map_store(crate::store::read_todos(std::path::Path::new(&p)))
            }
            Some(_) => err(404, "unknown project path (not in registry)"),
            None => err(400, "missing query param: project"),
        },
        ("GET", "/api/epics") => match query_get(req, "project") {
            Some(p) if project_allowed(&p) => {
                map_store(crate::epic_store::read_epics(std::path::Path::new(&p)))
            }
            Some(_) => err(404, "unknown project path (not in registry)"),
            None => err(400, "missing query param: project"),
        },
        ("GET", "/api/executions") => map_store(crate::execution_store::list_executions()),
        ("GET", "/api/decision-requests") => {
            let include_resolved = query_get(req, "include_resolved")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false);
            map_store(crate::decision_store::list_requests(include_resolved))
        }
        ("POST", "/api/decision-requests/resolve") => match parse_body(req) {
            Ok(v) => {
                let id = v.get("id").and_then(|x| x.as_str());
                let resolution = v.get("resolution").and_then(|x| x.as_str());
                match (id, resolution) {
                    // Same contract as the GUI command (§5.1): a resolution
                    // triggers its legal action (retry/cancel/blocked) and a
                    // FAILED action rolls the decision back — never resolved
                    // without its effect (audit M2).
                    (Some(id), Some(res)) => {
                        map_store(crate::commands::resolve_decision_request(
                            id.to_string(),
                            res.to_string(),
                        ))
                    }
                    _ => err(400, "body needs non-empty 'id' and 'resolution'"),
                }
            }
            Err(e) => e,
        },
        ("GET", "/api/fleet-health") => {
            map_store(crate::fleet_health::collect(&crate::discovery::discover_projects()))
        }
        ("GET", "/api/runner/status") => {
            ok(serde_json::to_value(crate::runner::runtime_status()).unwrap_or_default())
        }
        // Remote kill switch: global_enabled=false stops claim + thread.
        ("POST", "/api/runner/global-enabled") => match parse_body(req) {
            Ok(v) => match v.get("enabled").and_then(|x| x.as_bool()) {
                Some(enabled) => match crate::runner_config::set_globally_enabled(enabled) {
                    Ok(()) => ok(serde_json::json!({
                        "enabled": crate::runner_config::is_globally_enabled()
                    })),
                    Err(e) => err(409, e),
                },
                None => err(400, "body needs boolean 'enabled'"),
            },
            Err(e) => e,
        },
        ("POST", "/api/runner/project-enabled") => match parse_body(req) {
            Ok(v) => {
                let path = v.get("project_path").and_then(|x| x.as_str());
                let enabled = v.get("enabled").and_then(|x| x.as_bool());
                match (path, enabled) {
                    (Some(p), Some(e)) => {
                        map_store(crate::runner_config::set_project_enabled(p, e).map(|_| {
                            serde_json::json!({ "project_path": p, "enabled": e })
                        }))
                    }
                    _ => err(400, "body needs 'project_path' and boolean 'enabled'"),
                }
            }
            Err(e) => e,
        },
        ("POST", "/api/executions/retry") => match parse_body(req) {
            Ok(v) => match v.get("id").and_then(|x| x.as_str()) {
                Some(id) => map_store(crate::execution_store::create_retry_attempt(id)),
                None => err(400, "body needs non-empty 'id'"),
            },
            Err(e) => e,
        },
        ("POST", "/api/executions/control") => match parse_body(req) {
            Ok(v) => {
                let id = v.get("id").and_then(|x| x.as_str());
                let pause = v.get("pause").and_then(|x| x.as_bool());
                match (id, pause) {
                    (Some(id), Some(pause)) => map_store(
                        crate::execution_store::request_control(id, pause).map(|_| {
                            serde_json::json!({ "id": id, "pause": pause })
                        }),
                    ),
                    _ => err(400, "body needs 'id' and boolean 'pause'"),
                }
            }
            Err(e) => e,
        },
        // ── S5: M4 client read surface (thin wraps of the commands' _impl
        // cores — same store locks as the GUI command layer). ──
        ("GET", "/api/trash") => map_store(crate::commands::get_trash()),
        ("GET", "/api/all-active") => map_store(crate::commands::get_all_active_impl(
            &crate::discovery::discover_projects(),
        )),
        ("GET", "/api/epics-global") => map_store(crate::commands::get_epics_global_impl(
            &crate::discovery::discover_projects(),
        )),
        ("GET", "/api/project-items") => match query_get(req, "project") {
            Some(p) if project_allowed(&p) => map_store(
                crate::commands::get_project_items_impl(&crate::discovery::discover_projects(), p),
            ),
            Some(_) => err(404, "unknown project path (not in registry)"),
            None => err(400, "missing query param: project"),
        },
        ("GET", "/api/epic-auto-execute") => {
            match (query_get(req, "epic_id"), query_get(req, "project")) {
                (Some(epic_id), Some(project)) if project_allowed(&project) => {
                    map_store(crate::commands::get_epic_auto_execute(epic_id, project))
                }
                (Some(_), Some(_)) => err(404, "unknown project path (not in registry)"),
                _ => err(400, "missing query params: epic_id and project"),
            }
        }
        ("POST", "/api/epic-auto-execute") => match parse_body(req) {
            Ok(v) => match (need(&v, "epic_id"), need(&v, "project_path")) {
                (Ok(epic_id), Ok(project_path)) if project_allowed(project_path) => map_store(
                    crate::commands::set_epic_auto_execute(
                        epic_id.to_string(),
                        project_path.to_string(),
                        ob(&v, "auto_execute"),
                    ),
                ),
                (Ok(_), Ok(_)) => err(404, "unknown project path (not in registry)"),
                (Err(e), _) | (_, Err(e)) => e,
            },
            Err(e) => e,
        },
        ("GET", "/api/runner-project-status") => match query_get(req, "project") {
            Some(p) => map_store(crate::commands::get_runner_project_status(p)),
            None => err(400, "missing query param: project"),
        },
        ("GET", "/api/executor-options") => map_store(crate::commands::get_executor_options()),
        ("GET", "/api/todo-counts") => {
            let mut counts = Vec::new();
            let mut total: u64 = 0;
            for (name, path) in crate::discovery::discover_projects() {
                let active = crate::store::read_todos(&path)
                    .map(|d| {
                        d.items
                            .iter()
                            .filter(|i| {
                                i.status == crate::models::ItemStatus::Active
                                    && i.item_type == crate::models::ItemType::Todo
                            })
                            .count() as u64
                    })
                    .unwrap_or(0);
                total += active;
                counts.push(serde_json::json!({
                    "name": name,
                    "path": path.to_string_lossy(),
                    "active": active
                }));
            }
            ok(serde_json::json!({ "counts": counts, "total": total }))
        }
        ("GET", "/api/todo-status") => map_store(crate::commands::get_todo_status_impl(
            &crate::discovery::discover_projects(),
            query_get(req, "project"),
        )),
        // ── S5: M4 client write surface (thin wraps of the _impl cores). ──
        ("POST", "/api/todos/add") => match parse_body(req) {
            Ok(v) => match (need(&v, "project_path"), need(&v, "item_type"), need(&v, "title")) {
                (Ok(pp), Ok(item_type), Ok(title)) => {
                    map_store(crate::commands::add_todo_impl(
                        &crate::discovery::discover_projects(),
                        pp.to_string(),
                        item_type.to_string(),
                        title.to_string(),
                        os(&v, "content"),
                        os(&v, "suggestion"),
                        ovec(&v, "tags"),
                        os(&v, "priority"),
                    ))
                }
                (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => e,
            },
            Err(e) => e,
        },
        ("POST", "/api/todos/update") => match parse_body(req) {
            Ok(v) => match (need(&v, "project_path"), need(&v, "id")) {
                (Ok(pp), Ok(id)) => map_store(crate::commands::update_todo_impl(
                    &crate::discovery::discover_projects(),
                    pp.to_string(),
                    id.to_string(),
                    os(&v, "status"),
                    os(&v, "content"),
                    ovec(&v, "tags"),
                    os(&v, "priority"),
                    os(&v, "title"),
                    os(&v, "item_type"),
                    ob(&v, "processing"),
                    ob(&v, "auto_process"),
                    ob(&v, "auto_ack"),
                )),
                (Err(e), _) | (_, Err(e)) => e,
            },
            Err(e) => e,
        },
        ("POST", "/api/epics/add") => match parse_body(req) {
            Ok(v) => match (need(&v, "project_path"), need(&v, "title")) {
                (Ok(pp), Ok(title)) => map_store(crate::commands::add_epic_impl(
                    &crate::discovery::discover_projects(),
                    pp.to_string(),
                    title.to_string(),
                    os(&v, "description"),
                    os(&v, "priority"),
                    ovec(&v, "depends_on"),
                    os(&v, "parent_id"),
                    os(&v, "acceptance"),
                )),
                (Err(e), _) | (_, Err(e)) => e,
            },
            Err(e) => e,
        },
        ("POST", "/api/epics/update") => match parse_body(req) {
            Ok(v) => match (need(&v, "project_path"), need(&v, "id")) {
                (Ok(pp), Ok(id)) => map_store(crate::commands::update_epic_impl(
                    &crate::discovery::discover_projects(),
                    pp.to_string(),
                    id.to_string(),
                    os(&v, "title"),
                    os(&v, "description"),
                    os(&v, "status"),
                    os(&v, "priority"),
                    ovec(&v, "depends_on"),
                    os(&v, "links_doc"),
                    os(&v, "links_branch"),
                    os(&v, "links_note"),
                    os(&v, "acceptance"),
                    os(&v, "close_reason"),
                )),
                (Err(e), _) | (_, Err(e)) => e,
            },
            Err(e) => e,
        },
        ("POST", "/api/epics/delete") => match parse_body(req) {
            Ok(v) => match (need(&v, "project_path"), need(&v, "id")) {
                (Ok(pp), Ok(id)) => map_store(crate::commands::delete_epic_impl(
                    &crate::discovery::discover_projects(),
                    pp.to_string(),
                    id.to_string(),
                )),
                (Err(e), _) | (_, Err(e)) => e,
            },
            Err(e) => e,
        },
        ("POST", "/api/steps/add") => match parse_body(req) {
            Ok(v) => {
                match (need(&v, "project_path"), need(&v, "epic_id"), need(&v, "title")) {
                    (Ok(pp), Ok(epic_id), Ok(title)) => {
                        map_store(crate::commands::add_step_impl(
                            &crate::discovery::discover_projects(),
                            pp.to_string(),
                            epic_id.to_string(),
                            title.to_string(),
                            os(&v, "description"),
                            ovec(&v, "depends_on"),
                        ))
                    }
                    (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => e,
                }
            }
            Err(e) => e,
        },
        ("POST", "/api/steps/update") => match parse_body(req) {
            Ok(v) => match (need(&v, "project_path"), need(&v, "id")) {
                (Ok(pp), Ok(id)) => map_store(crate::commands::update_step_impl(
                    &crate::discovery::discover_projects(),
                    pp.to_string(),
                    id.to_string(),
                    os(&v, "title"),
                    os(&v, "description"),
                    os(&v, "stage"),
                    v.get("order").and_then(|x| x.as_u64()).map(|o| o as u32),
                    ovec(&v, "depends_on"),
                    os(&v, "todo_ref"),
                )),
                (Err(e), _) | (_, Err(e)) => e,
            },
            Err(e) => e,
        },
        ("POST", "/api/steps/delete") => match parse_body(req) {
            Ok(v) => match (need(&v, "project_path"), need(&v, "id")) {
                (Ok(pp), Ok(id)) => map_store(crate::commands::delete_step_impl(
                    &crate::discovery::discover_projects(),
                    pp.to_string(),
                    id.to_string(),
                )),
                (Err(e), _) | (_, Err(e)) => e,
            },
            Err(e) => e,
        },
        (m, p) if p.starts_with("/api/") || p == "/health" => {
            err(405, format!("method {m} not allowed for {p}"))
        }
        _ => err(404, "no route"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_header_boundary() {
        assert_eq!(find_subslice(b"abc\r\n\r\nbody", b"\r\n\r\n"), Some(3));
        assert_eq!(find_subslice(b"abcdef", b"\r\n\r\n"), None);
    }

    #[test]
    fn decodes_percent_and_plus() {
        assert_eq!(url_decode("a%2Fb+c"), "a/b c");
        assert_eq!(url_decode("%E6%B8%85%E5%8D%95"), "清单");
    }

    #[test]
    fn route_health_is_get_only() {
        let req = Request {
            method: "POST".into(),
            path: "/health".into(),
            query: vec![],
            content_type: None,
            body: vec![],
        };
        let (code, _) = route(&req);
        assert_eq!(code, 405);
    }

    #[test]
    fn route_unknown_404() {
        let req = Request {
            method: "GET".into(),
            path: "/nope".into(),
            query: vec![],
            content_type: None,
            body: vec![],
        };
        let (code, body) = route(&req);
        assert_eq!(code, 404);
        assert!(body.get("error").is_some());
    }

    #[test]
    fn route_todos_requires_project() {
        let req = Request {
            method: "GET".into(),
            path: "/api/todos".into(),
            query: vec![],
            content_type: None,
            body: vec![],
        };
        let (code, _) = route(&req);
        assert_eq!(code, 400);
    }

    #[test]
    fn route_todos_rejects_unknown_project() {
        let req = Request {
            method: "GET".into(),
            path: "/api/todos".into(),
            query: vec![("project".into(), "/definitely/not/registered".into())],
            content_type: None,
            body: vec![],
        };
        let (code, body) = route(&req);
        assert_eq!(code, 404);
        assert!(body.get("error").is_some());
    }
}
