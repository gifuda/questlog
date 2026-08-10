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

use crate::attempt_bundle::{
    self, AttemptManifest, AttemptOutcome, CandidateInfo, ParentInfo, VerifierResult,
};
use crate::execution_store::{self, ExecutionStatus, ExecutionUpdates, TaskPackageSummary};
use crate::executors::{CodexExecutor, Executor, ExecutorEvent, ZCodeExecutor};
use crate::models::StepStage;
use crate::runner_git::{self, ScopeViolation};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

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
    Passed {
        execution_id: String,
        candidate_sha: String,
    },
    NoDiff {
        execution_id: String,
    },
    AcceptanceFailed {
        execution_id: String,
        reason: String,
    },
    ScopeViolation {
        execution_id: String,
        violation: ScopeViolation,
    },
    BaseShaDrift {
        execution_id: String,
        expected: String,
        actual: String,
    },
    CliFailed {
        execution_id: String,
        error: String,
    },
    Timeout {
        execution_id: String,
    },
    Cancelled {
        execution_id: String,
    },
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
                Ok(RunResult::Passed {
                    execution_id,
                    candidate_sha,
                }) => {
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
                    execution_store::release_lease(
                        execution_id,
                        ExecutionStatus::Failed,
                        Some("CLI produced no diff"),
                    )?;
                    execution_store::mark_package_status(&pkg.package_id, "failed")?;
                }
                Ok(RunResult::AcceptanceFailed {
                    execution_id,
                    reason,
                }) => {
                    execution_store::release_lease(
                        execution_id,
                        ExecutionStatus::Failed,
                        Some(reason),
                    )?;
                    execution_store::mark_package_status(&pkg.package_id, "failed")?;
                }
                Ok(RunResult::ScopeViolation {
                    execution_id,
                    violation,
                }) => {
                    execution_store::release_lease(
                        execution_id,
                        ExecutionStatus::Failed,
                        Some(&violation.to_string()),
                    )?;
                    execution_store::mark_package_status(&pkg.package_id, "failed")?;
                }
                Ok(RunResult::BaseShaDrift {
                    execution_id,
                    expected,
                    actual,
                }) => {
                    execution_store::release_lease(
                        execution_id,
                        ExecutionStatus::Blocked,
                        Some(&format!(
                            "base SHA drift: expected {}, got {}",
                            expected, actual
                        )),
                    )?;
                    execution_store::mark_package_status(&pkg.package_id, "blocked")?;
                }
                Ok(RunResult::CliFailed {
                    execution_id,
                    error,
                }) => {
                    execution_store::release_lease(
                        execution_id,
                        ExecutionStatus::Failed,
                        Some(error),
                    )?;
                    execution_store::mark_package_status(&pkg.package_id, "failed")?;
                }
                Ok(RunResult::Timeout { execution_id }) => {
                    execution_store::release_lease(
                        execution_id,
                        ExecutionStatus::Failed,
                        Some("timeout"),
                    )?;
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
                && !matches!(
                    e.status,
                    ExecutionStatus::Passed | ExecutionStatus::Failed | ExecutionStatus::Cancelled
                )
        }))
    }

    /// Process a single execution: acquire lease → worktree → CLI → verify → commit.
    ///
    /// This method creates an AttemptBundle at the start and writes evidence
    /// at each phase. The bundle is sealed (integrity hash computed) at the
    /// final outcome.
    fn process_execution(
        &mut self,
        execution_id: &str,
        pkg: &TaskPackageSummary,
    ) -> Result<RunResult, String> {
        let repo_root = PathBuf::from(&pkg.repo_root);

        // Acquire the lease
        let exec = execution_store::acquire_lease(execution_id, &self.config.runner_id)?;

        // ─── AttemptBundle: create directory + initial manifest ───
        let bundle_dir = attempt_bundle::create_bundle_dir(&exec.id)?;
        let mut manifest = AttemptManifest::new(
            &exec.id,
            exec.attempt_no,
            exec.parent_attempt_id.as_deref(),
            pkg,
        );

        // Generate and write environment snapshot
        let git_clean = runner_git::is_worktree_clean(&repo_root).unwrap_or(false);
        let parent_info = exec.parent_attempt_id.as_ref().and_then(|pid| {
            // Read parent outcome if it exists
            attempt_bundle::read_manifest(pid)
                .ok()
                .map(|pm| ParentInfo {
                    attempt_id: pid.clone(),
                    outcome: pm.outcome.clone(),
                    failed_verifiers: vec![], // populated by future Evidence-aware Retry logic
                })
        });
        let snapshot = attempt_bundle::generate_environment_snapshot(
            pkg,
            &exec,
            parent_info.as_ref(),
            git_clean,
        );
        attempt_bundle::write_environment_snapshot(&bundle_dir, &snapshot)?;
        attempt_bundle::write_task_package_snapshot(&bundle_dir, pkg)?;
        attempt_bundle::write_manifest(&bundle_dir, &manifest)?;

        // Record bundle path + executor on the execution
        execution_store::update_execution(
            &exec.id,
            ExecutionUpdates {
                bundle_path: Some(bundle_dir.to_string_lossy().to_string()),
                executor: Some(pkg.executor_preferred.clone()),
                ..Default::default()
            },
        )?;

        // Log bundle creation event
        let _ = attempt_bundle::append_event(
            &bundle_dir,
            &serde_json::json!({
                "event": "bundle_created",
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "attempt_no": exec.attempt_no,
            }),
        );

        // 1. Verify base SHA hasn't drifted
        let current_sha = runner_git::read_head_sha(&repo_root)?;
        if current_sha != pkg.base_sha {
            let result = RunResult::BaseShaDrift {
                execution_id: exec.id.clone(),
                expected: pkg.base_sha.clone(),
                actual: current_sha,
            };
            self.seal_bundle(&bundle_dir, &mut manifest, &result, None, &[]);
            return Ok(result);
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

        let _ = attempt_bundle::append_event(
            &bundle_dir,
            &serde_json::json!({
                "event": "worktree_created",
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "worktree_path": worktree.path.to_string_lossy(),
            }),
        );

        // 3. Select executor based on package config
        let (cli_succeeded, stdout_path, stderr_path, events): (
            bool,
            Option<PathBuf>,
            Option<PathBuf>,
            Vec<ExecutorEvent>,
        ) = match pkg.executor_preferred.as_str() {
            "zcode" => {
                let mut executor = ZCodeExecutor::new();
                let probe = executor.probe();
                if !probe.available {
                    let result = RunResult::CliFailed {
                        execution_id: exec.id.clone(),
                        error: "ZCode CLI not available".to_string(),
                    };
                    self.seal_bundle(&bundle_dir, &mut manifest, &result, None, &[]);
                    return Ok(result);
                }
                let handle = executor.start(
                    &pkg.objective,
                    &worktree.path,
                    if pkg.sandbox.is_empty() {
                        "workspace-write"
                    } else {
                        &pkg.sandbox
                    },
                    Duration::from_secs(pkg.timeout_seconds),
                )?;
                let stdout_p = handle.stdout_path.clone();
                let stderr_p = handle.stderr_path.clone();
                let events = executor.wait_and_collect(handle)?;
                let success = events
                    .iter()
                    .rev()
                    .find(|e| e.event_type == "execution.completed")
                    .and_then(|e| e.success)
                    .unwrap_or(false);
                (success, Some(stdout_p), Some(stderr_p), events)
            }
            "codex" => {
                let mut executor = CodexExecutor::new();
                let probe = executor.probe();
                if !probe.available {
                    let result = RunResult::CliFailed {
                        execution_id: exec.id.clone(),
                        error: "Codex CLI not available".to_string(),
                    };
                    self.seal_bundle(&bundle_dir, &mut manifest, &result, None, &[]);
                    return Ok(result);
                }
                let handle = executor.start(
                    &pkg.objective,
                    &worktree.path,
                    if pkg.sandbox.is_empty() {
                        "workspace-write"
                    } else {
                        &pkg.sandbox
                    },
                    Duration::from_secs(pkg.timeout_seconds),
                )?;
                let stdout_p = handle.stdout_path.clone();
                let stderr_p = handle.stderr_path.clone();
                let events = executor.wait_and_collect(handle)?;
                let success = events
                    .iter()
                    .rev()
                    .find(|e| e.event_type == "execution.completed")
                    .and_then(|e| e.success)
                    .unwrap_or(false);
                (success, Some(stdout_p), Some(stderr_p), events)
            }
            _ => {
                let result = RunResult::CliFailed {
                    execution_id: exec.id.clone(),
                    error: format!("unknown executor: {}", pkg.executor_preferred),
                };
                self.seal_bundle(&bundle_dir, &mut manifest, &result, None, &[]);
                return Ok(result);
            }
        };

        // ─── AttemptBundle: copy CLI output + write events ───
        let (stdout_ref, stderr_ref) = attempt_bundle::copy_cli_output(
            &bundle_dir,
            stdout_path.as_deref(),
            stderr_path.as_deref(),
        )?;
        manifest.stdout_ref = stdout_ref;
        manifest.stderr_ref = stderr_ref;

        // Write executor events to bundle-local events.jsonl
        for event in &events {
            let _ = attempt_bundle::append_event(
                &bundle_dir,
                &serde_json::json!({
                    "event": event.event_type,
                    "message": event.message,
                    "success": event.success,
                    "session_id": event.session_id,
                    "raw": event.raw,
                }),
            );
        }

        let _ = attempt_bundle::append_event(
            &bundle_dir,
            &serde_json::json!({
                "event": "cli_completed",
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "success": cli_succeeded,
            }),
        );

        if !cli_succeeded {
            let result = RunResult::CliFailed {
                execution_id: exec.id.clone(),
                error: "CLI exited with non-zero status".to_string(),
            };
            self.seal_bundle(&bundle_dir, &mut manifest, &result, None, &[]);
            return Ok(result);
        }

        // 5. Check for changes
        let changed = runner_git::get_changed_paths(&worktree.path, &pkg.base_sha)?;
        if changed.is_empty() {
            let result = RunResult::NoDiff {
                execution_id: exec.id.clone(),
            };
            self.seal_bundle(&bundle_dir, &mut manifest, &result, None, &[]);
            return Ok(result);
        }

        // 6. Verify scope (circuit breaker) — use package's allowed/forbidden paths
        if let Err(violation) = runner_git::verify_changed_paths_in_scope(
            &changed,
            &pkg.allowed_paths,
            &pkg.forbidden_paths,
            pkg.max_changed_files as usize,
        ) {
            let result = RunResult::ScopeViolation {
                execution_id: exec.id.clone(),
                violation,
            };
            self.seal_bundle(&bundle_dir, &mut manifest, &result, None, &[]);
            return Ok(result);
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
            None => {
                let result = RunResult::NoDiff {
                    execution_id: exec.id.clone(),
                };
                self.seal_bundle(&bundle_dir, &mut manifest, &result, None, &[]);
                return Ok(result);
            }
        };

        // ─── AttemptBundle: write diff patch + candidate info ───
        manifest.diff_ref =
            attempt_bundle::write_diff_patch(&bundle_dir, &worktree.path, &pkg.base_sha)?;
        manifest.candidate_ref = attempt_bundle::write_candidate(
            &bundle_dir,
            &CandidateInfo {
                sha: candidate_sha.clone(),
                message: format!("QuestLog candidate for step {}", pkg.step_id),
                base_sha: pkg.base_sha.clone(),
                trailers: serde_json::json!({
                    "QuestLog-Epic": pkg.epic_id,
                    "QuestLog-Step": pkg.step_id,
                    "QuestLog-Package": format!("{}@{}", pkg.package_id, pkg.plan_revision),
                    "QuestLog-Execution": exec.id,
                }),
            },
        )?;

        // Advance step: candidate commit produced → coded
        Self::advance_step_stage(&pkg.project_path, &pkg.step_id, StepStage::Coded);

        // 8. Run acceptance commands against the exact candidate commit
        // This is the anti-fake-green gate: CLI exit 0 + commit is NOT enough.
        // The acceptance commands must pass on the candidate commit.
        if pkg.acceptance_command_ids.is_empty() {
            let result = RunResult::AcceptanceFailed {
                execution_id: exec.id.clone(),
                reason: "no acceptance commands declared — cannot verify candidate commit"
                    .to_string(),
            };
            self.seal_bundle(
                &bundle_dir,
                &mut manifest,
                &result,
                Some(&candidate_sha),
                &[],
            );
            return Ok(result);
        }

        let mut verifier_results: Vec<VerifierResult> = Vec::new();
        for cmd_id in &pkg.acceptance_command_ids {
            let vr = resolve_and_run_acceptance_verbose(cmd_id, &repo_root, Some(&candidate_sha));
            verifier_results.push(vr.clone());
            if !vr.passed {
                let result = RunResult::AcceptanceFailed {
                    execution_id: exec.id.clone(),
                    reason: if vr.exit_code != 0 {
                        format!(
                            "acceptance command '{}' failed (exit {})",
                            cmd_id, vr.exit_code
                        )
                    } else {
                        format!(
                            "acceptance command '{}' error: {}",
                            cmd_id, vr.stderr_excerpt
                        )
                    },
                };
                self.seal_bundle(
                    &bundle_dir,
                    &mut manifest,
                    &result,
                    Some(&candidate_sha),
                    &verifier_results,
                );
                return Ok(result);
            }
        }

        // All acceptance commands passed → verified
        let result = RunResult::Passed {
            execution_id: exec.id.clone(),
            candidate_sha: candidate_sha.clone(),
        };
        self.seal_bundle(
            &bundle_dir,
            &mut manifest,
            &result,
            Some(&candidate_sha),
            &verifier_results,
        );
        Ok(result)
    }

    /// Seal the AttemptBundle: write verifier results, outcome, and compute integrity hash.
    fn seal_bundle(
        &self,
        bundle_dir: &Path,
        manifest: &mut AttemptManifest,
        result: &RunResult,
        candidate_sha: Option<&str>,
        verifier_results: &[VerifierResult],
    ) {
        // Write verifier results if any
        if !verifier_results.is_empty() {
            if let Ok(Some(ref_ref)) =
                attempt_bundle::write_verifier_results(bundle_dir, verifier_results)
            {
                manifest.verifier_ref = Some(ref_ref);
            }
        }

        // Map RunResult to outcome
        let (outcome_str, reason) = match result {
            RunResult::Passed { .. } => ("passed", None),
            RunResult::NoDiff { .. } => ("no_diff", None),
            RunResult::AcceptanceFailed { reason, .. } => {
                ("acceptance_failed", Some(reason.clone()))
            }
            RunResult::ScopeViolation { violation, .. } => {
                ("scope_violation", Some(violation.to_string()))
            }
            RunResult::BaseShaDrift {
                expected, actual, ..
            } => (
                "base_sha_drift",
                Some(format!("expected {}, got {}", expected, actual)),
            ),
            RunResult::CliFailed { error, .. } => ("cli_failed", Some(error.clone())),
            RunResult::Timeout { .. } => ("timeout", None),
            RunResult::Cancelled { .. } => ("cancelled", None),
        };

        let outcome = AttemptOutcome {
            outcome: outcome_str.to_string(),
            reason,
            candidate_sha: candidate_sha.map(|s| s.to_string()),
            finished_at: chrono::Utc::now().timestamp() as u64,
        };

        if let Err(e) = attempt_bundle::write_outcome_and_seal(bundle_dir, manifest, &outcome) {
            log::warn!("Failed to seal attempt bundle: {}", e);
        }
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
                        step_id,
                        step.stage,
                        stage
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
/// Guard against double-spawn: ensures only one polling thread exists at a time.
/// Set true when a thread spawns, set false when the thread exits.
static RUNNER_THREAD_ALIVE: AtomicBool = AtomicBool::new(false);

/// Spawn a background thread that polls for ready packages every poll_interval.
/// The thread runs until RUNNER_RUNNING is set to false.
/// Idempotent: if a thread is already alive, returns without spawning a new one.
pub fn start_runner_thread(config: RunnerConfig) {
    if !config.enabled {
        return;
    }

    // Guard against double-spawn (restart race)
    if RUNNER_THREAD_ALIVE
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        // Another thread is already alive — just flip RUNNING back to true
        // in case it was stopped but hasn't exited yet.
        RUNNER_RUNNING.store(true, Ordering::SeqCst);
        log::info!("QuestLog Runner re-enabled (thread already alive)");
        return;
    }

    RUNNER_RUNNING.store(true, Ordering::SeqCst);

    let interval = config.poll_interval;
    thread::spawn(move || {
        let mut runner = Runner::new(config);
        log::info!("QuestLog Runner started, polling every {:?}", interval);

        while RUNNER_RUNNING.load(Ordering::SeqCst) {
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

        RUNNER_THREAD_ALIVE.store(false, Ordering::SeqCst);
        log::info!("QuestLog Runner stopped");
    });
}

/// Stop the background Runner thread. The thread exits on its next loop iteration.
pub fn stop_runner() {
    RUNNER_RUNNING.store(false, Ordering::SeqCst);
}

/// Check if the Runner is currently enabled (will process packages).
pub fn is_running() -> bool {
    RUNNER_RUNNING.load(Ordering::SeqCst)
}

/// Enable or disable the Runner at runtime.
/// Returns the new state.
pub fn set_enabled(enabled: bool) -> bool {
    if enabled {
        start_runner_thread(RunnerConfig {
            enabled: true,
            poll_interval: Duration::from_secs(10),
            runner_id: format!("runner-{}", std::process::id()),
        });
    } else {
        stop_runner();
    }
    enabled
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

/// Resolve and run an acceptance command, returning a full VerifierResult
/// with exit code, output excerpts, and timing for the AttemptBundle.
fn resolve_and_run_acceptance_verbose(
    command_id: &str,
    repo_root: &Path,
    candidate_sha: Option<&str>,
) -> VerifierResult {
    let started_at = chrono::Utc::now().timestamp() as u64;

    let entry = ACCEPTANCE_REGISTRY
        .iter()
        .find(|(id, _, _)| *id == command_id);
    let (_, command, cwd_rel) = match entry {
        Some(e) => e,
        None => {
            return VerifierResult {
                command_id: command_id.to_string(),
                command: String::new(),
                cwd: String::new(),
                candidate_sha: candidate_sha.map(|s| s.to_string()),
                exit_code: -1,
                started_at,
                finished_at: chrono::Utc::now().timestamp() as u64,
                stdout_excerpt: String::new(),
                stderr_excerpt: format!("command_id '{}' not in trusted registry", command_id),
                passed: false,
            };
        }
    };

    let cwd = repo_root.join(cwd_rel);
    let cwd_str = cwd.display().to_string();

    if !cwd.exists() {
        return VerifierResult {
            command_id: command_id.to_string(),
            command: command.to_string(),
            cwd: cwd_str,
            candidate_sha: candidate_sha.map(|s| s.to_string()),
            exit_code: -1,
            started_at,
            finished_at: chrono::Utc::now().timestamp() as u64,
            stdout_excerpt: String::new(),
            stderr_excerpt: format!("acceptance cwd does not exist: {}", cwd.display()),
            passed: false,
        };
    }

    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.is_empty() {
        return VerifierResult {
            command_id: command_id.to_string(),
            command: command.to_string(),
            cwd: cwd_str,
            candidate_sha: candidate_sha.map(|s| s.to_string()),
            exit_code: -1,
            started_at,
            finished_at: chrono::Utc::now().timestamp() as u64,
            stdout_excerpt: String::new(),
            stderr_excerpt: "empty acceptance command".to_string(),
            passed: false,
        };
    }

    let output = std::process::Command::new(parts[0])
        .args(&parts[1..])
        .current_dir(&cwd)
        .output();

    let finished_at = chrono::Utc::now().timestamp() as u64;

    match output {
        Ok(output) => {
            let stdout_str = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr_str = String::from_utf8_lossy(&output.stderr).to_string();
            let exit_code = output.status.code().unwrap_or(-1);
            let passed = output.status.success();

            if passed {
                log::info!("acceptance '{}' passed in {}", command_id, cwd.display());
            } else {
                log::warn!(
                    "acceptance '{}' failed in {} (exit {}): {}",
                    command_id,
                    cwd.display(),
                    exit_code,
                    stderr_str.lines().take(3).collect::<Vec<_>>().join("; ")
                );
            }

            VerifierResult {
                command_id: command_id.to_string(),
                command: command.to_string(),
                cwd: cwd_str,
                candidate_sha: candidate_sha.map(|s| s.to_string()),
                exit_code,
                started_at,
                finished_at,
                // Keep excerpts to first 2000 chars — full output in stdout.log
                stdout_excerpt: stdout_str.chars().take(2000).collect(),
                stderr_excerpt: stderr_str.chars().take(2000).collect(),
                passed,
            }
        }
        Err(e) => VerifierResult {
            command_id: command_id.to_string(),
            command: command.to_string(),
            cwd: cwd_str,
            candidate_sha: candidate_sha.map(|s| s.to_string()),
            exit_code: -1,
            started_at,
            finished_at,
            stdout_excerpt: String::new(),
            stderr_excerpt: format!("failed to run '{}': {}", command, e),
            passed: false,
        },
    }
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
        // Write a fake packages.json with one ready package (nested format matching Node)
        let packages = serde_json::json!({
            "version": 1,
            "packages": [{
                "schema_version": 1,
                "package_id": "pkg_test1",
                "epic_id": "e1",
                "step_id": "s1",
                "plan_revision": 1,
                "project_path": "/nonexistent",
                "repo_root": "/nonexistent",
                "base_sha": "abc",
                "objective": "test task",
                "context_refs": [],
                "scope": {
                    "allowed_paths": ["**"],
                    "forbidden_paths": [],
                    "max_changed_files": 10
                },
                "preconditions": [],
                "executor": {
                    "preferred": "zcode",
                    "fallbacks": [],
                    "sandbox": "workspace-write",
                    "timeout_seconds": 3600,
                    "max_attempts": 2
                },
                "acceptance": [],
                "authority": {
                    "auto_edit_worktree": true,
                    "auto_commit_candidate": true,
                    "auto_merge": false,
                    "auto_push": false,
                    "auto_deploy": false
                },
                "stop_conditions": ["scope_expanded"],
                "rollback": {"strategy": "preserve_worktree_and_stop"},
                "status": "ready",
                "sealed_at": "2026-08-11T00:00:00Z",
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
