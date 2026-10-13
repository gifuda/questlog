//! Runtime Store — execution state for the Runner.
//!
//! This stores the high-frequency execution state (queue, running, leases,
//! events, outputs) in the OS local app data directory, NOT in .epics.json.
//! This prevents execution events from racing with plan edits and keeps
//! absolute paths and CLI sessions out of the Git-committed plan store.
//!
//! Layout:
//!   <app-data>/questlog-runtime/
//!   ├── executions.json       — queue/running/blocked/failed/passed + leases
//!   ├── events.jsonl           — append-only event log
//!   ├── executions.json.lock  — cross-process lock
//!   └── outputs/<execution_id>/ — per-execution CLI output files

use crate::file_lock;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const RUNTIME_DIRNAME: &str = "questlog-runtime";
const EXECUTIONS_FILENAME: &str = "executions.json";
const EVENTS_FILENAME: &str = "events.jsonl";
const LEASE_TIMEOUT_SECS: u64 = 300; // 5 minutes — lease expires if no heartbeat

// ─── Types ───

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    /// Package is ready, waiting for Runner to pick up.
    Queued,
    /// Runner has acquired the lease and is starting work.
    Running,
    /// CLI produced a candidate commit; awaiting acceptance verification.
    Candidate,
    /// External verifier is running against the exact candidate commit.
    Verifying,
    /// Acceptance passed; step advanced to verified.
    Passed,
    /// Acceptance failed; worktree preserved for inspection.
    Failed,
    /// Cancelled by user or system.
    Cancelled,
    /// User requested cancellation; the Runner must stop the process tree.
    CancelRequested,
    /// User requested pause; the Runner stops and creates a DecisionRequest.
    PauseRequested,
    /// Deadline elapsed and the process tree was terminated.
    TimedOut,
    /// Blocked — needs human decision (DecisionRequest).
    Blocked,
    /// Lease expired without completion.
    Stale,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionLease {
    pub execution_id: String,
    pub acquired_at: u64,    // unix timestamp
    pub last_heartbeat: u64, // unix timestamp
    pub pid: u32,
    pub runner_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Execution {
    pub id: String,
    pub package_id: String,
    pub epic_id: String,
    pub step_id: String,
    pub project_path: String,
    pub status: ExecutionStatus,
    pub lease: Option<ExecutionLease>,
    /// CLI session/thread ID (for resume).
    pub cli_session: Option<String>,
    /// Worktree path for this execution.
    pub worktree_path: Option<String>,
    /// Branch name for this execution.
    pub branch: Option<String>,
    /// Base SHA the execution started from.
    pub base_sha: String,
    /// Candidate commit SHA (if produced).
    pub candidate_sha: Option<String>,
    /// Number of attempts so far.
    pub attempts: u32,
    /// Last error message (if failed).
    pub last_error: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    // ─── Execution memory fields (backward-compatible via serde defaults) ───
    /// Attempt number in the retry chain (1 for first attempt).
    #[serde(default = "default_attempt_no")]
    pub attempt_no: u32,
    /// Terminal-outcome kind (RunResult variant string). None while running
    /// and on pre-migration rows (legacy budget fallback applies there).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Cumulative auto-retry budget consumed along the attempt chain.
    /// Separated from attempt_no (S0-C): human cancel/pause and preflight/
    /// policy blocks do NOT consume budget; executor-attributable failures do.
    #[serde(default)]
    pub budget_used: u32,
    /// Idempotency flag: budget settled exactly once per terminal outcome.
    #[serde(default)]
    pub budget_charged: bool,
    /// VIEW-ONLY (never persisted): remaining auto-retry budget, filled by
    /// the get_executions command from the package's max_attempts cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retries_remaining: Option<u32>,
    /// VIEW-ONLY (never persisted): human-readable package number, filled by
    /// the get_executions command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_display_no: Option<u32>,
    /// S1: whether the attempt's full worktree scene was captured into the
    /// bundle before any worktree-clearing terminal. Some(false) → the
    /// worktree IS the remaining evidence and must be kept. None = not
    /// applicable (no worktree yet, e.g. preflight/policy/cli-probe paths).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_preserved: Option<bool>,
    /// S2: when this attempt recovered a previous ScopeViolation scene into
    /// its worktree, the source execution id. Inherited by retry children.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_recovered_from: Option<String>,
    /// Parent execution ID (for retries). None for first attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_attempt_id: Option<String>,
    /// Path to the AttemptBundle directory for this execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_path: Option<String>,
    /// Executor used for this execution (zcode/codex/manual).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor: Option<String>,
}

fn default_attempt_no() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionsFile {
    pub version: u32,
    pub executions: Vec<Execution>,
    #[serde(default)]
    pub revision: u64,
}

impl Default for ExecutionsFile {
    fn default() -> Self {
        Self {
            version: 1,
            executions: vec![],
            revision: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionEvent {
    pub execution_id: String,
    pub timestamp: u64,
    pub event_type: String,
    pub message: String,
    pub data: Option<serde_json::Value>,
}

// ─── Path resolution ───

pub fn runtime_dir() -> PathBuf {
    // Priority: remote mirror override > QUESTLOG_RUNTIME env > home dir
    if let Some(dir) = crate::data_root::runtime_override() {
        return dir;
    }
    if let Ok(dir) = std::env::var("QUESTLOG_RUNTIME") {
        return PathBuf::from(dir);
    }
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(RUNTIME_DIRNAME)
}

fn executions_path() -> PathBuf {
    runtime_dir().join(EXECUTIONS_FILENAME)
}

fn events_path() -> PathBuf {
    runtime_dir().join(EVENTS_FILENAME)
}

fn outputs_dir() -> PathBuf {
    runtime_dir().join("outputs")
}

fn ensure_runtime_dir() -> Result<(), String> {
    std::fs::create_dir_all(runtime_dir()).map_err(|e| format!("mkdir error: {e}"))?;
    std::fs::create_dir_all(outputs_dir()).map_err(|e| format!("mkdir outputs error: {e}"))?;
    Ok(())
}

fn now_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ─── Read/Write ───

/// Read executions file. Fail-closed on corrupt JSON.
pub fn read_executions() -> Result<ExecutionsFile, String> {
    let path = executions_path();
    if !path.exists() {
        return Ok(ExecutionsFile::default());
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("read error: {e}"))?;
    serde_json::from_str(&raw).map_err(|e| format!("parse error: {e}"))
}

fn write_executions(data: &mut ExecutionsFile) -> Result<(), String> {
    ensure_runtime_dir()?;
    let path = executions_path();
    data.revision = data.revision.saturating_add(1);
    let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let json = serde_json::to_string_pretty(data).map_err(|e| format!("serialize error: {e}"))?;
    std::fs::write(&tmp, &json).map_err(|e| format!("write tmp error: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename error: {e}"))?;
    Ok(())
}

/// Lock-protected read-modify-write.
fn mutate_executions<F>(f: F) -> Result<ExecutionsFile, String>
where
    F: FnOnce(&mut ExecutionsFile) -> Result<(), String>,
{
    mutate_executions_with(|data| f(data).map(|_| ())).map(|(data, ())| data)
}

/// Lock-protected read-modify-write that also returns a caller value
/// computed inside the same lock (needed by atomic claim/retry, where the
/// check and the mutation must not be separable).
fn mutate_executions_with<F, T>(f: F) -> Result<(ExecutionsFile, T), String>
where
    F: FnOnce(&mut ExecutionsFile) -> Result<T, String>,
{
    ensure_runtime_dir()?;
    let path = executions_path();
    file_lock::with_file_lock(&path, || {
        let mut data = read_executions()?;
        let value = f(&mut data)?;
        write_executions(&mut data)?;
        Ok((data, value))
    })
}

// ─── Event logging ───

/// Append an event to the append-only event log (JSONL).
pub fn log_event(event: &ExecutionEvent) -> Result<(), String> {
    ensure_runtime_dir()?;
    let path = events_path();
    let line = serde_json::to_string(event).map_err(|e| format!("serialize event error: {e}"))?;
    // Append mode — each event is one JSON line.
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("open events error: {e}"))?;
    writeln!(file, "{line}").map_err(|e| format!("write event error: {e}"))?;
    Ok(())
}

// ─── Queue operations ───

/// Enqueue an execution: create a new execution record with status Queued.
pub fn enqueue_execution(
    package_id: &str,
    epic_id: &str,
    step_id: &str,
    project_path: &str,
    base_sha: &str,
) -> Result<String, String> {
    enqueue_execution_with_parent(
        package_id,
        epic_id,
        step_id,
        project_path,
        base_sha,
        None,
        1,
    )
}

/// Enqueue an execution with parent attempt info (for retries).
pub fn enqueue_execution_with_parent(
    package_id: &str,
    epic_id: &str,
    step_id: &str,
    project_path: &str,
    base_sha: &str,
    parent_attempt_id: Option<&str>,
    attempt_no: u32,
) -> Result<String, String> {
    let exec_id = format!("exec_{}", uuid::Uuid::new_v4());
    let now = now_ts();
    let exec = Execution {
        id: exec_id.clone(),
        package_id: package_id.to_string(),
        epic_id: epic_id.to_string(),
        step_id: step_id.to_string(),
        project_path: project_path.to_string(),
        status: ExecutionStatus::Queued,
        lease: None,
        cli_session: None,
        worktree_path: None,
        branch: None,
        base_sha: base_sha.to_string(),
        candidate_sha: None,
        attempts: 0,
        last_error: None,
        created_at: now,
        updated_at: now,
        attempt_no,
        outcome: None,
        budget_used: 0,
        budget_charged: false,
        retries_remaining: None,
        package_display_no: None,
        scene_preserved: None,
        scene_recovered_from: None,
        parent_attempt_id: parent_attempt_id.map(|s| s.to_string()),
        bundle_path: None,
        executor: None,
    };

    mutate_executions(|data| {
        data.executions.push(exec);
        Ok(())
    })?;

    log_event(&ExecutionEvent {
        execution_id: exec_id.clone(),
        timestamp: now,
        event_type: "enqueued".to_string(),
        message: format!("Execution enqueued for package {}", package_id),
        data: None,
    })?;

    Ok(exec_id)
}

/// Atomically claim a package for execution — the single entry point that
/// turns a claimable package into a Running execution.
///
/// Under ONE lock this:
///   1. rejects the claim if any execution for this package is already
///      in-flight (Running/Candidate/Verifying/Cancel/PauseRequested) — the
///      package-level uniqueness constraint that makes double-poller races
///      impossible instead of merely unlikely;
///   2. adopts an existing Queued/Stale execution (a retry child pre-created
///      by `create_retry_attempt`, or a crashed runner's leftover) by leasing
///      it, preserving its attempt lineage;
///   3. otherwise creates a fresh first attempt directly in Running with the
///      lease already held.
///
/// Returns the leased execution. `Err` means "someone else holds it" — the
/// caller (poll loop) quietly skips to the next package.
pub fn claim_package(
    package_id: &str,
    epic_id: &str,
    step_id: &str,
    project_path: &str,
    base_sha: &str,
    runner_id: &str,
) -> Result<Execution, String> {
    crate::data_root::guard_write()?;
    let now = now_ts();
    let pid = std::process::id();
    let lease = ExecutionLease {
        execution_id: String::new(), // patched below
        acquired_at: now,
        last_heartbeat: now,
        pid,
        runner_id: runner_id.to_string(),
    };

    let (_data, (exec, event_type, message)) = mutate_executions_with(|data| {
        // 1. Uniqueness: refuse while any attempt is in-flight.
        if data.executions.iter().any(|e| {
            e.package_id == package_id
                && matches!(
                    e.status,
                    ExecutionStatus::Running
                        | ExecutionStatus::Candidate
                        | ExecutionStatus::Verifying
                        | ExecutionStatus::CancelRequested
                        | ExecutionStatus::PauseRequested
                )
        }) {
            return Err(format!("package '{package_id}' is already claimed"));
        }

        // 2. Adopt a pre-created Queued/Stale attempt (retry child or stale
        //    leftover) — newest first so a fresh retry child wins over an
        //    ancient stale row.
        if let Some(existing) = data.executions.iter_mut().rev().find(|e| {
            e.package_id == package_id
                && matches!(e.status, ExecutionStatus::Queued | ExecutionStatus::Stale)
        }) {
            let mut lease = lease.clone();
            lease.execution_id = existing.id.clone();
            existing.status = ExecutionStatus::Running;
            existing.lease = Some(lease);
            existing.attempts += 1;
            existing.updated_at = now;
            let adopted = existing.clone();
            return Ok((
                adopted,
                "claimed_adopted",
                "Claim adopted an existing queued/stale attempt",
            ));
        }

        // 3. Fresh first attempt, born Running with the lease held.
        let exec_id = format!("exec_{}", uuid::Uuid::new_v4());
        let mut lease = lease;
        lease.execution_id = exec_id.clone();
        let exec = Execution {
            id: exec_id.clone(),
            package_id: package_id.to_string(),
            epic_id: epic_id.to_string(),
            step_id: step_id.to_string(),
            project_path: project_path.to_string(),
            status: ExecutionStatus::Running,
            lease: Some(lease),
            cli_session: None,
            worktree_path: None,
            branch: None,
            base_sha: base_sha.to_string(),
            candidate_sha: None,
            attempts: 1,
            last_error: None,
            created_at: now,
            updated_at: now,
            attempt_no: 1,
            outcome: None,
            budget_used: 0,
            budget_charged: false,
            retries_remaining: None,
            package_display_no: None,
            scene_preserved: None,
            scene_recovered_from: None,
            parent_attempt_id: None,
            bundle_path: None,
            executor: None,
        };
        data.executions.push(exec.clone());
        Ok((
            exec,
            "claimed",
            "Package claimed atomically (fresh attempt)",
        ))
    })?;

    log_event(&ExecutionEvent {
        execution_id: exec.id.clone(),
        timestamp: now,
        event_type: event_type.to_string(),
        message: format!("{message} for package {package_id}"),
        data: None,
    })?;

    Ok(exec)
}

/// Find the next queued execution that a Runner can pick up.
/// Checks: status=Queued, no stale lease on same project, dependencies met.
/// Returns None if no work is available.
pub fn find_next_queued(project_path: &str) -> Result<Option<Execution>, String> {
    let data = read_executions()?;
    // Clean up stale leases first
    let mut cleaned = clean_stale_leases_internal(&data)?;

    // Find the first queued execution for this project that doesn't conflict
    // with a running execution's lease.
    let running_projects: std::collections::HashSet<String> = cleaned
        .executions
        .iter()
        .filter(|e| {
            e.status == ExecutionStatus::Running
                && e.lease.is_some()
                && !is_lease_stale(e.lease.as_ref().unwrap())
        })
        .map(|e| e.project_path.clone())
        .collect();

    let next = cleaned
        .executions
        .iter()
        .find(|e| {
            e.status == ExecutionStatus::Queued
                && e.project_path == project_path
                && !running_projects.contains(&e.project_path)
        })
        .cloned();

    // If we cleaned stale leases, persist the cleanup.
    if cleaned
        .executions
        .iter()
        .any(|e| e.status == ExecutionStatus::Stale)
    {
        write_executions(&mut cleaned)?;
    }

    Ok(next)
}

/// Acquire a lease on an execution — marks it Running and records the lease.
/// Fails if another runner already holds the lease.
pub fn acquire_lease(execution_id: &str, runner_id: &str) -> Result<Execution, String> {
    let now = now_ts();
    let pid = std::process::id();

    let data = mutate_executions(|data| {
        let exec = data
            .executions
            .iter_mut()
            .find(|e| e.id == execution_id)
            .ok_or_else(|| format!("execution '{}' not found", execution_id))?;

        // Check for existing non-stale lease
        if let Some(lease) = &exec.lease {
            if !is_lease_stale(lease) {
                if lease.runner_id != runner_id {
                    return Err(format!(
                        "execution '{}' is already leased by runner '{}'",
                        execution_id, lease.runner_id
                    ));
                }
                // Same-runner re-entry on a live lease (claim_package already
                // leased it before process_execution runs): renew the
                // heartbeat, keep status/attempts untouched.
                if let Some(lease) = exec.lease.as_mut() {
                    lease.last_heartbeat = now;
                }
                exec.updated_at = now;
                return Ok(());
            }
        }

        // Only queued or stale executions can be acquired
        if exec.status != ExecutionStatus::Queued && exec.status != ExecutionStatus::Stale {
            return Err(format!(
                "execution '{}' is '{}' — only 'queued' or 'stale' can be acquired",
                execution_id,
                serde_json::to_string(&exec.status).unwrap_or_default()
            ));
        }

        exec.status = ExecutionStatus::Running;
        exec.lease = Some(ExecutionLease {
            execution_id: execution_id.to_string(),
            acquired_at: now,
            last_heartbeat: now,
            pid,
            runner_id: runner_id.to_string(),
        });
        exec.attempts += 1;
        exec.updated_at = now;
        Ok(())
    })?;

    let exec = data
        .executions
        .iter()
        .find(|e| e.id == execution_id)
        .cloned()
        .unwrap();

    log_event(&ExecutionEvent {
        execution_id: execution_id.to_string(),
        timestamp: now,
        event_type: "lease_acquired".to_string(),
        message: format!("Lease acquired by runner {}", runner_id),
        data: None,
    })?;

    Ok(exec)
}

/// Send a heartbeat — updates the lease timestamp.
pub fn heartbeat(execution_id: &str) -> Result<(), String> {
    let now = now_ts();
    mutate_executions(|data| {
        let exec = data
            .executions
            .iter_mut()
            .find(|e| e.id == execution_id)
            .ok_or_else(|| format!("execution '{}' not found", execution_id))?;
        if let Some(lease) = &mut exec.lease {
            lease.last_heartbeat = now;
        }
        exec.updated_at = now;
        Ok(())
    })?;
    Ok(())
}

/// Release the lease and set the final status.
///
/// Evidence-preserving guard: a truly terminal, evidence-bearing state
/// (Passed/Failed/Cancelled/TimedOut) is NEVER overwritten — a late cancel
/// via a DecisionRequest must not erase why the attempt actually ended.
/// Blocked stays overwritable: the DR "cancel" flow legitimately moves a
/// blocked execution to Cancelled.
pub fn release_lease(
    execution_id: &str,
    final_status: ExecutionStatus,
    error: Option<&str>,
) -> Result<(), String> {
    let now = now_ts();
    let _data = mutate_executions(|data| {
        let exec = data
            .executions
            .iter_mut()
            .find(|e| e.id == execution_id)
            .ok_or_else(|| format!("execution '{}' not found", execution_id))?;
        if matches!(
            exec.status,
            ExecutionStatus::Passed
                | ExecutionStatus::Failed
                | ExecutionStatus::Cancelled
                | ExecutionStatus::TimedOut
        ) {
            return Err(format!(
                "execution '{}' is already terminal ({:?}) — refusing to overwrite with {:?}",
                execution_id, exec.status, final_status
            ));
        }
        exec.status = final_status.clone();
        exec.lease = None;
        exec.last_error = error.map(|s| s.to_string());
        exec.updated_at = now;
        Ok(())
    })?;

    log_event(&ExecutionEvent {
        execution_id: execution_id.to_string(),
        timestamp: now,
        event_type: format!("released:{:?}", final_status).to_lowercase(),
        message: format!("Lease released, final status: {:?}", final_status),
        data: None,
    })?;

    Ok(())
}

/// Record a user control request without pretending the process has already
/// stopped. Queued work can be cancelled immediately; running work is moved to
/// a request state that the supervising Runner observes and acts upon.
pub fn request_control(execution_id: &str, pause: bool) -> Result<ExecutionStatus, String> {
    crate::data_root::guard_write()?;
    let now = now_ts();
    let data = mutate_executions(|data| {
        let exec = data
            .executions
            .iter_mut()
            .find(|e| e.id == execution_id)
            .ok_or_else(|| format!("execution '{}' not found", execution_id))?;
        exec.status = match exec.status {
            ExecutionStatus::Queued | ExecutionStatus::Stale if !pause => {
                exec.lease = None;
                ExecutionStatus::Cancelled
            }
            ExecutionStatus::Queued | ExecutionStatus::Stale => ExecutionStatus::Blocked,
            ExecutionStatus::Running | ExecutionStatus::Candidate | ExecutionStatus::Verifying => {
                if pause {
                    ExecutionStatus::PauseRequested
                } else {
                    ExecutionStatus::CancelRequested
                }
            }
            ExecutionStatus::CancelRequested
            | ExecutionStatus::PauseRequested
            | ExecutionStatus::Passed
            | ExecutionStatus::Failed
            | ExecutionStatus::Cancelled
            | ExecutionStatus::Blocked
            | ExecutionStatus::TimedOut => {
                return Err(format!(
                    "execution '{}' cannot be controlled from terminal/requested state {:?}",
                    execution_id, exec.status
                ));
            }
        };
        exec.last_error = Some(if pause {
            "pause requested by user".to_string()
        } else {
            "cancel requested by user".to_string()
        });
        exec.updated_at = now;
        Ok(())
    })?;
    let status = data
        .executions
        .iter()
        .find(|e| e.id == execution_id)
        .map(|e| e.status.clone())
        .ok_or_else(|| format!("execution '{}' not found", execution_id))?;
    log_event(&ExecutionEvent {
        execution_id: execution_id.to_string(),
        timestamp: now,
        event_type: if pause {
            "pause_requested".to_string()
        } else {
            "cancel_requested".to_string()
        },
        message: if pause {
            "Pause requested by user".to_string()
        } else {
            "Cancellation requested by user".to_string()
        },
        data: None,
    })?;
    Ok(status)
}

/// Outcomes attributable to the executor's own retryable failure — these
/// consume auto-retry budget (S0-C contract: timeout/CLI/acceptance/etc).
/// Human and environment attributions (cancel/pause/policy/preflight/drift/
/// scope) do not.
pub fn outcome_counts_budget(outcome: &str) -> bool {
    matches!(
        outcome,
        "timeout" | "cli_failed" | "acceptance_failed" | "no_diff" | "evidence_seal_failed"
    )
}

/// Effective budget consumed by this attempt's chain. Legacy rows (terminal,
/// written before the outcome field existed) fall back to the old
/// attempt-based accounting so pre-migration chains keep their old ceiling.
pub fn effective_budget_used(exec: &Execution) -> u32 {
    if exec.outcome.is_none() && exec.attempt_no > 1 {
        exec.attempt_no - 1
    } else {
        exec.budget_used
    }
}

/// Finalize with terminal CAS semantics. A late success can never overwrite a
/// cancellation or pause request. Returns the authoritative terminal status.
/// `outcome` is the RunResult kind string; when it names an executor-attributable
/// failure the chain budget settles exactly once (budget_charged guard).
pub fn finish_execution(
    execution_id: &str,
    desired: ExecutionStatus,
    error: Option<&str>,
    outcome: Option<&str>,
) -> Result<ExecutionStatus, String> {
    let now = now_ts();
    let data = mutate_executions(|data| {
        let exec = data
            .executions
            .iter_mut()
            .find(|e| e.id == execution_id)
            .ok_or_else(|| format!("execution '{}' not found", execution_id))?;
        let final_status = match exec.status {
            ExecutionStatus::CancelRequested => ExecutionStatus::Cancelled,
            ExecutionStatus::PauseRequested => ExecutionStatus::Blocked,
            ExecutionStatus::Passed
            | ExecutionStatus::Failed
            | ExecutionStatus::Cancelled
            | ExecutionStatus::Blocked
            | ExecutionStatus::TimedOut => exec.status.clone(),
            _ => desired.clone(),
        };
        exec.status = final_status;
        exec.lease = None;
        exec.last_error = error.map(str::to_string);
        if let Some(kind) = outcome {
            exec.outcome = Some(kind.to_string());
            if !exec.budget_charged && outcome_counts_budget(kind) {
                exec.budget_used += 1;
                exec.budget_charged = true;
            }
        }
        exec.updated_at = now;
        Ok(())
    })?;
    let final_status = data
        .executions
        .iter()
        .find(|e| e.id == execution_id)
        .map(|e| e.status.clone())
        .ok_or_else(|| format!("execution '{}' not found", execution_id))?;
    log_event(&ExecutionEvent {
        execution_id: execution_id.to_string(),
        timestamp: now,
        event_type: format!("finished:{:?}", final_status).to_lowercase(),
        message: format!("Execution finalized as {:?}", final_status),
        data: None,
    })?;
    Ok(final_status)
}

/// Update execution fields (worktree, branch, candidate SHA, etc.).
pub fn update_execution(execution_id: &str, updates: ExecutionUpdates) -> Result<(), String> {
    let now = now_ts();
    mutate_executions(|data| {
        let exec = data
            .executions
            .iter_mut()
            .find(|e| e.id == execution_id)
            .ok_or_else(|| format!("execution '{}' not found", execution_id))?;
        if let Some(worktree) = updates.worktree_path {
            exec.worktree_path = Some(worktree);
        }
        if let Some(branch) = updates.branch {
            exec.branch = Some(branch);
        }
        if let Some(session) = updates.cli_session {
            exec.cli_session = Some(session);
        }
        if let Some(sha) = updates.candidate_sha {
            exec.candidate_sha = Some(sha);
        }
        if let Some(status) = updates.status {
            if matches!(
                exec.status,
                ExecutionStatus::CancelRequested | ExecutionStatus::PauseRequested
            ) && !matches!(
                status,
                ExecutionStatus::Cancelled | ExecutionStatus::Blocked
            ) {
                return Err(format!(
                    "terminal control request prevents transition to {:?}",
                    status
                ));
            }
            exec.status = status;
        }
        if let Some(bundle) = updates.bundle_path {
            exec.bundle_path = Some(bundle);
        }
        if let Some(executor) = updates.executor {
            exec.executor = Some(executor);
        }
        if let Some(scene_preserved) = updates.scene_preserved {
            exec.scene_preserved = Some(scene_preserved);
        }
        if let Some(recovered) = updates.scene_recovered_from {
            exec.scene_recovered_from = Some(recovered);
        }
        exec.updated_at = now;
        Ok(())
    })?;
    Ok(())
}

#[derive(Default, Clone)]
pub struct ExecutionUpdates {
    pub worktree_path: Option<String>,
    pub branch: Option<String>,
    pub cli_session: Option<String>,
    pub candidate_sha: Option<String>,
    pub status: Option<ExecutionStatus>,
    pub bundle_path: Option<String>,
    pub executor: Option<String>,
    pub scene_preserved: Option<bool>,
    pub scene_recovered_from: Option<String>,
}

// ─── Stale lease detection and cleanup ───

fn is_lease_stale(lease: &ExecutionLease) -> bool {
    let now = now_ts();
    now.saturating_sub(lease.last_heartbeat) > LEASE_TIMEOUT_SECS
}

/// Find and mark stale leases. Returns the cleaned data without writing.
fn clean_stale_leases_internal(data: &ExecutionsFile) -> Result<ExecutionsFile, String> {
    let mut cleaned = data.clone();
    let now = now_ts();
    for exec in cleaned.executions.iter_mut() {
        if exec.status == ExecutionStatus::Running {
            if let Some(lease) = &exec.lease {
                if is_lease_stale(lease) {
                    exec.status = ExecutionStatus::Stale;
                    exec.lease = None;
                    exec.updated_at = now;
                }
            }
        }
    }
    Ok(cleaned)
}

/// Public cleanup: detect stale leases, mark them, and return to queued.
pub fn cleanup_stale_leases() -> Result<Vec<String>, String> {
    ensure_runtime_dir()?;
    let data = read_executions()?;
    let cleaned = clean_stale_leases_internal(&data)?;

    let stale_ids: Vec<String> = cleaned
        .executions
        .iter()
        .filter(|e| e.status == ExecutionStatus::Stale)
        .map(|e| e.id.clone())
        .collect();

    if !stale_ids.is_empty() {
        let mut to_write = cleaned.clone();
        write_executions(&mut to_write)?;
        for id in &stale_ids {
            log_event(&ExecutionEvent {
                execution_id: id.clone(),
                timestamp: now_ts(),
                event_type: "stale_detected".to_string(),
                message: "Lease expired — marked stale".to_string(),
                data: None,
            })?;
        }
    }

    Ok(stale_ids)
}

/// Get all executions (for UI display).
pub fn list_executions() -> Result<Vec<Execution>, String> {
    read_executions().map(|f| f.executions)
}

/// Get a specific execution by ID.
pub fn get_execution(id: &str) -> Result<Execution, String> {
    let data = read_executions()?;
    data.executions
        .iter()
        .find(|e| e.id == id)
        .cloned()
        .ok_or_else(|| format!("execution '{}' not found", id))
}

/// Create a new retry attempt for a given parent execution.
///
/// This preserves the parent execution row (never modifies it) and creates
/// a new Execution with `attempt_no = parent.attempt_no + 1` and
/// `parent_attempt_id = parent.id`. The new execution starts at Queued.
/// The parent is NOT changed — its evidence stays sealed.
///
/// Atomic: the retryability check, the idempotency check (an already-queued
/// child of this parent), and the child creation happen under ONE lock, so
/// a Runner timeout auto-retry racing a human resolve("retry") yields exactly
/// one child, never two.
pub fn create_retry_attempt(parent_exec_id: &str) -> Result<String, String> {
    crate::data_root::guard_write()?;
    // Package policy read (separate store): the attempt budget cap.
    // S0-C: the cap counts CONSUMED BUDGET, not attempt numbers — a human
    // cancel/pause or a preflight/policy block leaves the budget untouched.
    let parent_snapshot = get_execution(parent_exec_id)?;
    let package = read_package(&parent_snapshot.package_id)?;
    let parent_budget = effective_budget_used(&parent_snapshot);
    if parent_budget >= package.max_attempts.max(1) {
        return Err(format!(
            "maximum attempts reached ({}) — revise the package before retrying",
            package.max_attempts.max(1)
        ));
    }

    let now = now_ts();
    let (_data, (child_id, created)) = mutate_executions_with(|data| {
        let parent = data
            .executions
            .iter()
            .find(|e| e.id == parent_exec_id)
            .ok_or_else(|| format!("execution '{parent_exec_id}' not found"))?;
        if !matches!(
            parent.status,
            ExecutionStatus::Failed
                | ExecutionStatus::Blocked
                | ExecutionStatus::TimedOut
                | ExecutionStatus::Cancelled
                | ExecutionStatus::Stale
        ) {
            return Err(format!(
                "execution '{}' is not in a retryable terminal state",
                parent_exec_id
            ));
        }
        // Idempotency under the same lock: a queued/running child of this
        // parent already exists → return it instead of multiplying attempts.
        if let Some(existing) = data.executions.iter().find(|e| {
            e.parent_attempt_id.as_deref() == Some(parent_exec_id)
                && matches!(e.status, ExecutionStatus::Queued | ExecutionStatus::Running)
        }) {
            return Ok((existing.id.clone(), false));
        }
        let child_id = format!("exec_{}", uuid::Uuid::new_v4());
        data.executions.push(Execution {
            id: child_id.clone(),
            package_id: parent.package_id.clone(),
            epic_id: parent.epic_id.clone(),
            step_id: parent.step_id.clone(),
            project_path: parent.project_path.clone(),
            status: ExecutionStatus::Queued,
            lease: None,
            cli_session: None,
            worktree_path: None,
            branch: None,
            base_sha: parent.base_sha.clone(),
            candidate_sha: None,
            attempts: 0,
            last_error: None,
            created_at: now,
            updated_at: now,
            attempt_no: parent.attempt_no + 1,
            outcome: None,
            budget_used: parent_budget,
            budget_charged: false,
            retries_remaining: None,
            package_display_no: None,
            scene_preserved: None,
            // Retry children physically carry the recovered worktree.
            scene_recovered_from: parent.scene_recovered_from.clone(),
            parent_attempt_id: Some(parent.id.clone()),
            bundle_path: None,
            executor: None,
        });
        Ok((child_id, true))
    })?;

    if created {
        log_event(&ExecutionEvent {
            execution_id: child_id.clone(),
            timestamp: now_ts(),
            event_type: "retry_created".to_string(),
            message: format!(
                "Retry attempt {} created, parent={}",
                parent_snapshot.attempt_no + 1,
                parent_exec_id
            ),
            data: Some(serde_json::json!({
                "parent_attempt_id": parent_exec_id,
                "attempt_no": parent_snapshot.attempt_no + 1,
            })),
        })?;

        mark_package_status(&parent_snapshot.package_id, "ready")?;
    }

    Ok(child_id)
}

// ─── Package bridge: read packages.json written by Node MCP ───

/// Minimal TaskPackage representation for the Runner to consume.
/// Only the fields the Runner needs — the full schema lives in Node.
///
/// This is the **flat projection** of the nested `RawTaskPackage` that Node
/// actually writes to packages.json.  The `From<RawTaskPackage>` impl below
/// handles the projection so the Runner never touches the nested shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskPackageSummary {
    pub package_id: String,
    pub epic_id: String,
    pub step_id: String,
    pub plan_revision: u32,
    pub project_path: String,
    pub repo_root: String,
    pub base_sha: String,
    pub objective: String,
    /// Human-readable package number assigned by the publish/prepare gate
    /// (stable across plan revisions of the same package_id).
    #[serde(default)]
    pub display_no: Option<u32>,
    /// Provenance (待办租户 G2): origin type of the source that authorized
    /// this package. `Some("todo")` = todo-sourced — execution happens on the
    /// collector platform, the local Runner must never claim it.
    #[serde(default)]
    pub origin_kind: Option<String>,
    #[serde(default)]
    pub origin_todo_id: Option<String>,
    #[serde(default)]
    pub origin_task_key: Option<String>,
    pub allowed_paths: Vec<String>,
    #[serde(default)]
    pub create_paths: Vec<String>,
    pub forbidden_paths: Vec<String>,
    /// 待办租户 G3: approved read scope (strict read-only packages only).
    #[serde(default)]
    pub read_paths: Vec<String>,
    pub max_changed_files: u32,
    #[serde(default)]
    pub expected_changed_files: Option<u32>,
    #[serde(default)]
    pub warn_changed_files: Option<u32>,
    pub executor_preferred: String,
    pub executor_fallbacks: Vec<String>,
    /// Executor policy fields consumed by the Runner (F7 complete).
    #[serde(default)]
    pub sandbox: String,
    #[serde(default)]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub max_attempts: u32,
    pub acceptance_command_ids: Vec<String>,
    /// 待办租户 G3: native verifier entries from acceptance.
    #[serde(default)]
    pub acceptance_verifier_ids: Vec<String>,
    pub preconditions: Vec<PackagePrecondition>,
    /// G3 audit B1: strict read-only classification requires the authority
    /// object to have been explicitly present in the package JSON.
    #[serde(default)]
    pub authority_present: bool,
    pub auto_edit_worktree: bool,
    pub auto_commit_candidate: bool,
    pub auto_merge: bool,
    pub auto_push: bool,
    pub auto_deploy: bool,
    pub sealed_at: String,
    pub status: String,
    pub package_hash: String,
    /// Recomputed from all sealed content except status/package_hash.
    #[serde(default)]
    pub computed_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackagePrecondition {
    pub kind: String,
    pub step_id: Option<String>,
    pub at_least: Option<String>,
    pub value: Option<bool>,
}

/// The nested shape that Node's `savePackage` actually writes to packages.json.
///
/// Node `TaskPackage` nests scope/executor/acceptance as objects/arrays.
/// Rust deserializes this first, then projects to flat `TaskPackageSummary`.
/// This mirrors `mcp-server/src/task-package.ts` TaskPackage interface exactly.
#[derive(Debug, Deserialize)]
pub struct RawTaskPackage {
    pub package_id: String,
    pub epic_id: String,
    pub step_id: String,
    pub plan_revision: u32,
    #[serde(default)]
    pub source_session: Option<String>,
    /// Provenance binding (待办租户 G2) — mirrors Node's PackageOrigin.
    #[serde(default)]
    pub origin: Option<RawOrigin>,
    pub project_path: String,
    pub repo_root: String,
    pub base_sha: String,
    pub objective: String,
    /// Human-readable package number (stable across plan revisions).
    #[serde(default)]
    pub display_no: Option<u32>,
    pub scope: RawScope,
    pub executor: RawExecutor,
    pub acceptance: Vec<RawAcceptance>,
    #[serde(default)]
    pub preconditions: Vec<RawPrecondition>,
    /// Authority must be EXPLICITLY present: an absent authority object is
    /// not "all false" (that would misclassify a hand-edited package as
    /// strict read-only — G3 audit B1). Absent ⇒ authority_present=false ⇒
    /// the package can never classify as strict read-only.
    #[serde(default)]
    pub authority: Option<RawAuthority>,
    #[serde(default)]
    pub status: String,
    pub package_hash: String,
    #[serde(default)]
    pub sealed_at: String,
    // Fields we don't yet consume but must tolerate during deserialization:
    // schema_version, source_session, context_refs, preconditions, authority,
    // stop_conditions, rollback, sealed_at — all #[serde(default)] or skipped.
}

#[derive(Debug, Deserialize)]
pub struct RawOrigin {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub tenant_id: String,
    #[serde(default)]
    pub todo_id: String,
    #[serde(default)]
    pub task_revision: String,
    #[serde(default)]
    pub consent_revision: u64,
    #[serde(default)]
    pub task_key: String,
}

#[derive(Debug, Deserialize)]
pub struct RawScope {
    #[serde(default)]
    pub allowed_paths: Vec<String>,
    /// S3 (TaskScope v2): creation rights. Empty on v1 packages — creation
    /// then falls back to allowed_paths (today's behaviour, unchanged).
    #[serde(default)]
    pub create_paths: Vec<String>,
    #[serde(default)]
    pub forbidden_paths: Vec<String>,
    /// 待办租户 G3: approved READ scope for strict read-only packages — a
    /// separate grant that never implies any write right.
    #[serde(default)]
    pub read_paths: Vec<String>,
    #[serde(default)]
    pub max_changed_files: u32,
    #[serde(default)]
    pub expected_changed_files: Option<u32>,
    #[serde(default)]
    pub warn_changed_files: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct RawExecutor {
    #[serde(default = "default_executor")]
    pub preferred: String,
    #[serde(default)]
    pub fallbacks: Vec<String>,
    #[serde(default)]
    pub sandbox: String,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    // fallbacks ignored by Runner (no auto-fallback in production)
}

#[derive(Debug, Deserialize)]
pub struct RawPrecondition {
    #[serde(rename = "type")]
    pub kind: String,
    pub step_id: Option<String>,
    pub at_least: Option<String>,
    pub value: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
pub struct RawAuthority {
    #[serde(default)]
    pub auto_edit_worktree: bool,
    #[serde(default)]
    pub auto_commit_candidate: bool,
    #[serde(default)]
    pub auto_merge: bool,
    #[serde(default)]
    pub auto_push: bool,
    #[serde(default)]
    pub auto_deploy: bool,
}

fn default_executor() -> String {
    "zcode".to_string()
}
fn default_timeout() -> u64 {
    3600
}
fn default_max_attempts() -> u32 {
    2
}

#[derive(Debug, Deserialize)]
pub struct RawAcceptance {
    #[serde(rename = "type")]
    pub kind: String,
    pub command_id: Option<String>,
    /// 待办租户 G3: native verifier acceptance entry (e.g. "todo:report").
    #[serde(default)]
    pub verifier_id: Option<String>,
}

impl TaskPackageSummary {
    /// 待办租户 G3 (§4.3): the STRICT read-only policy — every write right is
    /// false and the change breaker is 0. Read scope is granted solely by
    /// `read_paths`. Legacy read-only packages (default authority + max ≥ 1,
    /// the existing read-only report contract) are NOT strict and keep their
    /// own policy.
    pub fn is_strict_read_only(&self) -> bool {
        self.authority_present
            && self.sandbox == "read-only"
            && !self.auto_edit_worktree
            && !self.auto_commit_candidate
            && !self.auto_merge
            && !self.auto_push
            && !self.auto_deploy
            && self.max_changed_files == 0
    }
}

impl From<RawTaskPackage> for TaskPackageSummary {
    fn from(raw: RawTaskPackage) -> Self {
        TaskPackageSummary {
            package_id: raw.package_id,
            epic_id: raw.epic_id,
            step_id: raw.step_id,
            plan_revision: raw.plan_revision,
            project_path: raw.project_path,
            repo_root: raw.repo_root,
            base_sha: raw.base_sha,
            objective: raw.objective,
            display_no: raw.display_no,
            origin_kind: raw.origin.as_ref().map(|o| o.kind.clone()),
            origin_todo_id: raw.origin.as_ref().map(|o| o.todo_id.clone()),
            origin_task_key: raw.origin.as_ref().map(|o| o.task_key.clone()),
            allowed_paths: raw.scope.allowed_paths,
            create_paths: raw.scope.create_paths,
            forbidden_paths: raw.scope.forbidden_paths,
            read_paths: raw.scope.read_paths,
            max_changed_files: raw.scope.max_changed_files,
            expected_changed_files: raw.scope.expected_changed_files,
            warn_changed_files: raw.scope.warn_changed_files,
            executor_preferred: raw.executor.preferred,
            executor_fallbacks: raw.executor.fallbacks,
            sandbox: raw.executor.sandbox,
            timeout_seconds: raw.executor.timeout_seconds,
            max_attempts: raw.executor.max_attempts,
            acceptance_command_ids: raw
                .acceptance
                .iter()
                .filter(|a| a.kind == "command")
                .filter_map(|a| a.command_id.clone())
                .collect(),
            acceptance_verifier_ids: raw
                .acceptance
                .iter()
                .filter(|a| a.kind == "verifier")
                .filter_map(|a| a.verifier_id.clone())
                .collect(),
            preconditions: raw
                .preconditions
                .into_iter()
                .map(|precondition| PackagePrecondition {
                    kind: precondition.kind,
                    step_id: precondition.step_id,
                    at_least: precondition.at_least,
                    value: precondition.value,
                })
                .collect(),
            authority_present: raw.authority.is_some(),
            auto_edit_worktree: raw.authority.as_ref().map(|a| a.auto_edit_worktree).unwrap_or(false),
            auto_commit_candidate: raw.authority.as_ref().map(|a| a.auto_commit_candidate).unwrap_or(false),
            auto_merge: raw.authority.as_ref().map(|a| a.auto_merge).unwrap_or(false),
            auto_push: raw.authority.as_ref().map(|a| a.auto_push).unwrap_or(false),
            auto_deploy: raw.authority.as_ref().map(|a| a.auto_deploy).unwrap_or(false),
            sealed_at: raw.sealed_at,
            status: raw.status,
            package_hash: raw.package_hash,
            computed_hash: String::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct PackagesFile {
    packages: Vec<RawTaskPackage>,
}

pub fn read_package(package_id: &str) -> Result<TaskPackageSummary, String> {
    let path = packages_path();
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("read packages.json: {e}"))?;
    let file: PackagesFile =
        serde_json::from_str(&raw).map_err(|e| format!("parse packages.json: {e}"))?;
    file.packages
        .into_iter()
        .filter(|package| package.package_id == package_id)
        .max_by_key(|package| package.plan_revision)
        .map(TaskPackageSummary::from)
        .ok_or_else(|| format!("package '{}' not found", package_id))
}

fn packages_path() -> PathBuf {
    runtime_dir().join("packages.json")
}

/// S2: find the freshest recoverable ScopeViolation scene for a NEW first
/// attempt of the same package. The scene is only a valid recovery source
/// when (a) it ended as a scope_violation, (b) it is terminal, (c) its base
/// matches the new attempt's base (a drifted base cannot be git-applied
/// cleanly), and (d) its scene was fully preserved. Safety is NOT decided
/// here — the sealed scope of the new revision plus the post-run scope
/// check remain the only gate; the source only saves work.
pub fn find_recoverable_scene(
    execs: &[Execution],
    package_id: &str,
    exclude_id: &str,
    base_sha: &str,
) -> Option<String> {
    execs
        .iter()
        .filter(|e| {
            e.id != exclude_id
                && e.package_id == package_id
                && e.outcome.as_deref() == Some("scope_violation")
                && matches!(
                    e.status,
                    ExecutionStatus::Blocked | ExecutionStatus::Cancelled
                )
                && e.base_sha == base_sha
                && e.scene_preserved == Some(true)
        })
        .max_by_key(|e| e.updated_at)
        .map(|e| e.id.clone())
}

/// One-shot map package_id → display_no (highest plan_revision wins) —
/// feeds the get_executions join so the UI can show the human-readable
/// package number on every execution row.
pub fn display_nos_by_package() -> Result<std::collections::HashMap<String, u32>, String> {
    let path = packages_path();
    if !path.exists() {
        return Ok(std::collections::HashMap::new());
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("read packages.json: {e}"))?;
    let file: PackagesFile =
        serde_json::from_str(&raw).map_err(|e| format!("parse packages.json: {e}"))?;
    let mut best: std::collections::HashMap<String, (u32, u32)> =
        std::collections::HashMap::new();
    for package in file.packages {
        let Some(no) = package.display_no else { continue };
        match best.get(&package.package_id) {
            Some((rev, _)) if *rev >= package.plan_revision => {}
            _ => {
                best.insert(package.package_id, (package.plan_revision, no));
            }
        }
    }
    Ok(best.into_iter().map(|(id, (_, no))| (id, no)).collect())
}

/// One-shot map package_id → max_attempts (highest plan_revision wins).
/// Feeds the get_executions retries_remaining join without N+1 file reads.
pub fn budget_caps_by_package() -> Result<std::collections::HashMap<String, u32>, String> {
    let path = packages_path();
    if !path.exists() {
        return Ok(std::collections::HashMap::new());
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("read packages.json: {e}"))?;
    let file: PackagesFile =
        serde_json::from_str(&raw).map_err(|e| format!("parse packages.json: {e}"))?;
    let mut caps: std::collections::HashMap<String, (u32, u32)> =
        std::collections::HashMap::new(); // package_id → (revision, max_attempts)
    for package in file.packages {
        let cap = package.executor.max_attempts;
        let revision = package.plan_revision;
        match caps.get(&package.package_id) {
            Some((best_revision, _)) if *best_revision >= revision => {}
            _ => {
                caps.insert(package.package_id, (revision, cap));
            }
        }
    }
    Ok(caps
        .into_iter()
        .map(|(id, (_, cap))| (id, cap.max(1)))
        .collect())
}

/// Read all claimable packages from packages.json (written by Node MCP).
///
/// Accepts both "sealed" and "ready": with the "switch = authorization"
/// model, a sealed package in a project/epic whose switches are ON is
/// claimable directly — the enqueue flip to "ready" is a conversation-side
/// convenience, not a precondition for the Runner. Packages that must not
/// be auto-claimed (manual executor, unmet dependencies) are skipped
/// quietly by `poll_once`, not by this filter.
///
/// Deserializes the **nested** `RawTaskPackage` shape that Node writes,
/// then projects to flat `TaskPackageSummary` for the Runner.
pub fn read_ready_packages() -> Result<Vec<TaskPackageSummary>, String> {
    let path = packages_path();
    if !path.exists() {
        return Ok(vec![]);
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("read packages.json: {e}"))?;
    let value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse packages.json: {e}"))?;
    let packages = value
        .get("packages")
        .and_then(|packages| packages.as_array())
        .ok_or_else(|| "packages.json missing packages array".to_string())?;
    let mut ready = Vec::new();
    for package_value in packages {
        match package_value
            .get("status")
            .and_then(|status| status.as_str())
        {
            Some("sealed") | Some("ready") => {}
            _ => continue,
        }
        let raw_package: RawTaskPackage = serde_json::from_value(package_value.clone())
            .map_err(|e| format!("parse TaskPackage: {e}"))?;
        let mut summary = TaskPackageSummary::from(raw_package);
        summary.computed_hash = compute_package_hash(package_value)?;
        ready.push(summary);
    }
    Ok(ready)
}

/// Count claimable packages by status: (ready, sealed). One file read —
/// `runtime_status` needs both numbers for the UI without parsing twice.
pub fn count_claimable_by_status() -> Result<(usize, usize), String> {
    let path = packages_path();
    if !path.exists() {
        return Ok((0, 0));
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("read packages.json: {e}"))?;
    let value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse packages.json: {e}"))?;
    let packages = value
        .get("packages")
        .and_then(|packages| packages.as_array())
        .ok_or_else(|| "packages.json missing packages array".to_string())?;
    let (mut ready, mut sealed) = (0usize, 0usize);
    for package_value in packages {
        match package_value
            .get("status")
            .and_then(|status| status.as_str())
        {
            Some("ready") => ready += 1,
            Some("sealed") => sealed += 1,
            _ => {}
        }
    }
    Ok((ready, sealed))
}

/// Recursively canonicalize a JSON value into the SAME byte form as Node's
/// `computePackageHash` (task-package.ts): object keys sorted ascending,
/// `status`/`package_hash` keys skipped at EVERY level, arrays kept in order.
/// The key order must not depend on serde_json's map implementation — the
/// `preserve_order` feature changes Value iteration order, so it is pinned
/// here instead.
///
/// Byte-equivalence domain (matches the TaskPackage schema as of G1):
/// ASCII field names and integer-valued numbers. Float formatting
/// (Rust "1.0" vs JS "1") and non-BMP key ordering can diverge — if the
/// schema ever gains such fields, both sides need shared fixtures.
fn canonical_json_sorted(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&str> = map.keys().map(|k| k.as_str()).collect();
            keys.sort();
            out.push('{');
            let mut first = true;
            for k in keys {
                if k == "status" || k == "package_hash" {
                    continue;
                }
                if !first {
                    out.push(',');
                }
                first = false;
                out.push_str(&serde_json::to_string(k).unwrap_or_default());
                out.push(':');
                canonical_json_sorted(&map[k], out);
            }
            out.push('}');
        }
        serde_json::Value::Array(arr) => {
            out.push('[');
            let mut first = true;
            for v in arr {
                if !first {
                    out.push(',');
                }
                first = false;
                canonical_json_sorted(v, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&serde_json::to_string(scalar).unwrap_or_default()),
    }
}

pub fn compute_package_hash(package: &serde_json::Value) -> Result<String, String> {
    if !package.is_object() {
        return Err("TaskPackage must be an object".to_string());
    }
    let mut canonical = String::new();
    canonical_json_sorted(package, &mut canonical);
    Ok(crate::attempt_bundle::sha256_hex(canonical.as_bytes()))
}

/// Auto re-anchor a sealed/ready package to a new base SHA: append a new
/// revision row (base_sha updated, plan_revision +1, package_hash recomputed),
/// supersede the old row, and sync the step's package summary in .epics.json.
///
/// This is a mechanical content-preserving transform — objective/scope/
/// authority fields are untouched — so the "seal → HEAD moved → blocked →
/// manually reseal" dance disappears: the Runner re-anchors at claim time.
///
/// Only packages with NO execution history are eligible (a package that was
/// attempted must be revised by a human, not silently re-based).
/// Returns the fresh TaskPackageSummary for the new revision.
pub fn reanchor_package(
    package_id: &str,
    new_base_sha: &str,
    project_path: &str,
    step_id: &str,
) -> Result<TaskPackageSummary, String> {
    let path = packages_path();
    if !path.exists() {
        return Err("packages.json missing".to_string());
    }
    file_lock::with_file_lock(&path, || {
        let raw = std::fs::read_to_string(&path).map_err(|e| format!("read: {e}"))?;
        let mut json: serde_json::Value =
            serde_json::from_str(&raw).map_err(|e| format!("parse: {e}"))?;
        let packages = json
            .get_mut("packages")
            .and_then(|p| p.as_array_mut())
            .ok_or("packages.json missing packages array")?;

        // Latest non-superseded row for this id must be claimable
        let mut current: Option<(usize, serde_json::Value)> = None;
        for (idx, pkg) in packages.iter().enumerate() {
            if pkg.get("package_id").and_then(|v| v.as_str()) != Some(package_id) {
                continue;
            }
            if pkg.get("status").and_then(|v| v.as_str()) == Some("superseded") {
                continue;
            }
            // Convention: one claimable row per id; keep the last seen.
            current = Some((idx, pkg.clone()));
        }
        let (idx, mut row) = current.ok_or_else(|| format!("package '{package_id}' not found"))?;
        let status = row
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if !matches!(status.as_str(), "sealed" | "ready") {
            return Err(format!(
                "cannot re-anchor package in status '{status}' (only sealed/ready)"
            ));
        }

        // New revision: update base, bump revision, recompute hash.
        let revision = row
            .get("plan_revision")
            .and_then(|v| v.as_u64())
            .unwrap_or(1)
            + 1;
        row["base_sha"] = serde_json::Value::String(new_base_sha.to_string());
        row["plan_revision"] = serde_json::Value::from(revision);
        let hash = compute_package_hash(&row)?;
        row["package_hash"] = serde_json::Value::String(hash.clone());

        // Supersede the old row, append the new one.
        packages[idx]["status"] = serde_json::Value::String("superseded".to_string());
        row["status"] = serde_json::Value::String(status);
        packages.push(row.clone());

        let tmp = path.with_extension("json.tmp-bridge");
        std::fs::write(&tmp, serde_json::to_string_pretty(&json).unwrap())
            .map_err(|e| format!("write tmp: {e}"))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;

        let mut summary = TaskPackageSummary::from(
            serde_json::from_value::<RawTaskPackage>(row.clone())
                .map_err(|e| format!("parse re-anchored row: {e}"))?,
        );
        summary.computed_hash = hash.clone();

        // Keep the step's package summary in .epics.json in sync — the
        // claim-time preflight rejects packages whose summary drifted.
        crate::epic_store::mutate_epics(&std::path::PathBuf::from(project_path), |data| {
            let step = data
                .steps
                .iter_mut()
                .find(|s| s.id == step_id)
                .ok_or_else(|| format!("step '{step_id}' not found"))?;
            if let Some(summary) = step.package.as_mut() {
                summary.plan_revision = revision as u32;
                summary.package_hash = hash.clone();
            }
            Ok(())
        })?;

        Ok(summary)
    })
}

/// Mark a package as consumed (status → "passed" or "failed") in packages.json.
/// This prevents the Runner from re-picking the same package.
pub fn mark_package_status(package_id: &str, status: &str) -> Result<(), String> {
    let path = packages_path();
    if !path.exists() {
        return Ok(());
    }
    file_lock::with_file_lock(&path, || {
        let raw = std::fs::read_to_string(&path).map_err(|e| format!("read: {e}"))?;
        let mut json: serde_json::Value =
            serde_json::from_str(&raw).map_err(|e| format!("parse: {e}"))?;
        if let Some(packages) = json.get_mut("packages").and_then(|p| p.as_array_mut()) {
            for pkg in packages.iter_mut() {
                if pkg.get("package_id").and_then(|v| v.as_str()) == Some(package_id) {
                    // Historical rows keep their tombstone — only the live
                    // row(s) transition. Flipping a superseded row would
                    // rewrite history and could resurrect an old revision
                    // into a claimable-looking state.
                    if pkg.get("status").and_then(|v| v.as_str()) == Some("superseded") {
                        continue;
                    }
                    pkg["status"] = serde_json::Value::String(status.to_string());
                }
            }
        }
        let tmp = path.with_extension("json.tmp-bridge");
        std::fs::write(&tmp, serde_json::to_string_pretty(&json).unwrap())
            .map_err(|e| format!("write tmp: {e}"))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
        Ok(())
    })
}

/// Shared mutex for tests that touch QUESTLOG_RUNTIME env var.
/// Placed at module level (not in #[cfg(test)]) so cross-module tests can reference it.
#[cfg(test)]
pub static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_temp_runtime() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        // A failed assertion must not cascade into unrelated tests by poisoning
        // the process-global environment guard.
        let guard = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("QUESTLOG_RUNTIME", dir.path());
        ensure_runtime_dir().unwrap();
        (dir, guard)
    }

    #[test]
    fn enqueue_and_find() {
        let (_dir, _guard) = setup_temp_runtime();
        let id = enqueue_execution("pkg1", "e1", "s1", "/tmp/proj", "abc123").unwrap();
        let next = find_next_queued("/tmp/proj").unwrap();
        assert!(next.is_some());
        assert_eq!(next.unwrap().id, id);
    }

    #[test]
    fn acquire_lease_marks_running() {
        let (_dir, _guard) = setup_temp_runtime();
        let id = enqueue_execution("pkg1", "e1", "s1", "/tmp/proj", "abc123").unwrap();
        let exec = acquire_lease(&id, "runner-1").unwrap();
        assert_eq!(exec.status, ExecutionStatus::Running);
        assert!(exec.lease.is_some());
        assert_eq!(exec.attempts, 1);
    }

    #[test]
    fn double_acquire_rejected() {
        let (_dir, _guard) = setup_temp_runtime();
        let id = enqueue_execution("pkg1", "e1", "s1", "/tmp/proj", "abc123").unwrap();
        let _first = acquire_lease(&id, "runner-1").unwrap();
        let second = acquire_lease(&id, "runner-2");
        assert!(second.is_err());
    }

    #[test]
    fn release_sets_final_status() {
        let (_dir, _guard) = setup_temp_runtime();
        let id = enqueue_execution("pkg1", "e1", "s1", "/tmp/proj", "abc123").unwrap();
        let _exec = acquire_lease(&id, "runner-1").unwrap();
        release_lease(&id, ExecutionStatus::Passed, None).unwrap();
        let exec = get_execution(&id).unwrap();
        assert_eq!(exec.status, ExecutionStatus::Passed);
        assert!(exec.lease.is_none());
    }

    #[test]
    fn find_next_skips_running_project() {
        let (_dir, _guard) = setup_temp_runtime();
        let id1 = enqueue_execution("pkg1", "e1", "s1", "/tmp/proj", "abc1").unwrap();
        let _id2 = enqueue_execution("pkg2", "e2", "s2", "/tmp/proj", "abc2").unwrap();
        // Acquire lease on first — second should not be picked up (same project)
        let _exec = acquire_lease(&id1, "runner-1").unwrap();
        let next = find_next_queued("/tmp/proj").unwrap();
        assert!(
            next.is_none(),
            "should not pick up from same project while running"
        );
    }

    #[test]
    fn cleanup_detects_stale_leases() {
        let (_dir, _guard) = setup_temp_runtime();
        let id = enqueue_execution("pkg1", "e1", "s1", "/tmp/proj", "abc123").unwrap();
        let _exec = acquire_lease(&id, "runner-1").unwrap();

        // Manually age the lease past the timeout
        let data = read_executions().unwrap();
        let mut aged = data;
        for exec in aged.executions.iter_mut() {
            if exec.id == id {
                if let Some(lease) = &mut exec.lease {
                    lease.last_heartbeat = now_ts().saturating_sub(LEASE_TIMEOUT_SECS + 100);
                }
            }
        }
        write_executions(&mut aged).unwrap();

        let stale = cleanup_stale_leases().unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0], id);
    }

    #[test]
    fn packages_json_nested_deserialization() {
        // Simulates what Node MCP savePackage actually writes: nested scope/executor/acceptance
        let (_dir, _guard) = setup_temp_runtime();
        let nested_json = serde_json::json!({
            "version": 1,
            "packages": [{
                "schema_version": 1,
                "package_id": "pkg_abc",
                "epic_id": "epic_1",
                "step_id": "step_1",
                "plan_revision": 1,
                "project_path": "/tmp/proj",
                "repo_root": "/tmp/proj",
                "base_sha": "deadbeef",
                "objective": "change value to 42",
                "context_refs": [],
                "scope": {
                    "allowed_paths": ["src.ts"],
                    "forbidden_paths": ["secrets/**"],
                    "max_changed_files": 5
                },
                "preconditions": [],
                "executor": {
                    "preferred": "zcode",
                    "fallbacks": [],
                    "sandbox": "workspace-write",
                    "timeout_seconds": 1800,
                    "max_attempts": 3
                },
                "acceptance": [
                    {"type": "command", "command_id": "test:mcp"},
                    {"type": "git_diff_scope"},
                    {"type": "verifier", "verifier_id": "default"}
                ],
                "authority": {
                    "auto_edit_worktree": true,
                    "auto_commit_candidate": true,
                    "auto_merge": false,
                    "auto_push": false,
                    "auto_deploy": false
                },
                "stop_conditions": ["scope_expanded"],
                "rollback": {"strategy": "preserve_worktree_and_stop"},
                "status": "ready",
                "sealed_at": "2026-08-11T00:00:00Z",
                "package_hash": "abc123hash"
            }]
        });
        std::fs::write(
            packages_path(),
            serde_json::to_string_pretty(&nested_json).unwrap(),
        )
        .unwrap();

        let packages = read_ready_packages().unwrap();
        assert_eq!(packages.len(), 1);
        let pkg = &packages[0];

        // Flat fields
        assert_eq!(pkg.package_id, "pkg_abc");
        assert_eq!(pkg.base_sha, "deadbeef");
        assert_eq!(pkg.status, "ready");

        // Projected from nested scope
        assert_eq!(pkg.allowed_paths, vec!["src.ts"]);
        assert_eq!(pkg.forbidden_paths, vec!["secrets/**"]);
        assert_eq!(pkg.max_changed_files, 5);

        // Projected from nested executor
        assert_eq!(pkg.executor_preferred, "zcode");
        assert_eq!(pkg.sandbox, "workspace-write");
        assert_eq!(pkg.timeout_seconds, 1800);
        assert_eq!(pkg.max_attempts, 3);

        // Projected from nested acceptance (only command type, git_diff_scope/verifier excluded)
        assert_eq!(pkg.acceptance_command_ids, vec!["test:mcp"]);
    }

    #[test]
    fn packages_json_flat_legacy_format_still_works() {
        // The e2e test uses a flat synthetic format — ensure backward compat
        // by verifying that missing nested fields don't crash (serde defaults).
        let (_dir, _guard) = setup_temp_runtime();
        let flat_json = serde_json::json!({
            "version": 1,
            "packages": [{
                "package_id": "pkg_flat",
                "epic_id": "e1",
                "step_id": "s1",
                "plan_revision": 1,
                "project_path": "/tmp/p",
                "repo_root": "/tmp/p",
                "base_sha": "aaa",
                "objective": "do thing",
                "scope": {
                    "allowed_paths": ["*.ts"],
                    "forbidden_paths": [],
                    "max_changed_files": 10
                },
                "executor": {
                    "preferred": "zcode"
                },
                "acceptance": [],
                "status": "ready",
                "package_hash": "h1"
            }]
        });
        std::fs::write(
            packages_path(),
            serde_json::to_string_pretty(&flat_json).unwrap(),
        )
        .unwrap();

        let packages = read_ready_packages().unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].executor_preferred, "zcode");
        // Defaults should kick in for missing executor subfields
        assert_eq!(packages[0].timeout_seconds, 3600);
        assert_eq!(packages[0].max_attempts, 2);
    }

    /// S7-followup: display_nos_by_package picks the highest-revision row's
    /// number (revisions of one package_id share their display number).
    #[test]
    fn display_nos_pick_highest_revision() {
        let (_dir, _guard) = setup_temp_runtime();
        let packages = serde_json::json!({
            "version": 1,
            "packages": [
                {"package_id": "pkgA", "epic_id": "e1", "step_id": "s1", "plan_revision": 1,
                 "display_no": 7, "project_path": "/tmp/p", "repo_root": "/tmp/p",
                 "base_sha": "a", "objective": "o",
                 "scope": {"allowed_paths": ["**"], "forbidden_paths": [], "max_changed_files": 5},
                 "executor": {"preferred": "zcode", "fallbacks": [], "sandbox": "workspace-write",
                              "timeout_seconds": 60, "max_attempts": 2},
                 "acceptance": [], "status": "superseded",
                 "sealed_at": "t", "package_hash": "h"},
                {"package_id": "pkgA", "epic_id": "e1", "step_id": "s1", "plan_revision": 3,
                 "display_no": 7, "project_path": "/tmp/p", "repo_root": "/tmp/p",
                 "base_sha": "a", "objective": "o",
                 "scope": {"allowed_paths": ["**"], "forbidden_paths": [], "max_changed_files": 5},
                 "executor": {"preferred": "zcode", "fallbacks": [], "sandbox": "workspace-write",
                              "timeout_seconds": 60, "max_attempts": 2},
                 "acceptance": [], "status": "blocked",
                 "sealed_at": "t", "package_hash": "h"},
                {"package_id": "pkgB", "epic_id": "e1", "step_id": "s2", "plan_revision": 1,
                 "project_path": "/tmp/p", "repo_root": "/tmp/p",
                 "base_sha": "a", "objective": "o",
                 "scope": {"allowed_paths": ["**"], "forbidden_paths": [], "max_changed_files": 5},
                 "executor": {"preferred": "zcode", "fallbacks": [], "sandbox": "workspace-write",
                              "timeout_seconds": 60, "max_attempts": 2},
                 "acceptance": [], "status": "sealed",
                 "sealed_at": "t", "package_hash": "h"}
            ]
        });
        std::fs::write(packages_path(), serde_json::to_string_pretty(&packages).unwrap()).unwrap();
        let map = display_nos_by_package().unwrap();
        assert_eq!(map.get("pkgA"), Some(&7));
        // Packages without a display_no (legacy) simply stay absent.
        assert!(!map.contains_key("pkgB"));
    }

    /// S2: recoverable-scene selection — same package, scope_violation
    /// outcome, terminal, matching base, fully preserved scene; newest wins;
    /// everything else is filtered out.
    #[test]
    fn find_recoverable_scene_filters_and_selects_newest() {
        let mk = |id: &str, pkg: &str, outcome: Option<&str>, base: &str, preserved: Option<bool>, updated: u64| Execution {
            id: id.to_string(),
            package_id: pkg.to_string(),
            epic_id: "e1".into(),
            step_id: "s1".into(),
            project_path: "/tmp".into(),
            status: if outcome == Some("scope_violation") {
                ExecutionStatus::Cancelled
            } else {
                ExecutionStatus::TimedOut
            },
            lease: None,
            cli_session: None,
            worktree_path: None,
            branch: None,
            base_sha: base.to_string(),
            candidate_sha: None,
            attempts: 1,
            last_error: None,
            created_at: updated,
            updated_at: updated,
            attempt_no: 1,
            outcome: outcome.map(str::to_string),
            budget_used: 0,
            budget_charged: false,
            retries_remaining: None,
            package_display_no: None,
            scene_preserved: preserved,
            scene_recovered_from: None,
            parent_attempt_id: None,
            bundle_path: None,
            executor: None,
        };
        let execs = vec![
            mk("old_sv", "pkg1", Some("scope_violation"), "baseA", Some(true), 100),
            mk("new_sv", "pkg1", Some("scope_violation"), "baseA", Some(true), 200),
            mk("wrong_base", "pkg1", Some("scope_violation"), "baseB", Some(true), 300),
            mk("not_preserved", "pkg1", Some("scope_violation"), "baseA", Some(false), 400),
            mk("timeout", "pkg1", Some("timeout"), "baseA", Some(true), 500),
            mk("other_pkg", "pkg2", Some("scope_violation"), "baseA", Some(true), 600),
            mk("self", "pkg1", Some("scope_violation"), "baseA", Some(true), 700),
        ];
        let picked = find_recoverable_scene(&execs, "pkg1", "self", "baseA");
        assert_eq!(picked.as_deref(), Some("new_sv"));
        // No candidate at all when nothing matches.
        assert_eq!(find_recoverable_scene(&execs, "pkg1", "self", "baseZ"), None);
    }

    /// S0-C: a human cancel does not consume the auto-retry budget — the
    /// chain's attempt_no advances but budget_used stays untouched, so a
    /// retry after a cancel is not rejected by the cap.
    #[test]
    fn retry_after_human_cancel_keeps_budget() {
        let (_dir, _guard) = setup_temp_runtime();
        let packages = serde_json::json!({
            "version": 1,
            "packages": [{
                "package_id": "pkg1", "epic_id": "e1", "step_id": "s1",
                "plan_revision": 1, "project_path": "/tmp/proj", "repo_root": "/tmp/proj",
                "base_sha": "abc123", "objective": "budget semantics",
                "scope": {"allowed_paths": ["**"], "forbidden_paths": [], "max_changed_files": 10},
                "executor": {"preferred": "zcode", "fallbacks": [], "sandbox": "workspace-write",
                             "timeout_seconds": 3600, "max_attempts": 2},
                "acceptance": [], "status": "blocked",
                "sealed_at": "2026-08-17T00:00:00Z", "package_hash": "fixture"
            }]
        });
        std::fs::write(packages_path(), serde_json::to_string_pretty(&packages).unwrap()).unwrap();

        let first = enqueue_execution("pkg1", "e1", "s1", "/tmp/proj", "abc123").unwrap();
        let _ = acquire_lease(&first, "runner-1").unwrap();
        finish_execution(&first, ExecutionStatus::Cancelled, None, Some("cancelled")).unwrap();
        let after_cancel = get_execution(&first).unwrap();
        assert_eq!(after_cancel.budget_used, 0, "human cancel must not charge budget");
        assert_eq!(after_cancel.outcome.as_deref(), Some("cancelled"));

        // With max_attempts=2 the old attempt-based cap would still allow one
        // retry; with several cancels it would wrongly exhaust. Loop three
        // cancels — budget must stay 0 and retries must keep succeeding.
        let mut current = first;
        for expected_attempt in 2..=4 {
            let child = create_retry_attempt(&current).unwrap();
            let _ = acquire_lease(&child, "runner-1").unwrap();
            finish_execution(&child, ExecutionStatus::Cancelled, None, Some("cancelled")).unwrap();
            let row = get_execution(&child).unwrap();
            assert_eq!(row.attempt_no, expected_attempt);
            assert_eq!(row.budget_used, 0, "cancel chain must never charge budget");
            current = child;
        }
    }

    /// S0-C: executor-attributable failures (timeout) consume budget; the
    /// cap rejects a retry only once the budget is genuinely exhausted.
    #[test]
    fn timeout_budget_exhaustion_rejects_retry() {
        let (_dir, _guard) = setup_temp_runtime();
        let packages = serde_json::json!({
            "version": 1,
            "packages": [{
                "package_id": "pkg1", "epic_id": "e1", "step_id": "s1",
                "plan_revision": 1, "project_path": "/tmp/proj", "repo_root": "/tmp/proj",
                "base_sha": "abc123", "objective": "budget semantics",
                "scope": {"allowed_paths": ["**"], "forbidden_paths": [], "max_changed_files": 10},
                "executor": {"preferred": "zcode", "fallbacks": [], "sandbox": "workspace-write",
                             "timeout_seconds": 3600, "max_attempts": 2},
                "acceptance": [], "status": "blocked",
                "sealed_at": "2026-08-17T00:00:00Z", "package_hash": "fixture"
            }]
        });
        std::fs::write(packages_path(), serde_json::to_string_pretty(&packages).unwrap()).unwrap();

        let first = enqueue_execution("pkg1", "e1", "s1", "/tmp/proj", "abc123").unwrap();
        let _ = acquire_lease(&first, "runner-1").unwrap();
        finish_execution(&first, ExecutionStatus::TimedOut, Some("timeout"), Some("timeout")).unwrap();
        assert_eq!(get_execution(&first).unwrap().budget_used, 1);

        let second = create_retry_attempt(&first).unwrap();
        let child_row = get_execution(&second).unwrap();
        assert_eq!(child_row.budget_used, 1, "child inherits the charged budget");
        assert_eq!(child_row.attempt_no, 2);

        let _ = acquire_lease(&second, "runner-1").unwrap();
        finish_execution(&second, ExecutionStatus::TimedOut, Some("timeout"), Some("timeout")).unwrap();
        assert_eq!(get_execution(&second).unwrap().budget_used, 2);

        let rejected = create_retry_attempt(&second).unwrap_err();
        assert!(rejected.contains("maximum attempts reached"), "{rejected}");
    }

    #[test]
    fn create_retry_preserves_parent() {
        let (_dir, _guard) = setup_temp_runtime();
        let package = serde_json::json!({
            "version": 1,
            "packages": [{
                "package_id": "pkg1",
                "epic_id": "e1",
                "step_id": "s1",
                "plan_revision": 1,
                "project_path": "/tmp/proj",
                "repo_root": "/tmp/proj",
                "base_sha": "abc123",
                "objective": "test retry",
                "scope": {
                    "allowed_paths": ["**"],
                    "forbidden_paths": [],
                    "max_changed_files": 10
                },
                "executor": {
                    "preferred": "zcode",
                    "fallbacks": [],
                    "sandbox": "workspace-write",
                    "timeout_seconds": 3600,
                    "max_attempts": 2
                },
                "acceptance": [],
                "status": "blocked",
                "sealed_at": "2026-08-11T00:00:00Z",
                "package_hash": "fixture"
            }]
        });
        std::fs::write(
            packages_path(),
            serde_json::to_string_pretty(&package).unwrap(),
        )
        .unwrap();
        let parent_id = enqueue_execution("pkg1", "e1", "s1", "/tmp/proj", "abc123").unwrap();
        // Simulate the parent having been processed (running + attempt 1)
        let _exec = acquire_lease(&parent_id, "runner-1").unwrap();
        release_lease(&parent_id, ExecutionStatus::Failed, Some("test failure")).unwrap();

        let parent_before = get_execution(&parent_id).unwrap();
        assert_eq!(parent_before.status, ExecutionStatus::Failed);

        // Create retry
        let child_id = create_retry_attempt(&parent_id).unwrap();
        assert_ne!(child_id, parent_id);

        // Parent must be unchanged
        let parent_after = get_execution(&parent_id).unwrap();
        assert_eq!(parent_after.status, ExecutionStatus::Failed);
        assert_eq!(parent_after.attempt_no, 1);

        // Child must have correct linkage
        let child = get_execution(&child_id).unwrap();
        assert_eq!(child.attempt_no, 2);
        assert_eq!(child.parent_attempt_id, Some(parent_id.clone()));
        assert_eq!(child.status, ExecutionStatus::Queued);
        assert_eq!(child.package_id, "pkg1");

        let package_after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(packages_path()).unwrap()).unwrap();
        assert_eq!(package_after["packages"][0]["status"], "ready");
    }

    #[test]
    fn claim_is_atomic_and_unique() {
        let (_dir, _guard) = setup_temp_runtime();
        let first = claim_package("pkg1", "e1", "s1", "/tmp/proj", "abc123", "runner-1").unwrap();
        assert_eq!(first.status, ExecutionStatus::Running);
        assert!(first.lease.is_some());
        assert_eq!(first.attempt_no, 1);

        // A concurrent poller claiming the same package must be refused.
        let second = claim_package("pkg1", "e1", "s1", "/tmp/proj", "abc123", "runner-2");
        assert!(second.is_err(), "double claim must be rejected");
        assert_eq!(
            list_executions()
                .unwrap()
                .iter()
                .filter(|e| e.package_id == "pkg1")
                .count(),
            1,
            "rejected claim must not create a second execution row"
        );
    }

    #[test]
    fn claim_adopts_precreated_retry_child() {
        let (_dir, _guard) = setup_temp_runtime();
        // A retry child is pre-created in Queued state carrying lineage.
        let child_id = enqueue_execution_with_parent(
            "pkg1",
            "e1",
            "s1",
            "/tmp/proj",
            "abc123",
            Some("exec_parent"),
            2,
        )
        .unwrap();

        let claimed = claim_package("pkg1", "e1", "s1", "/tmp/proj", "abc123", "runner-1").unwrap();
        assert_eq!(claimed.id, child_id, "claim must adopt the queued child");
        assert_eq!(claimed.status, ExecutionStatus::Running);
        assert_eq!(claimed.attempt_no, 2, "lineage preserved");
        assert_eq!(claimed.parent_attempt_id.as_deref(), Some("exec_parent"));
    }

    #[test]
    fn mark_package_status_preserves_superseded_rows() {
        let (_dir, _guard) = setup_temp_runtime();
        let packages = serde_json::json!({
            "version": 1,
            "packages": [
                {"package_id": "pkg1", "status": "superseded"},
                {"package_id": "pkg1", "status": "sealed"}
            ]
        });
        std::fs::write(
            packages_path(),
            serde_json::to_string_pretty(&packages).unwrap(),
        )
        .unwrap();

        mark_package_status("pkg1", "blocked").unwrap();

        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(packages_path()).unwrap()).unwrap();
        assert_eq!(after["packages"][0]["status"], "superseded");
        assert_eq!(after["packages"][1]["status"], "blocked");
    }

    #[test]
    fn release_lease_refuses_to_overwrite_terminal_evidence() {
        let (_dir, _guard) = setup_temp_runtime();
        let id = enqueue_execution("pkg1", "e1", "s1", "/tmp/proj", "abc123").unwrap();
        acquire_lease(&id, "runner-1").unwrap();
        finish_execution(&id, ExecutionStatus::TimedOut, Some("timeout"), Some("timeout")).unwrap();

        // A late cancel must NOT rewrite the terminal state.
        let result = release_lease(&id, ExecutionStatus::Cancelled, Some("late cancel"));
        assert!(result.is_err(), "terminal overwrite must be refused");
        let exec = get_execution(&id).unwrap();
        assert_eq!(exec.status, ExecutionStatus::TimedOut);
        assert_eq!(exec.last_error.as_deref(), Some("timeout"));

        // A blocked execution CAN still be cancelled (the DR cancel flow).
        let id2 = enqueue_execution("pkg2", "e1", "s2", "/tmp/proj", "abc123").unwrap();
        acquire_lease(&id2, "runner-1").unwrap();
        finish_execution(&id2, ExecutionStatus::Blocked, Some("scope"), None).unwrap();
        release_lease(&id2, ExecutionStatus::Cancelled, Some("DR cancel")).unwrap();
        assert_eq!(
            get_execution(&id2).unwrap().status,
            ExecutionStatus::Cancelled
        );
    }

    #[test]
    fn retry_idempotency_returns_existing_child() {
        let (_dir, _guard) = setup_temp_runtime();
        let package = serde_json::json!({
            "version": 1,
            "packages": [{
                "package_id": "pkg1", "epic_id": "e1", "step_id": "s1",
                "plan_revision": 1, "project_path": "/tmp/proj", "repo_root": "/tmp/proj",
                "base_sha": "abc123", "objective": "t",
                "scope": {"allowed_paths": ["**"], "forbidden_paths": [], "max_changed_files": 10},
                "executor": {"preferred": "zcode", "fallbacks": [], "sandbox": "workspace-write",
                             "timeout_seconds": 3600, "max_attempts": 3},
                "acceptance": [], "status": "blocked",
                "sealed_at": "2026-08-11T00:00:00Z", "package_hash": "fixture"
            }]
        });
        std::fs::write(
            packages_path(),
            serde_json::to_string_pretty(&package).unwrap(),
        )
        .unwrap();

        let parent_id = enqueue_execution("pkg1", "e1", "s1", "/tmp/proj", "abc123").unwrap();
        acquire_lease(&parent_id, "runner-1").unwrap();
        release_lease(&parent_id, ExecutionStatus::TimedOut, Some("t")).unwrap();

        let first = create_retry_attempt(&parent_id).unwrap();
        let second = create_retry_attempt(&parent_id).unwrap();
        assert_eq!(first, second, "racing retries must collapse to one child");
        assert_eq!(
            list_executions()
                .unwrap()
                .iter()
                .filter(|e| e.parent_attempt_id.as_deref() == Some(parent_id.as_str()))
                .count(),
            1
        );
    }

    #[test]
    fn late_success_cannot_override_cancel_request() {
        let (_dir, _guard) = setup_temp_runtime();
        let id = enqueue_execution("pkg1", "e1", "s1", "/tmp/proj", "abc123").unwrap();
        acquire_lease(&id, "runner-1").unwrap();
        assert_eq!(
            request_control(&id, false).unwrap(),
            ExecutionStatus::CancelRequested
        );

        let final_status = finish_execution(&id, ExecutionStatus::Passed, None, None).unwrap();
        assert_eq!(final_status, ExecutionStatus::Cancelled);
        assert_eq!(
            get_execution(&id).unwrap().status,
            ExecutionStatus::Cancelled
        );
    }

    #[test]
    fn package_hash_matches_node_canonical_vector() {
        let value = serde_json::json!({
            "z": 1,
            "a": {"y": 2, "x": [{"b": 2, "a": 1}]},
            "status": "ready",
            "package_hash": "ignored"
        });
        assert_eq!(
            compute_package_hash(&value).unwrap(),
            "5a0838288edf4a4f2df2106be08daed20d6b4e86ff9e17b00eee9df629cf22c9"
        );
    }

    /// 待办租户 G2: a todo-sourced package carries `origin` — it must parse
    /// (unknown to old readers, known to us), project into the summary, and
    /// keep the local Runner from ever claiming it (runner.rs origin skip).
    #[test]
    fn raw_package_with_todo_origin_parses_and_projects() {
        let raw = serde_json::json!({
            "package_id": "pkg_x",
            "epic_id": "epic_1",
            "step_id": "step_1",
            "plan_revision": 1,
            "origin": {
                "type": "todo",
                "tenant_id": "test-project",
                "todo_id": "todo-1",
                "task_revision": "abc123",
                "consent_revision": 2,
                "task_key": "key456"
            },
            "project_path": "/tmp/p",
            "repo_root": "/tmp/p",
            "base_sha": "deadbeef",
            "objective": "研究任务",
            "scope": {"allowed_paths": ["a/**"], "forbidden_paths": [], "max_changed_files": 1},
            "executor": {"preferred": "manual", "fallbacks": [], "sandbox": "read-only", "timeout_seconds": 900, "max_attempts": 1},
            "acceptance": [{"type": "command", "command_id": "verify:readonly"}],
            "status": "sealed",
            "package_hash": "h"
        });
        let parsed: RawTaskPackage = serde_json::from_value(raw).expect("origin must parse");
        assert_eq!(parsed.origin.as_ref().unwrap().kind, "todo");
        assert_eq!(parsed.origin.as_ref().unwrap().consent_revision, 2);

        let summary: TaskPackageSummary = parsed.into();
        assert_eq!(summary.origin_kind.as_deref(), Some("todo"));
        assert_eq!(summary.origin_todo_id.as_deref(), Some("todo-1"));
        assert_eq!(summary.origin_task_key.as_deref(), Some("key456"));
        assert_eq!(summary.executor_preferred, "manual");
        assert_eq!(summary.sandbox, "read-only");
    }

    /// Old packages without `origin` still parse — the field is additive.
    #[test]
    fn raw_package_without_origin_still_parses() {
        let raw = serde_json::json!({
            "package_id": "pkg_y",
            "epic_id": "e",
            "step_id": "s",
            "plan_revision": 1,
            "project_path": "/tmp/p",
            "repo_root": "/tmp/p",
            "base_sha": "beef",
            "objective": "o",
            "scope": {"allowed_paths": ["a/**"], "forbidden_paths": [], "max_changed_files": 1},
            "executor": {"preferred": "zcode", "fallbacks": [], "sandbox": "workspace-write", "timeout_seconds": 60, "max_attempts": 2},
            "acceptance": [],
            "status": "sealed",
            "package_hash": "h"
        });
        let parsed: RawTaskPackage = serde_json::from_value(raw).unwrap();
        assert!(parsed.origin.is_none());
        let summary: TaskPackageSummary = parsed.into();
        assert!(summary.origin_kind.is_none());
    }

    /// 待办租户 G3: the STRICT read-only shape parses and projects — read
    /// scope granted by read_paths alone, verifier acceptance entries
    /// surface in the summary, and is_strict_read_only() recognizes it.
    #[test]
    fn strict_read_only_package_parses_and_projects() {
        let raw = serde_json::json!({
            "package_id": "pkg_ro",
            "epic_id": "e",
            "step_id": "s",
            "plan_revision": 1,
            "origin": {"type": "todo", "tenant_id": "t", "todo_id": "d", "task_revision": "r", "consent_revision": 1, "task_key": "k"},
            "project_path": "/tmp/p",
            "repo_root": "/tmp/p",
            "base_sha": "beef",
            "objective": "研究",
            "scope": {"allowed_paths": [], "forbidden_paths": [], "read_paths": ["**"], "max_changed_files": 0},
            "executor": {"preferred": "manual", "fallbacks": [], "sandbox": "read-only", "timeout_seconds": 900, "max_attempts": 1},
            "acceptance": [
                {"type": "command", "command_id": "verify:readonly"},
                {"type": "verifier", "verifier_id": "todo:report"}
            ],
            "authority": {"auto_edit_worktree": false, "auto_commit_candidate": false, "auto_merge": false, "auto_push": false, "auto_deploy": false},
            "status": "sealed",
            "package_hash": "h"
        });
        let parsed: RawTaskPackage = serde_json::from_value(raw).unwrap();
        let summary: TaskPackageSummary = parsed.into();
        assert!(summary.is_strict_read_only());
        assert_eq!(summary.read_paths, vec!["**".to_string()]);
        assert!(summary.allowed_paths.is_empty());
        assert_eq!(summary.max_changed_files, 0);
        assert_eq!(summary.acceptance_command_ids, vec!["verify:readonly".to_string()]);
        assert_eq!(summary.acceptance_verifier_ids, vec!["todo:report".to_string()]);
        assert!(!summary.auto_edit_worktree && !summary.auto_commit_candidate);
    }

    /// Legacy read-only (default authority + max ≥ 1, the existing read-only
    /// report contract) is NOT strict — regression guard for S6 chains.
    #[test]
    fn legacy_read_only_shape_is_not_strict() {
        let summary = TaskPackageSummary {
            package_id: "p".into(),
            epic_id: "e".into(),
            step_id: "s".into(),
            plan_revision: 1,
            project_path: "/p".into(),
            repo_root: "/p".into(),
            base_sha: "b".into(),
            objective: "o".into(),
            display_no: None,
            origin_kind: None,
            origin_todo_id: None,
            origin_task_key: None,
            allowed_paths: vec!["docs/**".into()],
            create_paths: vec![],
            forbidden_paths: vec![],
            read_paths: vec![],
            max_changed_files: 1,
            expected_changed_files: None,
            warn_changed_files: None,
            executor_preferred: "zcode".into(),
            executor_fallbacks: vec![],
            sandbox: "read-only".into(),
            timeout_seconds: 900,
            max_attempts: 2,
            acceptance_command_ids: vec!["verify:readonly".into()],
            acceptance_verifier_ids: vec![],
            preconditions: vec![],
            authority_present: true,
            auto_edit_worktree: true,
            auto_commit_candidate: true,
            auto_merge: false,
            auto_push: false,
            auto_deploy: false,
            package_hash: "h".into(),
            computed_hash: "h".into(),
            status: "sealed".into(),
            sealed_at: "t".into(),
        };
        assert!(!summary.is_strict_read_only());
    }
}
