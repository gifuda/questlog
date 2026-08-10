//! AttemptBundle — immutable per-execution evidence directory.
//!
//! Each execution creates an independent, immutable evidence directory under
//! `<runtime>/questlog-runtime/attempts/<execution_id>/`. This is the data
//! foundation for execution memory: every attempt from creation to outcome
//! has its raw artifacts sealed here, queryable locally without AI summaries.
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
pub fn bundle_dir(execution_id: &str) -> PathBuf {
    attempts_base_dir().join(execution_id)
}

/// Create the bundle directory for an execution. Idempotent.
pub fn create_bundle_dir(execution_id: &str) -> Result<PathBuf, String> {
    let dir = bundle_dir(execution_id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir bundle dir: {e}"))?;
    Ok(dir)
}

// ─── Manifest ───

/// The manifest is the index/entry point for an attempt bundle.
/// It records provenance, parent linkage, and an integrity hash over all files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptManifest {
    pub execution_id: String,
    pub attempt_no: u32,
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
        parent_attempt_id: Option<&str>,
        pkg: &TaskPackageSummary,
    ) -> Self {
        Self {
            execution_id: execution_id.to_string(),
            attempt_no,
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
/// Excludes `manifest.json` itself (the manifest stores the hash — self-reference
/// would make it impossible to verify) and any temp files.
/// Files are sorted by name for deterministic ordering.
fn compute_integrity_hash(dir: &Path) -> Result<String, String> {
    use std::io::Read;
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("read bundle dir: {e}"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            // Exclude manifest (stores the hash itself) and temp files
            name != "manifest.json" && !name.starts_with(".tmp-") && !name.contains("tmp-")
        })
        .collect();
    entries.sort_by_key(|e| e.file_name());

    let mut hasher = sha256::Sha256::new();
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

    Ok(hasher.finalize_hex())
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
    let path = bundle_dir(execution_id).join("manifest.json");
    if !path.exists() {
        return Err(format!("no bundle for execution '{}'", execution_id));
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("read manifest: {e}"))?;
    serde_json::from_str(&raw).map_err(|e| format!("parse manifest: {e}"))
}

/// Read the full bundle (manifest + check dir exists).
pub fn read_bundle(execution_id: &str) -> Result<(AttemptManifest, PathBuf), String> {
    let manifest = read_manifest(execution_id)?;
    let dir = bundle_dir(execution_id);
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
    let remaining = pkg.max_attempts.saturating_sub(exec.attempts);

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

// ─── SHA-256 minimal impl (no extra crate dependency) ───

mod sha256 {
    //! Minimal SHA-256 implementation to avoid adding a crypto crate dependency.
    //! Based on the FIPS 180-4 specification.

    pub struct Sha256 {
        state: [u32; 8],
        buffer: Vec<u8>,
        length: u64,
    }

    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    impl Sha256 {
        pub fn new() -> Self {
            Self {
                state: [
                    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c,
                    0x1f83d9ab, 0x5be0cd19,
                ],
                buffer: Vec::with_capacity(64),
                length: 0,
            }
        }

        pub fn update(&mut self, data: &[u8]) {
            self.length += data.len() as u64;
            self.buffer.extend_from_slice(data);
            while self.buffer.len() >= 64 {
                let block: Vec<u8> = self.buffer.drain(..64).collect();
                self.process_block(&block);
            }
        }

        fn process_block(&mut self, block: &[u8]) {
            let mut w = [0u32; 64];
            for i in 0..16 {
                w[i] = u32::from_be_bytes([
                    block[i * 4],
                    block[i * 4 + 1],
                    block[i * 4 + 2],
                    block[i * 4 + 3],
                ]);
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[i - 7])
                    .wrapping_add(s1);
            }

            let mut a = self.state[0];
            let mut b = self.state[1];
            let mut c = self.state[2];
            let mut d = self.state[3];
            let mut e = self.state[4];
            let mut f = self.state[5];
            let mut g = self.state[6];
            let mut h = self.state[7];

            for i in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ ((!e) & g);
                let temp1 = h
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(K[i])
                    .wrapping_add(w[i]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let maj = (a & b) ^ (a & c) ^ (b & c);
                let temp2 = s0.wrapping_add(maj);

                h = g;
                g = f;
                f = e;
                e = d.wrapping_add(temp1);
                d = c;
                c = b;
                b = a;
                a = temp1.wrapping_add(temp2);
            }

            self.state[0] = self.state[0].wrapping_add(a);
            self.state[1] = self.state[1].wrapping_add(b);
            self.state[2] = self.state[2].wrapping_add(c);
            self.state[3] = self.state[3].wrapping_add(d);
            self.state[4] = self.state[4].wrapping_add(e);
            self.state[5] = self.state[5].wrapping_add(f);
            self.state[6] = self.state[6].wrapping_add(g);
            self.state[7] = self.state[7].wrapping_add(h);
        }

        pub fn finalize_hex(mut self) -> String {
            let length = self.length;

            // Padding
            self.buffer.push(0x80);
            while self.buffer.len() % 64 != 56 {
                self.buffer.push(0);
            }
            let bit_length = length.wrapping_mul(8);
            self.buffer.extend_from_slice(&bit_length.to_be_bytes());

            // Take ownership of the buffer to avoid borrow conflict with process_block
            let padded = std::mem::take(&mut self.buffer);
            for chunk in padded.chunks(64) {
                self.process_block(chunk);
            }

            let mut result = String::with_capacity(64);
            for &word in &self.state {
                result.push_str(&format!("{:08x}", word));
            }
            result
        }
    }
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
            sandbox: "workspace-write".to_string(),
            timeout_seconds: 3600,
            max_attempts: 2,
            acceptance_command_ids: vec!["test:mcp".to_string()],
            status: "ready".to_string(),
            package_hash: "hash123".to_string(),
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

        let manifest = AttemptManifest::new(&exec.id, 1, None, &pkg);
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
        let mut manifest = AttemptManifest::new(&exec.id, 1, None, &pkg);
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
        assert_eq!(snapshot.remaining_attempts, 1); // max=2, attempts=1
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
            let manifest = AttemptManifest::new(&exec_id, attempt_no, parent_id, &pkg);
            write_manifest(&dir, &manifest).unwrap();
        }

        // Create a bundle for a different step
        let mut pkg2 = mock_pkg();
        pkg2.step_id = "step_other".to_string();
        let dir3 = create_bundle_dir("exec_3").unwrap();
        let manifest3 = AttemptManifest::new("exec_3", 1, None, &pkg2);
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
        // Verify our SHA-256 against a known vector: SHA-256("abc") = ba7816bf...
        let mut hasher = sha256::Sha256::new();
        hasher.update(b"abc");
        let hash = hasher.finalize_hex();
        assert_eq!(
            hash,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
