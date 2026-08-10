//! The Runner — picks up sealed TaskPackages from the queue and executes them.
//!
//! Data flow (all three breakpoints bridged):
//!   Node MCP enqueue_step → packages.json (status=ready)
//!   → Runner polls packages.json, finds ready packages
//!   → Creates an execution in executions.json
//!   → Acquires lease, creates worktree
//!   → Runs ZCode/Codex CLI with the package's objective as prompt
//!   → Verifies diff scope, creates candidate commit
//!   → Updates package status + execution status
//!
//! The Runner runs in a background thread spawned by lib.rs.

use crate::executors::{CodexExecutor, Executor, ZCodeExecutor};
use crate::execution_store::{self, ExecutionStatus, ExecutionUpdates, TaskPackageSummary};
use crate::runner_git::{self, ScopeViolation};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;
use std::sync::atomic::{AtomicBool, Ordering};

/// Configuration for the Runner.
#[derive(Clone)]
pub struct RunnerConfig {
    pub enabled: bool,
    pub poll_interval: Duration,
    pub runner_id: String,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            enabled: false, // Disabled by default — opt-in via config
            poll_interval: Duration::from_secs(10),
            runner_id: format!("runner-{}", std::process::id()),
        }
    }
}

/// The result of processing a single execution.
#[derive(Debug)]
pub enum RunResult {
    Passed { execution_id: String, candidate_sha: String },
    NoDiff { execution_id: String },
    AcceptanceFailed { execution_id: String, reason: String },
    ScopeViolation { execution_id: String, violation: ScopeViolation },
    BaseShaDrift { execution_id: String, expected: String, actual: String },
    CliFailed { execution_id: String, error: String },
    Timeout { execution_id: String },
    Cancelled { execution_id: String },
}

/// The Runner — bridges packages.json → executions.json → CLI execution.
pub struct Runner {
    config: RunnerConfig,
}

impl Runner {
    pub fn new(config: RunnerConfig) -> Self {
        Self { config }
    }

    /// One poll cycle:
    /// 1. Read ready packages from packages.json
    /// 2. For each, create an execution and process it
    /// 3. Mark the package as consumed
    ///
    /// Returns the number of packages processed.
    pub fn poll_once(&mut self) -> Result<usize, String> {
        if !self.config.enabled {
            return Ok(0);
        }

        let packages = execution_store::read_ready_packages()?;
        let mut processed = 0;

        for pkg in packages {
            // Skip if there's already a non-terminal execution for this package
            if self.has_active_execution(&pkg.package_id)? {
                continue;
            }

            // Bridge: create an execution from the ready package
            let exec_id = execution_store::enqueue_execution(
                &pkg.package_id,
                &pkg.epic_id,
                &pkg.step_id,
                &pkg.project_path,
                &pkg.base_sha,
            )?;

            // Process the execution
            let result = self.process_execution(&exec_id, &pkg);

            // Update statuses based on result
            match &result {
                Ok(RunResult::Passed { execution_id, candidate_sha }) => {
                    execution_store::update_execution(
                        execution_id,
                        ExecutionUpdates {
                            candidate_sha: Some(candidate_sha.clone()),
                            status: Some(ExecutionStatus::Passed),
                            ..Default::default()
                        },
                    )?;
                    execution_store::release_lease(execution_id, ExecutionStatus::Passed, None)?;
                    execution_store::mark_package_status(&pkg.package_id, "passed")?;
                }
                Ok(RunResult::NoDiff { execution_id }) => {
                    execution_store::release_lease(execution_id, ExecutionStatus::Failed, Some("CLI produced no diff"))?;
                    execution_store::mark_package_status(&pkg.package_id, "failed")?;
                }
                Ok(RunResult::AcceptanceFailed { execution_id, reason }) => {
                    execution_store::release_lease(execution_id, ExecutionStatus::Failed, Some(reason))?;
                    execution_store::mark_package_status(&pkg.package_id, "failed")?;
                }
                Ok(RunResult::ScopeViolation { execution_id, violation }) => {
                    execution_store::release_lease(execution_id, ExecutionStatus::Failed, Some(&violation.to_string()))?;
                    execution_store::mark_package_status(&pkg.package_id, "failed")?;
                }
                Ok(RunResult::BaseShaDrift { execution_id, expected, actual }) => {
                    execution_store::release_lease(execution_id, ExecutionStatus::Blocked,
                        Some(&format!("base SHA drift: expected {}, got {}", expected, actual)))?;
                    execution_store::mark_package_status(&pkg.package_id, "blocked")?;
                }
                Ok(RunResult::CliFailed { execution_id, error }) => {
                    execution_store::release_lease(execution_id, ExecutionStatus::Failed, Some(error))?;
                    execution_store::mark_package_status(&pkg.package_id, "failed")?;
                }
                Ok(RunResult::Timeout { execution_id }) => {
                    execution_store::release_lease(execution_id, ExecutionStatus::Failed, Some("timeout"))?;
                    execution_store::mark_package_status(&pkg.package_id, "failed")?;
                }
                Ok(RunResult::Cancelled { execution_id }) => {
                    execution_store::release_lease(execution_id, ExecutionStatus::Cancelled, None)?;
                    execution_store::mark_package_status(&pkg.package_id, "cancelled")?;
                }
                Err(e) => {
                    execution_store::release_lease(&exec_id, ExecutionStatus::Failed, Some(e))?;
                    execution_store::mark_package_status(&pkg.package_id, "failed")?;
                }
            }

            processed += 1;
        }

        // Also process any stale queued executions (from a previous run)
        let _ = execution_store::cleanup_stale_leases();

        Ok(processed)
    }

    /// Check if there's already a non-terminal execution for this package.
    fn has_active_execution(&self, package_id: &str) -> Result<bool, String> {
        let execs = execution_store::list_executions()?;
        Ok(execs.iter().any(|e| {
            e.package_id == package_id
                && !matches!(e.status, ExecutionStatus::Passed | ExecutionStatus::Failed | ExecutionStatus::Cancelled)
        }))
    }

    /// Process a single execution: acquire lease → worktree → CLI → verify → commit.
    fn process_execution(
        &mut self,
        execution_id: &str,
        pkg: &TaskPackageSummary,
    ) -> Result<RunResult, String> {
        let repo_root = PathBuf::from(&pkg.repo_root);

        // Acquire the lease
        let exec = execution_store::acquire_lease(execution_id, &self.config.runner_id)?;

        // 1. Verify base SHA hasn't drifted
        let current_sha = runner_git::read_head_sha(&repo_root)?;
        if current_sha != pkg.base_sha {
            return Ok(RunResult::BaseShaDrift {
                execution_id: exec.id.clone(),
                expected: pkg.base_sha.clone(),
                actual: current_sha,
            });
        }

        // 2. Create worktree
        let branch_name = format!("questlog/{}", exec.id);
        let worktree = runner_git::create_worktree(&repo_root, &pkg.base_sha, &branch_name)?;

        execution_store::update_execution(
            &exec.id,
            ExecutionUpdates {
                worktree_path: Some(worktree.path.to_string_lossy().to_string()),
                branch: Some(branch_name.clone()),
                ..Default::default()
            },
        )?;

        // 3. Select executor based on package config
        let cli_succeeded = match pkg.executor_preferred.as_str() {
            "zcode" => {
                let mut executor = ZCodeExecutor::new();
                let probe = executor.probe();
                if !probe.available {
                    return Err("ZCode CLI not available".to_string());
                }
                let handle = executor.start(
                    &pkg.objective,
                    &worktree.path,
                    "workspace-write",
                    Duration::from_secs(3600),
                )?;
                let events = executor.wait_and_collect(handle)?;
                events.iter().rev()
                    .find(|e| e.event_type == "execution.completed")
                    .and_then(|e| e.success)
                    .unwrap_or(false)
            }
            "codex" => {
                let mut executor = CodexExecutor::new();
                let probe = executor.probe();
                if !probe.available {
                    return Err("Codex CLI not available".to_string());
                }
                let handle = executor.start(
                    &pkg.objective,
                    &worktree.path,
                    "workspace-write",
                    Duration::from_secs(3600),
                )?;
                let events = executor.wait_and_collect(handle)?;
                events.iter().rev()
                    .find(|e| e.event_type == "execution.completed")
                    .and_then(|e| e.success)
                    .unwrap_or(false)
            }
            _ => {
                return Err(format!("unknown executor: {}", pkg.executor_preferred));
            }
        };

        if !cli_succeeded {
            return Ok(RunResult::CliFailed {
                execution_id: exec.id.clone(),
                error: "CLI exited with non-zero status".to_string(),
            });
        }

        // 5. Check for changes
        let changed = runner_git::get_changed_paths(&worktree.path, &pkg.base_sha)?;
        if changed.is_empty() {
            return Ok(RunResult::NoDiff { execution_id: exec.id.clone() });
        }

        // 6. Verify scope (circuit breaker) — use package's allowed/forbidden paths
        if let Err(violation) = runner_git::verify_changed_paths_in_scope(
            &changed,
            &pkg.allowed_paths,
            &pkg.forbidden_paths,
            pkg.max_changed_files as usize,
        ) {
            return Ok(RunResult::ScopeViolation {
                execution_id: exec.id.clone(),
                violation,
            });
        }

        // 7. Create candidate commit with QuestLog trailers
        let candidate_sha = runner_git::create_candidate_commit(
            &worktree.path,
            &pkg.epic_id,
            &pkg.step_id,
            &pkg.package_id,
            pkg.plan_revision,
            &exec.id,
            &pkg.allowed_paths,
            &pkg.base_sha,
        )?;

        match candidate_sha {
            Some(sha) => Ok(RunResult::Passed {
                execution_id: exec.id.clone(),
                candidate_sha: sha,
            }),
            None => Ok(RunResult::NoDiff { execution_id: exec.id.clone() }),
        }
    }

    /// Shadow mode: report what would be picked up without starting any CLI.
    pub fn shadow_pick() -> Result<Vec<String>, String> {
        let packages = execution_store::read_ready_packages()?;
        Ok(packages.into_iter().map(|p| p.package_id).collect())
    }
}

// ─── Background polling thread ───

static RUNNER_RUNNING: AtomicBool = AtomicBool::new(false);

/// Spawn a background thread that polls for ready packages every poll_interval.
/// The thread runs until RUNNER_RUNNING is set to false.
/// Call this from Tauri's setup() to start the Runner.
pub fn start_runner_thread(config: RunnerConfig) {
    if !config.enabled {
        return;
    }

    RUNNER_RUNNING.store(true, Ordering::Relaxed);

    let interval = config.poll_interval;
    thread::spawn(move || {
        let mut runner = Runner::new(config);
        log::info!("QuestLog Runner started, polling every {:?}", interval);

        while RUNNER_RUNNING.load(Ordering::Relaxed) {
            match runner.poll_once() {
                Ok(n) if n > 0 => {
                    log::info!("Runner processed {} package(s)", n);
                }
                Ok(_) => {} // No work — sleep and retry
                Err(e) => {
                    log::warn!("Runner poll error: {}", e);
                }
            }
            thread::sleep(interval);
        }

        log::info!("QuestLog Runner stopped");
    });
}

/// Stop the background Runner thread.
pub fn stop_runner() {
    RUNNER_RUNNING.store(false, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution_store::TEST_LOCK;

    fn setup() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        let guard = TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("QUESTLOG_RUNTIME", dir.path().join("runtime"));
        (dir, guard)
    }

    #[test]
    fn runner_disabled_by_default() {
        let config = RunnerConfig::default();
        assert!(!config.enabled);
    }

    #[test]
    fn poll_returns_zero_when_disabled() {
        let (_dir, _guard) = setup();
        let mut runner = Runner::new(RunnerConfig::default());
        let count = runner.poll_once().unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn shadow_pick_returns_empty_when_no_packages() {
        let (_dir, _guard) = setup();
        let result = Runner::shadow_pick().unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn poll_picks_up_ready_package() {
        let (_dir, _guard) = setup();
        let runtime = std::env::var("QUESTLOG_RUNTIME").unwrap();
        std::fs::create_dir_all(&runtime).unwrap();
        // Write a fake packages.json with one ready package
        let packages = serde_json::json!({
            "version": 1,
            "packages": [{
                "package_id": "pkg_test1",
                "epic_id": "e1",
                "step_id": "s1",
                "plan_revision": 1,
                "project_path": "/nonexistent",
                "repo_root": "/nonexistent",
                "base_sha": "abc",
                "objective": "test task",
                "allowed_paths": ["**"],
                "forbidden_paths": [],
                "max_changed_files": 10,
                "executor_preferred": "zcode",
                "acceptance_command_ids": [],
                "status": "ready",
                "package_hash": "fake_hash"
            }]
        });
        std::fs::write(
            std::path::Path::new(&runtime).join("packages.json"),
            serde_json::to_string_pretty(&packages).unwrap(),
        )
        .unwrap();

        // poll_once should find the package and try to process it
        // (it will fail because /nonexistent is not a real git repo,
        // but the package should be consumed)
        let mut runner = Runner::new(RunnerConfig {
            enabled: true,
            poll_interval: Duration::from_secs(1),
            runner_id: "test".to_string(),
        });
        let count = runner.poll_once().unwrap();
        assert_eq!(count, 1, "should have processed 1 package");

        // Package should no longer be "ready"
        let remaining = execution_store::read_ready_packages().unwrap();
        assert!(remaining.is_empty(), "package should be consumed");
    }
}
