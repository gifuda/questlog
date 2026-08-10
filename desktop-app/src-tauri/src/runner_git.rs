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

    Ok(WorktreeInfo {
        path: worktree_path,
        branch: branch_name.to_string(),
        base_sha: base_sha.to_string(),
    })
}

/// Check if a worktree's working directory is clean (no uncommitted changes).
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

    Ok(String::from_utf8_lossy(&output.stdout).trim().is_empty())
}

/// Get the list of changed paths in the worktree (relative to base).
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

    let paths = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();

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
    if pattern.ends_with("/**") {
        let prefix = &pattern[..pattern.len() - 3];
        return path.starts_with(prefix);
    }

    // If pattern ends with *, it's a prefix + suffix match
    if pattern.ends_with('*') {
        let prefix = &pattern[..pattern.len() - 1];
        return path.starts_with(prefix);
    }

    // If pattern starts with *, suffix match
    if pattern.starts_with('*') {
        let suffix = &pattern[1..];
        return path.ends_with(suffix);
    }

    // Exact match
    pattern == path
}

/// Create a candidate commit in the worktree with QuestLog trailers.
/// Only commits paths that are in the allowed scope.
pub fn create_candidate_commit(
    worktree_path: &Path,
    epic_id: &str,
    step_id: &str,
    package_id: &str,
    plan_revision: u32,
    execution_id: &str,
    allowed_patterns: &[String],
) -> Result<Option<String>, String> {
    // Get changed paths
    let base_sha_output = Command::new("git")
        .args(["merge-base", "HEAD", "HEAD~0"])
        .current_dir(worktree_path)
        .output()
        .map_err(|e| format!("git merge-base: {e}"))?;

    // Get changes relative to the initial state
    let changed = get_changed_paths(worktree_path, "")?;
    if changed.is_empty() {
        return Ok(None); // No changes — no commit
    }

    // Stage only allowed paths
    let mut staged_any = false;
    for path in &changed {
        let is_allowed = allowed_patterns.iter().any(|p| glob_match(p, path));
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

    let sha = String::from_utf8_lossy(&sha_output.stdout).trim().to_string();
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
}
