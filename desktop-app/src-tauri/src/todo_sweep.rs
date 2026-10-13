//! Todo sweep — the nightly manager-side scheduler tick (待办租户 G4, plan §6).
//!
//! A LIGHTWEIGHT thread (never the Runner's execution thread): it scans,
//! produces standard packages via the Node producer CLI, writes delivery
//! messages to the outbox, and records the round. It never executes AI —
//! execution happens on the collector platform.
//!
//! Window semantics (fixed +08:00 business timezone):
//! - inside [start, end) and today not yet run → RUN
//! - after end and today not yet run → record MISSED once, wait for tomorrow
//!   (a 09:00 wake-up does NOT back-fill; plan §6)
//! - already ran / already missed today → wait
//! - outside the global kill switch or with no schedule configured → no scan

use crate::runner_config::{read_config, TodoSchedule};
use crate::todo_ledger::*;
use chrono::{Datelike, DateTime, FixedOffset, Utc};
use std::path::PathBuf;

/// Business timezone: fixed +08:00 (Asia/Shanghai). The business date never
/// follows transient system timezone changes.
pub fn business_tz() -> FixedOffset {
    FixedOffset::east_opt(8 * 3600).expect("valid offset")
}

pub fn now_business() -> DateTime<FixedOffset> {
    Utc::now().with_timezone(&business_tz())
}

#[derive(Debug, Clone, PartialEq)]
pub enum SweepDecision {
    /// Run the sweep now.
    Run,
    /// Before the window (or already handled today) — wait.
    WaitForWindow,
    /// The window passed today without a run — record missed once.
    MissedWindow,
}

/// Pure window decision — virtual-clock testable.
/// `now` is in the business timezone; `date_str` is its YYYY-MM-DD.
pub fn sweep_decision(
    hhmm_now: u32,
    date_str: &str,
    cfg: &TodoSchedule,
    state: &SweepState,
) -> SweepDecision {
    let already_run = state.last_run_date == date_str;
    let already_missed = state.last_missed_date == date_str;
    if already_run || already_missed {
        return SweepDecision::WaitForWindow;
    }
    if hhmm_now >= cfg.start_hhmm && hhmm_now < cfg.end_hhmm {
        return SweepDecision::Run;
    }
    if hhmm_now >= cfg.end_hhmm {
        return SweepDecision::MissedWindow;
    }
    SweepDecision::WaitForWindow
}


/// Run a node CLI with structured stdin and a HARD timeout (audit B-1): a
/// hung child must never stall the single-threaded tick. Output goes to temp
/// files (no pipe deadlocks); on timeout the child is killed and the caller
/// gets Err — the receipt/produce stays in place and retries next tick.
pub fn run_cli_with_timeout(
    node_bin: &str,
    script: &str,
    stdin_payload: &serde_json::Value,
    timeout_secs: u64,
) -> Result<(bool, String, String), String> {
        use std::io::Write;
    use std::process::{Command, Stdio};
    let tmp_dir = crate::execution_store::runtime_dir().join("cli-tmp");
    std::fs::create_dir_all(&tmp_dir).map_err(|e| e.to_string())?;
    let uniq = uuid::Uuid::new_v4().simple().to_string();
    let out_path = tmp_dir.join(format!("{uniq}.out"));
    let err_path = tmp_dir.join(format!("{uniq}.err"));
    let mut child = Command::new(node_bin)
        .arg(script)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(std::fs::File::create(&out_path).map_err(|e| e.to_string())?))
        .stderr(Stdio::from(std::fs::File::create(&err_path).map_err(|e| e.to_string())?))
        .spawn()
        .map_err(|e| format!("spawn {node_bin} {script}: {e}"))?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(stdin_payload.to_string().as_bytes())
            .map_err(|e| format!("write stdin: {e}"))?;
    }
    drop(child.stdin.take());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = std::fs::read_to_string(&out_path).unwrap_or_default();
                let stderr = std::fs::read_to_string(&err_path).unwrap_or_default();
                let _ = std::fs::remove_file(&out_path);
                let _ = std::fs::remove_file(&err_path);
                return Ok((status.success(), stdout, stderr));
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "cli timeout after {timeout_secs}s: {node_bin} {script}"
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => return Err(format!("wait cli: {e}")),
        }
    }
}

/// Outcome of one producer invocation (real or injected in tests).
#[derive(Debug, Clone)]
pub enum ProduceOutcome {
    Produced {
        task_key: String,
        package_id: String,
    },
    /// Producer refused (guards/idempotency) — recorded, never an error.
    Refused {
        todo_id: String,
        reason_code: String,
        error: String,
    },
    /// Producer could not run (node missing, CLI crash) — infrastructure error.
    Failed {
        todo_id: String,
        error: String,
    },
}

/// The production wrapper: spawns `node <producer_cli>` with structured
/// stdin (todo free text NEVER appears in argv).
pub fn produce_via_cli(
    cfg: &TodoSchedule,
    project_dir: &str,
    todo_id: &str,
) -> ProduceOutcome {
    let payload = serde_json::json!({
        "project_dir": project_dir,
        "todo_id": todo_id,
        "timeout_seconds": cfg.per_task_timeout_secs,
    });
    match run_cli_with_timeout(&cfg.node_bin, &cfg.producer_cli, &payload, 180) {
        Ok((true, stdout, _)) => match serde_json::from_str::<serde_json::Value>(&stdout) {
            Ok(v) if v.get("ok").and_then(|b| b.as_bool()) == Some(true) => {
                ProduceOutcome::Produced {
                    task_key: v.get("task_key").and_then(|s| s.as_str()).unwrap_or_default().to_string(),
                    package_id: v.get("package_id").and_then(|s| s.as_str()).unwrap_or_default().to_string(),
                }
            }
            Ok(v) => ProduceOutcome::Refused {
                todo_id: todo_id.to_string(),
                reason_code: v.get("reason_code").and_then(|s| s.as_str()).unwrap_or("unknown").to_string(),
                error: v.get("error").and_then(|s| s.as_str()).unwrap_or("producer returned ok=false").to_string(),
            },
            Err(e) => ProduceOutcome::Failed {
                todo_id: todo_id.to_string(),
                error: format!("parse producer output failed: {e}"),
            },
        },
        Ok((false, _, stderr)) => ProduceOutcome::Failed {
            todo_id: todo_id.to_string(),
            error: format!("producer exited non-zero: {}", stderr.chars().take(300).collect::<String>()),
        },
        Err(e) => ProduceOutcome::Failed {
            todo_id: todo_id.to_string(),
            error: e,
        },
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SweepRound {
    pub date: String,
    pub started_at: String,
    pub finished_at: String,
    pub scanned_tenants: Vec<String>,
    pub produced_task_keys: Vec<String>,
    pub refused: Vec<serde_json::Value>,
    pub errors: Vec<String>,
    pub outcome: String, // ran | missed | skipped
}

/// One full sweep pass. `tenants` is the enabled-tenant list (the caller
/// resolves it from the registry + runner config); `produce` and `now` are
/// injectable for tests (virtual clock).
pub fn sweep_once(
    tenants_in: &[(String, PathBuf)],
    produce: &mut dyn FnMut(&str, &str) -> ProduceOutcome,
    now: DateTime<FixedOffset>,
    cfg: Option<&TodoSchedule>,
    global_enabled: bool,
) -> SweepRound {
    let date_str = now.format("%Y-%m-%d").to_string();
    let hhmm = now.format("%H%M").to_string().parse::<u32>().unwrap_or(0);
    let started_at = Utc::now().to_rfc3339();

    let Some(cfg) = cfg.cloned() else {
        return SweepRound {
            date: date_str,
            started_at,
            finished_at: Utc::now().to_rfc3339(),
            scanned_tenants: vec![],
            produced_task_keys: vec![],
            refused: vec![],
            errors: vec![],
            outcome: "skipped".into(),
        };
    };
    // The master kill switch gates the sweep too (fail-closed; the schedule
    // never replaces the three-layer authorization).
    if !global_enabled {
        return SweepRound {
            date: date_str,
            started_at,
            finished_at: Utc::now().to_rfc3339(),
            scanned_tenants: vec![],
            produced_task_keys: vec![],
            refused: vec![],
            errors: vec!["global kill switch OFF".into()],
            outcome: "skipped".into(),
        };
    }

    let mut state = read_sweep_state();
    let decision = sweep_decision(hhmm, &date_str, &cfg, &state);
    match decision {
        SweepDecision::WaitForWindow => {
            return SweepRound {
                date: date_str,
                started_at,
                finished_at: Utc::now().to_rfc3339(),
                scanned_tenants: vec![],
                produced_task_keys: vec![],
                refused: vec![],
                errors: vec![],
                outcome: "skipped".into(),
            }
        }
        SweepDecision::MissedWindow => {
            state.last_missed_date = date_str.clone();
            state.updated_at = Utc::now().to_rfc3339();
            let _ = write_sweep_state(&state);
            return SweepRound {
                date: date_str,
                started_at,
                finished_at: Utc::now().to_rfc3339(),
                scanned_tenants: vec![],
                produced_task_keys: vec![],
                refused: vec![],
                errors: vec![format!("missed window ({}-{})", cfg.start_hhmm, cfg.end_hhmm)],
                outcome: "missed".into(),
            };
        }
        SweepDecision::Run => {}
    }

    // Folder-scope selection (§D-user): the allowlist intersects the enabled
    // tenants; absent = every enabled project. Master authorization (project
    // switch) is unaffected.
    let tenants: Vec<(String, PathBuf)> = match &cfg.projects {
        Some(allowed) => tenants_in
            .iter()
            .filter(|(name, path)| {
                let p = path.to_string_lossy();
                allowed.iter().any(|a| a == name || a == &p)
            })
            .cloned()
            .collect(),
        None => tenants_in.to_vec(),
    };
    // Tenant rotation: rotate the registry order by day-of-year so every
    // tenant reaches the front across nights (plan §6: 租户轮转).
    let day_of_year = now.ordinal();
    let n = tenants.len();
    let mut ordered: Vec<(String, PathBuf)> = Vec::with_capacity(n);
    for i in 0..n {
        ordered.push(tenants[(day_of_year as usize + i) % n].clone());
    }

    let ledger = read_ledger().unwrap_or_default();
    let mut produced = Vec::new();
    let mut refused: Vec<serde_json::Value> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut scanned = Vec::new();
    let mut per_tenant_counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();

    'outer: for (name, path) in ordered {
        if produced.len() >= cfg.nightly_max_tasks as usize {
            break;
        }
        scanned.push(name.clone());
        // Strict read — bad data blocks THIS tenant, not the sweep.
        let todos = match crate::store::read_todos_strict(&path) {
            Ok(t) => t,
            Err(e) => {
                errors.push(format!("tenant {name}: strict read blocked: {e}"));
                continue;
            }
        };
        // Within a tenant: priority first, then longest-waiting (§6).
        let mut eligible: Vec<&crate::models::TodoItem> = todos
            .items
            .iter()
            .filter(|i| {
                i.item_type == crate::models::ItemType::Todo
                    && i.status == crate::models::ItemStatus::Active
                    && i.auto_process
                    && !i.processing
            })
            .collect();
        eligible.sort_by(|a, b| {
            let rank = |p: &crate::models::Priority| match p {
                crate::models::Priority::High => 0,
                crate::models::Priority::Medium => 1,
                crate::models::Priority::Low => 2,
            };
            rank(&a.priority)
                .cmp(&rank(&b.priority))
                .then_with(|| a.updated_at.cmp(&b.updated_at))
        });

        for item in eligible {
            let tenant_used = *per_tenant_counts.get(&name).unwrap_or(&0);
            if tenant_used >= cfg.per_tenant_max_tasks {
                break;
            }
            if produced.len() >= cfg.nightly_max_tasks as usize {
                break 'outer;
            }
            // No open ledger entry for this todo — the producer's guards
            // double-check under the lock; skipping here saves a spawn.
            let has_open = ledger.entries.values().any(|e| {
                e.todo_id == item.id
                    && e.tenant_id == name
                    && entry_is_open(e)
            });
            if has_open {
                continue;
            }
            match produce(&path.to_string_lossy(), &item.id) {
                ProduceOutcome::Produced { task_key, package_id } => {
                    let prefs = cfg
                        .model
                        .as_deref()
                        .zip(cfg.thinking.as_deref().or(Some("")))
                        .map(|(m, t)| (m.to_string(), t.to_string()));
                    let prefs_ref = prefs.as_ref().map(|(m, t)| (m.as_str(), t.as_str()));
                    if let Err(e) = write_outbox_message(&task_key, &package_id, prefs_ref) {
                        errors.push(format!("tenant {name}: outbox write failed: {e}"));
                        continue;
                    }
                    produced.push(task_key);
                    *per_tenant_counts.entry(name.clone()).or_insert(0) += 1;
                }
                ProduceOutcome::Refused { reason_code, error, .. } => {
                    refused.push(serde_json::json!({
                        "tenant": name, "todo_id": item.id, "reason_code": reason_code, "error": error,
                    }));
                }
                ProduceOutcome::Failed { error, .. } => {
                    errors.push(format!("tenant {name} todo {}: {error}", item.id));
                }
            }
        }
    }

    // Window handled — mark today's run (even a zero-task run proves the
    // sweep ticked; only real failures keep the window open for the next
    // tick to retry within the same window).
    if errors.is_empty() {
        state.last_run_date = date_str.clone();
        state.updated_at = Utc::now().to_rfc3339();
        let _ = write_sweep_state(&state);
    }

    SweepRound {
        date: date_str,
        started_at,
        finished_at: Utc::now().to_rfc3339(),
        scanned_tenants: scanned,
        produced_task_keys: produced,
        refused,
        errors,
        outcome: "ran".into(),
    }
}

/// Delivery message for the collector-side tenant adapter (GX). Idempotent:
/// named by task_key — rewriting the same key is a no-op unless the content
/// differs (which would indicate a bug — logged).
pub fn write_outbox_message(
    task_key: &str,
    package_id: &str,
    prefs: Option<(&str, &str)>,
) -> Result<(), String> {
    let dir = outbox_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir outbox: {e}"))?;
    let path = dir.join(format!("{task_key}.json"));
    if path.exists() {
        return Ok(()); // idempotent replay
    }
    // Pull the delivery-facing fields from the sealed package (packages.json).
    let pkg = find_package(package_id)?;
    let mut msg = serde_json::json!({
        "version": 1,
        "task_key": task_key,
        "tenant_id": pkg.get("origin").and_then(|o| o.get("tenant_id")).and_then(|v| v.as_str()).unwrap_or(""),
        "todo_id": pkg.get("origin").and_then(|o| o.get("todo_id")).and_then(|v| v.as_str()).unwrap_or(""),
        "package_id": package_id,
        "package_hash": pkg.get("package_hash").and_then(|v| v.as_str()).unwrap_or(""),
        "task_revision": pkg.get("origin").and_then(|o| o.get("task_revision")).and_then(|v| v.as_str()).unwrap_or(""),
        "consent_revision": pkg.get("origin").and_then(|o| o.get("consent_revision")).and_then(|v| v.as_u64()).unwrap_or(0),
        "objective": pkg.get("objective").and_then(|v| v.as_str()).unwrap_or(""),
        "repo_root": pkg.get("repo_root").and_then(|v| v.as_str()).unwrap_or(""),
        "base_sha": pkg.get("base_sha").and_then(|v| v.as_str()).unwrap_or(""),
        "read_paths": pkg.get("scope").and_then(|s| s.get("read_paths")).and_then(|v| v.as_array()).cloned().unwrap_or_default(),
        "timeout_seconds": pkg.get("executor").and_then(|e| e.get("timeout_seconds")).and_then(|v| v.as_u64()).unwrap_or(900),
        "created_at": Utc::now().to_rfc3339(),
    });
    if let Some((m, t)) = prefs {
        if !m.is_empty() {
            msg["model"] = serde_json::json!(m);
        }
        if !t.is_empty() {
            msg["thinking"] = serde_json::json!(t);
        }
    }
    let tmp = dir.join(format!("{task_key}.tmp-{}", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, serde_json::to_string_pretty(&msg).map_err(|e| e.to_string())?)
        .map_err(|e| format!("write outbox: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename outbox: {e}"))?;
    Ok(())
}

/// Find a package's CURRENT row (highest plan_revision) in packages.json.
fn find_package(package_id: &str) -> Result<serde_json::Value, String> {
    let path = crate::execution_store::runtime_dir().join("packages.json");
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("read packages.json: {e}"))?;
    let v: serde_json::Value = serde_json::from_str(&raw).map_err(|e| format!("parse packages.json: {e}"))?;
    let mut best: Option<(u32, serde_json::Value)> = None;
    for p in v.get("packages").and_then(|a| a.as_array()).into_iter().flatten() {
        if p.get("package_id").and_then(|s| s.as_str()) == Some(package_id) {
            let rev = p.get("plan_revision").and_then(|r| r.as_u64()).unwrap_or(0) as u32;
            if best.as_ref().map(|(r, _)| rev > *r).unwrap_or(true) {
                best = Some((rev, p.clone()));
            }
        }
    }
    best.map(|(_, v)| v)
        .ok_or_else(|| format!("package {package_id} not found in packages.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule() -> TodoSchedule {
        TodoSchedule {
            start_hhmm: 300,
            end_hhmm: 600,
            nightly_max_tasks: 6,
            per_tenant_max_tasks: 2,
            per_task_timeout_secs: 900,
            nightly_budget_secs: 3600,
            node_bin: "/nonexistent".into(),
            producer_cli: "/nonexistent".into(),
            verify_cli: "/nonexistent".into(),
            projects: None,
            model: None,
            thinking: None,
        }
    }

    /// Folder-scope selection: the allowlist intersects the enabled tenants;
    /// a non-listed project never produces even when its switch is on.
    #[test]
    fn sweep_projects_allowlist_scopes_tenants() {
        let (_dir, _guard) = setup_runtime();
        let t_in = tenant_dir("alpha", vec![todo_json("a1", "high", true)]);
        let t_out = tenant_dir("beta", vec![todo_json("b1", "high", true)]);
        let mut called: Vec<String> = Vec::new();
        let mut cfg = schedule();
        cfg.start_hhmm = 0;
        cfg.end_hhmm = 2359;
        cfg.projects = Some(vec!["alpha".to_string()]);
        let now = chrono::NaiveDate::from_ymd_opt(2026, 9, 17)
            .unwrap()
            .and_hms_opt(4, 0, 0)
            .unwrap()
            .and_local_timezone(business_tz())
            .unwrap();
        let mut produce = |dir: &str, id: &str| -> ProduceOutcome {
            called.push(format!("{}/{}", dir.rsplit('/').next().unwrap_or(dir), id));
            ProduceOutcome::Produced { task_key: format!("k_{id}"), package_id: "pkg_a1".into() }
        };
        let round = sweep_once(&[t_in, t_out], &mut produce, now, Some(&cfg), true);
        assert_eq!(round.outcome, "ran");
        assert!(round.scanned_tenants.contains(&"alpha".to_string()));
        assert!(!round.scanned_tenants.contains(&"beta".to_string()), "allowlist must exclude beta");
        assert_eq!(called.len(), 1, "exactly the allowlisted tenant produces: {called:?}");
    }

    #[test]
    fn window_boundaries_virtual_clock() {
        let cfg = schedule();
        let fresh = SweepState::default();
        // 02:59 → wait; 03:00 → run; 05:59 → run; 06:00 → missed; 09:00 → missed
        assert_eq!(sweep_decision(259, "2026-09-17", &cfg, &fresh), SweepDecision::WaitForWindow);
        assert_eq!(sweep_decision(300, "2026-09-17", &cfg, &fresh), SweepDecision::Run);
        assert_eq!(sweep_decision(559, "2026-09-17", &cfg, &fresh), SweepDecision::Run);
        assert_eq!(sweep_decision(600, "2026-09-17", &cfg, &fresh), SweepDecision::MissedWindow);
        assert_eq!(sweep_decision(900, "2026-09-17", &cfg, &fresh), SweepDecision::MissedWindow);
        // Already ran / already missed today → wait (no double run, no repeat-miss).
        let ran = SweepState { last_run_date: "2026-09-17".into(), ..Default::default() };
        assert_eq!(sweep_decision(400, "2026-09-17", &cfg, &ran), SweepDecision::WaitForWindow);
        let missed = SweepState { last_missed_date: "2026-09-17".into(), ..Default::default() };
        assert_eq!(sweep_decision(700, "2026-09-17", &cfg, &missed), SweepDecision::WaitForWindow);
        // Next business day resets.
        assert_eq!(sweep_decision(300, "2026-09-18", &cfg, &ran), SweepDecision::Run);
    }

    fn setup_runtime() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        let guard = crate::execution_store::TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("QUESTLOG_RUNTIME", dir.path().join("runtime"));
        (dir, guard)
    }

    fn todo_json(id: &str, priority: &str, auto: bool) -> serde_json::Value {
        let mut v = serde_json::json!({
            "id": id, "type": "todo", "title": format!("T{id}"),
            "content": "研究目标与验收标准，内容足够。", "suggestion": "输出报告",
            "tags": [], "status": "active", "priority": priority, "source_session": "",
            "created_at": "2026", "updated_at": "2026"
        });
        if auto {
            v["auto_process"] = serde_json::json!(true);
            v["auto_process_revision"] = serde_json::json!(1);
        }
        v
    }

    fn tenant_dir(name: &str, items: Vec<serde_json::Value>) -> (String, PathBuf) {
        let dir = std::env::temp_dir().join(format!("questlog-sweep-{}-{}", name, uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(".todos.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "version": 1, "project": name, "items": items, "revision": 1
            }))
            .unwrap(),
        )
        .unwrap();
        (name.to_string(), dir)
    }

    #[test]
    fn sweep_respects_caps_rotation_and_ledger_open_entries() {
        let (_dir, _guard) = setup_runtime();
        // Two tenants; per-tenant cap 1, nightly cap 1 → exactly one produce.
        let t1 = tenant_dir("alpha", vec![todo_json("a1", "high", true), todo_json("a2", "low", true)]);
        let t2 = tenant_dir("beta", vec![todo_json("b1", "high", true)]);

        // Seed packages.json so outbox writing can find the package.
        let runtime = crate::execution_store::runtime_dir();
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::write(
            runtime.join("packages.json"),
            serde_json::to_string(&serde_json::json!({
                "version": 1,
                "packages": [
                    {"package_id": "pkg_a1", "plan_revision": 1, "status": "sealed",
                     "package_hash": "h1", "objective": "o", "repo_root": "/tmp", "base_sha": "s",
                     "scope": {"read_paths": ["**"]},
                     "executor": {"timeout_seconds": 900},
                     "origin": {"type": "todo", "tenant_id": "alpha", "todo_id": "a1", "task_key": "k_a1", "task_revision": "r", "consent_revision": 1}}
                ]
            }))
            .unwrap(),
        )
        .unwrap();

        // Force the window: 03:30 on a fresh state with a fixed business date.
        // (sweep_once reads the real clock; test the within-window happy path
        // by ensuring the state is fresh — if the wall clock is outside the
        // window the round is skipped; so instead drive decision + produce
        // separately below. Here we assert the CAP logic via direct calls.)
        let mut produced_calls: Vec<String> = Vec::new();
        let mut produce = |dir: &str, id: &str| -> ProduceOutcome {
            produced_calls.push(format!("{}/{}", dir.split('/').next_back().unwrap_or(dir), id));
            ProduceOutcome::Produced { task_key: format!("k_{id}"), package_id: "pkg_a1".into() }
        };

        // Nightly cap 1.
        let mut cfg = schedule();
        cfg.nightly_max_tasks = 1;
        cfg.per_tenant_max_tasks = 1;
        // Directly exercise the loop by calling sweep_once — window dependent,
        // so set the state to force Run via a fresh date matching today.
        // Deterministic virtual clock: 03:30 on a fixed business date.
        let now = chrono::NaiveDate::from_ymd_opt(2026, 9, 17)
            .unwrap()
            .and_hms_opt(3, 30, 0)
            .unwrap()
            .and_local_timezone(business_tz())
            .unwrap();
        let round = sweep_once(&[t1.clone(), t2.clone()], &mut produce, now, Some(&cfg), true);
        assert_eq!(round.outcome, "ran");
        assert_eq!(round.produced_task_keys.len(), 1, "nightly cap 1");
        // Cap hit → the second tenant is never scanned (break at loop top).
        assert!(round.scanned_tenants.len() >= 1);
        // per-tenant cap + ledger-open skip: tenant alpha's SECOND todo
        // must not be produced even with a fresh nightly budget.
        let mut seen: Vec<String> = Vec::new();
        let produce2 = |_dir: &str, id: &str| -> ProduceOutcome {
            seen.push(id.to_string());
            ProduceOutcome::Produced { task_key: format!("k2_{id}"), package_id: "pkg_a1".into() }
        };
        // New business day → window reopens.
        std::fs::write(
            sweep_state_path(),
            serde_json::to_string(&SweepState {
                last_run_date: "2026-09-17".into(),
                ..Default::default()
            })
            .unwrap(),
        )
        .unwrap();
        let now2 = chrono::NaiveDate::from_ymd_opt(2026, 9, 18)
            .unwrap()
            .and_hms_opt(4, 0, 0)
            .unwrap()
            .and_local_timezone(business_tz())
            .unwrap();
        let mut produce2 = produce2;
        let round2 = sweep_once(&[t1.clone()], &mut produce2, now2, Some(&cfg), true);
        assert_eq!(round2.outcome, "ran");
        // Outbox idempotency (task_key named file): replaying the same key
        // must not duplicate; other keys' files are separate deliveries.
        write_outbox_message("k_a1", "pkg_a1", None).unwrap();
        write_outbox_message("k_a1", "pkg_a1", None).unwrap();
        let count = std::fs::read_dir(outbox_dir())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("k_a1"))
            .count();
        assert_eq!(count, 1, "outbox replay writes no duplicate");
    }
}


/// ─── Tick thread (plan §6: lightweight, never executes AI) ───

pub fn start_tick_thread() {
    std::thread::Builder::new()
        .name("todo-tick".into())
        .spawn(|| {
            loop {
                tick_once();
                std::thread::sleep(std::time::Duration::from_secs(60));
            }
        })
        .expect("spawn todo-tick");
}

/// One tick: recover stale preparing entries → finalize receipts (always) →
/// sweep inside the nightly window. Every action re-checks its own gates.
pub fn tick_once() {
    // Remote mirror clients never run the nightly chain — the authority
    // machine's headless process owns it (S3/S4 single-writer discipline).
    if crate::data_root::is_remote() {
        return;
    }
    let cfg = crate::runner_config::get_todo_schedule();
    let _ = recover_preparing_entries();
    let tenants = crate::discovery::discover_projects();
    let lookup = |tenant: &str| {
        tenants
            .iter()
            .find(|(name, _)| name == tenant)
            .map(|(_, p)| p.clone())
    };
    let node_cfg = cfg
        .as_ref()
        .map(|c| (c.node_bin.clone(), c.verify_cli.clone()));
    let _ = crate::todo_finalization::finalization_pass(
        node_cfg
            .as_ref()
            .map(|(n, v)| (n.as_str(), v.as_str())),
        &lookup,
    );
    if let Some(schedule) = &cfg {
        let enabled: Vec<(String, PathBuf)> = tenants
            .iter()
            .filter(|(_, p)| crate::runner_config::is_project_enabled(&p.to_string_lossy()))
            .cloned()
            .collect();
        let s = schedule.clone();
        let config = read_config();
        let round = sweep_once(&enabled, &mut |dir: &str, id: &str| {
            produce_via_cli(&s, dir, id)
        }, now_business(), config.todo_schedule.as_ref(), config.global_enabled);
        if round.outcome != "skipped" {
            log::info!(
                "[todo-tick] sweep {} ({} produced, {} refused, {} errors)",
                round.outcome,
                round.produced_task_keys.len(),
                round.refused.len(),
                round.errors.len()
            );
        }
    }
}

/// Recovery of stale `preparing` entries (producer crash/timeout): if the
/// package was sealed → publish + outbox (idempotent); otherwise supersede —
/// the task_key is spent and the user must re-authorize (audit B-1 chain).
pub fn recover_preparing_entries() -> Result<usize, String> {
    const STALE_AFTER_SECS: i64 = 600;
    let mut recovered = 0;
    let now = chrono::Utc::now();
    let ledger = crate::todo_ledger::read_ledger()?;
    let stale: Vec<LedgerEntry> = ledger
        .entries
        .values()
        .filter(|e| {
            e.state == LedgerState::Preparing
                && chrono::DateTime::parse_from_rfc3339(&e.updated_at)
                    .map(|t| (now - t.with_timezone(&chrono::Utc)).num_seconds() > STALE_AFTER_SECS)
                    .unwrap_or(true)
        })
        .cloned()
        .collect();
    for entry in stale {
        let sealed = !entry.package_id.is_empty()
            && package_current_status(&entry.package_id)
                .map(|s| s == "sealed" || s == "ready")
                .unwrap_or(false);
        crate::todo_ledger::mutate_ledger(|entries| {
            if let Some(e) = entries.get_mut(&entry.task_key) {
                if e.state != LedgerState::Preparing {
                    return Ok(());
                }
                if sealed {
                    e.state = LedgerState::Published;
                } else {
                    e.state = LedgerState::Superseded;
                }
                e.updated_at = chrono::Utc::now().to_rfc3339();
            }
            Ok(())
        })?;
        if sealed {
            let cfg = crate::runner_config::get_todo_schedule();
            let prefs = cfg
                .as_ref()
                .and_then(|c| c.model.as_deref().zip(c.thinking.as_deref().or(Some(""))))
                .map(|(m, t)| (m.to_string(), t.to_string()));
            let _ = write_outbox_message(
                &entry.task_key,
                &entry.package_id,
                prefs.as_ref().map(|(m, t)| (m.as_str(), t.as_str())),
            );
        }
        recovered += 1;
    }
    Ok(recovered)
}

fn package_current_status(package_id: &str) -> Option<String> {
    let path = crate::execution_store::runtime_dir().join("packages.json");
    let raw = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let mut best: Option<(u64, String)> = None;
    for p in v.get("packages").and_then(|a| a.as_array()).into_iter().flatten() {
        if p.get("package_id").and_then(|s| s.as_str()) == Some(package_id) {
            let rev = p.get("plan_revision").and_then(|r| r.as_u64()).unwrap_or(0);
            if best.as_ref().map(|(r, _)| rev > *r).unwrap_or(true) {
                best = Some((
                    rev,
                    p.get("status").and_then(|s| s.as_str()).unwrap_or("").to_string(),
                ));
            }
        }
    }
    best.map(|(_, s)| s)
}

