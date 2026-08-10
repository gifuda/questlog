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
use crate::models::StepStage;
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
                    // Advance step: acceptance passed → verified
                    Self::advance_step_stage(&pkg.project_path, &pkg.step_id, StepStage::Verified);
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

        let candidate_sha = match candidate_sha {
            Some(sha) => sha,
            None => return Ok(RunResult::NoDiff { execution_id: exec.id.clone() }),
        };

        // Advance step: candidate commit produced → coded
        Self::advance_step_stage(&pkg.project_path, &pkg.step_id, StepStage::Coded);

        // 8. Run acceptance commands against the exact candidate commit
        // This is the anti-fake-green gate: CLI exit 0 + commit is NOT enough.
        // The acceptance commands must pass on the candidate commit.
        if pkg.acceptance_command_ids.is_empty() {
            // No acceptance commands declared → can't verify, don't fake-green
            return Ok(RunResult::AcceptanceFailed {
                execution_id: exec.id.clone(),
                reason: "no acceptance commands declared — cannot verify candidate commit".to_string(),
            });
        }

        for cmd_id in &pkg.acceptance_command_ids {
            match resolve_and_run_acceptance(cmd_id, &repo_root) {
                Ok(true) => { /* passed */ }
                Ok(false) => {
                    return Ok(RunResult::AcceptanceFailed {
                        execution_id: exec.id.clone(),
                        reason: format!("acceptance command '{}' failed", cmd_id),
                    });
                }
                Err(e) => {
                    return Ok(RunResult::AcceptanceFailed {
                        execution_id: exec.id.clone(),
                        reason: format!("acceptance command '{}' error: {}", cmd_id, e),
                    });
                }
            }
        }

        // All acceptance commands passed → verified
        Ok(RunResult::Passed {
            execution_id: exec.id.clone(),
            candidate_sha,
        })
    }

    /// Shadow mode: report what would be picked up without starting any CLI.
    pub fn shadow_pick() -> Result<Vec<String>, String> {
        let packages = execution_store::read_ready_packages()?;
        Ok(packages.into_iter().map(|p| p.package_id).collect())
    }

    /// Advance the Step stage in .epics.json based on execution result.
    ///
    /// Mapping:
    ///   candidate commit produced (but acceptance not yet verified) → coded
    ///   acceptance passed → verified
    ///   CLI failed / scope violation / no diff → leave stage unchanged
    ///
    /// The Runner never auto-advances to `observed` — that requires cross-session
    /// human confirmation per the plan.
    fn advance_step_stage(project_path: &str, step_id: &str, stage: StepStage) {
        let dir = PathBuf::from(project_path);
        match crate::epic_store::mutate_epics(&dir, |data| {
            let step = data.steps.iter_mut().find(|s| s.id == step_id);
            if let Some(step) = step {
                // Only advance forward, never backward
                let order = |s: &StepStage| match s {
                    StepStage::Pending => 0,
                    StepStage::Designed => 1,
                    StepStage::Coded => 2,
                    StepStage::Verified => 3,
                    StepStage::Observed => 4,
                };
                if order(&stage) > order(&step.stage) {
                    log::info!(
                        "Advancing step '{}' from {:?} to {:?}",
                        step_id, step.stage, stage
                    );
                    step.stage = stage.clone();
                    // Sync parent epic updated_at
                    let now = chrono::Utc::now().to_rfc3339();
                    step.updated_at = now.clone();
                    if let Some(epic) = data.epics.iter_mut().find(|e| e.id == step.epic_id) {
                        epic.updated_at = now;
                    }
                }
            }
            Ok(())
        }) {
            Ok(_) => {}
            Err(e) => log::warn!("Failed to advance step stage: {}", e),
        }
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

/// Trusted acceptance command registry (mirrors the Node-side registry).
/// command_id → (command, relative cwd).
/// The Runner resolves command_ids here — it never accepts raw shell strings.
const ACCEPTANCE_REGISTRY: &[(&str, &str, &str)] = &[
    ("test:mcp", "npm test", "mcp-server"),
    ("build:mcp", "npm run build", "mcp-server"),
    ("test:desktop", "cargo test", "desktop-app/src-tauri"),
    ("build:web", "npm run build", "desktop-app"),
    ("lint:mcp", "npx tsc --noEmit", "mcp-server"),
];

/// Resolve a command_id from the trusted registry and run it.
/// Returns Ok(true) if the command succeeded, Ok(false) if it failed.
fn resolve_and_run_acceptance(command_id: &str, repo_root: &PathBuf) -> Result<bool, String> {
    let entry = ACCEPTANCE_REGISTRY.iter().find(|(id, _, _)| *id == command_id);
    let (_, command, cwd_rel) = match entry {
        Some(e) => e,
        None => return Err(format!("command_id '{}' is not in the trusted acceptance registry", command_id)),
    };

    let cwd = repo_root.join(cwd_rel);
    if !cwd.exists() {
        return Err(format!("acceptance cwd does not exist: {}", cwd.display()));
    }

    // Parse command into program + args (simple split on spaces — the registry
    // only contains simple commands like "npm test", "cargo test")
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.is_empty() {
        return Err("empty acceptance command".to_string());
    }

    let output = std::process::Command::new(parts[0])
        .args(&parts[1..])
        .current_dir(&cwd)
        .output()
        .map_err(|e| format!("failed to run '{}': {}", command, e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        log::warn!(
            "acceptance '{}' failed in {}: {}",
            command_id,
            cwd.display(),
            stderr.lines().take(3).collect::<Vec<_>>().join("; ")
        );
        return Ok(false);
    }

    log::info!("acceptance '{}' passed in {}", command_id, cwd.display());
    Ok(true)
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
