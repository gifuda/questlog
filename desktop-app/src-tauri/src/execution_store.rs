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
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const RUNTIME_DIRNAME: &str = "questlog-runtime";
const EXECUTIONS_FILENAME: &str = "executions.json";
const EVENTS_FILENAME: &str = "events.jsonl";
const LEASE_TIMEOUT_SECS: u64 = 300; // 5 minutes — lease expires if no heartbeat

// ─── Types ───

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionStatus {
    /// Package is ready, waiting for Runner to pick up.
    Queued,
    /// Runner has acquired the lease and is starting work.
    Running,
    /// CLI produced a candidate commit; awaiting acceptance verification.
    Candidate,
    /// Acceptance passed; step advanced to verified.
    Passed,
    /// Acceptance failed; worktree preserved for inspection.
    Failed,
    /// Cancelled by user or system.
    Cancelled,
    /// Blocked — needs human decision (DecisionRequest).
    Blocked,
    /// Lease expired without completion.
    Stale,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionLease {
    pub execution_id: String,
    pub acquired_at: u64,     // unix timestamp
    pub last_heartbeat: u64,  // unix timestamp
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
    // Priority: QUESTLOG_RUNTIME env > home dir
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
    std::fs::create_dir_all(&runtime_dir()).map_err(|e| format!("mkdir error: {e}"))?;
    std::fs::create_dir_all(&outputs_dir()).map_err(|e| format!("mkdir outputs error: {e}"))?;
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
    ensure_runtime_dir()?;
    let path = executions_path();
    file_lock::with_file_lock(&path, || {
        let mut data = read_executions()?;
        f(&mut data)?;
        write_executions(&mut data)?;
        Ok(data)
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
pub fn acquire_lease(
    execution_id: &str,
    runner_id: &str,
) -> Result<Execution, String> {
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
            if !is_lease_stale(lease) && lease.runner_id != runner_id {
                return Err(format!(
                    "execution '{}' is already leased by runner '{}'",
                    execution_id, lease.runner_id
                ));
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
pub fn release_lease(
    execution_id: &str,
    final_status: ExecutionStatus,
    error: Option<&str>,
) -> Result<(), String> {
    let now = now_ts();
    let data = mutate_executions(|data| {
        let exec = data
            .executions
            .iter_mut()
            .find(|e| e.id == execution_id)
            .ok_or_else(|| format!("execution '{}' not found", execution_id))?;
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

/// Update execution fields (worktree, branch, candidate SHA, etc.).
pub fn update_execution(
    execution_id: &str,
    updates: ExecutionUpdates,
) -> Result<(), String> {
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
            exec.status = status;
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

/// Get the output directory for an execution.
pub fn execution_output_dir(execution_id: &str) -> PathBuf {
    outputs_dir().join(execution_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Serialize tests that share the QUESTLOG_RUNTIME env var — Rust runs
    // tests in parallel by default, and env vars are process-global.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn setup_temp_runtime() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        let guard = TEST_LOCK.lock().unwrap();
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
        let id2 = enqueue_execution("pkg2", "e2", "s2", "/tmp/proj", "abc2").unwrap();
        // Acquire lease on first — second should not be picked up (same project)
        let _exec = acquire_lease(&id1, "runner-1").unwrap();
        let next = find_next_queued("/tmp/proj").unwrap();
        assert!(next.is_none(), "should not pick up from same project while running");
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
}
