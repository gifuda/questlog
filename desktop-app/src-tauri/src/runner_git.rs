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
    path == ".questlog-output" || path.starts_with(".questlog-output/")
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

    Ok(WorktreeInfo {
        path: worktree_path,
        branch: branch_name.to_string(),
        base_sha: base_sha.to_string(),
    })
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
pub fn is_worktree_clean(worktree_path: &Path) -> Result<bool, String> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
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
        .map(normalized_status_path)
        .find(|path| !is_control_plane_path(path) && !is_runner_internal_path(path))
        .is_none())
}

/// Get all changed paths relative to base, including untracked files. Runner
/// output files are evidence plumbing, not candidate source, so they are
/// excluded here and copied into the AttemptBundle separately.
pub fn get_changed_paths(worktree_path: &Path, base_sha: &str) -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .args(["diff", "--name-only", base_sha])
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
        .args(["ls-files", "--others", "--exclude-standard"])
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
pub fn verify_changed_paths_in_scope(
    changed: &[String],
    allowed_patterns: &[String],
    forbidden_patterns: &[String],
    max_files: usize,
) -> Result<(), ScopeViolation> {
    if changed.len() > max_files {
        return Err(ScopeViolation::TooManyFiles {
            count: changed.len(),
            max: max_files,
        });
    }

    for path in changed {
        if is_control_plane_path(path) || is_runner_internal_path(path) {
            return Err(ScopeViolation::ForbiddenPath {
                path: path.clone(),
                pattern: "QuestLog control-plane/runtime files".to_string(),
            });
        }
        // Check forbidden first
        for pattern in forbidden_patterns {
            if glob_match(pattern, path) {
                return Err(ScopeViolation::ForbiddenPath {
                    path: path.clone(),
                    pattern: pattern.clone(),
                });
            }
        }

        // Check allowed
        let allowed = allowed_patterns.iter().any(|p| glob_match(p, path));
        if !allowed {
            return Err(ScopeViolation::OutOfScope {
                path: path.clone(),
                allowed: allowed_patterns.to_vec(),
            });
        }
    }

    Ok(())
}

#[derive(Debug)]
pub enum ScopeViolation {
    OutOfScope { path: String, allowed: Vec<String> },
    ForbiddenPath { path: String, pattern: String },
    TooManyFiles { count: usize, max: usize },
}

impl std::fmt::Display for ScopeViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutOfScope { path, allowed } => {
                write!(f, "path '{}' is not in allowed scope: {:?}", path, allowed)
            }
            Self::ForbiddenPath { path, pattern } => {
                write!(f, "path '{}' matches forbidden pattern '{}'", path, pattern)
            }
            Self::TooManyFiles { count, max } => {
                write!(f, "changed {} files, max allowed is {}", count, max)
            }
        }
    }
}

/// Simple glob matcher supporting * and ** patterns.
/// `src/**` matches any file under src/.
/// `*.ts` matches any .ts file.
/// Exact paths match exactly.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    // Normalize: if pattern ends with /**, it matches everything under that dir
    if let Some(prefix) = pattern.strip_suffix("/**") {
        return path.starts_with(prefix);
    }

    // If pattern ends with *, it's a prefix + suffix match
    if let Some(prefix) = pattern.strip_suffix('*') {
        return path.starts_with(prefix);
    }

    // If pattern starts with *, suffix match
    if let Some(suffix) = pattern.strip_prefix('*') {
        return path.ends_with(suffix);
    }

    // Exact match
    pattern == path
}

/// Create a candidate commit in the worktree with QuestLog trailers.
/// Only commits paths that are in the allowed scope.
#[allow(clippy::too_many_arguments)]
pub fn create_candidate_commit(
    worktree_path: &Path,
    epic_id: &str,
    step_id: &str,
    package_id: &str,
    plan_revision: u32,
    execution_id: &str,
    allowed_patterns: &[String],
    base_sha: &str,
) -> Result<Option<String>, String> {
    // Get changes relative to the base SHA
    let changed = get_changed_paths(worktree_path, base_sha)?;
    if changed.is_empty() {
        return Ok(None); // No changes — no commit
    }

    // Stage only allowed paths
    let mut staged_any = false;
    for path in &changed {
        let is_allowed = !is_control_plane_path(path)
            && !is_runner_internal_path(path)
            && allowed_patterns.iter().any(|p| glob_match(p, path));
        if is_allowed {
            let _ = Command::new("git")
                .args(["add", path])
                .current_dir(worktree_path)
                .output();
            staged_any = true;
        }
    }

    if !staged_any {
        return Ok(None);
    }

    // Create the commit with trailers
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

    let output = Command::new("git")
        .args(["commit", "-m", &message])
        .current_dir(worktree_path)
        .output()
        .map_err(|e| format!("git commit failed: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "git commit failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
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
    fn glob_matches_wildcard_suffix() {
        assert!(glob_match("*.ts", "foo.ts"));
        assert!(glob_match("*.ts", "bar.ts"));
        assert!(!glob_match("*.ts", "foo.js"));
    }

    #[test]
    fn glob_matches_exact() {
        assert!(glob_match("README.md", "README.md"));
        assert!(!glob_match("README.md", "CHANGELOG.md"));
    }

    #[test]
    fn scope_violation_on_out_of_scope() {
        let result = verify_changed_paths_in_scope(
            &["src/foo.ts".to_string(), "secrets/key.txt".to_string()],
            &["src/**".to_string()],
            &[],
            10,
        );
        assert!(matches!(result, Err(ScopeViolation::OutOfScope { .. })));
    }

    #[test]
    fn scope_violation_on_forbidden() {
        let result = verify_changed_paths_in_scope(
            &["src/.env".to_string()],
            &["src/**".to_string()],
            &["src/.env".to_string()],
            10,
        );
        assert!(matches!(result, Err(ScopeViolation::ForbiddenPath { .. })));
    }

    #[test]
    fn scope_always_rejects_control_plane_files() {
        let changed = vec!["nested/.epics.json".to_string()];
        let result = verify_changed_paths_in_scope(&changed, &["**".to_string()], &[], 10);
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
    fn scope_violation_on_too_many_files() {
        let changed: Vec<String> = (0..15).map(|i| format!("src/file{i}.ts")).collect();
        let result = verify_changed_paths_in_scope(&changed, &["src/**".to_string()], &[], 10);
        assert!(matches!(result, Err(ScopeViolation::TooManyFiles { .. })));
    }

    #[test]
    fn scope_ok_for_all_in_scope() {
        let result = verify_changed_paths_in_scope(
            &["src/foo.ts".to_string(), "src/bar.ts".to_string()],
            &["src/**".to_string()],
            &[],
            10,
        );
        assert!(result.is_ok());
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
}
