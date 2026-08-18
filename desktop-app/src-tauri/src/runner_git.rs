//! Git worktree management for the Runner.
//!
//! Write tasks execute in dedicated worktrees, not the main working directory.
//! This module handles:
//! - Creating a worktree + branch from a base SHA
//! - Checking the baseline is clean
//! - Reading changed paths after execution
//! - Verifying paths are within the allowed scope
//! - Creating candidate commits with QuestLog trailers
//!
//! Safety guarantees:
//! - The main working directory is NEVER touched
//! - Out-of-scope paths trigger a circuit breaker (execution fails)
//! - base SHA drift is detected and blocks execution
//! - The Runner owns the commits it creates (not the user's commits)

use std::path::{Path, PathBuf};
use std::process::Command;

const QUESTLOG_TRAILER_PREFIX: &str = "QuestLog-";
const CONTROL_PLANE_FILES: &[&str] = &[".todos.json", ".epics.json", ".todo-board.json"];

fn normalized_status_path(line: &str) -> &str {
    line.get(3..).unwrap_or(line).trim_matches('"')
}

fn is_control_plane_path(path: &str) -> bool {
    CONTROL_PLANE_FILES.iter().any(|name| {
        path == *name
            || path.ends_with(&format!("/{name}"))
            || path == format!("{name}.lock")
            || path.ends_with(&format!("/{name}.lock"))
    })
}

fn is_runner_internal_path(path: &str) -> bool {
    path == ".questlog-output"
        || path.starts_with(".questlog-output/")
        // node_modules symlinks are dependency provisioning, not candidate source
        || path == "node_modules"
        || path.starts_with("node_modules/")
        || path.ends_with("/node_modules")
        || path.contains("/node_modules/")
        // .zcode surfaces that future sessions AUTO-LOAD must never be
        // committable by the executor: config.json (Runner-provisioned
        // model/thinking), hooks/agents/skills (executable surfaces — a
        // poisoned one would outlive the sandbox and run unsandboxed later).
        // Other .zcode content (plans, notes) stays normal candidate source.
        || path == ".zcode/config.json"
        || path.ends_with("/.zcode/config.json")
        || path.starts_with(".zcode/hooks/")
        || path.contains("/.zcode/hooks/")
        || path.starts_with(".zcode/agents/")
        || path.contains("/.zcode/agents/")
        || path.starts_with(".zcode/skills/")
        || path.contains("/.zcode/skills/")
}

/// Result of creating a worktree.
pub struct WorktreeInfo {
    pub path: PathBuf,
    pub branch: String,
    pub base_sha: String,
}

/// Create a dedicated worktree + branch from the given base SHA.
/// The worktree is placed in the OS temp directory to avoid polluting the repo.
pub fn create_worktree(
    repo_root: &Path,
    base_sha: &str,
    branch_name: &str,
) -> Result<WorktreeInfo, String> {
    let worktree_base = std::env::temp_dir().join("questlog-worktrees");
    std::fs::create_dir_all(&worktree_base).map_err(|e| format!("mkdir worktrees: {e}"))?;

    let worktree_path = worktree_base.join(branch_name);

    // Remove if it already exists (stale from a previous run)
    if worktree_path.exists() {
        let _ = std::fs::remove_dir_all(&worktree_path);
    }

    // Create the worktree at the base SHA
    let output = Command::new("git")
        .args(["worktree", "add", "--detach"])
        .arg(&worktree_path)
        .arg(base_sha)
        .current_dir(repo_root)
        .output()
        .map_err(|e| format!("git worktree add failed: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "git worktree add failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    // Create a branch at the current position (for commit tracking)
    let _ = Command::new("git")
        .args(["branch", branch_name, "HEAD"])
        .current_dir(&worktree_path)
        .output();

    // Checkout the branch so commits go to it
    let _ = Command::new("git")
        .args(["checkout", branch_name])
        .current_dir(&worktree_path)
        .output();

    // Provision node_modules symlinks from the main checkout so that
    // acceptance commands like `npm run build` / `npx tsc` work in the
    // worktree. node_modules is gitignored, so a fresh worktree has none.
    // Failures are non-fatal: the acceptance command will report exit 127
    // with a clear stderr, which is captured in the AttemptBundle.
    provision_node_modules(repo_root, &worktree_path);

    // Provision the executor model as a project-level ZCode config inside the
    // worktree (the CLI has no --model flag; a project .zcode/config.json is
    // the only verified override point). Skipped when no model is configured.
    if let Some(model) = crate::runner_config::get_executor_model() {
        provision_zcode_config(&worktree_path, &model);
    }

    Ok(WorktreeInfo {
        path: worktree_path,
        branch: branch_name.to_string(),
        base_sha: base_sha.to_string(),
    })
}

/// Write a project-level `.zcode/config.json` into the worktree so the ZCode
/// CLI uses the user-selected model for this execution.
///
/// The file must carry BOTH the model string and the full provider definition
/// (verified: model-only configs fail with "provider is missing baseURL"
/// because project configs don't merge the global provider table). The
/// provider def is copied verbatim from `~/.zcode/cli/config.json`.
///
/// `thinking` optionally pins the executor's thinking level by REWRITING the
/// model's `reasoning` block: every level in `providerOptionsByLevel` gets the
/// chosen level's params (normalized injection, mock-verified). This decouples
/// thinking from the model choice without any CLI flag — the runtime DB-level
/// `reasoningLevel` setting can then name any level; they all resolve to the
/// same injected params.
///
/// Zero-intrusion: the file lives only inside the temp worktree and is
/// removed with it. Failures are non-fatal (warn) — execution then falls back
/// to the CLI's own default model.
fn provision_zcode_config(worktree_path: &Path, model: &str) {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => {
            log::warn!("no home dir; skipping zcode config provisioning");
            return;
        }
    };
    let thinking = crate::runner_config::get_executor_thinking();
    provision_zcode_config_from(&home, worktree_path, model, thinking.as_deref());
}

/// Level names the CLI treats as "thinking disabled" (bundle: sve()).
const OFF_LEVELS: [&str; 4] = ["off", "nothink", "disabled", "none"];

/// Budget tokens per level, mirroring the CLI 0.16.3 bundle constants:
/// eFr = {max: 32000, high: 16000} (anthropic), kKo = {low: 8000, high: 16000,
/// max: 32000} (openai-compatible). Unknown levels fall back to 16000.
fn native_budget_tokens(model_name: &str, level: &str) -> u64 {
    // CLI builtin deepseek-v4 matcher uses a fixed small budget (aFr = 1024)
    // for both its levels.
    if model_name.contains("deepseek-v4") {
        return 1024;
    }
    match level {
        "max" => 32000,
        "high" => 16000,
        "low" => 8000,
        _ => 16000,
    }
}

/// The per-level provider options the CLI itself would send, for anthropic
/// kind. Priority: user-declared `providerOptionsByLevel[level]` (CLI-native
/// schema, copied verbatim — user's ZCode config wins) → off-like level →
/// generic native table (effort + budgetTokens).
fn native_thinking_options(
    model_name: &str,
    model_def: &serde_json::Value,
    level: &str,
) -> serde_json::Value {
    if let Some(user) = model_def
        .get("reasoning")
        .and_then(|r| r.get("providerOptionsByLevel"))
        .and_then(|p| p.get(level))
        .filter(|v| v.is_object())
    {
        return user.clone();
    }
    if OFF_LEVELS.contains(&level) {
        return serde_json::json!({"anthropic": {"thinking": {"type": "disabled"}}});
    }
    serde_json::json!({
        "anthropic": {
            "effort": level,
            "thinking": {"budgetTokens": native_budget_tokens(model_name, level), "type": "enabled"}
        }
    })
}

/// The model's thinking level list, following the user's ZCode config:
/// `reasoning.levels` (CLI naming) → `reasoning.variants` (v2 desktop naming,
/// what the user's config actually carries) → CLI builtin matchers → generic.
pub fn thinking_levels_for(model_name: &str, model_def: &serde_json::Value) -> Vec<String> {
    let reasoning = model_def.get("reasoning");
    for key in ["levels", "variants"] {
        if let Some(list) = reasoning
            .and_then(|r| r.get(key))
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            })
            .filter(|v: &Vec<String>| !v.is_empty())
        {
            return list;
        }
    }
    // CLI builtin capability matchers (bundle): deepseek-v4 → [high, max];
    // glm-5.2 → [max, high, nothink]
    let lower = model_name.to_lowercase();
    if lower.contains("deepseek-v4") {
        return vec!["high".into(), "max".into()];
    }
    if lower.starts_with("glm-5.2") {
        return vec!["max".into(), "high".into(), "nothink".into()];
    }
    vec!["high".into(), "max".into()]
}

/// Resolve the provider def for `provider_name` from the user's ZCode
/// configs: `~/.zcode/v2/config.json` (the desktop app — where the user
/// manages models) first, then `~/.zcode/cli/config.json` (the legacy
/// headless CLI config) as fallback. Returns (def, "v2"|"v1") or None.
fn resolve_provider_def(
    zcode_home: &Path,
    provider_name: &str,
) -> Option<(serde_json::Value, &'static str)> {
    for (rel, tag) in [
        (".zcode/v2/config.json", "v2"),
        (".zcode/cli/config.json", "v1"),
    ] {
        if let Ok(raw) = std::fs::read_to_string(zcode_home.join(rel)) {
            if let Ok(config) = serde_json::from_str::<serde_json::Value>(&raw) {
                if let Some(def) = config
                    .get("provider")
                    .and_then(|p| p.get(provider_name))
                    .cloned()
                {
                    return Some((def, tag));
                }
            }
        }
    }
    None
}

/// Testable core of `provision_zcode_config`: reads the ZCode config from
/// `<zcode_home>/.zcode/{v2,cli}/config.json` instead of the real home.
fn provision_zcode_config_from(
    zcode_home: &Path,
    worktree_path: &Path,
    model: &str,
    thinking: Option<&str>,
) {
    let provider_name = model.split('/').next().unwrap_or("");
    if provider_name.is_empty() || !model.contains('/') {
        log::warn!(
            "executor model '{}' is not in provider/model form; skipping zcode config provisioning",
            model
        );
        return;
    }

    let provider_def = match resolve_provider_def(zcode_home, provider_name) {
        Some((def, tag)) => {
            log::debug!(
                "executor provider '{}' resolved from {} config",
                provider_name,
                tag
            );
            def
        }
        None => {
            log::warn!(
                "provider '{}' not found in zcode configs (v2/cli); executor falls back to CLI default model",
                provider_name
            );
            return;
        }
    };

    let mut provider_def = provider_def;
    let model_name = model.split('/').nth(1).unwrap_or("");
    if let Some(level) = thinking {
        // Normalize: every declared level maps to the CHOSEN level's params,
        // so whichever level the runtime resolves to, the request carries the
        // user's selection.
        let models = provider_def
            .get_mut("models")
            .and_then(|m| m.as_object_mut());
        let model_entry = models
            .and_then(|m| m.get_mut(model_name))
            .filter(|e| e.is_object());
        match model_entry {
            Some(entry) => {
                let levels = thinking_levels_for(model_name, entry);
                let mut povl = serde_json::Map::new();
                for lv in &levels {
                    povl.insert(
                        lv.clone(),
                        native_thinking_options(model_name, entry, level),
                    );
                }
                let reasoning = serde_json::json!({
                    "enabled": true,
                    "levels": levels,
                    "defaultLevel": level,
                    "providerOptionsByLevel": povl,
                });
                entry
                    .as_object_mut()
                    .expect("filtered is_object above")
                    .insert("reasoning".to_string(), reasoning);
            }
            None => {
                log::warn!(
                    "model '{}' not found in provider '{}'; thinking level not injected",
                    model_name,
                    provider_name
                );
            }
        }
    }

    let mut project_config = serde_json::Map::new();
    project_config.insert(
        "model".to_string(),
        serde_json::Value::String(model.to_string()),
    );
    let mut providers = serde_json::Map::new();
    providers.insert(provider_name.to_string(), provider_def);
    project_config.insert("provider".to_string(), serde_json::Value::Object(providers));

    let config_dir = worktree_path.join(".zcode");
    if let Err(e) = std::fs::create_dir_all(&config_dir) {
        log::warn!("mkdir .zcode in worktree failed: {e}");
        return;
    }
    let file = config_dir.join("config.json");
    match serde_json::to_string_pretty(&serde_json::Value::Object(project_config)) {
        Ok(content) => {
            if let Err(e) = std::fs::write(&file, content) {
                log::warn!("write worktree zcode config failed: {e}");
            } else {
                log::info!(
                    "executor model '{}' (thinking {:?}) provisioned into worktree",
                    model,
                    thinking
                );
            }
        }
        Err(e) => log::warn!("serialize worktree zcode config failed: {e}"),
    }
}

/// Symlink node_modules directories from the main checkout into the worktree.
///
/// Scans the repo root and one level deep for `node_modules` directories
/// (covers `mcp-server/node_modules`, `desktop-app/node_modules`). For each
/// found, creates a symlink at the corresponding worktree path. Existing
/// entries are left untouched. All errors are logged at warn level and do
/// not propagate — a missing symlink surfaces as an exit-127 verifier result.
fn provision_node_modules(repo_root: &Path, worktree_path: &Path) {
    // Candidate locations: <root>/node_modules and <root>/<sub>/node_modules
    let mut candidates: Vec<(PathBuf, PathBuf)> = Vec::new();
    let root_nm = repo_root.join("node_modules");
    if root_nm.is_dir() {
        candidates.push((root_nm.clone(), worktree_path.join("node_modules")));
    }
    if let Ok(entries) = std::fs::read_dir(repo_root) {
        for entry in entries.flatten() {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let sub_name = entry.file_name();
            let sub_name = match sub_name.to_str() {
                Some(s) => s,
                None => continue,
            };
            // Skip obvious non-project dirs
            if sub_name.starts_with('.') || sub_name == "node_modules" {
                continue;
            }
            let sub_nm = entry.path().join("node_modules");
            if sub_nm.is_dir() {
                let target = worktree_path.join(sub_name).join("node_modules");
                candidates.push((sub_nm, target));
            }
        }
    }

    for (src, dst) in candidates {
        if dst.exists() || dst.is_symlink() {
            continue; // Already provisioned
        }
        // Ensure parent dir exists (worktree may not have the subdir if it was
        // added after base_sha)
        if let Some(parent) = dst.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            if let Err(e) = symlink(&src, &dst) {
                log::warn!(
                    "provision_node_modules: symlink {} -> {} failed: {}",
                    src.display(),
                    dst.display(),
                    e
                );
            }
        }
        #[cfg(not(unix))]
        {
            log::warn!(
                "provision_node_modules: symlink not supported on this platform, skipping {}",
                dst.display()
            );
        }
    }
}

/// Check whether source code is clean. QuestLog's local Plan Store files are
/// control-plane metadata and are intentionally excluded from source Git truth.
/// Untracked files (`??` lines) are also ignored: the worktree is created from
/// HEAD regardless of untracked files, so they don't affect base_sha semantics
/// — active projects routinely carry untracked work products.
pub fn is_worktree_clean(worktree_path: &Path) -> Result<bool, String> {
    let output = Command::new("git")
        .args(["-c", "core.quotePath=false", "status", "--porcelain"])
        .current_dir(worktree_path)
        .output()
        .map_err(|e| format!("git status failed: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "git status failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.starts_with("?? "))
        .map(normalized_status_path)
        .find(|path| !is_control_plane_path(path) && !is_runner_internal_path(path))
        .is_none())
}

/// Get all changed paths relative to base, including untracked files. Runner
/// output files are evidence plumbing, not candidate source, so they are
/// excluded here and copied into the AttemptBundle separately.
pub fn get_changed_paths(worktree_path: &Path, base_sha: &str) -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .args([
            "-c",
            "core.quotePath=false",
            "diff",
            "--name-only",
            base_sha,
        ])
        .current_dir(worktree_path)
        .output()
        .map_err(|e| format!("git diff failed: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let mut paths: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();

    let untracked = Command::new("git")
        .args([
            "-c",
            "core.quotePath=false",
            "ls-files",
            "--others",
            "--exclude-standard",
        ])
        .current_dir(worktree_path)
        .output()
        .map_err(|e| format!("git ls-files failed: {e}"))?;
    if !untracked.status.success() {
        return Err(format!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&untracked.stderr)
        ));
    }
    paths.extend(
        String::from_utf8_lossy(&untracked.stdout)
            .lines()
            .filter(|line| !line.is_empty())
            .map(String::from),
    );
    paths.retain(|path| !is_runner_internal_path(path));
    paths.sort();
    paths.dedup();

    Ok(paths)
}

/// Check if all changed paths match at least one allowed glob pattern.
/// Uses simple glob matching (* and **).
/// S1 shared path classifier — `verify_changed_paths_in_scope` (the circuit
/// breaker verdict) and the AttemptBundle scene snapshot (the recorded
/// classification) both call this single function, so the two can never
/// drift apart.
#[derive(Debug, Clone, PartialEq)]
pub enum ChangedPathCategory {
    /// QuestLog control-plane / runtime files — always forbidden.
    ForbiddenControlPlane,
    /// Matches a sealed forbidden pattern (payload = the pattern).
    Forbidden(String),
    /// S3 v2: exists in the base tree but has no modification rights —
    /// create rights never apply to an existing file.
    UnauthorizedModify,
    /// S3 v2: a new path with no creation rights.
    UnauthorizedCreate,
    /// Inside the sealed scope.
    InScope,
}

/// The base-tree file set — the FACT SOURCE for the modify/create split
/// (S3 TaskScope v2): a path present in the base tree needs modification
/// rights; an absent path needs creation rights. Fetched in ONE
/// `git ls-tree` call and queried per changed path in O(1).
pub fn base_tree_files(worktree: &Path, base_sha: &str) -> Result<std::collections::HashSet<String>, String> {
    let output = Command::new("git")
        .args(["ls-tree", "-r", "--name-only", base_sha])
        .current_dir(worktree)
        .output()
        .map_err(|e| format!("git ls-tree: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git ls-tree failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect())
}

/// S3 three-tier file-count semantics. `max` stays the last-resort circuit
/// breaker against runaway formatters/generators; `warn` only annotates the
/// bundle and the candidate; `expected` is recorded, never enforced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeVerdict {
    Ok,
    OverExpected,
    OverWarn,
    OverMax,
}

pub fn scope_size_verdict(
    total: usize,
    expected: Option<u32>,
    warn: Option<u32>,
    max: usize,
) -> SizeVerdict {
    if total > max {
        return SizeVerdict::OverMax;
    }
    if let Some(warn) = warn {
        if total > warn as usize {
            return SizeVerdict::OverWarn;
        }
    }
    if let Some(expected) = expected {
        if total > expected as usize {
            return SizeVerdict::OverExpected;
        }
    }
    SizeVerdict::Ok
}

pub fn classify_path_in_scope(
    path: &str,
    exists_in_base: bool,
    allowed_patterns: &[String],
    create_patterns: &[String],
    forbidden_patterns: &[String],
) -> ChangedPathCategory {
    if is_control_plane_path(path) || is_runner_internal_path(path) {
        return ChangedPathCategory::ForbiddenControlPlane;
    }
    for pattern in forbidden_patterns {
        if glob_match(pattern, path) {
            return ChangedPathCategory::Forbidden(pattern.clone());
        }
    }
    // TaskScope v2: modification and creation are separate rights, decided
    // by the base tree. create_patterns matching an EXISTING file does not
    // confer modification rights ("create cannot modify a file that already
    // exists"). Empty create_patterns (v1 packages) falls back to
    // allowed_paths for creation — today's behaviour, unchanged.
    if exists_in_base {
        if allowed_patterns.iter().any(|p| glob_match(p, path)) {
            ChangedPathCategory::InScope
        } else {
            ChangedPathCategory::UnauthorizedModify
        }
    } else {
        let create_ok = if create_patterns.is_empty() {
            allowed_patterns.iter().any(|p| glob_match(p, path))
        } else {
            create_patterns.iter().any(|p| glob_match(p, path))
                || allowed_patterns.iter().any(|p| glob_match(p, path))
        };
        if create_ok {
            ChangedPathCategory::InScope
        } else {
            ChangedPathCategory::UnauthorizedCreate
        }
    }
}

/// S3: path violations are reported FIRST (the operator must see the exact
/// offending path before any count judgement); only when every path is
/// inside its rights does the extreme-count breaker fire. Renames surface
/// as two lines under `git diff --name-only` (old deleted + new added) and
/// therefore pass BOTH gates — delete-side needs modify rights on the old
/// path, add-side needs create rights on the new one.
pub fn verify_changed_paths_in_scope(
    changed: &[String],
    base_tree: &std::collections::HashSet<String>,
    allowed_patterns: &[String],
    create_patterns: &[String],
    forbidden_patterns: &[String],
    max_files: usize,
) -> Result<(), ScopeViolation> {
    for path in changed {
        let exists_in_base = base_tree.contains(path);
        match classify_path_in_scope(path, exists_in_base, allowed_patterns, create_patterns, forbidden_patterns) {
            ChangedPathCategory::ForbiddenControlPlane => {
                return Err(ScopeViolation::ForbiddenPath {
                    path: path.clone(),
                    pattern: "QuestLog control-plane/runtime files".to_string(),
                });
            }
            ChangedPathCategory::Forbidden(pattern) => {
                return Err(ScopeViolation::ForbiddenPath {
                    path: path.clone(),
                    pattern,
                });
            }
            ChangedPathCategory::UnauthorizedModify => {
                return Err(ScopeViolation::UnauthorizedModify {
                    path: path.clone(),
                    allowed: allowed_patterns.to_vec(),
                });
            }
            ChangedPathCategory::UnauthorizedCreate => {
                return Err(ScopeViolation::UnauthorizedCreate {
                    path: path.clone(),
                    create: if create_patterns.is_empty() {
                        allowed_patterns.to_vec()
                    } else {
                        create_paths_view(create_patterns, allowed_patterns)
                    },
                });
            }
            ChangedPathCategory::InScope => {}
        }
    }

    if changed.len() > max_files {
        return Err(ScopeViolation::TooManyFiles {
            count: changed.len(),
            max: max_files,
        });
    }

    Ok(())
}

/// Show both creation sources in the violation message (create set plus the
/// v1 fallback view) so the operator sees every legitimate way forward.
fn create_paths_view(create: &[String], allowed: &[String]) -> Vec<String> {
    let mut view = create.to_vec();
    for pattern in allowed {
        if !view.contains(pattern) {
            view.push(pattern.clone());
        }
    }
    view
}

#[derive(Debug)]
pub enum ScopeViolation {
    ForbiddenPath { path: String, pattern: String },
    UnauthorizedModify { path: String, allowed: Vec<String> },
    UnauthorizedCreate { path: String, create: Vec<String> },
    TooManyFiles { count: usize, max: usize },
}

impl std::fmt::Display for ScopeViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ForbiddenPath { path, pattern } => {
                write!(f, "path '{}' matches forbidden pattern '{}'", path, pattern)
            }
            Self::UnauthorizedModify { path, allowed } => {
                write!(
                    f,
                    "existing path '{}' has no modification rights (allowed/modify scope: {:?}); create rights do not apply to existing files",
                    path, allowed
                )
            }
            Self::UnauthorizedCreate { path, create } => {
                write!(
                    f,
                    "new path '{}' has no creation rights (create scope: {:?})",
                    path, create
                )
            }
            Self::TooManyFiles { count, max } => {
                write!(f, "changed {} files, max allowed is {}", count, max)
            }
        }
    }
}

/// Gitignore-style glob matcher (component-wise, directory-boundary safe).
///
/// Semantics:
/// - A pattern WITHOUT `/` matches the path's final segment at any depth
///   (gitignore convention): `*.ts` matches `src/a.ts` but not `src/a.tsx`.
/// - A pattern WITH `/` is matched segment-by-segment from the repo root:
///   `*` matches within one segment only, `**` spans zero or more segments.
///   `src/**` matches `src/x` and `src/a/b` but NOT `src_evil/x`.
/// - No wildcards → exact string equality.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    /// Wildcard match within a single path segment (`*` = any byte run).
    fn match_segment(pattern: &str, segment: &str) -> bool {
        let (p, s) = (pattern.as_bytes(), segment.as_bytes());
        let (mut pi, mut si) = (0usize, 0usize);
        let (mut star_p, mut star_s) = (usize::MAX, 0usize);
        while si < s.len() {
            if pi < p.len() && p[pi] == s[si] {
                pi += 1;
                si += 1;
            } else if pi < p.len() && p[pi] == b'*' {
                star_p = pi;
                star_s = si;
                pi += 1;
            } else if star_p != usize::MAX {
                pi = star_p + 1;
                star_s += 1;
                si = star_s;
            } else {
                return false;
            }
        }
        while pi < p.len() && p[pi] == b'*' {
            pi += 1;
        }
        pi == p.len()
    }

    /// Segment-wise match; `**` consumes zero or more leading segments.
    fn match_segments(patterns: &[&str], segments: &[&str]) -> bool {
        if patterns.is_empty() {
            return segments.is_empty();
        }
        if patterns[0] == "**" {
            return (0..=segments.len())
                .any(|skip| match_segments(&patterns[1..], &segments[skip..]));
        }
        if segments.is_empty() || !match_segment(patterns[0], segments[0]) {
            return false;
        }
        match_segments(&patterns[1..], &segments[1..])
    }

    let pattern = pattern.trim_matches('/');
    let path = path.trim_matches('/');
    if pattern.is_empty() {
        return false;
    }
    if !pattern.contains('/') {
        let base = path.rsplit('/').next().unwrap_or(path);
        return match_segment(pattern, base);
    }
    let pattern_segments: Vec<&str> = pattern.split('/').collect();
    let path_segments: Vec<&str> = path.split('/').collect();
    match_segments(&pattern_segments, &path_segments)
}

/// Create a candidate commit in the worktree with QuestLog trailers.
/// Only commits paths authorized by the TaskScope v2 rights union:
/// allowed_paths (modify rights) ∪ create_paths (creation rights).
///
/// If the CLI executor already created its own commit (common with ZCode
/// which follows "commit after change" conventions), the existing commit
/// is amended to add QuestLog trailers. If there are uncommitted changes,
/// they are staged and committed. If there are no changes at all, returns None.
#[allow(clippy::too_many_arguments)]
pub fn create_candidate_commit(
    worktree_path: &Path,
    epic_id: &str,
    step_id: &str,
    package_id: &str,
    plan_revision: u32,
    execution_id: &str,
    allowed_patterns: &[String],
    create_patterns: &[String],
    base_sha: &str,
) -> Result<Option<String>, String> {
    // Check if the CLI executor already committed (HEAD != base_sha).
    // This happens with ZCode, which auto-commits after making changes.
    let current_head = read_head_sha(worktree_path)?;
    let cli_already_committed = current_head != base_sha;

    // Get changes relative to the base SHA (includes committed + uncommitted)
    let changed = get_changed_paths(worktree_path, base_sha)?;
    if changed.is_empty() {
        return Ok(None); // No changes — no commit
    }

    // Stage only scope-authorized paths (for uncommitted changes; harmless if
    // already committed). A brand-new file passes verify_changed_paths_in_scope
    // via create_paths even when it matches no allowed_patterns pattern — the
    // staging filter must honor the same rights union or creation-only work
    // (docs/design deliverables) stages nothing and is misread as no_diff.
    let mut staged_any = false;
    for path in &changed {
        let is_stageable = !is_control_plane_path(path)
            && !is_runner_internal_path(path)
            && (allowed_patterns.iter().any(|p| glob_match(p, path))
                || create_patterns.iter().any(|p| glob_match(p, path)));
        if is_stageable {
            let _ = Command::new("git")
                .args(["-c", "core.quotePath=false", "add", path])
                .current_dir(worktree_path)
                .output();
            staged_any = true;
        }
    }

    // QuestLog trailer message
    let message = format!(
        "QuestLog auto-execution\n\n\
        {prefix}Epic: {epic_id}\n\
        {prefix}Step: {step_id}\n\
        {prefix}Package: {package_id}@{plan_revision}\n\
        {prefix}Execution: {execution_id}",
        prefix = QUESTLOG_TRAILER_PREFIX,
        epic_id = epic_id,
        step_id = step_id,
        package_id = package_id,
        plan_revision = plan_revision,
        execution_id = execution_id,
    );

    if cli_already_committed {
        // CLI executor already committed. Amend to add QuestLog trailers.
        // `git commit --amend -m` replaces the message but keeps the tree.
        // Staged changes (if any) get folded in too.
        // --no-verify: candidate commits are machine-generated in an isolated
        // worktree; scope safety is verified by verify_changed_paths_in_scope.
        // The pre-commit guard (PII leak protection) applies to human commits
        // in the main workspace, not Runner candidate commits.
        let output = Command::new("git")
            .args(["commit", "--no-verify", "--amend", "-m", &message])
            .current_dir(worktree_path)
            .output()
            .map_err(|e| format!("git commit --amend failed: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "git commit --amend failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    } else if !staged_any {
        return Ok(None);
    } else {
        // Normal path: commit the staged changes with QuestLog trailers.
        // --no-verify: same rationale as above (isolated worktree, scope already checked).
        let output = Command::new("git")
            .args(["commit", "--no-verify", "-m", &message])
            .current_dir(worktree_path)
            .output()
            .map_err(|e| format!("git commit failed: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "git commit failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }

    // Get the commit SHA
    let sha_output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(worktree_path)
        .output()
        .map_err(|e| format!("git rev-parse failed: {e}"))?;

    let sha = String::from_utf8_lossy(&sha_output.stdout)
        .trim()
        .to_string();
    if sha.is_empty() {
        return Err("empty commit SHA".to_string());
    }

    Ok(Some(sha))
}

/// Read the current HEAD SHA of a repository.
pub fn read_head_sha(repo_path: &Path) -> Result<String, String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("git rev-parse failed: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "git rev-parse failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Remove a worktree (cleanup). Does NOT remove the branch.
pub fn remove_worktree(repo_root: &Path, worktree_path: &Path) -> Result<(), String> {
    let _ = Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(worktree_path)
        .current_dir(repo_root)
        .output();

    let _ = std::fs::remove_dir_all(worktree_path);
    Ok(())
}

/// Is `sha` an ancestor of the repo's current HEAD (i.e. already merged)?
/// False for anything else: not an ancestor, unknown/garbage-collected object,
/// or git error — callers treat "can't prove merged" as "not merged".
pub fn is_ancestor_of_head(repo_root: &Path, sha: &str) -> bool {
    is_ancestor(repo_root, sha, "HEAD")
}

/// Is `ancestor` an ancestor of `descendant`? Both may be any rev (SHA, ref).
/// False on git error / unknown objects — fail-closed for lineage decisions.
pub fn is_ancestor(repo_root: &Path, ancestor: &str, descendant: &str) -> bool {
    Command::new("git")
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .current_dir(repo_root)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matches_double_star() {
        assert!(glob_match("src/**", "src/foo.ts"));
        assert!(glob_match("src/**", "src/sub/bar.ts"));
        assert!(!glob_match("src/**", "test/foo.ts"));
    }

    #[test]
    fn glob_double_star_respects_directory_boundary() {
        // The audit case: `src/**` must NOT match `src_evil/file`.
        assert!(!glob_match("src/**", "src_evil/file.ts"));
        assert!(!glob_match("src/**", "src2/foo.ts"));
        assert!(glob_match("src/**", "src/foo.ts"));
        // `**` alone matches everything (old prefix matcher matched NOTHING).
        assert!(glob_match("**", "any/deep/path.rs"));
        assert!(glob_match("**", "toplevel.txt"));
        // `**` mid-pattern spans segments.
        assert!(glob_match("**/*.plist", "a/b/c.plist"));
        assert!(glob_match("**/*secret*", "x/y/mysecretkey.pem"));
        assert!(!glob_match("**/*.plist", "a/b/c.txt"));
    }

    #[test]
    fn glob_single_star_stays_within_segment() {
        // `dir/*.py` = direct children only (standard glob), not deep files.
        assert!(glob_match("automation/*.py", "automation/run.py"));
        assert!(!glob_match("automation/*.py", "automation/tests/run.py"));
        assert!(!glob_match("automation/*.py", "automation2/run.py"));
    }

    #[test]
    fn glob_matches_wildcard_suffix() {
        assert!(glob_match("*.ts", "foo.ts"));
        assert!(glob_match("*.ts", "bar.ts"));
        // Slash-less patterns match the basename at any depth (gitignore
        // convention) — but never bleed across the extension boundary.
        assert!(glob_match("*.ts", "src/deep/foo.ts"));
        assert!(!glob_match("*.ts", "src/deep/foo.tsx"));
        assert!(!glob_match("*.ts", "foo.js"));
    }

    #[test]
    fn glob_matches_exact() {
        assert!(glob_match("README.md", "README.md"));
        assert!(!glob_match("README.md", "CHANGELOG.md"));
        // Slash-less exact names also follow the basename convention.
        assert!(glob_match(".gitignore", "sub/.gitignore"));
        // Slashed exact paths are anchored at the repo root.
        assert!(!glob_match("docs/README.md", "README.md"));
    }

    #[test]
    fn scope_violation_on_unauthorized_create() {
        // New path (absent from the base tree) without creation rights.
        let result = verify_changed_paths_in_scope(
            &["src/foo.ts".to_string(), "secrets/key.txt".to_string()],
            &std::collections::HashSet::new(),
            &["src/**".to_string()],
            &[],
            &[],
            10,
        );
        assert!(matches!(
            result,
            Err(ScopeViolation::UnauthorizedCreate { .. })
        ));
    }

    #[test]
    fn scope_violation_on_unauthorized_modify() {
        // Existing path (present in the base tree) whose only rights are
        // creation rights — create must never confer modification.
        let base: std::collections::HashSet<String> =
            ["config.ts".to_string()].into_iter().collect();
        let result = verify_changed_paths_in_scope(
            &["config.ts".to_string()],
            &base,
            &[],
            &["config.ts".to_string()],
            &[],
            10,
        );
        assert!(matches!(
            result,
            Err(ScopeViolation::UnauthorizedModify { .. })
        ));
    }

    #[test]
    fn create_rights_allow_new_files_v1_fallback_and_v2_split() {
        // v1 fallback: no create_paths → allowed_paths confer creation.
        let r = verify_changed_paths_in_scope(
            &["src/new.ts".to_string()],
            &std::collections::HashSet::new(),
            &["src/**".to_string()],
            &[],
            &[],
            10,
        );
        assert!(r.is_ok());
        // v2 split: create_paths present → creation needs the create set…
        let r = verify_changed_paths_in_scope(
            &["docs/new.md".to_string()],
            &std::collections::HashSet::new(),
            &["src/**".to_string()],
            &["docs/*.md".to_string()],
            &[],
            10,
        );
        assert!(r.is_ok());
        // …and an existing file matching only the create set stays unauthorized.
        let base: std::collections::HashSet<String> =
            ["docs/exists.md".to_string()].into_iter().collect();
        let r = verify_changed_paths_in_scope(
            &["docs/exists.md".to_string()],
            &base,
            &[],
            &["docs/*.md".to_string()],
            &[],
            10,
        );
        assert!(matches!(
            r,
            Err(ScopeViolation::UnauthorizedModify { .. })
        ));
    }

    /// S3: specific path violations are reported BEFORE the count breaker —
    /// the operator must see the offending path even when the count is also
    /// over the max.
    #[test]
    fn path_violations_reported_before_count_breaker() {
        let changed: Vec<String> = (0..20).map(|i| format!("out/{i}.txt")).collect();
        let result = verify_changed_paths_in_scope(
            &changed,
            &std::collections::HashSet::new(),
            &["src/**".to_string()],
            &[],
            &[],
            5,
        );
        assert!(
            matches!(result, Err(ScopeViolation::UnauthorizedCreate { .. })),
            "expected the path violation, not TooManyFiles: {result:?}"
        );
        // Count-only breach (all paths in rights) still fires the breaker.
        let changed: Vec<String> = (0..7).map(|i| format!("src/f{i}.ts")).collect();
        let base: std::collections::HashSet<String> = changed.iter().cloned().collect();
        let result = verify_changed_paths_in_scope(
            &changed,
            &base,
            &["src/**".to_string()],
            &[],
            &[],
            5,
        );
        assert!(matches!(result, Err(ScopeViolation::TooManyFiles { .. })));
    }

    /// S3 three-tier count semantics.
    #[test]
    fn scope_size_verdict_tiers() {
        use super::SizeVerdict;
        assert_eq!(scope_size_verdict(5, Some(5), Some(8), 10), SizeVerdict::Ok);
        assert_eq!(
            scope_size_verdict(6, Some(5), Some(8), 10),
            SizeVerdict::OverExpected
        );
        assert_eq!(
            scope_size_verdict(9, Some(5), Some(8), 10),
            SizeVerdict::OverWarn
        );
        assert_eq!(
            scope_size_verdict(11, Some(5), Some(8), 10),
            SizeVerdict::OverMax
        );
        // None tiers never fire; max alone keeps today's behaviour.
        assert_eq!(scope_size_verdict(9, None, None, 10), SizeVerdict::Ok);
        assert_eq!(scope_size_verdict(11, None, None, 10), SizeVerdict::OverMax);
    }

    /// S3: base_tree_files reads the full base tree in one call.
    #[test]
    fn base_tree_files_reflects_base_commit() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "t"],
        ] {
            Command::new("git")
                .args(&args)
                .current_dir(repo)
                .output()
                .unwrap();
        }
        std::fs::write(repo.join("base-file.ts"), "x").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-qm", "base"])
            .current_dir(repo)
            .output()
            .unwrap();
        let sha = String::from_utf8_lossy(
            &Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(repo)
                .output()
                .unwrap()
                .stdout,
        )
        .trim()
        .to_string();
        // Post-base untracked file must NOT appear.
        std::fs::write(repo.join("later.txt"), "y").unwrap();

        let tree = base_tree_files(repo, &sha).unwrap();
        assert!(tree.contains("base-file.ts"));
        assert!(!tree.contains("later.txt"));
    }

    #[test]
    fn scope_violation_on_forbidden() {
        let result = verify_changed_paths_in_scope(
            &["src/.env".to_string()],
            &std::collections::HashSet::new(),
            &["src/**".to_string()],
            &[],
            &["src/.env".to_string()],
            10,
        );
        assert!(matches!(result, Err(ScopeViolation::ForbiddenPath { .. })));
    }

    #[test]
    fn scope_always_rejects_control_plane_files() {
        let changed = vec!["nested/.epics.json".to_string()];
        let result = verify_changed_paths_in_scope(&changed, &std::collections::HashSet::new(), &["**".to_string()], &[], &[], 10);
        assert!(matches!(result, Err(ScopeViolation::ForbiddenPath { .. })));
    }

    #[test]
    fn changed_paths_include_untracked_source_but_exclude_runner_output() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        Command::new("git")
            .arg("init")
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(repo)
            .output()
            .unwrap();
        std::fs::write(repo.join("tracked.txt"), "base\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "base"])
            .current_dir(repo)
            .output()
            .unwrap();
        let base = read_head_sha(repo).unwrap();

        std::fs::write(repo.join("new-source.rs"), "fn main() {}\n").unwrap();
        std::fs::create_dir_all(repo.join(".questlog-output")).unwrap();
        std::fs::write(repo.join(".questlog-output/stdout.log"), "evidence").unwrap();

        assert_eq!(
            get_changed_paths(repo, &base).unwrap(),
            vec!["new-source.rs"]
        );
    }

    #[test]
    fn changed_paths_preserve_chinese_filenames() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        Command::new("git")
            .arg("init")
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(repo)
            .output()
            .unwrap();
        std::fs::write(repo.join("README.md"), "base\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "base"])
            .current_dir(repo)
            .output()
            .unwrap();
        let base = read_head_sha(repo).unwrap();

        // Create a file with a Chinese name
        let chinese_name = "文档/收口方案.md";
        std::fs::create_dir_all(repo.join("文档")).unwrap();
        std::fs::write(repo.join("文档").join("收口方案.md"), "# content\n").unwrap();

        let changed = get_changed_paths(repo, &base).unwrap();
        assert!(
            changed.contains(&chinese_name.to_string()),
            "Chinese filename should appear as UTF-8, not octal-escaped. Got: {:?}",
            changed
        );

        // Scope check should match with UTF-8 pattern
        let result = verify_changed_paths_in_scope(&changed, &std::collections::HashSet::new(), &["文档/**".to_string()], &[], &[], 10);
        assert!(
            result.is_ok(),
            "Chinese path should match UTF-8 glob pattern"
        );
    }

    #[test]
    fn scope_violation_on_too_many_files() {
        let changed: Vec<String> = (0..15).map(|i| format!("src/file{i}.ts")).collect();
        let result = verify_changed_paths_in_scope(&changed, &std::collections::HashSet::new(), &["src/**".to_string()], &[], &[], 10);
        assert!(matches!(result, Err(ScopeViolation::TooManyFiles { .. })));
    }

    #[test]
    fn scope_ok_for_all_in_scope() {
        let result = verify_changed_paths_in_scope(
            &["src/foo.ts".to_string(), "src/bar.ts".to_string()],
            &std::collections::HashSet::new(),
            &["src/**".to_string()],
            &[],
            &[],
            10,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn scope_rejects_node_modules_paths() {
        // node_modules symlinks are provisioned by create_worktree, not candidate source.
        // They must be rejected as scope violations (same as control-plane files).
        for path in &[
            "node_modules",
            "node_modules/foo",
            "desktop-app/node_modules",
            "desktop-app/node_modules/.bin/tsc",
            "mcp-server/node_modules/typescript/lib",
        ] {
            let result =
                verify_changed_paths_in_scope(&[path.to_string()], &std::collections::HashSet::new(), &["**".to_string()], &[], &[], 10);
            assert!(
                matches!(result, Err(ScopeViolation::ForbiddenPath { .. })),
                "node_modules path '{}' should be rejected as forbidden, got {:?}",
                path,
                result
            );
        }
    }

    #[test]
    fn worktree_clean_ignores_untracked_files() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        Command::new("git")
            .arg("init")
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(repo)
            .output()
            .unwrap();
        std::fs::write(repo.join("tracked.txt"), "base\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "base"])
            .current_dir(repo)
            .output()
            .unwrap();

        // Untracked work products (e.g. _Manager plans) are a normal state
        std::fs::create_dir_all(repo.join("_Manager")).unwrap();
        std::fs::write(repo.join("_Manager/plan.md"), "# plan\n").unwrap();
        std::fs::write(repo.join("notes.txt"), "scratch\n").unwrap();
        assert!(
            is_worktree_clean(repo).unwrap(),
            "untracked files should not make the worktree dirty"
        );

        // A tracked modification still makes the tree dirty
        std::fs::write(repo.join("tracked.txt"), "modified\n").unwrap();
        assert!(
            !is_worktree_clean(repo).unwrap(),
            "tracked modification should make the worktree dirty"
        );

        // Control-plane exclusions still apply after the untracked filter
        std::fs::write(repo.join("tracked.txt"), "base\n").unwrap();
        std::fs::write(repo.join(".epics.json"), "{}\n").unwrap();
        Command::new("git")
            .args(["add", ".epics.json"])
            .current_dir(repo)
            .output()
            .unwrap();
        std::fs::write(repo.join(".epics.json"), "{\"rev\": 1}\n").unwrap();
        assert!(
            is_worktree_clean(repo).unwrap(),
            "tracked control-plane modifications remain excluded"
        );
    }

    #[test]
    fn worktree_provisions_node_modules_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        // git init + base commit
        Command::new("git")
            .arg("init")
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(repo)
            .output()
            .unwrap();
        std::fs::write(repo.join("tracked.txt"), "base\n").unwrap();
        // Create a subdir with node_modules in the main checkout
        std::fs::create_dir_all(repo.join("mcp-server")).unwrap();
        std::fs::create_dir_all(repo.join("mcp-server/node_modules")).unwrap();
        std::fs::write(repo.join("mcp-server/node_modules/marker.txt"), "present").unwrap();
        // mcp-server/package.json must be committed so the worktree has the subdir
        std::fs::write(repo.join("mcp-server/package.json"), "{}\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "base"])
            .current_dir(repo)
            .output()
            .unwrap();
        let base = read_head_sha(repo).unwrap();

        let wt = create_worktree(repo, &base, "test-provision-nm").unwrap();
        let wt_nm = wt.path.join("mcp-server/node_modules");
        assert!(
            wt_nm.is_symlink() || wt_nm.exists(),
            "worktree node_modules should be provisioned"
        );
        assert!(
            wt_nm.join("marker.txt").exists(),
            "symlinked node_modules content should be readable"
        );

        // Cleanup
        let _ = remove_worktree(repo, &wt.path);
    }

    #[test]
    fn candidate_commit_amends_when_cli_already_committed() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        Command::new("git")
            .arg("init")
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(repo)
            .output()
            .unwrap();
        std::fs::write(repo.join("src.rs"), "fn main() {}\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "base"])
            .current_dir(repo)
            .output()
            .unwrap();
        let base = read_head_sha(repo).unwrap();

        // Simulate CLI executor: modify + commit on its own
        std::fs::write(repo.join("src.rs"), "fn main() { /* docs added */ }\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "docs: add comments (ZCode auto-commit)"])
            .current_dir(repo)
            .output()
            .unwrap();

        // Now create_candidate_commit should amend the existing commit
        let result = create_candidate_commit(
            repo,
            "epic_test",
            "step_test",
            "pkg_test",
            1,
            "exec_test",
            &["src.rs".to_string()],
            &[],
            &base,
        );
        match result {
            Ok(Some(sha)) => {
                // Verify the amended commit has QuestLog trailers
                let log = Command::new("git")
                    .args(["log", "-1", "--format=%B"])
                    .current_dir(repo)
                    .output()
                    .unwrap();
                let msg = String::from_utf8_lossy(&log.stdout);
                assert!(
                    msg.contains("QuestLog auto-execution"),
                    "amended message should have QuestLog header"
                );
                assert!(
                    msg.contains("QuestLog-Epic: epic_test"),
                    "should have trailer"
                );
                assert!(sha != base, "candidate SHA should differ from base");
            }
            Ok(None) => panic!("should have produced a candidate commit"),
            Err(e) => panic!("create_candidate_commit failed: {e}"),
        }
    }

    #[test]
    fn candidate_commit_stages_creation_only_files() {
        // Regression (2026-08-18 production incident): a step whose deliverable
        // is a BRAND-NEW file authorized only via create_paths produced a real
        // worktree change, passed the scope gate, then staged nothing (the old
        // filter matched allowed_paths only) and was misreported as no_diff —
        // blocking every design/docs step all morning. The staging filter must
        // honor the same rights union as verify_changed_paths_in_scope.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        Command::new("git").arg("init").current_dir(repo).output().unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(repo)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(repo)
            .output()
            .unwrap();
        std::fs::write(repo.join("README.md"), "base\n").unwrap();
        Command::new("git").args(["add", "."]).current_dir(repo).output().unwrap();
        Command::new("git")
            .args(["commit", "-m", "base"])
            .current_dir(repo)
            .output()
            .unwrap();
        let base = read_head_sha(repo).unwrap();

        // Executor writes ONE brand-new deliverable, untracked, uncommitted.
        let deliverable = "_Manager/方案/量化/S0事实基线与契约冻结.md";
        std::fs::create_dir_all(repo.join("_Manager/方案/量化")).unwrap();
        std::fs::write(repo.join(deliverable), "# S0\n事实基线\n").unwrap();

        // allowed_paths names a DIFFERENT file (the planned doc title); only
        // create_paths' glob covers what was actually written.
        let allowed = vec!["_Manager/方案/量化/统一治理_20260817.md".to_string()];
        let create = vec!["_Manager/方案/量化/**".to_string()];

        let result = create_candidate_commit(
            repo,
            "epic_test",
            "step_test",
            "pkg_test",
            5,
            "exec_test",
            &allowed,
            &create,
            &base,
        );
        let sha = match result {
            Ok(Some(sha)) => sha,
            Ok(None) => panic!("creation-only deliverable must yield a candidate commit, not no_diff"),
            Err(e) => panic!("create_candidate_commit failed: {e}"),
        };

        // The candidate commit must actually contain the new file.
        let tree = Command::new("git")
            .args(["-c", "core.quotepath=false", "ls-tree", "-r", "--name-only", &sha])
            .current_dir(repo)
            .output()
            .unwrap();
        let files = String::from_utf8_lossy(&tree.stdout).to_string();
        assert!(
            files.lines().any(|l| l == deliverable),
            "candidate tree should contain the creation-only deliverable; got:\n{files}"
        );
    }
}

#[test]
fn runner_internal_paths_exclude_zcode_config() {
    // The Runner writes .zcode/config.json into the worktree to pin the
    // executor model — it must never count as a candidate source change.
    assert!(is_runner_internal_path(".zcode/config.json"));
    assert!(is_runner_internal_path("sub/dir/.zcode/config.json"));
    // Auto-loaded executable surfaces are equally off-limits (a poisoned
    // hook/skill/agent would run unsandboxed in a future session).
    assert!(is_runner_internal_path(".zcode/hooks/pre.sh"));
    assert!(is_runner_internal_path(".zcode/skills/evil/SKILL.md"));
    assert!(is_runner_internal_path("sub/.zcode/agents/x.md"));
    // Ordinary .zcode content (plans/notes) stays normal candidate source.
    assert!(!is_runner_internal_path(".zcode/other.json"));
    assert!(!is_runner_internal_path("src/main.rs"));
}

#[test]
fn provision_zcode_config_writes_model_and_provider_def() {
    let home = tempfile::tempdir().unwrap();
    let worktree = tempfile::tempdir().unwrap();

    // Fabricate a main zcode config with two providers
    let main_config = serde_json::json!({
        "model": "deepseek/deepseek-v4-flash",
        "provider": {
            "deepseek": {
                "kind": "anthropic",
                "name": "DeepSeek (v4 flash)",
                "options": {"apiKey": "sk-test", "baseURL": "https://api.deepseek.com/anthropic"},
                "models": {
                    "deepseek-v4-flash": {"reasoning": {"enabled": true, "variants": ["off", "high", "max"], "defaultVariant": "max"}}
                }
            },
            "bigmodel": {
                "kind": "anthropic",
                "name": "BigModel (GLM-5.2)",
                "options": {"apiKey": "k2", "baseURL": "https://open.bigmodel.cn/api/anthropic"},
                "models": {
                    "GLM-5.2": {"reasoning": {"enabled": true, "variants": ["off", "high"], "defaultVariant": "high"}}
                }
            }
        }
    });
    let config_dir = home.path().join(".zcode").join("cli");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("config.json"), main_config.to_string()).unwrap();

    // Provision GLM-5.2 into the worktree
    provision_zcode_config_from(home.path(), worktree.path(), "bigmodel/GLM-5.2", None);

    let written = std::fs::read_to_string(worktree.path().join(".zcode").join("config.json"))
        .expect("worktree .zcode/config.json must exist");
    let parsed: serde_json::Value = serde_json::from_str(&written).unwrap();
    assert_eq!(parsed["model"].as_str(), Some("bigmodel/GLM-5.2"));
    // Full provider def must be carried (model-only fails with missing baseURL)
    assert_eq!(
        parsed["provider"]["bigmodel"]["options"]["baseURL"].as_str(),
        Some("https://open.bigmodel.cn/api/anthropic")
    );
    // Other providers must NOT leak into the worktree config
    assert!(parsed["provider"].get("deepseek").is_none());
}

#[test]
fn provision_zcode_config_skips_unknown_provider_without_panic() {
    let home = tempfile::tempdir().unwrap();
    let worktree = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".zcode").join("cli")).unwrap();
    std::fs::write(
        home.path().join(".zcode").join("cli").join("config.json"),
        serde_json::json!({"provider": {}}).to_string(),
    )
    .unwrap();

    // Unknown provider → warn + skip, no file written, no panic
    provision_zcode_config_from(home.path(), worktree.path(), "ghost/model-x", None);
    assert!(!worktree.path().join(".zcode").join("config.json").exists());
}

#[test]
fn provision_prefers_v2_config_over_v1() {
    // The ZCode desktop app writes user-managed models to
    // ~/.zcode/v2/config.json under uuid provider keys; the headless CLI's
    // legacy config is ~/.zcode/cli/config.json. Provisioning must resolve
    // v2 first so the selector's models (v2) and the injected def match.
    let home = tempfile::tempdir().unwrap();
    let worktree = tempfile::tempdir().unwrap();

    let v2_config = serde_json::json!({
        "provider": {
            "e7a18242-b22b-43bc-844f-00d6921ffcbd": {
                "kind": "anthropic",
                "name": "Deepseek",
                "source": "custom",
                "options": {"apiKey": "v2-key", "baseURL": "https://api.deepseek.com/anthropic"},
                "models": {
                    "deepseek-v4-pro": {"reasoning": {"variants": ["off", "high", "max"], "defaultVariant": "max"}}
                }
            }
        }
    });
    let v1_config = serde_json::json!({
        "provider": {
            "deepseek": {
                "kind": "anthropic",
                "options": {"apiKey": "v1-key", "baseURL": "https://v1.example"},
                "models": {"deepseek-v4-flash": {}}
            }
        }
    });
    for (rel, cfg) in [
        (".zcode/v2/config.json", &v2_config),
        (".zcode/cli/config.json", &v1_config),
    ] {
        let p = home.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, cfg.to_string()).unwrap();
    }

    // v2 provider (uuid key): injected from v2 with v2 credentials
    provision_zcode_config_from(
        home.path(),
        worktree.path(),
        "e7a18242-b22b-43bc-844f-00d6921ffcbd/deepseek-v4-pro",
        None,
    );
    let written: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(worktree.path().join(".zcode").join("config.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        written["provider"]["e7a18242-b22b-43bc-844f-00d6921ffcbd"]["options"]["apiKey"],
        "v2-key"
    );

    // v1-only provider (legacy key): still resolved from v1 fallback
    let worktree2 = tempfile::tempdir().unwrap();
    provision_zcode_config_from(
        home.path(),
        worktree2.path(),
        "deepseek/deepseek-v4-flash",
        None,
    );
    let written2: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(worktree2.path().join(".zcode").join("config.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        written2["provider"]["deepseek"]["options"]["apiKey"],
        "v1-key"
    );
}

#[test]
fn thinking_levels_follow_user_config() {
    // v2 naming (variants array) — what the user's config actually carries
    let ds = serde_json::json!({"reasoning": {"variants": ["off", "high", "max"]}});
    assert_eq!(
        thinking_levels_for("deepseek-v4-flash", &ds),
        vec!["off", "high", "max"]
    );
    // CLI naming wins when present
    let cli_named = serde_json::json!({"reasoning": {"levels": ["high", "max"]}});
    assert_eq!(
        thinking_levels_for("anything", &cli_named),
        vec!["high", "max"]
    );
    // Builtin fallbacks
    assert_eq!(
        thinking_levels_for("deepseek-v4-pro", &serde_json::json!({})),
        vec!["high", "max"]
    );
    assert_eq!(
        thinking_levels_for("GLM-5.2", &serde_json::json!({})),
        vec!["max", "high", "nothink"]
    );
}

#[test]
fn provision_zcode_config_normalizes_thinking_levels() {
    let home = tempfile::tempdir().unwrap();
    let worktree = tempfile::tempdir().unwrap();
    let main_config = serde_json::json!({
        "model": "deepseek/deepseek-v4-flash",
        "provider": {
            "deepseek": {
                "kind": "anthropic",
                "options": {"apiKey": "sk", "baseURL": "https://x"},
                "models": {
                    "deepseek-v4-flash": {"reasoning": {"enabled": true, "variants": ["off", "high", "max"], "defaultVariant": "max"}}
                }
            }
        }
    });
    let config_dir = home.path().join(".zcode").join("cli");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("config.json"), main_config.to_string()).unwrap();

    // Choose "off": every declared level must map to thinking disabled.
    provision_zcode_config_from(
        home.path(),
        worktree.path(),
        "deepseek/deepseek-v4-flash",
        Some("off"),
    );
    let written: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(worktree.path().join(".zcode").join("config.json")).unwrap(),
    )
    .unwrap();
    let reasoning = &written["provider"]["deepseek"]["models"]["deepseek-v4-flash"]["reasoning"];
    assert_eq!(reasoning["defaultLevel"].as_str(), Some("off"));
    for level in ["off", "high", "max"] {
        assert_eq!(
            reasoning["providerOptionsByLevel"][level]["anthropic"]["thinking"]["type"], "disabled",
            "level {level} must normalize to disabled"
        );
    }

    // Choose "max" on a fresh worktree: every level gets the deepseek-v4
    // native budget (1024) with type enabled.
    let worktree2 = tempfile::tempdir().unwrap();
    provision_zcode_config_from(
        home.path(),
        worktree2.path(),
        "deepseek/deepseek-v4-flash",
        Some("max"),
    );
    let written2: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(worktree2.path().join(".zcode").join("config.json")).unwrap(),
    )
    .unwrap();
    let r2 = &written2["provider"]["deepseek"]["models"]["deepseek-v4-flash"]["reasoning"];
    for level in ["off", "high", "max"] {
        assert_eq!(
            r2["providerOptionsByLevel"][level]["anthropic"]["thinking"]["budgetTokens"],
            1024
        );
        assert_eq!(
            r2["providerOptionsByLevel"][level]["anthropic"]["thinking"]["type"],
            "enabled"
        );
        assert_eq!(
            r2["providerOptionsByLevel"][level]["anthropic"]["effort"],
            "max"
        );
    }
}

#[test]
fn provision_zcode_config_user_provider_options_win() {
    // If the user's config carries CLI-native providerOptionsByLevel for the
    // chosen level, those params are copied verbatim (user config wins over
    // our builtin table).
    let home = tempfile::tempdir().unwrap();
    let worktree = tempfile::tempdir().unwrap();
    let main_config = serde_json::json!({
        "model": "bigmodel/GLM-5.2",
        "provider": {
            "bigmodel": {
                "kind": "anthropic",
                "options": {"apiKey": "k", "baseURL": "https://x"},
                "models": {
                    "GLM-5.2": {"reasoning": {
                        "enabled": true,
                        "levels": ["high", "max"],
                        "providerOptionsByLevel": {
                            "high": {"anthropic": {"thinking": {"budgetTokens": 5555, "type": "enabled"}}}
                        }
                    }}
                }
            }
        }
    });
    let config_dir = home.path().join(".zcode").join("cli");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("config.json"), main_config.to_string()).unwrap();

    provision_zcode_config_from(
        home.path(),
        worktree.path(),
        "bigmodel/GLM-5.2",
        Some("high"),
    );
    let written: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(worktree.path().join(".zcode").join("config.json")).unwrap(),
    )
    .unwrap();
    let povl = &written["provider"]["bigmodel"]["models"]["GLM-5.2"]["reasoning"]
        ["providerOptionsByLevel"];
    // The user's high params (5555) applied to every level
    for level in ["high", "max"] {
        assert_eq!(povl[level]["anthropic"]["thinking"]["budgetTokens"], 5555);
    }
}
