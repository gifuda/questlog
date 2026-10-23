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
use std::net::{IpAddr, TcpListener, TcpStream};

const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 1024 * 1024;

static STARTED: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// Concurrent-handler cap: one slow/stalled consumer (or a hanging file path
/// behind it) must never exhaust threads+FDs and take the whole board down
/// (2026-10-14 incident: iCloud-coordinated open() hung every all-projects
/// request until EMFILE). Over cap → immediate 503 + close.
const MAX_HANDLERS: usize = 48;

pub fn serve(bind: &str) -> Result<(), String> {
    let listener = TcpListener::bind(bind).map_err(|e| format!("bind {bind}: {e}"))?;
    let _ = STARTED.set(std::time::Instant::now());
    let active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    log::info!("[api] listening on http://{bind}");
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let now = active.load(std::sync::atomic::Ordering::SeqCst);
                if now >= MAX_HANDLERS {
                    log::warn!("[api] handler cap hit ({now}) — refusing connection");
                    drop(s);
                    continue;
                }
                let _ = s.set_write_timeout(Some(std::time::Duration::from_secs(10)));
                let guard = active.clone();
                guard.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                std::thread::spawn(move || {
                    let _g = HandlerGuard(guard);
                    handle(s);
                });
            }
            Err(e) => log::warn!("[api] accept error: {e}"),
        }
    }
    Ok(())
}

/// Decrements the active-handler counter on scope exit.
struct HandlerGuard(std::sync::Arc<std::sync::atomic::AtomicUsize>);
impl Drop for HandlerGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

struct Request {
    method: String,
    path: String,
    query: Vec<(String, String)>,
    content_type: Option<String>,
    body: Vec<u8>,
    /// Client sent Accept-Encoding containing gzip — response body may be
    /// gzip-encoded (only when it actually pays: see GZIP_MIN_BYTES).
    accept_gzip: bool,
}

fn handle(mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
    let peer = stream.peer_addr().ok().map(|addr| addr.ip());
    let Some(req) = read_request(&mut stream) else {
        return;
    };
    // POST endpoints are JSON-only: this also blocks browser-origin
    // simple-request forgeries (sendBeacon/form posts are text/plain or
    // urlencoded and get rejected without ever reaching a route; a JSON
    // cross-origin fetch needs a CORS preflight this server never grants).
    let (code, body) = if req.path == "/api/task-maintenance" {
        route_for_peer(&req, peer)
    } else if req.method == "POST"
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
    let status = status_line(code);
    // Compress only when the client asked and it actually pays: small
    // payloads gzip to roughly the same size with header overhead.
    let (payload, encoding) = if req.accept_gzip && payload.len() > GZIP_MIN_BYTES {
        match gzip_bytes(&payload) {
            Ok(z) => (z, "gzip"),
            // Compression failure must not fail the response — send identity.
            Err(_) => (payload, ""),
        }
    } else {
        (payload, "")
    };
    let encoding_hdr = if encoding.is_empty() {
        String::new()
    } else {
        format!("Content-Encoding: {encoding}\r\n")
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{encoding_hdr}Content-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    let _ = stream.write_all(&payload);
}

fn status_line(code: u16) -> &'static str {
    match code {
        200 => "200 OK",
        400 => "400 Bad Request",
        403 => "403 Forbidden",
        404 => "404 Not Found",
        405 => "405 Method Not Allowed",
        415 => "415 Unsupported Media Type",
        _ => "500 Internal Server Error",
    }
}

/// The task-maintenance projection is only for the authenticated local SSH
/// tunnel peer. Its fixed endpoint accepts no query or request body.
fn route_for_peer(req: &Request, peer: Option<IpAddr>) -> (u16, serde_json::Value) {
    if req.path != "/api/task-maintenance" {
        return route(req);
    }
    if !peer.map(|addr| addr.is_loopback()).unwrap_or(false) {
        return err(403, "loopback peer required");
    }
    if req.method != "GET" {
        return err(405, "method not allowed");
    }
    if !req.query.is_empty() || !req.body.is_empty() {
        return err(400, "task-maintenance accepts no query or body");
    }
    ok(crate::task_maintenance::get())
}

/// Below this size a gzipped JSON body is not meaningfully smaller.
const GZIP_MIN_BYTES: usize = 512;

fn gzip_bytes(data: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::Write as _;
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(data).map_err(|e| format!("gzip write: {e}"))?;
    enc.finish().map_err(|e| format!("gzip finish: {e}"))
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
    let mut accept_gzip = false;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            } else if name.trim().eq_ignore_ascii_case("content-type") {
                content_type = Some(value.trim().to_string());
            } else if name.trim().eq_ignore_ascii_case("accept-encoding")
                && value.to_ascii_lowercase().contains("gzip")
            {
                accept_gzip = true;
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
        accept_gzip,
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
        ("GET", "/api/task-maintenance") => err(403, "peer-bound route required"),
        ("GET", "/api/projects") => {
            let projects: Vec<serde_json::Value> = crate::discovery::discover_projects()
                .into_iter()
                .map(|(name, path)| serde_json::json!({ "name": name, "path": path }))
                .collect();
            ok(serde_json::json!({ "projects": projects }))
        }
        // A1: the SAME payload as the GUI get_projects command (counts/
        // active_epics/last_active_at assembled from authority data) — the
        // M4 client stops joining local mirror files for counts.
        ("GET", "/api/projects-full") => map_store(crate::commands::get_projects_impl(
            &crate::discovery::discover_projects(),
        )),
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
        // A2/A3: view fields (retries_remaining / package_display_no) are
        // filled HERE from authority-side runtime data — clients get the
        // fully-shaped list without any local mirror join.
        ("GET", "/api/executions") => map_store(
            crate::execution_store::list_executions().map(|mut execs| {
                crate::commands::fill_execution_view_fields(&mut execs);
                execs
            }),
        ),
        // A3: "currently running" banner payload — title lookup runs here
        // against the authority's own board paths.
        ("GET", "/api/current-execution") => {
            map_store(crate::commands::get_current_execution_impl())
        }
        // A6/A7: arb panel config — the registry lives on the authority
        // machine; the board.json FILE itself stays local on M4 (by design).
        ("GET", "/api/arb-config") => ok(match crate::discovery::get_arb_board_config() {
            Some(c) => serde_json::to_value(c).unwrap_or_default(),
            None => serde_json::json!({ "enabled": false, "board_path": "" }),
        }),
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
        ("GET", "/api/executor-options") => map_store(crate::commands::get_executor_options_impl()),
        // ── 注册表管理（S5 后置：GUI 远程模式的项目生命周期与排序）──
        ("POST", "/api/registry/reorder") => match parse_body(req) {
            Ok(v) => match v.get("names").and_then(|x| x.as_array()) {
                Some(arr) => {
                    let names: Vec<String> = arr
                        .iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect();
                    map_store(crate::discovery::reorder_projects(&names)
                        .map(|_| serde_json::json!({"reordered": names.len()})))
                }
                None => err(400, "body needs 'names': [string]"),
            },
            Err(e) => e,
        },
        ("POST", "/api/registry/remove") => match parse_body(req) {
            Ok(v) => match need(&v, "name") {
                Ok(name) => {
                    // 回传 path 供 GUI 撤销流（undo-remove 需要 name+path）
                    let path = crate::discovery::discover_projects()
                        .iter()
                        .find(|(n, _)| n == name)
                        .map(|(_, p)| p.to_string_lossy().to_string());
                    map_store(crate::discovery::remove_project(name)
                        .map(|_| serde_json::json!({"removed": name, "path": path})))
                }
                Err(e) => e,
            },
            Err(e) => e,
        },
        ("POST", "/api/registry/undo-remove") => match parse_body(req) {
            Ok(v) => match (need(&v, "name"), need(&v, "path")) {
                (Ok(n), Ok(p)) => map_store(crate::discovery::add_project(n, p)
                    .map(|_| serde_json::json!({"restored": n}))),
                (Err(e), _) | (_, Err(e)) => e,
            },
            Err(e) => e,
        },
        ("POST", "/api/registry/restore") => match parse_body(req) {
            Ok(v) => match need(&v, "name") {
                Ok(name) => map_store(crate::discovery::restore_project(name)
                    .map(|_| serde_json::json!({"restored": name}))),
                Err(e) => e,
            },
            Err(e) => e,
        },
        ("POST", "/api/registry/permanently-delete") => match parse_body(req) {
            Ok(v) => match need(&v, "name") {
                Ok(name) => map_store(crate::discovery::permanently_delete(name)
                    .map(|_| serde_json::json!({"deleted": name}))),
                Err(e) => e,
            },
            Err(e) => e,
        },
        // F5 补口：执行器模型/思考档（GUI 设置页写通道）
        ("POST", "/api/runner/executor-config") => match parse_body(req) {
            Ok(v) => {
                let model = v.get("model").and_then(|x| x.as_str());
                let thinking = v.get("thinking").and_then(|x| x.as_str());
                let clear_thinking = v.get("clear_thinking").map(|x| x == true).unwrap_or(false);
                if model.is_none() && thinking.is_none() && !clear_thinking {
                    err(400, "body needs 'model' and/or 'thinking' (or clear_thinking)")
                } else {
                    let r = (|| -> Result<(), String> {
                        if let Some(m) = model {
                            let m = if m.is_empty() { None } else { Some(m.to_string()) };
                            crate::runner_config::set_executor_model(m)?;
                        }
                        if clear_thinking {
                            crate::runner_config::set_executor_thinking(None)?;
                        } else if let Some(t) = thinking {
                            let t = if t.is_empty() { None } else { Some(t.to_string()) };
                            crate::runner_config::set_executor_thinking(t)?;
                        }
                        Ok(())
                    })();
                    match r {
                        Ok(()) => ok(serde_json::json!({
                            "model": crate::runner_config::get_executor_model(),
                            "thinking": crate::runner_config::get_executor_thinking(),
                        })),
                        Err(e) => err(409, e),
                    }
                }
            }
            Err(e) => e,
        },
        // 夜间研究链模型/思考档（全局设置入口；与施工执行器独立选型）
        ("POST", "/api/runner/todo-schedule") => match parse_body(req) {
            Ok(v) => {
                let model = v.get("model").and_then(|x| x.as_str()).map(str::to_string);
                let thinking = v.get("thinking").and_then(|x| x.as_str()).map(str::to_string);
                if model.is_none() && thinking.is_none() {
                    err(400, "body needs 'model' and/or 'thinking'")
                } else {
                    match crate::runner_config::set_todo_schedule_model(model, thinking) {
                        Ok(()) => {
                            let ts = crate::runner_config::get_todo_schedule();
                            ok(serde_json::json!({
                                "model": ts.as_ref().and_then(|t| t.model.clone()),
                                "thinking": ts.as_ref().and_then(|t| t.thinking.clone()),
                            }))
                        }
                        Err(e) => err(409, e),
                    }
                }
            }
            Err(e) => e,
        },
        // 一键启动步骤执行（app ▶ 按钮）——CLI 桥（走 prepare→validate→seal 管线，
        // 保证字段完整含 repo_root/base_sha；epic_id 从板读，不猜）
        ("POST", "/api/steps/seal") => match parse_body(req) {
            Ok(v) => match (need(&v, "step_id"), need(&v, "project_path")) {
                (Ok(sid), Ok(pp)) => {
                    let data = match crate::epic_store::read_epics(std::path::Path::new(pp)) {
                        Ok(d) => d,
                        Err(e) => return err(500, format!("read epics: {e}")),
                    };
                    let step = match data.steps.iter().find(|s| s.id == sid) {
                        Some(s) => s,
                        None => return err(404, format!("step '{}' not found", sid)),
                    };
                    let epic_id = &step.epic_id;
                    let mut obj = step.title.clone();
                    if !step.description.trim().is_empty() {
                        obj = format!("{}\n\n{}", step.title, step.description.trim());
                    }
                    if let Some(e) = data.epics.iter().find(|e| e.id == *epic_id) {
                        if !e.title.trim().is_empty() {
                            obj = format!("[{}]\n\n{}", e.title.trim(), obj);
                        }
                    }
                    let ts = crate::runner_config::get_todo_schedule();
                    match ts {
                        Some(t) => {
                            let cli = std::path::PathBuf::from(&t.producer_cli)
                                .with_file_name("step-seal-cli.js");
                            let input = serde_json::json!({
                                "step_id": sid,
                                "epic_id": epic_id,
                                "objective": obj,
                                "allowed_paths": ["*"],
                                "create_paths": ["*"],
                                "max_changed_files": 30,
                                "executor_preferred": "zcode",
                                "executor_timeout_seconds": 3600,
                                "executor_max_attempts": 2,
                                "executor_sandbox": "workspace-write",
                                "acceptance_command_ids": ["verify:candidate"],
                                "project_dir": pp,
                            });
                            let out = std::process::Command::new(&t.node_bin)
                                .arg(&cli).arg("--stdin")
                                .stdin(std::process::Stdio::piped())
                                .stdout(std::process::Stdio::piped())
                                .stderr(std::process::Stdio::piped())
                                .spawn()
                                .and_then(|mut ch| {
                                    use std::io::Write;
                                    ch.stdin.as_mut().unwrap()
                                        .write_all(input.to_string().as_bytes())?;
                                    ch.wait_with_output()
                                });
                            match out {
                                Ok(o) if o.status.success() => {
                                    let r: serde_json::Value = serde_json::from_slice(&o.stdout)
                                        .unwrap_or(serde_json::json!({"raw": String::from_utf8_lossy(&o.stdout)}));
                                    ok(r)
                                }
                                Ok(o) => err(409, String::from_utf8_lossy(&o.stdout).trim().to_string()),
                                Err(e) => err(500, format!("spawn: {e}")),
                            }
                        }
                        None => err(409, "todo_schedule 未配置"),
                    }
                }
                (Err(e), _) | (_, Err(e)) => e,
            },
            Err(e) => e,
        },
        ("POST", "/api/registry/arb-config") => match parse_body(req) {
            Ok(v) => match v.get("enabled").and_then(|x| x.as_bool()) {
                Some(enabled) => map_store(crate::discovery::set_arb_board_config(
                    enabled,
                    v.get("board_path").and_then(|x| x.as_str()).unwrap_or("exports/board.json"),
                ).map(|_| serde_json::json!({"enabled": enabled}))),
                None => err(400, "body needs boolean 'enabled'"),
            },
            Err(e) => e,
        },
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
                    os(&v, "auto_review"),
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

    fn task_request(method: &str, query: Vec<(String, String)>, body: Vec<u8>) -> Request {
        Request {
            method: method.into(), path: "/api/task-maintenance".into(), query,
            content_type: None, body, accept_gzip: false,
        }
    }

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
            accept_gzip: false,
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
            accept_gzip: false,
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
            accept_gzip: false,
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
            accept_gzip: false,
        };
        let (code, body) = route(&req);
        assert_eq!(code, 404);
        assert!(body.get("error").is_some());
    }

    #[test]
    fn task_maintenance_is_loopback_only_and_has_no_parameters() {
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        let external: IpAddr = "100.64.0.8".parse().unwrap();
        let (code, _) = route_for_peer(&task_request("GET", vec![], vec![]), Some(external));
        assert_eq!(code, 403);
        let (code, _) = route_for_peer(&task_request("GET", vec![], vec![]), None);
        assert_eq!(code, 403);
        let (code, _) = route_for_peer(&task_request("GET", vec![("x".into(), "1".into())], vec![]), Some(loopback));
        assert_eq!(code, 400);
        let (code, _) = route_for_peer(&task_request("GET", vec![], b"{}".to_vec()), Some(loopback));
        assert_eq!(code, 400);
        let (code, _) = route_for_peer(&task_request("POST", vec![], vec![]), Some(loopback));
        assert_eq!(code, 405);
        let (code, body) = route_for_peer(&task_request("GET", vec![], vec![]), Some(loopback));
        assert_eq!(code, 200);
        assert_eq!(body["state"], "UNKNOWN");
        let (code, _) = route(&task_request("GET", vec![], vec![]));
        assert_eq!(code, 403);
        assert_eq!(status_line(403), "403 Forbidden");
    }

    #[test]
    fn gzip_roundtrip_with_client_decoder() {
        // The exact pair the wire uses: server gzip_bytes ↔ client GzDecoder.
        let board = format!("{{\"projects\":[{}]}}", "\"x\",".repeat(200));
        let raw = board.as_bytes();
        let z = gzip_bytes(raw).expect("encode");
        assert!(
            z.len() < raw.len() / 4,
            "board JSON must compress hard: {} -> {}",
            raw.len(),
            z.len()
        );
        let mut dec = flate2::read::GzDecoder::new(&z[..]);
        let mut out = Vec::new();
        std::io::Read::read_to_end(&mut dec, &mut out).expect("decode");
        assert_eq!(out, raw);
    }
}
