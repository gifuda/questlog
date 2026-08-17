//! AttemptBundle — immutable per-execution evidence directory.
//!
//! This module is the data foundation for **execution memory**: each execution
//! creates an independent, immutable evidence directory under
//! `<runtime>/questlog-runtime/attempts/<execution_id>/`, where every artifact
//! from creation to outcome is sealed. Raw artifacts are queryable locally
//! without AI summaries — summaries and UI labels are always derived data,
//! rebuildable from these raw files.
//!
//! Layout (§14.2 of the finalization plan):
//! ```text
//! attempts/<execution_id>/
//! ├── manifest.json              — execution metadata + integrity hash
//! ├── task-package.json           — sealed package snapshot at execution time
//! ├── environment-snapshot.json   — machine-readable pre-execution environment
//! ├── events.jsonl                — full event stream for this execution
//! ├── stdout.log                  — CLI stdout (copied from worktree)
//! ├── stderr.log                  — CLI stderr (copied from worktree)
//! ├── diff.patch                  — candidate diff against base SHA
//! ├── candidate.json              — candidate commit (SHA, message, trailers)
//! ├── verifier-results.json       — acceptance command results
//! ├── decisions.json              — DecisionRequest references
//! └── outcome.json                — final conclusion
//! ```
//!
//! ## AttemptManifest
//!
//! `manifest.json` is the index/entry point of a bundle (`AttemptManifest`).
//! It records:
//! - **Provenance & linkage**: `execution_id`, `attempt_no` (position in the
//!   retry chain; 1 = first attempt), `parent_attempt_id` (links a retry back
//!   to the prior attempt; `None` for first attempts), `epic_id` / `step_id`,
//!   `base_sha` and `candidate_sha` (`None` until a candidate is committed).
//! - **Package context**: `package_id`, `plan_revision`, `executor`, and the
//!   optional `hypothesis` — the single primary fix hypothesis driving a retry
//!   (`None` for the first attempt).
//! - **File references**: relative filenames of every evidence artifact in the
//!   bundle (`task_package_ref`, `environment_snapshot_ref`, `raw_trace_ref`,
//!   and optional `stdout_ref` / `stderr_ref` / `diff_ref` / `candidate_ref` /
//!   `verifier_ref` / `decisions_ref`). Optional refs are `None` when the
//!   artifact was never produced.
//! - **Lifecycle state**: `outcome` (starts as `"running"` until sealed),
//!   `integrity_hash` (`None` until seal), `created_at` / `sealed_at` timestamps.
//!
//! ## Bundle lifecycle: create → write evidence → seal
//!
//! 1. **Create** — `create_bundle_dir(execution_id)` mkdirs the bundle dir
//!    (idempotent) after `validate_id` rejects path traversal, absolute paths,
//!    and shell metacharacters (F3 audit fix).
//! 2. **Write evidence** — as the execution progresses, the Runner appends
//!    artifacts to the dir: manifest, task-package snapshot, environment
//!    snapshot, append-only `events.jsonl`, CLI stdout/stderr copies, diff
//!    patch, candidate info, verifier results, decision requests. All writes
//!    are atomic (temp-file + rename) so a partial bundle never shows a torn file.
//! 3. **Seal** — `write_outcome_and_seal` writes `outcome.json`, updates the
//!    manifest (outcome, candidate SHA, `sealed_at`), computes the integrity
//!    hash over every content file, and rewrites the manifest with the final
//!    hash. A sealed bundle is immutable: retries create a NEW attempt dir,
//!    never overwrite the old one.
//!
//! ## Integrity hash
//!
//! `integrity_hash` is a SHA-256 over all content files in the bundle dir at
//! seal time, with files sorted by name for deterministic ordering.
//! `manifest.json` itself is excluded (it stores the hash — including it would
//! be a self-reference) and so are atomic-write temp files (`<name>.tmp-<uuid>`,
//! F1 audit fix). Its purpose is tamper evidence: `verify_integrity` recomputes
//! the hash over the dir and reports whether the recorded hash still matches,
//! so a sealed bundle can be trusted as the ground-truth record of an execution.
//!
//! ## Readers (Trace Query)
//!
//! The read side (`read_manifest`, `read_bundle`, `list_bundles_for_step`,
//! `list_bundles_for_epic`) powers Trace Query: the Tauri commands
//! `get_attempt_bundles(step_id)` and `get_attempt_detail(execution_id)`.
//! These readers scan sealed manifests to reconstruct execution history —
//! including retry chains via `parent_attempt_id` — without needing the AI
//! session that produced them.
//!
//! Rules:
//! - Raw files are append/seal-only; retry creates a NEW attempt dir, never overwrites.
//! - Summaries and UI labels are derived data, always rebuildable from raw artifacts.
//! - integrity_hash covers all files in the dir at seal time.

use crate::execution_store::{self, Execution, TaskPackageSummary};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ─── Path resolution ───

/// The base directory for all attempt bundles.
fn attempts_base_dir() -> PathBuf {
    execution_store::runtime_dir().join("attempts")
}

/// The bundle directory for a specific execution.
/// Validates execution_id to prevent path traversal (F3 audit fix).
pub fn bundle_dir(execution_id: &str) -> Result<PathBuf, String> {
    validate_id(execution_id)?;
    Ok(attempts_base_dir().join(execution_id))
}

/// Create the bundle directory for an execution. Idempotent.
pub fn create_bundle_dir(execution_id: &str) -> Result<PathBuf, String> {
    let dir = bundle_dir(execution_id)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir bundle dir: {e}"))?;
    Ok(dir)
}

/// Validate that an ID is safe for use as a path component.
/// Rejects path traversal, absolute paths, and shell metacharacters.
fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("execution_id is empty".to_string());
    }
    if id.contains('/') || id.contains('\\') || id.contains("..") {
        return Err(format!(
            "execution_id contains path separator or traversal: '{}'",
            id
        ));
    }
    // Only allow alphanumeric, underscore, hyphen — UUIDs and exec_* IDs
    if !id
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!(
            "execution_id contains invalid characters: '{}'",
            id
        ));
    }
    Ok(())
}

// ─── Manifest ───

/// The manifest is the index/entry point for an attempt bundle.
/// It records provenance, parent linkage, and an integrity hash over all files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptManifest {
    pub execution_id: String,
    pub attempt_no: u32,
    /// Auto-retry budget consumed along the chain at seal time (S0-C).
    /// Read-compatible with pre-S0 manifests (absent → 0).
    #[serde(default)]
    pub budget_used: u32,
    pub parent_attempt_id: Option<String>,
    pub package_id: String,
    pub plan_revision: u32,
    pub executor: String,
    /// Single primary fix hypothesis (for retries). None for first attempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hypothesis: Option<String>,
    pub base_sha: String,
    pub candidate_sha: Option<String>,
    pub epic_id: String,
    pub step_id: String,

    // File references (relative filenames within the bundle dir)
    pub task_package_ref: String,
    pub environment_snapshot_ref: String,
    pub raw_trace_ref: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verifier_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decisions_ref: Option<String>,

    pub outcome: String,
    /// SHA-256 over all files in the bundle dir at seal time.
    /// None until the bundle is sealed (outcome written).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub integrity_hash: Option<String>,

    pub created_at: u64,
    pub sealed_at: Option<u64>,
}

impl AttemptManifest {
    /// Create a new manifest with default file references.
    pub fn new(
        execution_id: &str,
        attempt_no: u32,
        budget_used: u32,
        parent_attempt_id: Option<&str>,
        pkg: &TaskPackageSummary,
    ) -> Self {
        Self {
            execution_id: execution_id.to_string(),
            attempt_no,
            budget_used,
            parent_attempt_id: parent_attempt_id.map(|s| s.to_string()),
            package_id: pkg.package_id.clone(),
            plan_revision: pkg.plan_revision,
            executor: pkg.executor_preferred.clone(),
            hypothesis: None,
            base_sha: pkg.base_sha.clone(),
            candidate_sha: None,
            epic_id: pkg.epic_id.clone(),
            step_id: pkg.step_id.clone(),
            task_package_ref: "task-package.json".to_string(),
            environment_snapshot_ref: "environment-snapshot.json".to_string(),
            raw_trace_ref: "events.jsonl".to_string(),
            stdout_ref: None,
            stderr_ref: None,
            diff_ref: None,
            candidate_ref: None,
            verifier_ref: None,
            decisions_ref: None,
            outcome: "running".to_string(),
            integrity_hash: None,
            created_at: now_ts(),
            sealed_at: None,
        }
    }
}

// ─── Writers ───

/// Write the manifest to the bundle dir (atomic temp-file rename).
pub fn write_manifest(dir: &Path, manifest: &AttemptManifest) -> Result<(), String> {
    write_json_atomic(&dir.join("manifest.json"), manifest)
}

/// Write the sealed TaskPackage snapshot to the bundle dir.
pub fn write_task_package_snapshot(dir: &Path, pkg: &TaskPackageSummary) -> Result<(), String> {
    write_json_atomic(&dir.join("task-package.json"), pkg)
}

/// Copy CLI stdout/stderr from the worktree's .questlog-output/ into the bundle.
pub fn copy_cli_output(
    dir: &Path,
    stdout_src: Option<&Path>,
    stderr_src: Option<&Path>,
) -> Result<(Option<String>, Option<String>), String> {
    let stdout_ref = if let Some(src) = stdout_src {
        if src.exists() {
            std::fs::copy(src, dir.join("stdout.log")).map_err(|e| format!("copy stdout: {e}"))?;
            Some("stdout.log".to_string())
        } else {
            None
        }
    } else {
        None
    };

    let stderr_ref = if let Some(src) = stderr_src {
        if src.exists() {
            std::fs::copy(src, dir.join("stderr.log")).map_err(|e| format!("copy stderr: {e}"))?;
            Some("stderr.log".to_string())
        } else {
            None
        }
    } else {
        None
    };

    Ok((stdout_ref, stderr_ref))
}

/// Write the diff patch (git diff of candidate against base SHA).
pub fn write_diff_patch(
    dir: &Path,
    worktree_path: &Path,
    base_sha: &str,
) -> Result<Option<String>, String> {
    let output = std::process::Command::new("git")
        .args(["diff", base_sha, "HEAD"])
        .current_dir(worktree_path)
        .output()
        .map_err(|e| format!("git diff: {e}"))?;

    if output.stdout.is_empty() {
        return Ok(None);
    }

    let patch_path = dir.join("diff.patch");
    std::fs::write(&patch_path, &output.stdout).map_err(|e| format!("write diff: {e}"))?;
    Ok(Some("diff.patch".to_string()))
}

/// Candidate commit info written after a successful commit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateInfo {
    pub sha: String,
    pub message: String,
    pub base_sha: String,
    pub trailers: serde_json::Value,
}

/// Write candidate commit info.
pub fn write_candidate(dir: &Path, info: &CandidateInfo) -> Result<Option<String>, String> {
    write_json_atomic(&dir.join("candidate.json"), info)?;
    Ok(Some("candidate.json".to_string()))
}

/// A single verifier (acceptance command) result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifierResult {
    pub command_id: String,
    pub command: String,
    pub cwd: String,
    pub candidate_sha: Option<String>,
    pub exit_code: i32,
    pub started_at: u64,
    pub finished_at: u64,
    /// Truncated stdout (first N chars) — full output stays in stdout.log.
    pub stdout_excerpt: String,
    pub stderr_excerpt: String,
    pub passed: bool,
}

/// Write verifier results (all acceptance command outcomes).
pub fn write_verifier_results(
    dir: &Path,
    results: &[VerifierResult],
) -> Result<Option<String>, String> {
    if results.is_empty() {
        return Ok(None);
    }
    write_json_atomic(&dir.join("verifier-results.json"), &results)?;
    Ok(Some("verifier-results.json".to_string()))
}

/// Persist the DecisionRequest bound to this attempt before sealing.
pub fn write_decision_request<T: Serialize>(
    dir: &Path,
    request: &T,
) -> Result<Option<String>, String> {
    write_json_atomic(&dir.join("decisions.json"), request)?;
    Ok(Some("decisions.json".to_string()))
}

/// The final outcome of an attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptOutcome {
    pub outcome: String,
    pub reason: Option<String>,
    pub candidate_sha: Option<String>,
    pub finished_at: u64,
}

/// Write the outcome and seal the bundle (compute integrity hash).
pub fn write_outcome_and_seal(
    dir: &Path,
    manifest: &mut AttemptManifest,
    outcome: &AttemptOutcome,
) -> Result<(), String> {
    write_json_atomic(&dir.join("outcome.json"), outcome)?;

    manifest.outcome = outcome.outcome.clone();
    manifest.candidate_sha = outcome.candidate_sha.clone();
    manifest.sealed_at = Some(outcome.finished_at);

    // Compute integrity hash over all files in the bundle dir
    let hash = compute_integrity_hash(dir)?;
    manifest.integrity_hash = Some(hash);

    // Re-write manifest with the final hash
    write_manifest(dir, manifest)
}

// ─── Events ───

/// Append a single event line to the bundle's local events.jsonl.
pub fn append_event(dir: &Path, event: &serde_json::Value) -> Result<(), String> {
    let path = dir.join("events.jsonl");
    let line = serde_json::to_string(event).map_err(|e| format!("serialize event: {e}"))?;
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("open events: {e}"))?;
    writeln!(file, "{line}").map_err(|e| format!("write event: {e}"))?;
    Ok(())
}

// ─── Integrity ───

/// Compute SHA-256 over all content files in the bundle directory.
/// Excludes `manifest.json` (stores the hash itself — self-reference would make
/// verification impossible) and atomic-write temp files (`<name>.tmp-<uuid>`).
/// Files are sorted by name for deterministic ordering.
fn compute_integrity_hash(dir: &Path) -> Result<String, String> {
    use std::io::Read;
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("read bundle dir: {e}"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
        .filter(|e| is_content_file(&e.file_name().to_string_lossy()))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    let mut hasher = sha2::Sha256::new();
    for entry in &entries {
        let name = entry.file_name();
        hasher.update(name.to_string_lossy().as_bytes());
        hasher.update(b"\0");

        let mut file =
            std::fs::File::open(entry.path()).map_err(|e| format!("open for hash: {e}"))?;
        let mut buf = [0u8; 8192];
        loop {
            let n = file
                .read(&mut buf)
                .map_err(|e| format!("read for hash: {e}"))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        hasher.update(b"\0");
    }

    Ok(format!("{:x}", hasher.finalize()))
}

/// Determine if a file is a content file (should be included in integrity hash).
/// Excludes manifest.json (stores the hash itself) and atomic-write temp files.
/// Atomic temp files have the pattern `<name>.tmp-<uuid>` (hex UUID suffix).
fn is_content_file(name: &str) -> bool {
    if name == "manifest.json" {
        return false;
    }
    // Atomic write temp: "<name>.tmp-<uuid>". Reject only the exact pattern
    // to avoid over-broad exclusion (F1 audit fix).
    if let Some(tmp_part) = name.rfind(".tmp-") {
        let suffix = &name[tmp_part + 5..]; // after ".tmp-"
                                            // UUID v4 is 36 chars: 8-4-4-4-12 hex + hyphens
        return suffix.is_empty() || !suffix.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    }
    true
}

/// Verify the integrity hash of a sealed bundle.
pub fn verify_integrity(dir: &Path) -> Result<bool, String> {
    let manifest_path = dir.join("manifest.json");
    if !manifest_path.exists() {
        return Err("manifest.json not found".to_string());
    }
    let manifest: AttemptManifest = serde_json::from_str(
        &std::fs::read_to_string(&manifest_path).map_err(|e| format!("read manifest: {e}"))?,
    )
    .map_err(|e| format!("parse manifest: {e}"))?;

    let recorded = manifest.integrity_hash.ok_or("bundle not sealed")?;
    let actual = compute_integrity_hash(dir)?;
    Ok(recorded == actual)
}

// ─── Readers (for Trace Query) ───

/// Read a bundle's manifest.
pub fn read_manifest(execution_id: &str) -> Result<AttemptManifest, String> {
    let path = bundle_dir(execution_id)?.join("manifest.json");
    if !path.exists() {
        return Err(format!("no bundle for execution '{}'", execution_id));
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("read manifest: {e}"))?;
    serde_json::from_str(&raw).map_err(|e| format!("parse manifest: {e}"))
}

/// Read the full bundle (manifest + check dir exists).
pub fn read_bundle(execution_id: &str) -> Result<(AttemptManifest, PathBuf), String> {
    let manifest = read_manifest(execution_id)?;
    let dir = bundle_dir(execution_id)?;
    if !dir.exists() {
        return Err(format!("bundle dir missing for '{}'", execution_id));
    }
    Ok((manifest, dir))
}

/// List all attempt manifests for a given step, sorted by attempt_no ascending.
pub fn list_bundles_for_step(step_id: &str) -> Result<Vec<AttemptManifest>, String> {
    list_bundles_filtered(|m| m.step_id == step_id)
}

/// List all attempt manifests for a given epic.
pub fn list_bundles_for_epic(epic_id: &str) -> Result<Vec<AttemptManifest>, String> {
    list_bundles_filtered(|m| m.epic_id == epic_id)
}

fn list_bundles_filtered<F>(f: F) -> Result<Vec<AttemptManifest>, String>
where
    F: Fn(&AttemptManifest) -> bool,
{
    let base = attempts_base_dir();
    if !base.exists() {
        return Ok(vec![]);
    }

    let mut manifests: Vec<AttemptManifest> = Vec::new();
    for entry in std::fs::read_dir(&base).map_err(|e| format!("read attempts dir: {e}"))? {
        let entry = entry.map_err(|e| format!("dir entry: {e}"))?;
        let manifest_path = entry.path().join("manifest.json");
        if !manifest_path.exists() {
            continue;
        }
        let raw = match std::fs::read_to_string(&manifest_path) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let manifest: AttemptManifest = match serde_json::from_str(&raw) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if f(&manifest) {
            manifests.push(manifest);
        }
    }

    manifests.sort_by_key(|m| m.attempt_no);
    Ok(manifests)
}

// ─── EnvironmentSnapshot ───

/// Machine-readable environment snapshot generated by the Runner before
/// spawning the executor. Reduces redundant probing on retries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentSnapshot {
    pub repo_root: String,
    pub project_path: String,
    pub base_sha: String,
    pub branch: Option<String>,
    pub git_clean: bool,

    // Scope
    pub allowed_paths: Vec<String>,
    pub forbidden_paths: Vec<String>,
    pub max_changed_files: u32,

    // Authority
    pub auto_edit_worktree: bool,
    pub auto_commit_candidate: bool,
    pub auto_merge: bool,
    pub auto_push: bool,
    pub auto_deploy: bool,

    // Executor policy
    pub executor: String,
    pub sandbox: String,
    pub timeout_seconds: u64,
    pub remaining_attempts: u32,

    // Acceptance
    pub acceptance_command_ids: Vec<String>,

    // Parent linkage (for retries)
    pub parent_attempt_id: Option<String>,
    pub parent_outcome: Option<String>,
    pub parent_failed_verifiers: Vec<String>,

    pub generated_at: u64,
}

/// Information about the parent attempt (for retries).
pub struct ParentInfo {
    pub attempt_id: String,
    pub outcome: String,
    pub failed_verifiers: Vec<String>,
}

/// Generate an environment snapshot from the package, execution, and optional parent info.
pub fn generate_environment_snapshot(
    pkg: &TaskPackageSummary,
    exec: &Execution,
    parent: Option<&ParentInfo>,
    git_clean: bool,
) -> EnvironmentSnapshot {
    // F6 audit fix: use attempt_no (retry-chain position), not attempts (lease count).
    // exec.attempts counts how many times this specific row was leased;
    // exec.attempt_no is the position in the retry chain (1=first, 2=first retry...).
    let remaining = pkg.max_attempts.saturating_sub(exec.attempt_no);

    let (parent_id, parent_outcome, parent_failed) = match parent {
        Some(p) => (
            Some(p.attempt_id.clone()),
            Some(p.outcome.clone()),
            p.failed_verifiers.clone(),
        ),
        None => (None, None, vec![]),
    };

    EnvironmentSnapshot {
        repo_root: pkg.repo_root.clone(),
        project_path: pkg.project_path.clone(),
        base_sha: pkg.base_sha.clone(),
        branch: exec.branch.clone(),
        git_clean,
        allowed_paths: pkg.allowed_paths.clone(),
        forbidden_paths: pkg.forbidden_paths.clone(),
        max_changed_files: pkg.max_changed_files,
        auto_edit_worktree: true,
        auto_commit_candidate: true,
        auto_merge: false,
        auto_push: false,
        auto_deploy: false,
        executor: pkg.executor_preferred.clone(),
        sandbox: if pkg.sandbox.is_empty() {
            "workspace-write".to_string()
        } else {
            pkg.sandbox.clone()
        },
        timeout_seconds: pkg.timeout_seconds,
        remaining_attempts: remaining,
        acceptance_command_ids: pkg.acceptance_command_ids.clone(),
        parent_attempt_id: parent_id,
        parent_outcome,
        parent_failed_verifiers: parent_failed,
        generated_at: now_ts(),
    }
}

/// Write the environment snapshot to the bundle dir.
pub fn write_environment_snapshot(
    dir: &Path,
    snapshot: &EnvironmentSnapshot,
) -> Result<(), String> {
    write_json_atomic(&dir.join("environment-snapshot.json"), snapshot)
}

// ─── Helpers ───

fn now_ts() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let json = serde_json::to_string_pretty(value).map_err(|e| format!("serialize: {e}"))?;
    let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, &json).map_err(|e| format!("write tmp: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

// ─── SHA-256 via the `sha2` crate (replaces hand-rolled impl) ───
// The `sha2` crate is already in the dependency tree via tauri's transitive deps.
// We add it as a direct dependency for the audit-recommended replacement of the
// hand-rolled SHA-256 (which was correct but a maintenance risk).

use sha2::Digest;

/// Shared SHA-256 helper used for cross-language TaskPackage hash checks.
pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution_store::{ExecutionStatus, TEST_LOCK};

    #[allow(unused_imports)]
    use crate::attempt_bundle as _ab;

    fn setup_temp_runtime() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        let guard = TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("QUESTLOG_RUNTIME", dir.path());
        (dir, guard)
    }

    fn mock_pkg() -> TaskPackageSummary {
        TaskPackageSummary {
            package_id: "pkg_test".to_string(),
            epic_id: "epic_1".to_string(),
            step_id: "step_1".to_string(),
            plan_revision: 1,
            project_path: "/tmp/proj".to_string(),
            repo_root: "/tmp/proj".to_string(),
            base_sha: "abc123".to_string(),
            objective: "test objective".to_string(),
            allowed_paths: vec!["src.ts".to_string()],
            forbidden_paths: vec![],
            max_changed_files: 5,
            executor_preferred: "zcode".to_string(),
            executor_fallbacks: vec![],
            sandbox: "workspace-write".to_string(),
            timeout_seconds: 3600,
            max_attempts: 2,
            acceptance_command_ids: vec!["test:mcp".to_string()],
            preconditions: vec![],
            auto_edit_worktree: true,
            auto_commit_candidate: true,
            auto_merge: false,
            auto_push: false,
            auto_deploy: false,
            sealed_at: "2026-08-11T00:00:00Z".to_string(),
            status: "ready".to_string(),
            package_hash: "hash123".to_string(),
            computed_hash: "hash123".to_string(),
        }
    }

    fn mock_exec(pkg: &TaskPackageSummary) -> Execution {
        Execution {
            id: "exec_test".to_string(),
            package_id: pkg.package_id.clone(),
            epic_id: pkg.epic_id.clone(),
            step_id: pkg.step_id.clone(),
            project_path: pkg.project_path.clone(),
            status: ExecutionStatus::Running,
            lease: None,
            cli_session: None,
            worktree_path: None,
            branch: None,
            base_sha: pkg.base_sha.clone(),
            candidate_sha: None,
            attempts: 1,
            last_error: None,
            created_at: 1000,
            updated_at: 1000,
            attempt_no: 1,
            outcome: None,
            budget_used: 0,
            budget_charged: false,
            retries_remaining: None,
            parent_attempt_id: None,
            bundle_path: None,
            executor: None,
        }
    }

    #[test]
    fn create_bundle_and_write_manifest() {
        let (_dir, _guard) = setup_temp_runtime();
        let pkg = mock_pkg();
        let exec = mock_exec(&pkg);

        let bundle_path = create_bundle_dir(&exec.id).unwrap();
        assert!(bundle_path.exists());

        let manifest = AttemptManifest::new(&exec.id, 1, 0, None, &pkg);
        write_manifest(&bundle_path, &manifest).unwrap();

        let snapshot = generate_environment_snapshot(&pkg, &exec, None, true);
        write_environment_snapshot(&bundle_path, &snapshot).unwrap();
        write_task_package_snapshot(&bundle_path, &pkg).unwrap();

        // All three files should exist
        assert!(bundle_path.join("manifest.json").exists());
        assert!(bundle_path.join("environment-snapshot.json").exists());
        assert!(bundle_path.join("task-package.json").exists());
    }

    #[test]
    fn integrity_hash_changes_on_tamper() {
        let (_dir, _guard) = setup_temp_runtime();
        let pkg = mock_pkg();
        let exec = mock_exec(&pkg);

        let dir = create_bundle_dir(&exec.id).unwrap();
        let mut manifest = AttemptManifest::new(&exec.id, 1, 0, None, &pkg);
        write_manifest(&dir, &manifest).unwrap();
        write_task_package_snapshot(&dir, &pkg).unwrap();

        // Seal the bundle
        let outcome = AttemptOutcome {
            outcome: "passed".to_string(),
            reason: None,
            candidate_sha: Some("cand_sha".to_string()),
            finished_at: now_ts(),
        };
        write_outcome_and_seal(&dir, &mut manifest, &outcome).unwrap();

        // Verify integrity
        assert!(verify_integrity(&dir).unwrap());

        // Tamper with a file
        std::fs::write(dir.join("task-package.json"), "tampered").unwrap();

        // Integrity should now fail
        assert!(!verify_integrity(&dir).unwrap());
    }

    #[test]
    fn environment_snapshot_with_parent() {
        let (_dir, _guard) = setup_temp_runtime();
        let pkg = mock_pkg();
        let exec = mock_exec(&pkg);

        let parent = ParentInfo {
            attempt_id: "exec_prev".to_string(),
            outcome: "failed".to_string(),
            failed_verifiers: vec!["test:mcp".to_string()],
        };

        let snapshot = generate_environment_snapshot(&pkg, &exec, Some(&parent), false);

        assert_eq!(snapshot.parent_attempt_id, Some("exec_prev".to_string()));
        assert_eq!(snapshot.parent_outcome, Some("failed".to_string()));
        assert_eq!(
            snapshot.parent_failed_verifiers,
            vec!["test:mcp".to_string()]
        );
        assert!(!snapshot.git_clean);
        assert_eq!(snapshot.remaining_attempts, 1); // max=2, attempt_no=1
    }

    #[test]
    fn test_list_bundles_for_step() {
        let (_dir, _guard) = setup_temp_runtime();
        let pkg = mock_pkg();

        // Create two bundles for the same step (simulating retry)
        for attempt_no in 1..=2 {
            let exec_id = format!("exec_{}", attempt_no);
            let dir = create_bundle_dir(&exec_id).unwrap();
            let parent_id = if attempt_no == 1 {
                None
            } else {
                Some("exec_1")
            };
            let manifest = AttemptManifest::new(&exec_id, attempt_no, 0, parent_id, &pkg);
            write_manifest(&dir, &manifest).unwrap();
        }

        // Create a bundle for a different step
        let mut pkg2 = mock_pkg();
        pkg2.step_id = "step_other".to_string();
        let dir3 = create_bundle_dir("exec_3").unwrap();
        let manifest3 = AttemptManifest::new("exec_3", 1, 0, None, &pkg2);
        write_manifest(&dir3, &manifest3).unwrap();

        let bundles = list_bundles_for_step("step_1").unwrap();
        assert_eq!(bundles.len(), 2);
        assert_eq!(bundles[0].attempt_no, 1);
        assert_eq!(bundles[1].attempt_no, 2);
        assert_eq!(bundles[1].parent_attempt_id, Some("exec_1".to_string()));
    }

    #[test]
    fn append_events_to_bundle() {
        let (_dir, _guard) = setup_temp_runtime();
        let pkg = mock_pkg();
        let exec = mock_exec(&pkg);
        let dir = create_bundle_dir(&exec.id).unwrap();

        let event1 = serde_json::json!({"type": "started", "ts": 1});
        let event2 = serde_json::json!({"type": "completed", "ts": 2, "success": true});

        append_event(&dir, &event1).unwrap();
        append_event(&dir, &event2).unwrap();

        let content = std::fs::read_to_string(dir.join("events.jsonl")).unwrap();
        let lines: Vec<&str> = content.trim().lines().collect();
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn copy_cli_output_from_files() {
        let (_dir, _guard) = setup_temp_runtime();
        let pkg = mock_pkg();
        let exec = mock_exec(&pkg);
        let dir = create_bundle_dir(&exec.id).unwrap();

        // Create fake stdout/stderr source files
        let tmp = tempfile::tempdir().unwrap();
        let stdout_src = tmp.path().join("stdout.log");
        let stderr_src = tmp.path().join("stderr.log");
        std::fs::write(&stdout_src, "stdout content").unwrap();
        std::fs::write(&stderr_src, "stderr content").unwrap();

        let (stdout_ref, stderr_ref) =
            copy_cli_output(&dir, Some(&stdout_src), Some(&stderr_src)).unwrap();

        assert_eq!(stdout_ref, Some("stdout.log".to_string()));
        assert_eq!(stderr_ref, Some("stderr.log".to_string()));
        assert_eq!(
            std::fs::read_to_string(dir.join("stdout.log")).unwrap(),
            "stdout content"
        );
    }

    #[test]
    fn sha256_known_vector() {
        // Verify SHA-256 via the sha2 crate against a known vector.
        let hash = sha256_hex(b"abc");
        assert_eq!(
            hash,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn path_traversal_rejected() {
        // F3 audit fix: execution_id must not allow path traversal
        assert!(bundle_dir("../../../etc/passwd").is_err());
        assert!(bundle_dir("../escape").is_err());
        assert!(bundle_dir("/absolute/path").is_err());
        assert!(bundle_dir("a/b").is_err());
        assert!(bundle_dir("").is_err());
        assert!(bundle_dir("valid_exec_id_123").is_ok());
        assert!(bundle_dir("exec_abc-def_123").is_ok());
    }

    #[test]
    fn integrity_hash_tmp_filter_precise() {
        // F1 audit fix: only exclude .tmp-<uuid> pattern, not arbitrary "tmp-" substrings
        let (_dir, _guard) = setup_temp_runtime();
        let pkg = mock_pkg();
        let exec = mock_exec(&pkg);
        let dir = create_bundle_dir(&exec.id).unwrap();

        // Write a legitimate file whose name happens to contain "tmp-"
        std::fs::write(dir.join("data.tmp-notes.json"), "content").unwrap();

        let mut manifest = AttemptManifest::new(&exec.id, 1, 0, None, &pkg);
        write_manifest(&dir, &manifest).unwrap();

        let outcome = AttemptOutcome {
            outcome: "passed".to_string(),
            reason: None,
            candidate_sha: None,
            finished_at: now_ts(),
        };
        write_outcome_and_seal(&dir, &mut manifest, &outcome).unwrap();

        // The "data.tmp-notes.json" file should be INCLUDED in the hash
        // (its suffix after ".tmp-" is "notes.json" which is not hex, so it's a content file)
        assert!(verify_integrity(&dir).unwrap());

        // Tamper with it — should now fail
        std::fs::write(dir.join("data.tmp-notes.json"), "tampered").unwrap();
        assert!(!verify_integrity(&dir).unwrap());
    }
}
