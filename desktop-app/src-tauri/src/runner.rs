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
use crate::executors::{
    terminate_process_tree, ExecutionHandle, Executor, ExecutorEvent, ZCodeExecutor,
};
use crate::models::{EpicStatus, StepStage};
use crate::runner_git::{self, ScopeViolation};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;
use std::time::Instant;

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
    Paused {
        execution_id: String,
    },
    PolicyViolation {
        execution_id: String,
        error: String,
    },
    PreflightBlocked {
        execution_id: String,
        error: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SupervisionOutcome {
    Completed,
    Cancelled,
    Paused,
    TimedOut,
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

        // Per-project / per-epic filtering: only pick up packages from
        // projects the user has explicitly enabled, and epics that allow
        // auto-execution.
        for pkg in packages {
            // Filter: project must be enabled in runner-config.json
            if !crate::runner_config::is_project_enabled(&pkg.project_path) {
                continue;
            }
            // Filter: epic auto_execute must allow it
            match crate::runner_config::is_epic_auto_execute(&pkg.project_path, &pkg.epic_id) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(e) => {
                    log::warn!(
                        "Runner: skipping epic check error for {}: {}",
                        pkg.epic_id,
                        e
                    );
                    continue;
                }
            }

            let executions = execution_store::list_executions()?;
            if executions.iter().any(|execution| {
                execution.package_id == pkg.package_id
                    && matches!(
                        execution.status,
                        ExecutionStatus::Running
                            | ExecutionStatus::Candidate
                            | ExecutionStatus::Verifying
                            | ExecutionStatus::CancelRequested
                            | ExecutionStatus::PauseRequested
                    )
            }) {
                continue;
            }

            // A retry command pre-creates a queued child so its parent linkage is
            // preserved. First attempts are created here.
            let exec_id = executions
                .iter()
                .rev()
                .find(|execution| {
                    execution.package_id == pkg.package_id
                        && execution.status == ExecutionStatus::Queued
                })
                .map(|execution| execution.id.clone())
                .map(Ok)
                .unwrap_or_else(|| {
                    execution_store::enqueue_execution(
                        &pkg.package_id,
                        &pkg.epic_id,
                        &pkg.step_id,
                        &pkg.project_path,
                        &pkg.base_sha,
                    )
                })?;

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
                            ..Default::default()
                        },
                    )?;
                    let final_status = execution_store::finish_execution(
                        execution_id,
                        ExecutionStatus::Passed,
                        None,
                    )?;
                    match final_status {
                        ExecutionStatus::Passed => {
                            execution_store::mark_package_status(&pkg.package_id, "passed")?;
                            Self::advance_step_stage(
                                &pkg.project_path,
                                &pkg.step_id,
                                StepStage::Verified,
                            );
                        }
                        ExecutionStatus::Cancelled => {
                            execution_store::mark_package_status(&pkg.package_id, "cancelled")?;
                        }
                        _ => {
                            execution_store::mark_package_status(&pkg.package_id, "blocked")?;
                        }
                    }
                }
                Ok(RunResult::NoDiff { execution_id }) => {
                    execution_store::finish_execution(
                        execution_id,
                        ExecutionStatus::Blocked,
                        Some("CLI produced no diff"),
                    )?;
                    execution_store::mark_package_status(&pkg.package_id, "blocked")?;
                }
                Ok(RunResult::AcceptanceFailed {
                    execution_id,
                    reason,
                }) => {
                    execution_store::finish_execution(
                        execution_id,
                        ExecutionStatus::Blocked,
                        Some(reason),
                    )?;
                    execution_store::mark_package_status(&pkg.package_id, "blocked")?;
                }
                Ok(RunResult::ScopeViolation {
                    execution_id,
                    violation,
                }) => {
                    execution_store::finish_execution(
                        execution_id,
                        ExecutionStatus::Blocked,
                        Some(&violation.to_string()),
                    )?;
                    execution_store::mark_package_status(&pkg.package_id, "blocked")?;
                }
                Ok(RunResult::BaseShaDrift {
                    execution_id,
                    expected,
                    actual,
                }) => {
                    execution_store::finish_execution(
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
                    execution_store::finish_execution(
                        execution_id,
                        ExecutionStatus::Blocked,
                        Some(error),
                    )?;
                    execution_store::mark_package_status(&pkg.package_id, "blocked")?;
                }
                Ok(RunResult::Timeout { execution_id }) => {
                    execution_store::finish_execution(
                        execution_id,
                        ExecutionStatus::TimedOut,
                        Some("timeout"),
                    )?;
                    execution_store::mark_package_status(&pkg.package_id, "blocked")?;
                }
                Ok(RunResult::Cancelled { execution_id }) => {
                    execution_store::finish_execution(
                        execution_id,
                        ExecutionStatus::Cancelled,
                        None,
                    )?;
                    execution_store::mark_package_status(&pkg.package_id, "cancelled")?;
                }
                Ok(RunResult::Paused { execution_id }) => {
                    execution_store::finish_execution(
                        execution_id,
                        ExecutionStatus::Blocked,
                        Some("paused by user"),
                    )?;
                    execution_store::mark_package_status(&pkg.package_id, "blocked")?;
                }
                Ok(RunResult::PolicyViolation {
                    execution_id,
                    error,
                }) => {
                    execution_store::finish_execution(
                        execution_id,
                        ExecutionStatus::Blocked,
                        Some(error),
                    )?;
                    execution_store::mark_package_status(&pkg.package_id, "blocked")?;
                }
                Ok(RunResult::PreflightBlocked {
                    execution_id,
                    error,
                }) => {
                    execution_store::finish_execution(
                        execution_id,
                        ExecutionStatus::Blocked,
                        Some(error),
                    )?;
                    execution_store::mark_package_status(&pkg.package_id, "blocked")?;
                }
                Err(e) => {
                    execution_store::finish_execution(&exec_id, ExecutionStatus::Failed, Some(e))?;
                    execution_store::mark_package_status(&pkg.package_id, "failed")?;
                }
            }

            processed += 1;
        }

        // Also process any stale queued executions (from a previous run)
        let _ = execution_store::cleanup_stale_leases();

        Ok(processed)
    }

    fn supervise_execution(
        handle: &mut ExecutionHandle,
        execution_id: &str,
        timeout: Duration,
    ) -> Result<SupervisionOutcome, String> {
        let started = Instant::now();
        let mut last_heartbeat = Instant::now();
        loop {
            if handle
                .child
                .try_wait()
                .map_err(|e| format!("executor status check failed: {e}"))?
                .is_some()
            {
                return Ok(SupervisionOutcome::Completed);
            }

            let status = execution_store::get_execution(execution_id)?.status;
            match status {
                ExecutionStatus::CancelRequested => {
                    terminate_process_tree(handle)?;
                    return Ok(SupervisionOutcome::Cancelled);
                }
                ExecutionStatus::PauseRequested => {
                    terminate_process_tree(handle)?;
                    return Ok(SupervisionOutcome::Paused);
                }
                _ => {}
            }

            if started.elapsed() >= timeout {
                terminate_process_tree(handle)?;
                return Ok(SupervisionOutcome::TimedOut);
            }
            if last_heartbeat.elapsed() >= Duration::from_secs(10) {
                execution_store::heartbeat(execution_id)?;
                last_heartbeat = Instant::now();
            }
            thread::sleep(Duration::from_millis(100));
        }
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

        if let Err(error) = Self::validate_before_claim(pkg, exec.attempt_no) {
            let result = RunResult::PreflightBlocked {
                execution_id: exec.id.clone(),
                error,
            };
            self.seal_bundle(&bundle_dir, &mut manifest, pkg, &result, None, &[]);
            return Ok(result);
        }

        // 1. Verify base SHA hasn't drifted
        let current_sha = runner_git::read_head_sha(&repo_root)?;
        if current_sha != pkg.base_sha {
            let result = RunResult::BaseShaDrift {
                execution_id: exec.id.clone(),
                expected: pkg.base_sha.clone(),
                actual: current_sha,
            };
            self.seal_bundle(&bundle_dir, &mut manifest, pkg, &result, None, &[]);
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

        // 3. Production policy: only ZCode may execute. Codex is allowed to
        // create/push packages through MCP but is never a Runner executor;
        // manual packages are tracking-only and must not reach this point.
        if pkg.executor_preferred != "zcode" {
            let result = RunResult::PolicyViolation {
                execution_id: exec.id.clone(),
                error: format!(
                    "executor '{}' is forbidden by the production Runner policy",
                    pkg.executor_preferred
                ),
            };
            self.seal_bundle(&bundle_dir, &mut manifest, pkg, &result, None, &[]);
            return Ok(result);
        }

        let mut executor: Box<dyn Executor> = Box::new(ZCodeExecutor::new());
        let probe = executor.probe();
        if !probe.available {
            let result = RunResult::CliFailed {
                execution_id: exec.id.clone(),
                error: "ZCode CLI not available".to_string(),
            };
            self.seal_bundle(&bundle_dir, &mut manifest, pkg, &result, None, &[]);
            return Ok(result);
        }
        let timeout = Duration::from_secs(pkg.timeout_seconds.max(1));
        let mut handle = executor.start(
            &pkg.objective,
            &worktree.path,
            if pkg.sandbox.is_empty() {
                "workspace-write"
            } else {
                &pkg.sandbox
            },
            timeout,
        )?;
        let stdout_path = Some(handle.stdout_path.clone());
        let stderr_path = Some(handle.stderr_path.clone());
        let supervision = Self::supervise_execution(&mut handle, &exec.id, timeout)?;
        let events: Vec<ExecutorEvent> = executor.wait_and_collect(handle)?;
        let cli_succeeded = events
            .iter()
            .rev()
            .find(|event| event.event_type == "execution.completed")
            .and_then(|event| event.success)
            .unwrap_or(false);

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

        match supervision {
            SupervisionOutcome::Cancelled => {
                let result = RunResult::Cancelled {
                    execution_id: exec.id.clone(),
                };
                self.seal_bundle(&bundle_dir, &mut manifest, pkg, &result, None, &[]);
                return Ok(result);
            }
            SupervisionOutcome::Paused => {
                let result = RunResult::Paused {
                    execution_id: exec.id.clone(),
                };
                self.seal_bundle(&bundle_dir, &mut manifest, pkg, &result, None, &[]);
                return Ok(result);
            }
            SupervisionOutcome::TimedOut => {
                let result = RunResult::Timeout {
                    execution_id: exec.id.clone(),
                };
                self.seal_bundle(&bundle_dir, &mut manifest, pkg, &result, None, &[]);
                return Ok(result);
            }
            SupervisionOutcome::Completed => {}
        }

        if !cli_succeeded {
            let result = RunResult::CliFailed {
                execution_id: exec.id.clone(),
                error: "CLI exited with non-zero status".to_string(),
            };
            self.seal_bundle(&bundle_dir, &mut manifest, pkg, &result, None, &[]);
            return Ok(result);
        }

        // 5. Check for changes
        let changed = runner_git::get_changed_paths(&worktree.path, &pkg.base_sha)?;
        if changed.is_empty() {
            let result = RunResult::NoDiff {
                execution_id: exec.id.clone(),
            };
            self.seal_bundle(&bundle_dir, &mut manifest, pkg, &result, None, &[]);
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
            self.seal_bundle(&bundle_dir, &mut manifest, pkg, &result, None, &[]);
            return Ok(result);
        }

        match execution_store::get_execution(&exec.id)?.status {
            ExecutionStatus::CancelRequested => {
                let result = RunResult::Cancelled {
                    execution_id: exec.id.clone(),
                };
                self.seal_bundle(&bundle_dir, &mut manifest, pkg, &result, None, &[]);
                return Ok(result);
            }
            ExecutionStatus::PauseRequested => {
                let result = RunResult::Paused {
                    execution_id: exec.id.clone(),
                };
                self.seal_bundle(&bundle_dir, &mut manifest, pkg, &result, None, &[]);
                return Ok(result);
            }
            _ => {}
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
                self.seal_bundle(&bundle_dir, &mut manifest, pkg, &result, None, &[]);
                return Ok(result);
            }
        };

        execution_store::update_execution(
            &exec.id,
            ExecutionUpdates {
                candidate_sha: Some(candidate_sha.clone()),
                status: Some(ExecutionStatus::Candidate),
                ..Default::default()
            },
        )?;

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
                pkg,
                &result,
                Some(&candidate_sha),
                &[],
            );
            return Ok(result);
        }

        execution_store::update_execution(
            &exec.id,
            ExecutionUpdates {
                status: Some(ExecutionStatus::Verifying),
                ..Default::default()
            },
        )?;

        let mut verifier_results: Vec<VerifierResult> = Vec::new();
        for cmd_id in &pkg.acceptance_command_ids {
            let vr =
                resolve_and_run_acceptance_verbose(cmd_id, &worktree.path, Some(&candidate_sha));
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
                    pkg,
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
            pkg,
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
        pkg: &TaskPackageSummary,
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
            RunResult::Paused { .. } => ("paused", None),
            RunResult::PolicyViolation { error, .. } => ("policy_violation", Some(error.clone())),
            RunResult::PreflightBlocked { error, .. } => ("preflight_blocked", Some(error.clone())),
        };

        let decision = match result {
            RunResult::BaseShaDrift { .. } => Some((
                "git_drift",
                "Git base changed after the package was sealed. Revise and reseal the package?",
                vec!["revise_package".to_string(), "cancel".to_string()],
            )),
            RunResult::ScopeViolation { .. } => Some((
                "blocked",
                "The candidate changed paths outside the sealed scope. Revise scope or cancel?",
                vec!["revise_scope".to_string(), "cancel".to_string()],
            )),
            RunResult::AcceptanceFailed { .. } => Some((
                "verifier_failed",
                "The exact candidate failed acceptance. Inspect evidence and choose retry, revise, or cancel.",
                vec!["retry".to_string(), "revise_package".to_string(), "cancel".to_string()],
            )),
            RunResult::NoDiff { .. } | RunResult::CliFailed { .. } | RunResult::Timeout { .. } => {
                let exhausted = manifest.attempt_no >= pkg.max_attempts.max(1);
                Some((
                    if exhausted { "retry_exhausted" } else { "blocked" },
                    if exhausted {
                        "The attempt budget is exhausted. Inspect evidence, revise the package, or cancel."
                    } else {
                        "The attempt did not produce a verified candidate. Inspect evidence and choose retry, revise, or cancel."
                    },
                    if exhausted {
                        vec!["revise_package".to_string(), "cancel".to_string()]
                    } else {
                        vec![
                            "retry".to_string(),
                            "revise_package".to_string(),
                            "cancel".to_string(),
                        ]
                    },
                ))
            }
            RunResult::Paused { .. } => Some((
                "approval",
                "Execution was paused. Resume with a new attempt, revise the package, or cancel?",
                vec!["resume".to_string(), "revise_package".to_string(), "cancel".to_string()],
            )),
            RunResult::PolicyViolation { .. } => Some((
                "approval",
                "The package violates the production executor policy. Change it to ZCode or keep it manual?",
                vec!["use_zcode".to_string(), "manual".to_string(), "cancel".to_string()],
            )),
            RunResult::PreflightBlocked { .. } => Some((
                "blocked",
                "Runner preflight detected sealed-package, dependency, authority, or Git drift. Revise and reseal before continuing.",
                vec!["revise_package".to_string(), "cancel".to_string()],
            )),
            RunResult::Passed { .. } | RunResult::Cancelled { .. } => None,
        };

        if let Some((kind, question, options)) = decision {
            let evidence_refs = vec![bundle_dir.to_string_lossy().to_string()];
            match crate::decision_store::create_request(crate::decision_store::NewDecisionRequest {
                epic_id: &pkg.epic_id,
                step_id: &pkg.step_id,
                execution_id: &manifest.execution_id,
                kind,
                question,
                options,
                evidence_refs,
                package_revision: pkg.plan_revision,
            }) {
                Ok(request) => {
                    if let Ok(reference) =
                        attempt_bundle::write_decision_request(bundle_dir, &request)
                    {
                        manifest.decisions_ref = reference;
                    }
                }
                Err(error) => log::warn!("Failed to create DecisionRequest: {}", error),
            }
        }

        let outcome = AttemptOutcome {
            outcome: outcome_str.to_string(),
            reason,
            candidate_sha: candidate_sha.map(|s| s.to_string()),
            finished_at: chrono::Utc::now().timestamp() as u64,
        };

        if let Err(e) = attempt_bundle::write_outcome_and_seal(bundle_dir, manifest, &outcome) {
            // F4 audit fix: seal failure is an error, not a silent warning.
            // The bundle is left unsealed — its evidence is incomplete.
            // Log at error level so the user can detect it; the execution
            // result is still recorded (the outcome happened), but the
            // bundle's integrity_hash remains None, signaling "unverified".
            log::error!(
                "Failed to seal attempt bundle for {}: {} — evidence may be incomplete",
                manifest.execution_id,
                e
            );
        }
    }

    /// Shadow mode: report what would be picked up without starting any CLI.
    pub fn shadow_pick() -> Result<Vec<String>, String> {
        let packages = execution_store::read_ready_packages()?;
        Ok(packages.into_iter().map(|p| p.package_id).collect())
    }

    fn validate_before_claim(pkg: &TaskPackageSummary, attempt_no: u32) -> Result<(), String> {
        if pkg.executor_preferred != "zcode" {
            return Err(format!(
                "production Runner only accepts zcode, got '{}'",
                pkg.executor_preferred
            ));
        }
        if !pkg.executor_fallbacks.is_empty() {
            return Err("automatic executor fallbacks are forbidden".to_string());
        }
        if !matches!(pkg.sandbox.as_str(), "workspace-write" | "read-only") {
            return Err(format!("unsupported sandbox '{}'", pkg.sandbox));
        }
        if pkg.timeout_seconds == 0 || pkg.max_attempts == 0 || attempt_no > pkg.max_attempts {
            return Err(format!(
                "invalid or exhausted execution budget: timeout={}, attempt={}/{}",
                pkg.timeout_seconds, attempt_no, pkg.max_attempts
            ));
        }
        if pkg.package_hash.is_empty() || pkg.package_hash != pkg.computed_hash {
            return Err(format!(
                "sealed package hash mismatch: stored={}, computed={}",
                pkg.package_hash, pkg.computed_hash
            ));
        }
        if pkg.sealed_at.is_empty() {
            return Err("ready package is missing sealed_at".to_string());
        }
        if !pkg.auto_edit_worktree
            || !pkg.auto_commit_candidate
            || pkg.auto_merge
            || pkg.auto_push
            || pkg.auto_deploy
        {
            return Err(
                "authority does not match the safe worktree/candidate-only policy".to_string(),
            );
        }
        if pkg.allowed_paths.is_empty() || pkg.max_changed_files == 0 {
            return Err("scope is empty or max_changed_files is zero".to_string());
        }
        if pkg.acceptance_command_ids.is_empty() {
            return Err("no trusted acceptance command declared".to_string());
        }

        let project = PathBuf::from(&pkg.project_path)
            .canonicalize()
            .map_err(|e| format!("canonicalize project path: {e}"))?;
        let repo = PathBuf::from(&pkg.repo_root)
            .canonicalize()
            .map_err(|e| format!("canonicalize repo root: {e}"))?;
        if !project.starts_with(&repo) {
            return Err("project path escapes repo root".to_string());
        }
        let current_sha = runner_git::read_head_sha(&repo)?;
        if current_sha != pkg.base_sha {
            return Err(format!(
                "base SHA drift: expected {}, got {}",
                pkg.base_sha, current_sha
            ));
        }
        if pkg.preconditions.iter().any(|precondition| {
            precondition.kind == "git_clean" && precondition.value == Some(true)
        }) && !runner_git::is_worktree_clean(&repo)?
        {
            return Err("git_clean precondition is no longer satisfied".to_string());
        }

        let epics = crate::epic_store::read_epics(&project)?;
        let epic = epics
            .epics
            .iter()
            .find(|epic| epic.id == pkg.epic_id)
            .ok_or_else(|| format!("epic '{}' not found", pkg.epic_id))?;
        if epic.status != EpicStatus::Active {
            return Err(format!("epic '{}' is not active", pkg.epic_id));
        }
        let step = epics
            .steps
            .iter()
            .find(|step| step.id == pkg.step_id && step.epic_id == pkg.epic_id)
            .ok_or_else(|| format!("step '{}' not found in epic", pkg.step_id))?;
        let summary = step
            .package
            .as_ref()
            .ok_or_else(|| "step has no sealed package summary".to_string())?;
        if summary.package_id != pkg.package_id
            || summary.plan_revision != pkg.plan_revision
            || summary.package_hash != pkg.package_hash
            || summary.executor != pkg.executor_preferred
        {
            return Err("step package summary drifted from the Runtime Store".to_string());
        }
        let stage_rank = |stage: &StepStage| match stage {
            StepStage::Pending => 0,
            StepStage::Designed => 1,
            StepStage::Coded => 2,
            StepStage::Verified => 3,
            StepStage::Observed => 4,
        };
        for precondition in &pkg.preconditions {
            if precondition.kind == "step_stage" {
                let dependency = precondition
                    .step_id
                    .as_deref()
                    .and_then(|id| epics.steps.iter().find(|step| step.id == id))
                    .ok_or_else(|| {
                        "step_stage precondition references a missing step".to_string()
                    })?;
                let required = match precondition.at_least.as_deref() {
                    Some("pending") => StepStage::Pending,
                    Some("designed") => StepStage::Designed,
                    Some("coded") => StepStage::Coded,
                    Some("verified") => StepStage::Verified,
                    Some("observed") => StepStage::Observed,
                    _ => return Err("step_stage precondition has an invalid stage".to_string()),
                };
                if stage_rank(&dependency.stage) < stage_rank(&required) {
                    return Err(format!(
                        "dependency '{}' is below required stage {:?}",
                        dependency.id, required
                    ));
                }
            }
        }
        let dependency_ids = epics
            .steps
            .iter()
            .filter(|candidate| candidate.epic_id == pkg.epic_id && candidate.order < step.order)
            .map(|candidate| candidate.id.as_str())
            .chain(step.depends_on.iter().map(String::as_str));
        for dependency_id in dependency_ids {
            let dep_stage = epics
                .steps
                .iter()
                .find(|candidate| candidate.id == dependency_id)
                .map(|candidate| &candidate.stage);
            // Order-predecessors must be at least coded (candidate produced).
            // Explicit depends_on must be observed (cross-session confirmed).
            let is_explicit_dep = step.depends_on.iter().any(|d| d == dependency_id);
            let required_stage = if is_explicit_dep {
                StepStage::Observed
            } else {
                StepStage::Coded
            };
            let sufficient = dep_stage
                .map(|stage| stage_rank(stage) >= stage_rank(&required_stage))
                .unwrap_or(false);
            if !sufficient {
                return Err(format!(
                    "dependency '{}' is below required stage {:?} (current: {:?})",
                    dependency_id,
                    required_stage,
                    dep_stage
                        .map(|s| format!("{:?}", s))
                        .unwrap_or_else(|| "missing".to_string())
                ));
            }
        }
        Ok(())
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

/// Ensure the Runner thread is running (start if not).
/// Called when a project is enabled — no-op if already running.
pub fn ensure_running() {
    if !is_running() {
        start_runner_thread(RunnerConfig {
            enabled: true,
            poll_interval: Duration::from_secs(10),
            runner_id: format!("runner-{}", std::process::id()),
        });
    } else {
        // Already running — make sure it hasn't been stopped
        RUNNER_RUNNING.store(true, Ordering::SeqCst);
    }
}

/// Check if the Runner is currently running (will process packages).
pub fn is_running() -> bool {
    RUNNER_RUNNING.load(Ordering::SeqCst)
}

/// Enable or disable the Runner at runtime.
/// Kept for backward compatibility (set_runner_enabled Tauri command).
/// Prefer per-project control via runner_config::set_project_enabled.
pub fn set_enabled(enabled: bool) -> bool {
    if enabled {
        ensure_running();
    } else {
        stop_runner();
    }
    enabled
}

/// Trusted acceptance command registry (mirrors the Node-side registry).
/// command_id → (command, relative cwd).
/// The Runner resolves command_ids here — it never accepts raw shell strings.
const ACCEPTANCE_REGISTRY: &[(&str, &str, &str)] = &[
    ("verify:candidate", "git diff --check HEAD^ HEAD", "."),
    // TypeScript (MCP server)
    ("test:mcp", "npm test", "mcp-server"),
    ("build:mcp", "npm run build", "mcp-server"),
    ("lint:mcp", "npx tsc --noEmit", "mcp-server"),
    // Rust (desktop backend) — cargo:* run from src-tauri
    ("cargo:check", "cargo check", "desktop-app/src-tauri"),
    (
        "cargo:clippy",
        "cargo clippy -- -D warnings",
        "desktop-app/src-tauri",
    ),
    ("cargo:build", "cargo build", "desktop-app/src-tauri"),
    ("test:desktop", "cargo test", "desktop-app/src-tauri"),
    // Frontend (desktop web)
    ("build:web", "npm run build", "desktop-app"),
    ("test:web", "npm test", "desktop-app"),
    ("lint:web", "npx tsc --noEmit", "desktop-app"),
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

    if let Some(expected_sha) = candidate_sha {
        let head = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo_root)
            .output();
        let actual_sha = head
            .as_ref()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string());
        if actual_sha.as_deref() != Some(expected_sha) {
            return VerifierResult {
                command_id: command_id.to_string(),
                command: command.to_string(),
                cwd: cwd_str,
                candidate_sha: Some(expected_sha.to_string()),
                exit_code: -1,
                started_at,
                finished_at: chrono::Utc::now().timestamp() as u64,
                stdout_excerpt: String::new(),
                stderr_excerpt: format!(
                    "verifier target mismatch: expected candidate {}, worktree HEAD {:?}",
                    expected_sha, actual_sha
                ),
                passed: false,
            };
        }
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
        let guard = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        // Enable the project in runner-config so poll_once picks it up
        crate::runner_config::set_project_enabled("/nonexistent", true).unwrap();
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

    #[test]
    fn preflight_rejects_tampered_package_hash_before_touching_project() {
        let package = TaskPackageSummary {
            package_id: "pkg_tampered".to_string(),
            epic_id: "e1".to_string(),
            step_id: "s1".to_string(),
            plan_revision: 1,
            project_path: "/does/not/exist".to_string(),
            repo_root: "/does/not/exist".to_string(),
            base_sha: "abc".to_string(),
            objective: "test".to_string(),
            allowed_paths: vec!["src/**".to_string()],
            forbidden_paths: vec![],
            max_changed_files: 1,
            executor_preferred: "zcode".to_string(),
            executor_fallbacks: vec![],
            sandbox: "workspace-write".to_string(),
            timeout_seconds: 60,
            max_attempts: 2,
            acceptance_command_ids: vec!["verify:candidate".to_string()],
            preconditions: vec![],
            auto_edit_worktree: true,
            auto_commit_candidate: true,
            auto_merge: false,
            auto_push: false,
            auto_deploy: false,
            sealed_at: "2026-08-11T00:00:00Z".to_string(),
            status: "ready".to_string(),
            package_hash: "stored".to_string(),
            computed_hash: "tampered".to_string(),
        };

        let error = Runner::validate_before_claim(&package, 1).unwrap_err();
        assert!(error.contains("hash mismatch"));
    }

    /// Run a git command inside `dir`, asserting success and returning stdout.
    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("failed to run git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// Seed a throwaway repo with an initial commit and return its SHA.
    fn init_repo(dir: &std::path::Path) -> String {
        git(dir, &["init", "-q"]);
        git(dir, &["config", "user.email", "runner@test.local"]);
        git(dir, &["config", "user.name", "Runner Test"]);
        std::fs::write(dir.join("file.txt"), "v1").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-q", "-m", "initial"]);
        git(dir, &["rev-parse", "HEAD"])
    }

    #[test]
    fn preflight_rejects_base_sha_drift() {
        // Real repo: package sealed against the FIRST commit, HEAD then moves
        // forward — validate_before_claim must refuse before touching anything.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let stale_sha = init_repo(repo);
        std::fs::write(repo.join("file.txt"), "v2").unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-q", "-m", "second"]);
        let head_sha = git(repo, &["rev-parse", "HEAD"]);
        assert_ne!(head_sha, stale_sha, "second commit must move HEAD");

        let repo_path = repo.to_string_lossy().to_string();
        let package = TaskPackageSummary {
            package_id: "pkg_drift".to_string(),
            epic_id: "e1".to_string(),
            step_id: "s1".to_string(),
            plan_revision: 1,
            project_path: repo_path.clone(),
            repo_root: repo_path,
            base_sha: stale_sha,
            objective: "test".to_string(),
            allowed_paths: vec!["src/**".to_string()],
            forbidden_paths: vec![],
            max_changed_files: 5,
            executor_preferred: "zcode".to_string(),
            executor_fallbacks: vec![],
            sandbox: "workspace-write".to_string(),
            timeout_seconds: 60,
            max_attempts: 2,
            acceptance_command_ids: vec!["verify:candidate".to_string()],
            preconditions: vec![],
            auto_edit_worktree: true,
            auto_commit_candidate: true,
            auto_merge: false,
            auto_push: false,
            auto_deploy: false,
            sealed_at: "2026-08-11T00:00:00Z".to_string(),
            status: "ready".to_string(),
            package_hash: "sealed".to_string(),
            computed_hash: "sealed".to_string(),
        };

        let error = Runner::validate_before_claim(&package, 1).unwrap_err();
        assert!(error.contains("base SHA drift"));
    }

    #[test]
    fn preflight_accepts_current_base_sha() {
        // Same fixture but sealed against the CURRENT HEAD — must not
        // false-positive on the drift check and must pass preflight entirely.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let head_sha = init_repo(repo);
        let repo_path = repo.to_string_lossy().to_string();

        // The checks after the drift gate read the epics store: an active epic
        // with a step whose sealed package summary matches the package.
        let epics = serde_json::json!({
            "version": 1,
            "project": "drift-test",
            "epics": [{
                "id": "e1",
                "title": "drift test",
                "status": "active",
                "priority": "P1",
                "created_at": "2026-08-11T00:00:00Z",
                "updated_at": "2026-08-11T00:00:00Z"
            }],
            "steps": [{
                "id": "s1",
                "epic_id": "e1",
                "title": "drift step",
                "order": 0,
                "depends_on": [],
                "stage": "pending",
                "created_at": "2026-08-11T00:00:00Z",
                "updated_at": "2026-08-11T00:00:00Z",
                "package": {
                    "package_id": "pkg_base_ok",
                    "plan_revision": 1,
                    "package_hash": "sealed",
                    "objective": "test",
                    "executor": "zcode",
                    "status": "ready"
                }
            }]
        });
        std::fs::write(
            repo.join(".epics.json"),
            serde_json::to_string_pretty(&epics).unwrap(),
        )
        .unwrap();

        let package = TaskPackageSummary {
            package_id: "pkg_base_ok".to_string(),
            epic_id: "e1".to_string(),
            step_id: "s1".to_string(),
            plan_revision: 1,
            project_path: repo_path.clone(),
            repo_root: repo_path,
            base_sha: head_sha,
            objective: "test".to_string(),
            allowed_paths: vec!["src/**".to_string()],
            forbidden_paths: vec![],
            max_changed_files: 5,
            executor_preferred: "zcode".to_string(),
            executor_fallbacks: vec![],
            sandbox: "workspace-write".to_string(),
            timeout_seconds: 60,
            max_attempts: 2,
            acceptance_command_ids: vec!["verify:candidate".to_string()],
            preconditions: vec![],
            auto_edit_worktree: true,
            auto_commit_candidate: true,
            auto_merge: false,
            auto_push: false,
            auto_deploy: false,
            sealed_at: "2026-08-11T00:00:00Z".to_string(),
            status: "ready".to_string(),
            package_hash: "sealed".to_string(),
            computed_hash: "sealed".to_string(),
        };

        Runner::validate_before_claim(&package, 1)
            .expect("package sealed against current HEAD must pass preflight");
    }

    #[cfg(unix)]
    #[test]
    fn timeout_terminates_executor_process_group() {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};

        let (_dir, _guard) = setup();
        let id =
            execution_store::enqueue_execution("pkg-timeout", "e1", "s1", "/tmp/proj", "abc123")
                .unwrap();
        execution_store::acquire_lease(&id, "runner-test").unwrap();

        let output_dir = tempfile::tempdir().unwrap();
        let stdout_path = output_dir.path().join("stdout.log");
        let stderr_path = output_dir.path().join("stderr.log");
        let stdout = std::fs::File::create(&stdout_path).unwrap();
        let stderr = std::fs::File::create(&stderr_path).unwrap();
        let mut command = Command::new("sh");
        command
            .args(["-c", "sleep 30 & wait"])
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        command.process_group(0);
        let child = command.spawn().unwrap();
        let pid = child.id();
        let mut handle = ExecutionHandle {
            child,
            session_id: None,
            stdout_path,
            stderr_path,
        };

        let outcome =
            Runner::supervise_execution(&mut handle, &id, Duration::from_millis(150)).unwrap();
        assert_eq!(outcome, SupervisionOutcome::TimedOut);
        let _ = handle.child.wait();
        let still_alive = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        assert!(!still_alive, "timed-out executor process must be gone");
    }

    /// Find the PID of the direct child of `parent` (e.g. the `sleep` the test
    /// shell spawns in the background). Returns None if there is none yet.
    fn child_pid(parent: u32) -> Option<u32> {
        let out = std::process::Command::new("pgrep")
            .args(["-P", &parent.to_string()])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .and_then(|line| line.trim().parse().ok())
    }

    /// Whether a process with the given PID exists (kill -0).
    fn process_alive(pid: u32) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    #[cfg(unix)]
    #[test]
    fn cancel_terminates_process_group() {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};

        let (_dir, _guard) = setup();
        let id =
            execution_store::enqueue_execution("pkg-cancel", "e1", "s1", "/tmp/proj", "abc123")
                .unwrap();
        execution_store::acquire_lease(&id, "runner-test").unwrap();

        let output_dir = tempfile::tempdir().unwrap();
        let stdout_path = output_dir.path().join("stdout.log");
        let stderr_path = output_dir.path().join("stderr.log");
        let stdout = std::fs::File::create(&stdout_path).unwrap();
        let stderr = std::fs::File::create(&stderr_path).unwrap();
        let mut command = Command::new("sh");
        command
            .args(["-c", "sleep 30 & wait"])
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        command.process_group(0);
        let child = command.spawn().unwrap();
        let pid = child.id();
        let mut handle = ExecutionHandle {
            child,
            session_id: None,
            stdout_path,
            stderr_path,
        };

        // Wait ~500ms for the process tree to come up, then locate the
        // grandchild `sleep` (child of the shell).
        let sleep_pid = (0..20)
            .find_map(|_| {
                std::thread::sleep(Duration::from_millis(100));
                child_pid(pid)
            })
            .expect("grandchild sleep process should have started");
        assert!(
            process_alive(sleep_pid),
            "sleep grandchild must be alive before cancel"
        );

        // Simulate what the supervisor does on CancelRequested.
        terminate_process_tree(&mut handle).unwrap();

        // The grandchild must be gone; death/reap is async, so poll briefly.
        let gone = (0..20).any(|_| {
            std::thread::sleep(Duration::from_millis(50));
            !process_alive(sleep_pid)
        });
        assert!(
            gone,
            "cancelled executor's sleep grandchild must be gone (pid {sleep_pid})"
        );
    }
}
