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
    /// S1: full worktree scene capture (scene/ dir) — tracked diff, untracked
    /// copies, post-base commits and the changed-path classification captured
    /// BEFORE any worktree-clearing terminal. Absent on pre-S1 bundles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_ref: Option<String>,
    /// S2: when this attempt recovered a previous ScopeViolation scene into
    /// its fresh worktree, the source execution id. Inherited by retry
    /// children (the worktree carries the recovered content along the chain).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_recovered_from: Option<String>,
    /// S2: the task string actually handed to the executor (objective plus
    /// any QuestLog recovery preamble). Persisted so audits can see what the
    /// executor was told, distinct from the sealed objective.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_prompt_ref: Option<String>,
    /// S3: set when the changed-file count exceeded the package's warn
    /// threshold — the candidate and acceptance still ran; this carries the
    /// warning into the sealed bundle and the final review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_size_warning: Option<String>,

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
            scene_ref: None,
            scene_recovered_from: None,
            task_prompt_ref: None,
            scope_size_warning: None,
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

/// S1 outcome of a scene capture.
#[derive(Debug)]
pub enum SceneCapture {
    /// Nothing to capture — clean tree, no untracked files, no post-base
    /// commits. Nothing to preserve counts as preserved.
    None,
    /// Fully written; `dir_ref` is the bundle-relative scene dir reference.
    Complete { dir_ref: String },
    /// Written, but at least one untracked file exceeded the size cap and
    /// was only LISTED (skipped_large), not copied — the worktree must be
    /// kept so the skipped content remains recoverable by hand.
    Partial { dir_ref: String },
}

/// Hard cap for a single untracked file copied into the scene. Above this
/// the file is recorded in changed-paths.json as skipped_large and the
/// capture is marked Partial (fail-safe direction: keep the worktree).
const SCENE_UNTRACKED_FILE_CAP: u64 = 50 * 1024 * 1024;

/// Capture the full worktree scene into the bundle's `scene/` directory —
/// the recoverable evidence a ScopeViolation (or any worktree-clearing
/// terminal state) must leave behind BEFORE the circuit breaker fires
/// (S1; verified: `git diff <base_sha>` covers staged+unstaged tracked
/// changes because it compares the commit against the working tree).
///
/// Contents:
///   scene/diff-full.patch   — `git diff --binary <base_sha>` (tracked net change)
///   scene/untracked/<rel>   — copies of untracked new files (gitignore-respecting)
///   scene/commits.txt       — `git log --oneline <base_sha>..HEAD` (resume chains)
///   scene/changed-paths.json — full classification of `changed` (single source:
///                             the same get_changed_paths output the scope
///                             verifier consumed) + skipped_large list + caps
#[allow(clippy::too_many_arguments)] // scene capture genuinely needs all of these
pub fn write_scene_snapshot(
    dir: &Path,
    worktree_path: &Path,
    base_sha: &str,
    changed: &[String],
    base_tree: &std::collections::HashSet<String>,
    allowed_patterns: &[String],
    create_patterns: &[String],
    forbidden_patterns: &[String],
    expected_changed_files: Option<u32>,
    warn_changed_files: Option<u32>,
    max_changed_files: usize,
) -> Result<SceneCapture, String> {
    let git = |args: &[&str]| -> Result<String, String> {
        let output = std::process::Command::new("git")
            .arg("-c")
            .arg("core.quotePath=false")
            .args(args)
            .current_dir(worktree_path)
            .output()
            .map_err(|e| format!("git {:?}: {e}", args))?;
        if !output.status.success() {
            return Err(format!(
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    };

    // 1. Tracked diff (staged + unstaged net change vs base).
    let diff = git(&["diff", "--binary", base_sha])?;

    // 2. Untracked new files (gitignore-respecting).
    let untracked_raw = git(&["ls-files", "--others", "--exclude-standard", "-z"])?;
    let untracked: Vec<std::path::PathBuf> = untracked_raw
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(std::path::PathBuf::from)
        .collect();

    // 3. Post-base commits (resume chains may carry committed work).
    let commits = git(&["log", "--oneline", &format!("{base_sha}..HEAD")])?;

    let diff_empty = diff.trim().is_empty();
    let commits_empty = commits.trim().is_empty();
    if diff_empty && untracked.is_empty() && commits_empty && changed.is_empty() {
        return Ok(SceneCapture::None);
    }

    let scene_dir = dir.join("scene");
    std::fs::create_dir_all(&scene_dir).map_err(|e| format!("mkdir scene: {e}"))?;

    if !diff_empty {
        std::fs::write(scene_dir.join("diff-full.patch"), diff.as_bytes())
            .map_err(|e| format!("write diff-full.patch: {e}"))?;
    }
    if !commits_empty {
        std::fs::write(scene_dir.join("commits.txt"), commits.as_bytes())
            .map_err(|e| format!("write commits.txt: {e}"))?;
    }

    // Copy untracked files (size-capped; oversize files are only listed).
    let mut skipped_large: Vec<String> = Vec::new();
    let untracked_set: std::collections::HashSet<String> = untracked
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    for rel in &untracked {
        let src = worktree_path.join(rel);
        let size = std::fs::metadata(&src).map_err(|e| format!("stat {}: {e}", rel.display()))?.len();
        if size > SCENE_UNTRACKED_FILE_CAP {
            skipped_large.push(rel.to_string_lossy().to_string());
            continue;
        }
        let dst = scene_dir.join("untracked").join(rel);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir untracked parent: {e}"))?;
        }
        std::fs::copy(&src, &dst).map_err(|e| format!("copy untracked {}: {e}", rel.display()))?;
    }

    // 4. Full changed-path classification (same input the scope verifier saw).
    let untracked_flag = |path: &str| untracked_set.contains(path);
    let classified: Vec<serde_json::Value> = changed
        .iter()
        .map(|path| {
            let category = crate::runner_git::classify_path_in_scope(
                path,
                base_tree.contains(path),
                allowed_patterns,
                create_patterns,
                forbidden_patterns,
            );
            let category_str = match category {
                crate::runner_git::ChangedPathCategory::ForbiddenControlPlane => {
                    "forbidden_control_plane"
                }
                crate::runner_git::ChangedPathCategory::Forbidden(_) => "forbidden",
                crate::runner_git::ChangedPathCategory::UnauthorizedModify => {
                    "unauthorized_modify"
                }
                crate::runner_git::ChangedPathCategory::UnauthorizedCreate => {
                    "unauthorized_create"
                }
                crate::runner_git::ChangedPathCategory::InScope => "in_scope",
            };
            serde_json::json!({
                "path": path,
                "category": category_str,
                "existed_in_base": base_tree.contains(path),
                "untracked": untracked_flag(path),
            })
        })
        .collect();
    let partial = !skipped_large.is_empty();
    let manifest_json = serde_json::json!({
        "total": changed.len(),
        "expected_changed_files": expected_changed_files,
        "warn_changed_files": warn_changed_files,
        "max_changed_files": max_changed_files,
        "skipped_large": skipped_large,
        "paths": classified,
    });
    std::fs::write(
        scene_dir.join("changed-paths.json"),
        serde_json::to_string_pretty(&manifest_json)
            .map_err(|e| format!("serialize changed-paths: {e}"))?,
    )
    .map_err(|e| format!("write changed-paths.json: {e}"))?;

    if partial {
        Ok(SceneCapture::Partial {
            dir_ref: "scene".to_string(),
        })
    } else {
        Ok(SceneCapture::Complete {
            dir_ref: "scene".to_string(),
        })
    }
}

/// S2 outcome of a scene recovery.
#[derive(Debug)]
pub struct SceneRecovery {
    /// Whether diff-full.patch existed and was applied.
    pub patched: bool,
    /// Number of untracked files copied back into the worktree.
    pub untracked_restored: usize,
}

/// S2: recover a captured scene into a FRESH worktree — the "reuse the work
/// instead of redoing it" half of the ScopeViolation loop. Only valid when
/// the recovery source shares the same base_sha (caller-enforced); `git
/// apply` fails loudly on drifted context lines and the caller degrades to
/// a clean re-run.
pub fn recover_scene_into_worktree(
    worktree: &Path,
    bundle_dir: &Path,
) -> Result<SceneRecovery, String> {
    let scene_dir = bundle_dir.join("scene");
    if !scene_dir.is_dir() {
        return Err(format!("scene dir missing under {}", bundle_dir.display()));
    }
    let has_diff = scene_dir.join("diff-full.patch").is_file();
    let has_classification = scene_dir.join("changed-paths.json").is_file();
    if !has_diff && !has_classification {
        return Err(format!(
            "scene dir incomplete under {} — neither diff nor classification present",
            bundle_dir.display()
        ));
    }

    let mut patched = false;
    if has_diff {
        let patch_abs = scene_dir.join("diff-full.patch");
        let output = std::process::Command::new("git")
            .args([
                "apply",
                "--whitespace=nowarn",
                &patch_abs.to_string_lossy(),
            ])
            .current_dir(worktree)
            .output()
            .map_err(|e| format!("git apply spawn: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "git apply failed (base drift or conflict): {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        patched = true;
    }

    // Copy untracked files back, preserving structure.
    let untracked_root = scene_dir.join("untracked");
    let mut untracked_restored = 0usize;
    if untracked_root.is_dir() {
        let mut stack = vec![untracked_root.clone()];
        while let Some(dir) = stack.pop() {
            let entries = std::fs::read_dir(&dir)
                .map_err(|e| format!("read untracked dir {}: {e}", dir.display()))?;
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                let rel = path
                    .strip_prefix(&untracked_root)
                    .map_err(|e| format!("rel path: {e}"))?;
                let dst = worktree.join(rel);
                if let Some(parent) = dst.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
                }
                std::fs::copy(&path, &dst)
                    .map_err(|e| format!("copy untracked {}: {e}", rel.display()))?;
                untracked_restored += 1;
            }
        }
    }

    Ok(SceneRecovery {
        patched,
        untracked_restored,
    })
}

/// S2: persist the task string the executor actually received. The sealed
/// objective stays untouched; this is the audit-side record of the delta.
pub fn write_task_prompt(dir: &Path, task: &str) -> Result<Option<String>, String> {
    let path = dir.join("task-prompt.txt");
    std::fs::write(&path, task).map_err(|e| format!("write task prompt: {e}"))?;
    Ok(Some("task-prompt.txt".to_string()))
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
    // S1: recurse into subdirectories (scene/ and any future evidence dir) —
    // a tampered scene file must fail verify_integrity just like a tampered
    // top-level evidence file. Keys are bundle-relative paths.
    let mut files: Vec<(String, std::path::PathBuf)> = Vec::new();
    collect_content_files(dir, "", &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut hasher = sha2::Sha256::new();
    for (rel, path) in &files {
        hasher.update(rel.as_bytes());
        hasher.update(b"\0");

        let mut file =
            std::fs::File::open(path).map_err(|e| format!("open for hash: {e}"))?;
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

fn collect_content_files(
    dir: &Path,
    prefix: &str,
    out: &mut Vec<(String, std::path::PathBuf)>,
) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("read bundle dir: {e}"))?;
    for entry in entries.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().to_string();
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let file_type = entry.file_type().map_err(|e| format!("stat bundle entry: {e}"))?;
        if file_type.is_dir() {
            collect_content_files(&entry.path(), &rel, out)?;
        } else if file_type.is_file() && is_content_file(&name) {
            out.push((rel, entry.path()));
        }
    }
    Ok(())
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
    #[serde(default)]
    pub create_paths: Vec<String>,
    pub forbidden_paths: Vec<String>,
    pub max_changed_files: u32,
    #[serde(default)]
    pub expected_changed_files: Option<u32>,
    #[serde(default)]
    pub warn_changed_files: Option<u32>,

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
        create_paths: pkg.create_paths.clone(),
        forbidden_paths: pkg.forbidden_paths.clone(),
        max_changed_files: pkg.max_changed_files,
        expected_changed_files: pkg.expected_changed_files,
        warn_changed_files: pkg.warn_changed_files,
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

    fn git(dir: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-c")
            .arg("user.email=t@t")
            .arg("-c")
            .arg("user.name=t")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// S1: the scene capture must preserve everything a worktree-clearing
    /// terminal would otherwise destroy — tracked staged+unstaged diff,
    /// untracked copies, post-base commits, and the full classification.
    #[test]
    fn scene_snapshot_captures_full_worktree_state() {
        let repo = tempfile::tempdir().unwrap();
        let repo_path = repo.path();
        git(repo_path, &["init", "-q"]);
        std::fs::write(repo_path.join("src.ts"), "base\n").unwrap();
        git(repo_path, &["add", "."]);
        git(repo_path, &["commit", "-qm", "base"]);
        let base_sha = {
            let out = std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(repo_path)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };

        // staged change, unstaged change on top, untracked new file,
        // and one post-base commit (resume-chain shape).
        std::fs::write(repo_path.join("src.ts"), "staged\n").unwrap();
        git(repo_path, &["add", "src.ts"]);
        std::fs::write(repo_path.join("src.ts"), "unstaged-final\n").unwrap();
        std::fs::create_dir_all(repo_path.join("docs")).unwrap();
        std::fs::write(repo_path.join("docs/new-note.md"), "untracked content\n").unwrap();
        std::fs::write(repo_path.join("extra.log"), "committed later\n").unwrap();
        git(repo_path, &["add", "extra.log"]);
        git(repo_path, &["commit", "-qm", "post-base"]);

        let bundle = tempfile::tempdir().unwrap();
        let changed = vec![
            "src.ts".to_string(),
            "docs/new-note.md".to_string(),
            "extra.log".to_string(),
        ];
        let allowed = vec!["src.ts".to_string(), "docs/**".to_string()];
        let capture = write_scene_snapshot(
            bundle.path(),
            repo_path,
            &base_sha,
            &changed,
            &crate::runner_git::base_tree_files(repo_path, &base_sha).unwrap(),
            &allowed,
            &[],
            &[],
            None,
            None,
            5,
        )
        .unwrap();

        match capture {
            SceneCapture::Complete { dir_ref } => assert_eq!(dir_ref, "scene"),
            other => panic!("expected Complete, got {other:?}"),
        }

        // diff-full.patch carries the net working-tree change vs base
        // (staged+unstaged; the post-base commit's extra.log shows too).
        let patch = std::fs::read_to_string(bundle.path().join("scene/diff-full.patch")).unwrap();
        assert!(patch.contains("unstaged-final"), "patch: {patch}");
        // untracked file copied with structure.
        let copied =
            std::fs::read_to_string(bundle.path().join("scene/untracked/docs/new-note.md"))
                .unwrap();
        assert_eq!(copied, "untracked content\n");
        // post-base commits listed.
        let commits =
            std::fs::read_to_string(bundle.path().join("scene/commits.txt")).unwrap();
        assert!(commits.contains("post-base"), "commits: {commits}");
        // classification: src/docs in scope, extra.log out of scope, untracked flagged.
        let classes =
            std::fs::read_to_string(bundle.path().join("scene/changed-paths.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&classes).unwrap();
        assert_eq!(v["total"], 3);
        assert_eq!(v["max_changed_files"], 5);
        let paths = v["paths"].as_array().unwrap();
        let by_path = |p: &str| {
            paths
                .iter()
                .find(|e| e["path"] == p)
                .unwrap_or_else(|| panic!("missing {p} in {classes}"))
                .clone()
        };
        assert_eq!(by_path("src.ts")["category"], "in_scope");
        assert_eq!(by_path("docs/new-note.md")["untracked"], true);
        assert_eq!(by_path("docs/new-note.md")["category"], "in_scope");
        // S3 v2: extra.log is a NEW path (post-base commit) without
        // creation rights — the violation class is unauthorized_create.
        assert_eq!(by_path("extra.log")["category"], "unauthorized_create");
        assert_eq!(by_path("extra.log")["existed_in_base"], false);
    }

    /// S1: a clean tree with nothing to capture reports None — nothing to
    /// preserve counts as preserved, and no scene/ dir is left behind.
    #[test]
    fn scene_snapshot_none_for_clean_tree() {
        let repo = tempfile::tempdir().unwrap();
        let repo_path = repo.path();
        git(repo_path, &["init", "-q"]);
        std::fs::write(repo_path.join("src.ts"), "base\n").unwrap();
        git(repo_path, &["add", "."]);
        git(repo_path, &["commit", "-qm", "base"]);
        let base_sha = {
            let out = std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(repo_path)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };

        let bundle = tempfile::tempdir().unwrap();
        let capture = write_scene_snapshot(
            bundle.path(),
            repo_path,
            &base_sha,
            &[],
            &crate::runner_git::base_tree_files(repo_path, &base_sha).unwrap(),
            &["src.ts".to_string()],
            &[],
            &[],
            None,
            None,
            5,
        )
        .unwrap();
        assert!(matches!(capture, SceneCapture::None));
        assert!(!bundle.path().join("scene").exists());
    }

    /// S1: scene/ files are INSIDE the integrity hash — tampering with a
    /// captured scene file must fail verify_integrity.
    #[test]
    fn integrity_hash_covers_scene_dir() {
        let repo = tempfile::tempdir().unwrap();
        let repo_path = repo.path();
        git(repo_path, &["init", "-q"]);
        std::fs::write(repo_path.join("src.ts"), "base\n").unwrap();
        git(repo_path, &["add", "."]);
        git(repo_path, &["commit", "-qm", "base"]);
        let base_sha = {
            let out = std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(repo_path)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        std::fs::write(repo_path.join("src.ts"), "changed\n").unwrap();

        let bundle = tempfile::tempdir().unwrap();
        let capture = write_scene_snapshot(
            bundle.path(),
            repo_path,
            &base_sha,
            &["src.ts".to_string()],
            &crate::runner_git::base_tree_files(repo_path, &base_sha).unwrap(),
            &["src.ts".to_string()],
            &[],
            &[],
            None,
            None,
            5,
        )
        .unwrap();
        assert!(matches!(capture, SceneCapture::Complete { .. }));

        let manifest = AttemptManifest::new(
            "exec_scene_hash",
            1,
            0,
            None,
            &mock_pkg(),
        );
        let mut manifest = manifest;
        manifest.scene_ref = Some("scene".to_string());
        let outcome = AttemptOutcome {
            outcome: "scope_violation".to_string(),
            reason: Some("test".to_string()),
            candidate_sha: None,
            finished_at: 1,
        };
        write_outcome_and_seal(bundle.path(), &mut manifest, &outcome).unwrap();
        assert!(verify_integrity(bundle.path()).unwrap(), "sealed bundle must verify");

        // Tamper with a scene file → the seal must catch it.
        std::fs::write(
            bundle.path().join("scene/diff-full.patch"),
            b"tampered",
        )
        .unwrap();
        assert!(
            !verify_integrity(bundle.path()).unwrap(),
            "tampered scene must fail verification"
        );
    }

    /// S2: a captured scene round-trips into a fresh worktree sharing the
    /// same base — tracked diff applied, untracked files restored byte-exact.
    #[test]
    fn scene_recovery_round_trips_into_fresh_worktree() {
        let repo_a = tempfile::tempdir().unwrap();
        let a = repo_a.path();
        git(a, &["init", "-q"]);
        std::fs::write(a.join("src.ts"), "base\n").unwrap();
        git(a, &["add", "."]);
        git(a, &["commit", "-qm", "base"]);
        let base_sha = {
            let out = std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(a)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        // The "out-of-scope" work: modify tracked + add untracked.
        std::fs::write(a.join("src.ts"), "reworked content\n").unwrap();
        std::fs::create_dir_all(a.join("docs")).unwrap();
        std::fs::write(a.join("docs/note.md"), "precious analysis\n").unwrap();

        let bundle = tempfile::tempdir().unwrap();
        let changed = vec!["src.ts".to_string(), "docs/note.md".to_string()];
        let capture = write_scene_snapshot(
            bundle.path(),
            a,
            &base_sha,
            &changed,
            &crate::runner_git::base_tree_files(a, &base_sha).unwrap(),
            &["src.ts".to_string()],
            &[],
            &[],
            None,
            None,
            5,
        )
        .unwrap();
        assert!(matches!(capture, SceneCapture::Complete { .. }));

        // Fresh worktree at the same base.
        let repo_b = tempfile::tempdir().unwrap();
        let b = repo_b.path();
        std::fs::create_dir_all(b).unwrap();
        git(b, &["init", "-q"]);
        std::fs::write(b.join("src.ts"), "base\n").unwrap();
        git(b, &["add", "."]);
        git(b, &["commit", "-qm", "base"]);

        let recovery = recover_scene_into_worktree(b, bundle.path()).unwrap();
        assert!(recovery.patched);
        assert_eq!(recovery.untracked_restored, 1);
        assert_eq!(
            std::fs::read_to_string(b.join("src.ts")).unwrap(),
            "reworked content\n"
        );
        assert_eq!(
            std::fs::read_to_string(b.join("docs/note.md")).unwrap(),
            "precious analysis\n"
        );
    }

    /// S2: a drifted base (context mismatch) must fail loudly so the caller
    /// degrades to a clean re-run — never a silent partial recovery.
    #[test]
    fn scene_recovery_fails_on_drifted_base() {
        let repo_a = tempfile::tempdir().unwrap();
        let a = repo_a.path();
        git(a, &["init", "-q"]);
        std::fs::write(a.join("src.ts"), "line1\nline2\nline3\n").unwrap();
        git(a, &["add", "."]);
        git(a, &["commit", "-qm", "base"]);
        let base_sha = {
            let out = std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(a)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        std::fs::write(a.join("src.ts"), "line1\nCHANGED\nline3\n").unwrap();

        let bundle = tempfile::tempdir().unwrap();
        write_scene_snapshot(
            bundle.path(),
            a,
            &base_sha,
            &["src.ts".to_string()],
            &crate::runner_git::base_tree_files(a, &base_sha).unwrap(),
            &["src.ts".to_string()],
            &[],
            &[],
            None,
            None,
            5,
        )
        .unwrap();

        // Fresh worktree whose base already diverged on the same lines.
        let repo_b = tempfile::tempdir().unwrap();
        let b = repo_b.path();
        git(b, &["init", "-q"]);
        std::fs::write(b.join("src.ts"), "line1\nDIVERGED\nline3\n").unwrap();
        git(b, &["add", "."]);
        git(b, &["commit", "-qm", "diverged"]);

        let result = recover_scene_into_worktree(b, bundle.path());
        assert!(result.is_err(), "expected apply failure on drifted base");
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
            display_no: None,
            allowed_paths: vec!["src.ts".to_string()],
            create_paths: vec![],
            forbidden_paths: vec![],
            max_changed_files: 5,
            expected_changed_files: None,
            warn_changed_files: None,
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
            package_display_no: None,
            scene_preserved: None,
            scene_recovered_from: None,
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
