//! Todo-task ledger, Rust side (待办租户 G4).
//!
//! Mirrors the Node producer's `todo-tasks.json` (snake_case, same lock
//! protocol). The Rust sweep/finalization share this file with the Node
//! producer CLI: both sides mutate under the owner-safe file lock, and
//! unknown fields survive round-trips on both sides.
//!
//! Also owns the runtime paths the pipeline uses:
//!   todo-tasks.json          — the ledger (shared with Node)
//!   todo-sweep-state.json    — nightly window bookkeeping (Rust only)
//!   todo-outbox/             — delivery messages the collector pulls (GX)
//!   todo-receipts/           — execution receipts dropped for finalization
//!   todo-reports/            — persisted reports (report.md + manifest)

use crate::file_lock::FileLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum LedgerState {
    Preparing,
    Published,
    Closed,
    Superseded,
}

/// Finalization bookkeeping on an entry (Rust-written; Node tolerates and
/// preserves the unknown field).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FinalizationRecord {
    /// pending | applied
    pub state: String,
    pub attempt_id: String,
    pub report_hash: String,
    /// accepted_report | cannot_auto | needs_review | execution_failed |
    /// timed_out | stale
    pub outcome: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub task_key: String,
    pub tenant_id: String,
    pub todo_id: String,
    pub task_revision: String,
    pub consent_revision: u64,
    pub operation_id: String,
    pub state: LedgerState,
    #[serde(default)]
    pub epic_id: String,
    #[serde(default)]
    pub step_id: String,
    #[serde(default)]
    pub package_id: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finalization: Option<FinalizationRecord>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct LedgerFile {
    pub version: u32,
    #[serde(default)]
    pub entries: HashMap<String, LedgerEntry>,
}

pub fn ledger_path() -> PathBuf {
    crate::execution_store::runtime_dir().join("todo-tasks.json")
}

pub fn sweep_state_path() -> PathBuf {
    crate::execution_store::runtime_dir().join("todo-sweep-state.json")
}

pub fn outbox_dir() -> PathBuf {
    crate::execution_store::runtime_dir().join("todo-outbox")
}

pub fn receipts_dir() -> PathBuf {
    crate::execution_store::runtime_dir().join("todo-receipts")
}

pub fn reports_dir() -> PathBuf {
    crate::execution_store::runtime_dir().join("todo-reports")
}

/// Read the ledger. Corrupt JSON BLOCKS (fail-closed) — an empty ledger
/// would let the producer duplicate every task.
pub fn read_ledger() -> Result<LedgerFile, String> {
    let path = ledger_path();
    if !path.exists() {
        return Ok(LedgerFile {
            version: 1,
            entries: HashMap::new(),
        });
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("read ledger: {e}"))?;
    let parsed: LedgerFile =
        serde_json::from_str(&raw).map_err(|e| format!("ledger unreadable ({e}) — manual repair required"))?;
    Ok(parsed)
}

/// Lock-protected read-modify-write. The closure mutates the entries map;
/// returning Err aborts with no write.
pub fn mutate_ledger<T>(
    f: impl FnOnce(&mut HashMap<String, LedgerEntry>) -> Result<T, String>,
) -> Result<T, String> {
    crate::data_root::guard_write()?;
    let dir = crate::execution_store::runtime_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir runtime: {e}"))?;
    let _lock = FileLock::acquire(&ledger_path())?;
    let mut file = read_ledger()?;
    let out = f(&mut file.entries)?;
    let tmp = ledger_path().with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let json = serde_json::to_string_pretty(&file).map_err(|e| format!("serialize ledger: {e}"))?;
    std::fs::write(&tmp, &json).map_err(|e| format!("write ledger tmp: {e}"))?;
    std::fs::rename(&tmp, ledger_path()).map_err(|e| format!("rename ledger: {e}"))?;
    Ok(out)
}

pub fn entry_is_open(e: &LedgerEntry) -> bool {
    matches!(e.state, LedgerState::Preparing | LedgerState::Published)
}

/// ─── Sweep state (nightly window bookkeeping, Rust only) ───

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SweepState {
    /// Last business date (+08:00) a sweep RAN, "YYYY-MM-DD".
    #[serde(default)]
    pub last_run_date: String,
    /// Last business date recorded as missed (window passed without a run).
    #[serde(default)]
    pub last_missed_date: String,
    #[serde(default)]
    pub updated_at: String,
}

pub fn read_sweep_state() -> SweepState {
    std::fs::read_to_string(sweep_state_path())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

pub fn write_sweep_state(state: &SweepState) -> Result<(), String> {
    crate::data_root::guard_write()?;
    let path = sweep_state_path();
    let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let json = serde_json::to_string_pretty(state).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, &json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(())
}

/// ─── Receipts ───

/// A receipt dropped into `todo-receipts/` by the collector-side channel
/// (GX adapter). All flags are supplied by the channel; the finalizer
/// verifies the REPORT itself and never trusts the receipt's self-assessment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub version: u32,
    pub task_key: String,
    pub attempt_id: String,
    #[serde(default)]
    pub platform_task_id: Option<String>,
    #[serde(default)]
    pub exit_ok: bool,
    #[serde(default)]
    pub timed_out: bool,
    #[serde(default)]
    pub budget_exceeded: bool,
    #[serde(default)]
    pub changes_detected: bool,
    /// Raw executor stdout (report text) — capped by the channel at the
    /// report_limit before delivery.
    #[serde(default)]
    pub report_stdout: String,
    #[serde(default)]
    pub received_at: String,
}
