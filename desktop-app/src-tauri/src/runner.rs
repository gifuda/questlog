//! The Runner — picks up sealed TaskPackages from the queue and executes them.
//!
//! Data flow (all three breakpoints bridged):
//!   Node MCP prepare/validate/seal → packages.json (status=sealed or ready)
//!   → Runner polls packages.json, claims sealed/ready packages whose
//!     project + epic switches are ON ("switch = authorization"; the
//!     enqueue flip to "ready" is a conversation-side convenience, not a
//!     precondition). Manual-executor and dependency-blocked packages are
//!     skipped quietly, keeping their status.
//!   → Creates an execution in executions.json
//!   → Acquires lease, creates worktree
//!   → Runs ZCode CLI with the package's objective as prompt
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
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
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
/// Connection-class failure detection (network-failure auto-retry): these
/// errors say nothing about the WORK — the CLI never got through to the
/// model — so within budget the Runner retries automatically instead of
/// paging a human. Genuine code/CLI errors keep the human DecisionRequest.
pub fn is_network_failure(error: &str) -> bool {
    let e = error.to_lowercase();
    const SIGNALS: [&str; 16] = [
        "connection refused",
        "connection reset",
        "connection closed",
        "connection error",
        "unreachable",
        "no route to host",
        "network is down",
        "network error",
        "dns",
        "getaddrinfo",
        "name resolution",
        "enotfound",
        "econnrefused",
        "econnreset",
        "failed to fetch",
        "rate limit",
    ];
    SIGNALS.iter().any(|sig| e.contains(sig))
}

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
    /// The work finished but the AttemptBundle could not be sealed (disk
    /// full, read-only FS, rename failure). A success without sealed,
    /// integrity-hashed evidence is NOT a success — this converts Passed/
    /// NoDiff into a blocked outcome with a DecisionRequest.
    EvidenceSealFailed {
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

/// The base a package's worktree must start from.
#[derive(Debug)]
enum EffectiveBase {
    /// Current main HEAD — all predecessor work is merged (or there are no
    /// predecessors).
    Head(String),
    /// A predecessor step's passed-but-unmerged candidate commit: the
    /// successor chains onto it so multi-step work sees earlier steps'
    /// changes before anything merges to main.
    PredecessorCandidate(String),
}

impl EffectiveBase {
    fn sha(&self) -> &str {
        match self {
            Self::Head(sha) | Self::PredecessorCandidate(sha) => sha,
        }
    }
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
        // Master kill switch — checked on every poll so a global OFF takes
        // effect at the next cycle without waiting for an app restart.
        if !crate::runner_config::is_globally_enabled() {
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

            // Quiet-skip gates: packages that must wait silently (keep their
            // status) instead of being claimed and then blocked.
            // 1. manual executor = tracking-only, never auto-executed.
            if pkg.executor_preferred == "manual" {
                continue;
            }
            // 2. Dependencies not yet satisfied (order-predecessor below
            //    coded, explicit depends_on below observed, step_stage
            //    preconditions unmet) — wait without burning an execution.
            //    Hard data errors fall through to the normal claim path so
            //    validate_before_claim surfaces them as PreflightBlocked.
            match Self::dependencies_ready(&pkg) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(e) => log::warn!(
                    "Runner: dependency check error for {}: {} (falling through to preflight)",
                    pkg.package_id,
                    e
                ),
            }

            let executions = execution_store::list_executions()?;

            // Auto re-anchor onto the EFFECTIVE base: the current HEAD when
            // all predecessor work is merged, or the single unmerged
            // predecessor candidate (commit lineage) when the chain has not
            // merged yet — successors must see predecessor changes. Content
            // fields are untouched. Eligible unless this package already
            // SUCCEEDED once (a passed execution is the trust anchor;
            // failed/timeout attempts don't block — their retry re-anchors
            // fresh, which also disables resume via the base-mismatch check).
            // A resolution failure (e.g. multiple unmerged predecessor
            // candidates) is NOT decided here — the claim path's preflight
            // surfaces it as PreflightBlocked with a DecisionRequest.
            let pkg = {
                let effective = Self::resolve_effective_base(&pkg).ok();
                match effective {
                    Some(base)
                        if base.sha() != pkg.base_sha
                            && !executions.iter().any(|e| {
                                e.package_id == pkg.package_id
                                    && e.status == ExecutionStatus::Passed
                            }) =>
                    {
                        match execution_store::reanchor_package(
                            &pkg.package_id,
                            base.sha(),
                            &pkg.project_path,
                            &pkg.step_id,
                        ) {
                            Ok(reshored) => {
                                log::info!(
                                    "Runner: re-anchored {} to effective base {} (rev {})",
                                    pkg.package_id,
                                    &base.sha()[..8.min(base.sha().len())],
                                    reshored.plan_revision
                                );
                                reshored
                            }
                            Err(e) => {
                                log::warn!(
                                    "Runner: re-anchor failed for {}: {} (claiming as-is)",
                                    pkg.package_id,
                                    e
                                );
                                pkg
                            }
                        }
                    }
                    _ => pkg,
                }
            };

            // Atomic claim: the uniqueness check, execution creation, and
            // lease acquisition happen under ONE lock — two pollers can
            // never both claim the same package. A pre-created retry child
            // (Queued, carrying parent linkage) is adopted, not duplicated.
            let claimed = match execution_store::claim_package(
                &pkg.package_id,
                &pkg.epic_id,
                &pkg.step_id,
                &pkg.project_path,
                &pkg.base_sha,
                &self.config.runner_id,
            ) {
                Ok(exec) => exec,
                Err(e) => {
                    log::debug!("Runner: claim skipped for {}: {}", pkg.package_id, e);
                    continue;
                }
            };
            let exec_id = claimed.id.clone();

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
                        Some("passed"),
                    )?;
                    match final_status {
                        ExecutionStatus::Passed => {
                            execution_store::mark_package_status(&pkg.package_id, "passed")?;
                            // Package acceptance IS the gate (user-directed
                            // B mode): a passing execution advances the step
                            // straight to observed, releasing dependent steps
                            // without a human confirmation hop. Deviations
                            // (blocked/failed) never reach here.
                            Self::advance_step_stage(
                                &pkg.project_path,
                                &pkg.step_id,
                                StepStage::Observed,
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
                    if pkg.sandbox == "read-only" {
                        // A read-only task legitimately produces no diff: its
                        // output IS the stdout report (no candidate commit is
                        // expected). Treat as success so read-only analysis
                        // chains can complete.
                        let final_status = execution_store::finish_execution(
                            execution_id,
                            ExecutionStatus::Passed,
                            None,
                            Some("no_diff_readonly"),
                        )?;
                        match final_status {
                            ExecutionStatus::Passed => {
                                execution_store::mark_package_status(&pkg.package_id, "passed")?;
                                Self::advance_step_stage(
                                    &pkg.project_path,
                                    &pkg.step_id,
                                    StepStage::Observed,
                                );
                            }
                            ExecutionStatus::Cancelled => {
                                execution_store::mark_package_status(&pkg.package_id, "cancelled")?;
                            }
                            _ => {
                                execution_store::mark_package_status(&pkg.package_id, "blocked")?;
                            }
                        }
                    } else {
                        execution_store::finish_execution(
                            execution_id,
                            ExecutionStatus::Blocked,
                            Some("CLI produced no diff"),
                            Some("no_diff"),
                        )?;
                        execution_store::mark_package_status(&pkg.package_id, "blocked")?;
                    }
                }
                Ok(RunResult::AcceptanceFailed {
                    execution_id,
                    reason,
                }) => {
                    execution_store::finish_execution(
                        execution_id,
                        ExecutionStatus::Blocked,
                        Some(reason),
                        Some("acceptance_failed"),
                    )?;
                    execution_store::mark_package_status(&pkg.package_id, "blocked")?;
                }
                Ok(RunResult::ScopeViolation {
                    execution_id,
                    violation,
                }) => {
                    let outcome_kind =
                        if matches!(violation, runner_git::ScopeViolation::TooManyFiles { .. }) {
                            // S3: count breach — never a scene-recovery source.
                            "scope_count_violation"
                        } else {
                            "scope_violation"
                        };
                    execution_store::finish_execution(
                        execution_id,
                        ExecutionStatus::Blocked,
                        Some(&violation.to_string()),
                        Some(outcome_kind),
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
                        Some("base_sha_drift"),
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
                        Some("cli_failed"),
                    )?;
                    // Network-failure governance (same spirit as timeouts):
                    // a connection-class failure is an environment signal,
                    // not a defect — retry within budget (the retry reuses
                    // the retained worktree/session when possible) instead
                    // of paging a human. Genuine CLI/code errors keep the
                    // human DR created by seal_bundle.
                    let network = is_network_failure(error);
                    let budget_used = execution_store::get_execution(execution_id)
                        .map(|e| execution_store::effective_budget_used(&e))
                        .unwrap_or(0);
                    if network && (budget_used as u64) < pkg.max_attempts.max(1) as u64 {
                        match execution_store::create_retry_attempt(execution_id) {
                            Ok(child_id) => {
                                log::info!(
                                    "Runner: network-class CLI failure (budget {}/{}) → auto-retry scheduled ({})",
                                    budget_used, pkg.max_attempts.max(1), child_id
                                );
                                // create_retry_attempt flipped the package
                                // back to ready — skip the blocked marking.
                            }
                            Err(e) => {
                                log::warn!("Runner: network auto-retry failed for {execution_id}: {e}");
                                execution_store::mark_package_status(&pkg.package_id, "blocked")?;
                                let question = format!(
                                    "Network-class CLI failure, and auto-retry scheduling failed (budget {}/{}): {}. Revise the package or cancel?",
                                    budget_used, pkg.max_attempts.max(1), e
                                );
                                let _ = crate::decision_store::create_request(
                                    crate::decision_store::NewDecisionRequest {
                                        epic_id: &pkg.epic_id,
                                        step_id: &pkg.step_id,
                                        execution_id,
                                        kind: "blocked",
                                        question: &question,
                                        options: vec![
                                            "revise_package".to_string(),
                                            "cancel".to_string(),
                                        ],
                                        evidence_refs: vec![],
                                        package_revision: pkg.plan_revision,
                                    },
                                );
                            }
                        }
                    } else {
                        execution_store::mark_package_status(&pkg.package_id, "blocked")?;
                    }
                }
                Ok(RunResult::Timeout { execution_id }) => {
                    execution_store::finish_execution(
                        execution_id,
                        ExecutionStatus::TimedOut,
                        Some("timeout"),
                        Some("timeout"),
                    )?;
                    // Budget governance: a timeout is a budget signal, not a
                    // defect. While budget remains, auto-schedule a retry
                    // (with exponentially escalated budget) instead of
                    // blocking the package and paging a human. Only an
                    // exhausted budget becomes a DecisionRequest.
                    let budget_used = execution_store::get_execution(execution_id)
                        .map(|e| execution_store::effective_budget_used(&e))
                        .unwrap_or(0);
                    if (budget_used as u64) < pkg.max_attempts.max(1) as u64 {
                        match execution_store::create_retry_attempt(execution_id) {
                            Ok(child_id) => {
                                log::info!(
                                    "Runner: timeout on budget {} → auto-retry scheduled ({}), worktree kept for resume",
                                    budget_used, child_id
                                );
                                // create_retry_attempt already flipped the
                                // package back to ready.
                            }
                            Err(e) => {
                                // S0-A2: the failure must reach the human
                                // inbox, not just the log — a swallowed
                                // auto-retry error leaves an unexplainable
                                // blocked package.
                                log::warn!("Runner: auto-retry failed for {execution_id}: {e}");
                                execution_store::mark_package_status(&pkg.package_id, "blocked")?;
                                let question = format!(
                                    "Auto-retry scheduling failed for execution {} (budget {}/{}): {}. Revise the package or cancel?",
                                    execution_id, budget_used, pkg.max_attempts.max(1), e
                                );
                                if let Err(dr_err) =
                                    crate::decision_store::create_request(
                                        crate::decision_store::NewDecisionRequest {
                                            epic_id: &pkg.epic_id,
                                            step_id: &pkg.step_id,
                                            execution_id,
                                            kind: "blocked",
                                            question: &question,
                                            options: vec![
                                                "revise_package".to_string(),
                                                "cancel".to_string(),
                                            ],
                                            evidence_refs: vec![],
                                            package_revision: pkg.plan_revision,
                                        },
                                    )
                                {
                                    log::error!(
                                        "Runner: failed to record auto-retry-failure DR for {execution_id}: {dr_err}"
                                    );
                                }
                            }
                        }
                    } else {
                        execution_store::mark_package_status(&pkg.package_id, "blocked")?;
                    }
                }
                Ok(RunResult::Cancelled { execution_id }) => {
                    execution_store::finish_execution(
                        execution_id,
                        ExecutionStatus::Cancelled,
                        None,
                        Some("cancelled"),
                    )?;
                    execution_store::mark_package_status(&pkg.package_id, "cancelled")?;
                }
                Ok(RunResult::Paused { execution_id }) => {
                    execution_store::finish_execution(
                        execution_id,
                        ExecutionStatus::Blocked,
                        Some("paused by user"),
                        Some("paused"),
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
                        Some("policy_violation"),
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
                        Some("preflight_blocked"),
                    )?;
                    execution_store::mark_package_status(&pkg.package_id, "blocked")?;
                }
                Ok(RunResult::EvidenceSealFailed {
                    execution_id,
                    error,
                }) => {
                    execution_store::finish_execution(
                        execution_id,
                        ExecutionStatus::Blocked,
                        Some(&format!("evidence seal failed: {error}")),
                        Some("evidence_seal_failed"),
                    )?;
                    execution_store::mark_package_status(&pkg.package_id, "blocked")?;
                }
                Err(e) => {
                    execution_store::finish_execution(&exec_id, ExecutionStatus::Failed, Some(e), None)?;
                    execution_store::mark_package_status(&pkg.package_id, "failed")?;
                }
            }

            // Worktree cleanup: removed on every terminal outcome EXCEPT:
            //   1. a timeout that scheduled an auto-retry (the retry resumes
            //      the same session in the same worktree), or
            //   2. S1 evidence-incomplete outcomes — scene_preserved=false
            //      (capture failed/partial) or process_execution returned Err
            //      (internal failure before sealing; the worktree IS the
            //      remaining evidence and must survive for manual recovery).
            // OR semantics: any one reason keeps the scene. A get_execution
            // failure also keeps it (fail-open: never delete what we cannot
            // inspect).
            let process_errored = result.is_err();
            let mut keep_worktree = process_errored;
            if let Ok(final_exec) = execution_store::get_execution(&exec_id) {
                if final_exec.scene_preserved == Some(false) {
                    log::error!(
                        "Runner: keeping worktree for {exec_id} — scene capture incomplete, the worktree is the remaining evidence"
                    );
                    keep_worktree = true;
                }
                // Network-class CLI failures with a scheduled auto-retry
                // keep the worktree too — the CLI may have made partial
                // progress before the connection dropped, and the retry's
                // resume path reuses the scene when possible.
                if !keep_worktree
                    && final_exec.status == ExecutionStatus::Blocked
                    && final_exec.outcome.as_deref() == Some("cli_failed")
                    && final_exec
                        .last_error
                        .as_deref()
                        .is_some_and(is_network_failure)
                {
                    if let Ok(all) = execution_store::list_executions() {
                        keep_worktree = all.iter().any(|e| {
                            e.parent_attempt_id.as_deref() == Some(final_exec.id.as_str())
                                && matches!(
                                    e.status,
                                    ExecutionStatus::Queued | ExecutionStatus::Running
                                )
                        });
                    }
                }
                if !keep_worktree && final_exec.status == ExecutionStatus::TimedOut {
                    // Auto-retry child (queued/running) exists for this package?
                    if let Ok(all) = execution_store::list_executions() {
                        keep_worktree = all.iter().any(|e| {
                            e.parent_attempt_id.as_deref() == Some(final_exec.id.as_str())
                                && matches!(
                                    e.status,
                                    ExecutionStatus::Queued | ExecutionStatus::Running
                                )
                        });
                    }
                }
            } else {
                // Cannot inspect the final state — do not delete the scene.
                keep_worktree = true;
            }
            if !keep_worktree {
                if let Ok(final_exec) = execution_store::get_execution(&exec_id) {
                    if let Some(worktree_path) = &final_exec.worktree_path {
                        let _ = runner_git::remove_worktree(
                            &PathBuf::from(&pkg.repo_root),
                            Path::new(worktree_path),
                        );
                    }
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
                // Beat the RUNNER heartbeat too: the thread is alive and
                // working (synchronously executing this package), it just
                // can't return to the poll loop. Without this beat the UI
                // reports a false "stale" for the whole execution duration.
                LAST_POLL_AT.store(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0),
                    Ordering::SeqCst,
                );
                last_heartbeat = Instant::now();
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// Create a fresh worktree for an execution and record it.
    fn fresh_worktree(
        repo_root: &Path,
        base_sha: &str,
        exec_id: &str,
        bundle_dir: &Path,
    ) -> Result<(PathBuf, String), String> {
        let branch_name = format!("questlog/{}", exec_id);
        let worktree = runner_git::create_worktree(repo_root, base_sha, &branch_name)?;
        execution_store::update_execution(
            exec_id,
            ExecutionUpdates {
                worktree_path: Some(worktree.path.to_string_lossy().to_string()),
                branch: Some(branch_name.clone()),
                ..Default::default()
            },
        )?;
        let _ = attempt_bundle::append_event(
            bundle_dir,
            &serde_json::json!({
                "event": "worktree_created",
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "worktree_path": worktree.path.to_string_lossy(),
            }),
        );
        Ok((worktree.path, branch_name))
    }

    /// Per-attempt execution budget: the package's base timeout grows
    /// exponentially with each attempt (capped at 2h). A timeout means the
    /// estimate was too small, not that the work is impossible — retries
    /// get more room. Purely runtime: never written back into the package
    /// (which would break the sealed hash).
    fn effective_timeout_secs(base: u64, attempt_no: u32) -> u64 {
        let exp = attempt_no.saturating_sub(1).min(8);
        base.saturating_mul(1u64 << exp).min(7200)
    }

    /// Resolve the CLI session id for a finished execution. The headless CLI
    /// does NOT print its session id, but it always records a row in the
    /// ZCode session DB keyed by the (resolved) working directory — look it
    /// up there, excluding subagent sessions. Best-effort: any failure means
    /// "no resume", never a hard error.
    fn resolve_cli_session(worktree_path: &Path) -> Option<String> {
        let dir = worktree_path
            .canonicalize()
            .unwrap_or_else(|_| worktree_path.to_path_buf());
        let dir = dir.to_string_lossy().replace('\'', "''");
        let home = dirs::home_dir()?;
        let db = home.join(".zcode").join("cli").join("db").join("db.sqlite");
        if !db.exists() {
            return None;
        }
        let sql = format!(
            "SELECT id FROM session WHERE directory = '{}' AND id NOT LIKE 'sess_subagent%' ORDER BY time_created DESC LIMIT 1",
            dir
        );
        let out = std::process::Command::new("sqlite3")
            .arg(db)
            .arg(&sql)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let sid = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if sid.starts_with("sess_") {
            Some(sid)
        } else {
            None
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
            execution_store::effective_budget_used(&exec),
            exec.parent_attempt_id.as_deref(),
            pkg,
        );
        // S2: retry/resume children inherit the recovery marker — the
        // worktree they resume already carries the recovered scene, and the
        // chain stays auditable back to the original ScopeViolation.
        if let Some(parent_id) = exec.parent_attempt_id.as_deref() {
            if let Ok(parent_manifest) = attempt_bundle::read_manifest(parent_id) {
                manifest.scene_recovered_from = parent_manifest.scene_recovered_from;
            }
        }

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

        if let Err(error) = Self::validate_before_claim(
            pkg,
            execution_store::effective_budget_used(&exec),
        ) {
            let result = RunResult::PreflightBlocked {
                execution_id: exec.id.clone(),
                error,
            };
            let result = self.seal_bundle(&bundle_dir, &mut manifest, pkg, result, None, &[], None);
            return Ok(result);
        }

        // 1. Verify the package still matches its effective base (HEAD, or
        // the unmerged predecessor candidate it chains onto). A lineage
        // conflict must not guess — surface it for a human decision.
        let effective_base = match Self::resolve_effective_base(pkg) {
            Ok(base) => base,
            Err(error) => {
                let result = RunResult::PreflightBlocked {
                    execution_id: exec.id.clone(),
                    error,
                };
                let result = self.seal_bundle(&bundle_dir, &mut manifest, pkg, result, None, &[], None);
                return Ok(result);
            }
        };
        if effective_base.sha() != pkg.base_sha {
            let result = RunResult::BaseShaDrift {
                execution_id: exec.id.clone(),
                expected: pkg.base_sha.clone(),
                actual: effective_base.sha().to_string(),
            };
            let result = self.seal_bundle(&bundle_dir, &mut manifest, pkg, result, None, &[], None);
            return Ok(result);
        }

        // 2. Create worktree
        // 2. Worktree — or a resume of the parent's interrupted scene.
        // A timeout auto-retry whose parent left a resumable CLI session AND
        // an intact worktree at the SAME base reuses both: the session
        // context (analysis already paid for) plus the half-done work. Any
        // mismatch (no session, worktree gone, base re-anchored) falls back
        // to a fresh worktree — resume is best-effort, never blocking.
        let mut resume: Option<String> = None;
        let (worktree_path, _branch_name) = if let Some(parent_id) = exec.parent_attempt_id.clone()
        {
            let parent = execution_store::get_execution(&parent_id)?;
            let eligible = parent.package_id == pkg.package_id
                && parent.base_sha == pkg.base_sha
                && parent
                    .cli_session
                    .as_deref()
                    .map(|s| !s.is_empty())
                    .unwrap_or(false);
            if eligible {
                if let Some(wtp) = parent.worktree_path.clone() {
                    let path = PathBuf::from(&wtp);
                    if path.is_dir() {
                        let branch = parent.branch.clone().unwrap_or_default();
                        execution_store::update_execution(
                            &exec.id,
                            ExecutionUpdates {
                                worktree_path: Some(wtp),
                                branch: Some(branch.clone()),
                                ..Default::default()
                            },
                        )?;
                        let _ = attempt_bundle::append_event(
                            &bundle_dir,
                            &serde_json::json!({
                                "event": "resumed_from_parent",
                                "timestamp": chrono::Utc::now().to_rfc3339(),
                                "parent_attempt_id": parent_id,
                                "cli_session": parent.cli_session,
                                "worktree_path": path.to_string_lossy(),
                            }),
                        );
                        resume = parent.cli_session.clone();
                        (path, branch)
                    } else {
                        Self::fresh_worktree(&repo_root, &pkg.base_sha, &exec.id, &bundle_dir)?
                    }
                } else {
                    Self::fresh_worktree(&repo_root, &pkg.base_sha, &exec.id, &bundle_dir)?
                }
            } else {
                // Parent kept its worktree for a resume that can no longer
                // happen (base re-anchored / no session) — clean the orphan
                // scene, then start fresh.
                if let Some(wtp) = &parent.worktree_path {
                    let _ = runner_git::remove_worktree(&repo_root, Path::new(wtp));
                }
                Self::fresh_worktree(&repo_root, &pkg.base_sha, &exec.id, &bundle_dir)?
            }
        } else {
            Self::fresh_worktree(&repo_root, &pkg.base_sha, &exec.id, &bundle_dir)?
        };

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
            let result = self.seal_bundle(&bundle_dir, &mut manifest, pkg, result, None, &[], Some(&worktree_path));
            return Ok(result);
        }

        let mut executor: Box<dyn Executor> = Box::new(ZCodeExecutor::new());
        let probe = executor.probe();
        if !probe.available {
            let result = RunResult::CliFailed {
                execution_id: exec.id.clone(),
                error: "ZCode CLI not available".to_string(),
            };
            let result = self.seal_bundle(&bundle_dir, &mut manifest, pkg, result, None, &[], Some(&worktree_path));
            return Ok(result);
        }
        // ─── S2: scene recovery for a NEW first attempt of this package ───
        // A previous revision's ScopeViolation left a fully-captured scene;
        // the operator approved accurate paths in the new revision — reuse
        // that work instead of redoing it. Only first attempts (no parent)
        // recover: retry/resume chains physically carry the worktree on.
        let mut recovery_note = String::new();
        if exec.parent_attempt_id.is_none() {
            if let Ok(all) = execution_store::list_executions() {
                if let Some(prev_id) = execution_store::find_recoverable_scene(
                    &all,
                    &pkg.package_id,
                    &exec.id,
                    &pkg.base_sha,
                ) {
                    let prev_bundle = attempt_bundle::create_bundle_dir(&prev_id)
                        .unwrap_or_else(|_| {
                            execution_store::runtime_dir()
                                .join("attempts")
                                .join(&prev_id)
                        });
                    match attempt_bundle::recover_scene_into_worktree(
                        &worktree_path,
                        &prev_bundle,
                    ) {
                        Ok(report) => {
                            log::info!(
                                "Runner: recovered scene from {prev_id} into {} (patched={}, untracked={})",
                                worktree_path.display(),
                                report.patched,
                                report.untracked_restored
                            );
                            manifest.scene_recovered_from = Some(prev_id.clone());
                            // Also stamp the execution row so the UI can show
                            // the recovery badge without reading the bundle.
                            let _ = execution_store::update_execution(
                                &exec.id,
                                ExecutionUpdates {
                                    scene_recovered_from: Some(prev_id.clone()),
                                    ..Default::default()
                                },
                            );
                            let _ = attempt_bundle::append_event(
                                &bundle_dir,
                                &serde_json::json!({
                                    "event": "scene_recovered",
                                    "timestamp": chrono::Utc::now().to_rfc3339(),
                                    "source_execution": prev_id,
                                    "patched": report.patched,
                                    "untracked_restored": report.untracked_restored,
                                }),
                            );
                            recovery_note = format!(
                                "\n\n[QuestLog 成果恢复] 前次尝试因范围封存被熔断，其改动已应用在当前工作区（patched={}，untracked 新文件 {} 个）。请在其基础上验证、调整并完成目标——不要从零重做。",
                                report.patched, report.untracked_restored
                            );
                        }
                        Err(e) => {
                            log::warn!(
                                "Runner: scene recovery from {prev_id} failed — degrading to a clean re-run: {e}"
                            );
                            let _ = attempt_bundle::append_event(
                                &bundle_dir,
                                &serde_json::json!({
                                    "event": "scene_recovery_failed",
                                    "timestamp": chrono::Utc::now().to_rfc3339(),
                                    "source_execution": prev_id,
                                    "error": e,
                                }),
                            );
                        }
                    }
                }
            }
        }

        // Budget governance: each retry attempt gets an exponentially larger
        // budget (capped) — a timeout means the estimate was small, not that
        // the work is impossible.
        let timeout = Duration::from_secs(Self::effective_timeout_secs(
            pkg.timeout_seconds,
            exec.attempt_no,
        ));
        let sandbox = if pkg.sandbox.is_empty() {
            "workspace-write"
        } else {
            &pkg.sandbox
        };
        let mut handle = if let Some(sid) = &resume {
            let task = format!(
                "{}\n\n[续接说明] 你在恢复一个因超时中断的会话：已完成的分析和阶段不要重做，先查看工作区当前状态再继续。剩余预算约 {} 秒。",
                pkg.objective,
                timeout.as_secs()
            );
            executor.start_with_resume(&task, sid, &worktree_path, sandbox, timeout)?
        } else {
            let task = if recovery_note.is_empty() {
                pkg.objective.clone()
            } else {
                format!("{}{}", pkg.objective, recovery_note)
            };
            // S2: persist the actual prompt so audits can see what the
            // executor was told, distinct from the sealed objective.
            if !recovery_note.is_empty() {
                match attempt_bundle::write_task_prompt(&bundle_dir, &task) {
                    Ok(Some(ref_name)) => manifest.task_prompt_ref = Some(ref_name),
                    Ok(None) => {}
                    Err(e) => {
                        log::warn!("Failed to persist task prompt: {e}");
                    }
                }
            }
            executor.start(&task, &worktree_path, sandbox, timeout)?
        };
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

        // Persist the CLI session id so a timeout auto-retry can --resume it.
        // The events rarely carry it (the CLI doesn't print it) — the session
        // DB lookup by worktree directory is the reliable path.
        {
            let sid = events
                .iter()
                .find_map(|event| event.session_id.clone())
                .or_else(|| Self::resolve_cli_session(&worktree_path));
            if let Some(sid) = sid {
                let _ = execution_store::update_execution(
                    &exec.id,
                    ExecutionUpdates {
                        cli_session: Some(sid),
                        ..Default::default()
                    },
                );
            }
        }

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
                let result = self.seal_bundle(&bundle_dir, &mut manifest, pkg, result, None, &[], Some(&worktree_path));
                return Ok(result);
            }
            SupervisionOutcome::Paused => {
                let result = RunResult::Paused {
                    execution_id: exec.id.clone(),
                };
                let result = self.seal_bundle(&bundle_dir, &mut manifest, pkg, result, None, &[], Some(&worktree_path));
                return Ok(result);
            }
            SupervisionOutcome::TimedOut => {
                let result = RunResult::Timeout {
                    execution_id: exec.id.clone(),
                };
                let result = self.seal_bundle(&bundle_dir, &mut manifest, pkg, result, None, &[], Some(&worktree_path));
                return Ok(result);
            }
            SupervisionOutcome::Completed => {}
        }

        if !cli_succeeded {
            let result = RunResult::CliFailed {
                execution_id: exec.id.clone(),
                error: "CLI exited with non-zero status".to_string(),
            };
            let result = self.seal_bundle(&bundle_dir, &mut manifest, pkg, result, None, &[], Some(&worktree_path));
            return Ok(result);
        }

        // 5. Check for changes
        let changed = runner_git::get_changed_paths(&worktree_path, &pkg.base_sha)?;
        if changed.is_empty() {
            // Read-only artifact contract: "no diff" only means "no code
            // change" — it is NOT acceptance. A read-only task must pass its
            // declared acceptance commands (run against the worktree as-is)
            // AND produce a non-empty stdout report before it may complete.
            if pkg.sandbox == "read-only" {
                if pkg.acceptance_command_ids.is_empty() {
                    let result = RunResult::AcceptanceFailed {
                        execution_id: exec.id.clone(),
                        reason: "read-only task declared no acceptance commands — cannot verify the report"
                            .to_string(),
                    };
                    let result =
                        self.seal_bundle(&bundle_dir, &mut manifest, pkg, result, None, &[], Some(&worktree_path));
                    return Ok(result);
                }
                let stdout_nonempty = stdout_path
                    .as_ref()
                    .and_then(|p| std::fs::metadata(p).ok())
                    .map(|m| m.len() > 0)
                    .unwrap_or(false);
                if !stdout_nonempty {
                    let result = RunResult::AcceptanceFailed {
                        execution_id: exec.id.clone(),
                        reason:
                            "read-only task produced an empty report (stdout) — nothing to accept"
                                .to_string(),
                    };
                    let result =
                        self.seal_bundle(&bundle_dir, &mut manifest, pkg, result, None, &[], Some(&worktree_path));
                    return Ok(result);
                }
                let mut verifier_results: Vec<VerifierResult> = Vec::new();
                for cmd_id in &pkg.acceptance_command_ids {
                    let vr = resolve_and_run_acceptance_verbose(cmd_id, &worktree_path, None);
                    let passed = vr.passed;
                    verifier_results.push(vr);
                    if !passed {
                        let result = RunResult::AcceptanceFailed {
                            execution_id: exec.id.clone(),
                            reason: format!("read-only acceptance command '{cmd_id}' failed"),
                        };
                        let result = self.seal_bundle(
                            &bundle_dir,
                            &mut manifest,
                            pkg,
                            result,
                            None,
                            &verifier_results,
                            Some(&worktree_path),
                        );
                        return Ok(result);
                    }
                }
                let result = RunResult::NoDiff {
                    execution_id: exec.id.clone(),
                };
                let result = self.seal_bundle(
                    &bundle_dir,
                    &mut manifest,
                    pkg,
                    result,
                    None,
                    &verifier_results,
                    Some(&worktree_path),
                );
                return Ok(result);
            }
            let result = RunResult::NoDiff {
                execution_id: exec.id.clone(),
            };
            let result = self.seal_bundle(&bundle_dir, &mut manifest, pkg, result, None, &[], Some(&worktree_path));
            return Ok(result);
        }

        // 6. Verify scope (circuit breaker) — path rights are the HARD gate
        // (modify vs create decided by the base tree); file COUNT uses the
        // S3 three-tier semantics: max is the last-resort breaker, warn only
        // annotates the bundle and the candidate, expected is recorded.
        let base_tree = runner_git::base_tree_files(&worktree_path, &pkg.base_sha)?;
        if let Err(violation) = runner_git::verify_changed_paths_in_scope(
            &changed,
            &base_tree,
            &pkg.allowed_paths,
            &pkg.create_paths,
            &pkg.forbidden_paths,
            pkg.max_changed_files as usize,
        ) {
            let result = RunResult::ScopeViolation {
                execution_id: exec.id.clone(),
                violation,
            };
            let result = self.seal_bundle(&bundle_dir, &mut manifest, pkg, result, None, &[], Some(&worktree_path));
            return Ok(result);
        }
        // Three-tier count semantics (path rights already passed): a warn
        // breach does NOT block — the candidate and acceptance continue,
        // carrying a visible scope-size warning for the final review.
        match runner_git::scope_size_verdict(
            changed.len(),
            pkg.expected_changed_files,
            pkg.warn_changed_files,
            pkg.max_changed_files as usize,
        ) {
            runner_git::SizeVerdict::OverWarn => {
                let warning = format!(
                    "{} files changed exceeds the warn threshold ({}) — max breaker is {}",
                    changed.len(),
                    pkg.warn_changed_files.unwrap_or(0),
                    pkg.max_changed_files
                );
                log::warn!("Runner: scope-size warning for {}: {warning}", exec.id);
                manifest.scope_size_warning = Some(warning);
            }
            runner_git::SizeVerdict::OverExpected => {
                log::info!(
                    "Runner: {} files changed exceeds the expected estimate ({})",
                    changed.len(),
                    pkg.expected_changed_files.unwrap_or(0)
                );
            }
            _ => {}
        }

        match execution_store::get_execution(&exec.id)?.status {
            ExecutionStatus::CancelRequested => {
                let result = RunResult::Cancelled {
                    execution_id: exec.id.clone(),
                };
                let result = self.seal_bundle(&bundle_dir, &mut manifest, pkg, result, None, &[], Some(&worktree_path));
                return Ok(result);
            }
            ExecutionStatus::PauseRequested => {
                let result = RunResult::Paused {
                    execution_id: exec.id.clone(),
                };
                let result = self.seal_bundle(&bundle_dir, &mut manifest, pkg, result, None, &[], Some(&worktree_path));
                return Ok(result);
            }
            _ => {}
        }

        // 7. Create candidate commit with QuestLog trailers
        let candidate_sha = runner_git::create_candidate_commit(
            &worktree_path,
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
                let result = self.seal_bundle(&bundle_dir, &mut manifest, pkg, result, None, &[], Some(&worktree_path));
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
            attempt_bundle::write_diff_patch(&bundle_dir, &worktree_path, &pkg.base_sha)?;
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
                    // S3: warn-breach marker so the final review sees the
                    // scope-size warning on the candidate itself.
                    "QuestLog-ScopeWarning": manifest
                        .scope_size_warning
                        .clone()
                        .unwrap_or_else(|| "none".to_string()),
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
            let result = self.seal_bundle(
                &bundle_dir,
                &mut manifest,
                pkg,
                result,
                Some(&candidate_sha),
                &[],
                Some(&worktree_path),
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
                resolve_and_run_acceptance_verbose(cmd_id, &worktree_path, Some(&candidate_sha));
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
                let result = self.seal_bundle(
                    &bundle_dir,
                    &mut manifest,
                    pkg,
                    result,
                    Some(&candidate_sha),
                    &verifier_results,
                    Some(&worktree_path),
                );
                return Ok(result);
            }
        }

        // All acceptance commands passed → verified
        let result = RunResult::Passed {
            execution_id: exec.id.clone(),
            candidate_sha: candidate_sha.clone(),
        };
        let result = self.seal_bundle(
            &bundle_dir,
            &mut manifest,
            pkg,
            result,
            Some(&candidate_sha),
            &verifier_results,
            Some(&worktree_path),
        );
        Ok(result)
    }

    /// Seal the AttemptBundle: write verifier results, outcome, and compute
    /// integrity hash. Takes the outcome by value and returns the EFFECTIVE
    /// outcome: if sealing fails, a success-class result (Passed/NoDiff) is
    /// converted to EvidenceSealFailed — evidence fail-closed, a success
    /// without a sealed bundle is not a success. Failure-class results pass
    /// through unchanged (their DecisionRequest is already created below).
    #[allow(clippy::too_many_arguments)] // sealing genuinely needs all of these
    fn seal_bundle(
        &self,
        bundle_dir: &Path,
        manifest: &mut AttemptManifest,
        pkg: &TaskPackageSummary,
        result: RunResult,
        candidate_sha: Option<&str>,
        verifier_results: &[VerifierResult],
        worktree_path: Option<&Path>,
    ) -> RunResult {
        // 0. S1 scene capture FIRST — before any worktree-clearing terminal
        //    this attempt can reach, the full recoverable scene (tracked
        //    staged+unstaged diff, untracked copies, post-base commits,
        //    changed-path classification) must already sit in the bundle.
        //    Runs before the DR creation (its question references scene/)
        //    and before write_outcome_and_seal (scene_ref is hashed).
        //    None = no worktree yet (preflight/policy/cli-probe) — N/A.
        //    A capture failure does NOT change the RunResult (the DR keeps
        //    its meaning); scene_preserved=Some(false) keeps the worktree.
        if let Some(worktree) = worktree_path {
            let scene_preserved = match runner_git::get_changed_paths(worktree, &pkg.base_sha) {
                Ok(changed) => {
                    match attempt_bundle::write_scene_snapshot(
                        bundle_dir,
                        worktree,
                        &pkg.base_sha,
                        &changed,
                        &runner_git::base_tree_files(worktree, &pkg.base_sha)
                            .unwrap_or_default(),
                        &pkg.allowed_paths,
                        &pkg.create_paths,
                        &pkg.forbidden_paths,
                        pkg.expected_changed_files,
                        pkg.warn_changed_files,
                        pkg.max_changed_files as usize,
                    ) {
                        Ok(attempt_bundle::SceneCapture::None) => Some(true),
                        Ok(attempt_bundle::SceneCapture::Complete { dir_ref }) => {
                            manifest.scene_ref = Some(dir_ref);
                            Some(true)
                        }
                        Ok(attempt_bundle::SceneCapture::Partial { dir_ref }) => {
                            log::error!(
                                "Scene capture PARTIAL for {} — oversize untracked files listed but not copied; worktree must be kept",
                                manifest.execution_id
                            );
                            manifest.scene_ref = Some(dir_ref);
                            Some(false)
                        }
                        Err(e) => {
                            log::error!(
                                "Scene capture FAILED for {}: {} — worktree must be kept",
                                manifest.execution_id,
                                e
                            );
                            Some(false)
                        }
                    }
                }
                Err(e) => {
                    log::error!(
                        "Scene capture could not enumerate changes for {}: {} — worktree must be kept",
                        manifest.execution_id,
                        e
                    );
                    Some(false)
                }
            };
            // The write-back itself failing must not break sealing (B2.1):
            // log it; the missing flag makes the poll loop err toward
            // keeping the worktree only when Some(false) was the value.
            if let Some(preserved) = scene_preserved {
                if let Err(err) = execution_store::update_execution(
                    &manifest.execution_id,
                    ExecutionUpdates {
                        scene_preserved: Some(preserved),
                        scene_recovered_from: None,
                        ..Default::default()
                    },
                ) {
                    log::error!(
                        "scene_preserved write-back failed for {}: {err}",
                        manifest.execution_id
                    );
                }
            }
        }

        // 1. Write verifier results FIRST — fail-closed for success-class
        //    outcomes: a pass without its verifier evidence is not a pass.
        let mut evidence_error: Option<String> = None;
        if !verifier_results.is_empty() {
            match attempt_bundle::write_verifier_results(bundle_dir, verifier_results) {
                Ok(Some(ref_ref)) => manifest.verifier_ref = Some(ref_ref),
                Ok(None) => {}
                Err(e) => {
                    log::error!(
                        "Failed to write verifier results for {}: {}",
                        manifest.execution_id,
                        e
                    );
                    evidence_error = Some(e);
                }
            }
        }

        // Success-class outcome with incomplete evidence converts BEFORE the
        // DecisionRequest mapping, so the DR reflects the effective result.
        let result = match (evidence_error, result) {
            (Some(e), RunResult::Passed { execution_id, .. })
            | (Some(e), RunResult::NoDiff { execution_id }) => RunResult::EvidenceSealFailed {
                execution_id,
                error: format!("verifier evidence write failed: {e}"),
            },
            (_, other) => other,
        };

        // 2. Map RunResult to outcome
        let (outcome_str, reason) = match &result {
            RunResult::Passed { .. } => ("passed", None),
            RunResult::NoDiff { .. } => ("no_diff", None),
            RunResult::AcceptanceFailed { reason, .. } => {
                ("acceptance_failed", Some(reason.clone()))
            }
            RunResult::ScopeViolation { violation, .. } => {
                // S3: a count breach is NOT a path-rights breach — the
                // outcomes differ so scene recovery (S2) only ever selects
                // path violations (recovering an over-count scene would just
                // re-trip the breaker).
                if matches!(violation, runner_git::ScopeViolation::TooManyFiles { .. }) {
                    ("scope_count_violation", Some(violation.to_string()))
                } else {
                    ("scope_violation", Some(violation.to_string()))
                }
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
            RunResult::EvidenceSealFailed { error, .. } => {
                ("evidence_seal_failed", Some(error.clone()))
            }
        };

        let outcome = AttemptOutcome {
            outcome: outcome_str.to_string(),
            reason,
            candidate_sha: candidate_sha.map(|s| s.to_string()),
            finished_at: chrono::Utc::now().timestamp() as u64,
        };

        // 3. DecisionRequest from the EFFECTIVE outcome — created and written
        //    into the bundle BEFORE sealing, so decisions.json is covered by
        //    the integrity hash and manifest.decisions_ref actually persists
        //    (writing it after the seal silently invalidated every sealed
        //    bundle that carried a decision).
        let decision = match &result {
            RunResult::BaseShaDrift { .. } => Some((
                "git_drift",
                "Git base changed after the package was sealed. Revise and reseal the package?",
                vec!["revise_package".to_string(), "cancel".to_string()],
            )),
            RunResult::ScopeViolation { violation, .. } => {
                // The RIGHT options depend on the violation class: path-rights
                // breaches are fixed by approving accurate paths in a new
                // revision (revise_scope); forbidden/control-plane breaches
                // can never be fixed by scope (revise_package); count
                // breaches are fixed by a higher max (revise_package).
                let control_plane_or_forbidden = matches!(
                    violation,
                    runner_git::ScopeViolation::ForbiddenPath { .. }
                );
                let count_breach =
                    matches!(violation, runner_git::ScopeViolation::TooManyFiles { .. });
                Some(if control_plane_or_forbidden {
                    (
                        "blocked",
                        "The candidate touched a forbidden/control-plane path — no scope revision can allow this. Revise the package (drop that goal) or cancel.",
                        vec!["revise_package".to_string(), "cancel".to_string()],
                    )
                } else if count_breach {
                    (
                        "blocked",
                        "The candidate changed more files than the last-resort breaker allows. The full scene is captured in the bundle's scene/ directory. Revise the package (raise max) or cancel.",
                        vec!["revise_package".to_string(), "cancel".to_string()],
                    )
                } else {
                    (
                        "blocked",
                        "The candidate changed paths outside the sealed scope. The full scene (diff, untracked files, changed-path classification) is captured in the bundle's scene/ directory; a revised package's first attempt automatically restores it (same base only). Revise scope or cancel?",
                        vec!["revise_scope".to_string(), "cancel".to_string()],
                    )
                })
            }
            RunResult::AcceptanceFailed { .. } => Some((
                "verifier_failed",
                "The exact candidate failed acceptance. Inspect evidence and choose retry, revise, or cancel.",
                vec!["retry".to_string(), "revise_package".to_string(), "cancel".to_string()],
            )),
            RunResult::EvidenceSealFailed { .. } => Some((
                "blocked",
                "The attempt's evidence bundle could not be sealed (disk/FS failure) — the outcome cannot be trusted. Inspect the environment, then retry or revise.",
                vec!["retry".to_string(), "revise_package".to_string(), "cancel".to_string()],
            )),
            RunResult::NoDiff { .. } => {
                if pkg.sandbox == "read-only" {
                    // read-only NoDiff reaches here only after its artifact
                    // contract (acceptance + non-empty report) already ran in
                    // the executor phase — no human decision needed.
                    None
                } else {
                    let exhausted = manifest.budget_used >= pkg.max_attempts.max(1);
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
            }
            // Timeout is budget governance territory: attempts remaining →
            // the poll loop auto-schedules an escalated retry (no human
            // involved); only an exhausted budget pages the human.
            RunResult::Timeout { .. } => {
                let exhausted = manifest.budget_used >= pkg.max_attempts.max(1);
                if exhausted {
                    Some((
                        "retry_exhausted",
                        "The attempt budget is exhausted. Inspect evidence, revise the package, or cancel.",
                        vec!["revise_package".to_string(), "cancel".to_string()],
                    ))
                } else {
                    None
                }
            }
            RunResult::CliFailed { error, .. } => {
                // Network-class failures within budget are auto-retried by
                // the poll loop (same governance as timeouts) — a DR here
                // would page a human for something the system already took
                // over. Genuine errors and exhausted budgets still page.
                if is_network_failure(error)
                    && manifest.budget_used < pkg.max_attempts.max(1)
                {
                    None
                } else {
                    let exhausted = manifest.budget_used >= pkg.max_attempts.max(1);
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
            }
            RunResult::Paused { .. } => Some((
                "approval",
                "Execution was paused. Resume with a new attempt, revise the package, or cancel?",
                vec!["resume".to_string(), "revise_package".to_string(), "cancel".to_string()],
            )),
            RunResult::PolicyViolation { .. } => Some((
                "approval",
                "The package violates the production executor policy. Keep it manual or cancel the execution?",
                vec!["manual".to_string(), "cancel".to_string()],
            )),
            RunResult::PreflightBlocked { .. } => Some((
                "blocked",
                "Runner preflight detected sealed-package, dependency, authority, or Git drift. Revise and reseal before continuing.",
                vec!["revise_package".to_string(), "cancel".to_string()],
            )),
            RunResult::Passed { .. } | RunResult::Cancelled { .. } => None,
        };

        if let Some((kind, question, options)) = decision {
            create_bundle_decision(bundle_dir, manifest, pkg, kind, question, options);
        }

        // 4. Seal LAST: outcome + manifest + integrity hash over every file
        //    written above (verifier results, decisions, …). A seal failure
        //    still converts a success-class outcome — a pass must never rest
        //    on unsealed evidence.
        let result = match attempt_bundle::write_outcome_and_seal(bundle_dir, manifest, &outcome) {
            Ok(()) => result,
            Err(e) => {
                log::error!(
                    "Failed to seal attempt bundle for {}: {} — evidence incomplete",
                    manifest.execution_id,
                    e
                );
                match result {
                    RunResult::Passed { execution_id, .. } | RunResult::NoDiff { execution_id } => {
                        // The converted outcome needs its own DecisionRequest
                        // (the pre-seal decision for a success was None). The
                        // bundle write is best-effort — the directory may be
                        // unwritable; the runtime DR store is the authority.
                        create_bundle_decision(
                            bundle_dir,
                            manifest,
                            pkg,
                            "blocked",
                            "The attempt's evidence bundle could not be sealed (disk/FS failure) — the outcome cannot be trusted. Inspect the environment, then retry or revise.",
                            vec![
                                "retry".to_string(),
                                "revise_package".to_string(),
                                "cancel".to_string(),
                            ],
                        );
                        RunResult::EvidenceSealFailed {
                            execution_id,
                            error: e,
                        }
                    }
                    other => other,
                }
            }
        };

        result
    }

    /// Shadow mode: report what would be picked up without starting any CLI.
    /// Includes sealed packages — they are claimable when their switches are
    /// ON ("switch = authorization").
    pub fn shadow_pick() -> Result<Vec<String>, String> {
        let packages = execution_store::read_ready_packages()?;
        Ok(packages.into_iter().map(|p| p.package_id).collect())
    }

    fn validate_before_claim(pkg: &TaskPackageSummary, budget_used: u32) -> Result<(), String> {
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
        if pkg.timeout_seconds == 0 || pkg.max_attempts == 0 || budget_used >= pkg.max_attempts {
            return Err(format!(
                "invalid or exhausted execution budget: timeout={}, budget={}/{}",
                pkg.timeout_seconds, budget_used, pkg.max_attempts
            ));
        }
        if pkg.package_hash.is_empty() || pkg.package_hash != pkg.computed_hash {
            return Err(format!(
                "sealed package hash mismatch: stored={}, computed={}",
                pkg.package_hash, pkg.computed_hash
            ));
        }
        if pkg.sealed_at.is_empty() {
            return Err("claimable package is missing sealed_at".to_string());
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
        // Drift check against the EFFECTIVE base: HEAD, or the unmerged
        // predecessor candidate this package chains onto. A lineage conflict
        // (multiple unmerged predecessor candidates) fails here too — the
        // Runner must not auto-guess a merge.
        let effective_base = Self::resolve_effective_base(pkg)?;
        if effective_base.sha() != pkg.base_sha {
            return Err(format!(
                "base SHA drift: expected {}, got {}",
                pkg.base_sha,
                effective_base.sha()
            ));
        }

        // Note: no git_clean check on the MAIN workspace. The standard package
        // is an independent carrier; execution happens in a worktree created
        // fresh from base_sha below, which is clean by construction. A dirty
        // main workspace (e.g. an actively-written project) must not block
        // claim. git_clean as a declared precondition is enforced by the
        // candidate-commit stage on the worktree itself if ever needed.

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
        if !Self::dependencies_ready(pkg)? {
            return Err("step dependencies are not yet satisfied".to_string());
        }
        Ok(())
    }

    /// Check whether a package's step dependencies are satisfied — without
    /// consuming an execution. `poll_once` calls this before claiming, so a
    /// sealed/ready package whose dependencies aren't ready yet waits quietly
    /// (keeps its status) instead of being claimed and blocked.
    ///
    /// Rules (previously inlined in validate_before_claim):
    /// - order-predecessors must be at least `coded` (candidate produced)
    /// - explicit `depends_on` must be `observed` (cross-session confirmed)
    /// - step_stage preconditions must be met
    ///
    /// `Ok(false)` = not yet ready, wait. `Err` = hard data problem (epic or
    /// step missing, invalid stage strings) — callers fall through to the
    /// normal claim path so it surfaces as PreflightBlocked.
    fn dependencies_ready(pkg: &TaskPackageSummary) -> Result<bool, String> {
        let project = PathBuf::from(&pkg.project_path)
            .canonicalize()
            .map_err(|e| format!("canonicalize project path: {e}"))?;
        let epics = crate::epic_store::read_epics(&project)?;
        let step = epics
            .steps
            .iter()
            .find(|step| step.id == pkg.step_id && step.epic_id == pkg.epic_id)
            .ok_or_else(|| format!("step '{}' not found in epic", pkg.step_id))?;
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
                    return Ok(false);
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
            // Unified with MCP actionableSteps: EVERY predecessor — order or
            // explicit depends_on — must be observed. Passing acceptance
            // auto-advances a step to observed (B mode), so healthy serial
            // chains flow without human hops; but a step whose predecessor
            // FAILED (stuck at coded or below) never releases its successor.
            let sufficient = dep_stage
                .map(|stage| stage_rank(stage) >= stage_rank(&StepStage::Observed))
                .unwrap_or(false);
            if !sufficient {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Resolve the effective base SHA for a package — the commit its worktree
    /// must start from so it sees all predecessor steps' work.
    ///
    /// For each predecessor step (lower `order` in the same epic, plus
    /// explicit `depends_on`), take its LATEST passed execution's candidate
    /// commit. A candidate already merged into HEAD needs no chaining.
    /// Exactly one unmerged candidate becomes the base; two or more distinct
    /// unmerged candidates are a conflict the Runner must not auto-guess —
    /// surfaced as an error, which the claim path turns into PreflightBlocked
    /// with a DecisionRequest for a human to combine or merge first.
    fn resolve_effective_base(pkg: &TaskPackageSummary) -> Result<EffectiveBase, String> {
        let repo = PathBuf::from(&pkg.repo_root);
        let head = runner_git::read_head_sha(&repo)?;
        let project = PathBuf::from(&pkg.project_path)
            .canonicalize()
            .map_err(|e| format!("canonicalize project path: {e}"))?;
        let epics = crate::epic_store::read_epics(&project)?;
        let step = epics
            .steps
            .iter()
            .find(|s| s.id == pkg.step_id && s.epic_id == pkg.epic_id)
            .ok_or_else(|| format!("step '{}' not found in epic", pkg.step_id))?;
        let predecessor_ids: std::collections::HashSet<&str> = epics
            .steps
            .iter()
            .filter(|candidate| candidate.epic_id == pkg.epic_id && candidate.order < step.order)
            .map(|candidate| candidate.id.as_str())
            .chain(step.depends_on.iter().map(String::as_str))
            .collect();
        if predecessor_ids.is_empty() {
            return Ok(EffectiveBase::Head(head));
        }

        let executions = execution_store::list_executions()?;
        let mut unmerged: Vec<String> = Vec::new();
        for predecessor_id in predecessor_ids {
            let candidate = executions
                .iter()
                .filter(|e| e.step_id == predecessor_id && e.status == ExecutionStatus::Passed)
                .filter_map(|e| e.candidate_sha.clone().map(|sha| (e.created_at, sha)))
                .max_by_key(|(created, _)| *created)
                .map(|(_, sha)| sha);
            if let Some(sha) = candidate {
                if !runner_git::is_ancestor_of_head(&repo, &sha) && !unmerged.contains(&sha) {
                    unmerged.push(sha);
                }
            }
        }
        // Reduce to MAXIMAL candidates by pairwise ancestry. In a linear
        // chain A→B→C, B's candidate descends from A's — only the tip is a
        // valid base (it already contains A's work). Candidates that can
        // reach neither from the other are a true parallel-branch conflict.
        let mut maximal: Vec<String> = Vec::new();
        for sha in unmerged {
            if maximal
                .iter()
                .any(|m| runner_git::is_ancestor(&repo, &sha, m))
            {
                continue; // subsumed by an existing maximal candidate
            }
            maximal.retain(|m| !runner_git::is_ancestor(&repo, m, &sha));
            maximal.push(sha);
        }
        match maximal.len() {
            0 => Ok(EffectiveBase::Head(head)),
            1 => Ok(EffectiveBase::PredecessorCandidate(
                maximal.pop().expect("len checked"),
            )),
            _ => Err(format!(
                "multiple unmerged predecessor candidates ({} parallel branches: {}) — combine them into one candidate or merge to main before continuing",
                maximal.len(),
                maximal
                    .iter()
                    .map(|sha| &sha[..8.min(sha.len())])
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    /// Advance the Step stage in .epics.json based on execution result.
    ///
    /// Mapping:
    ///   candidate commit produced (but acceptance not yet verified) → coded
    ///   acceptance passed → observed (package acceptance IS the gate —
    ///     user-directed B mode; dependent steps release automatically)
    ///   CLI failed / scope violation / no diff → leave stage unchanged
    ///
    /// Deviating outcomes never reach the observed advancement — they end
    /// blocked/failed and surface as DecisionRequests for a human.
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

/// Runner heartbeat: last successful poll (unix seconds), total poll count,
/// and how many packages the last poll processed. Updated on EVERY poll
/// iteration — an empty poll IS the proof of liveness, so the UI can tell
/// "running but idle" apart from "dead". In-memory only: after an App restart
/// the thread is rebuilt from any_project_enabled and the counters reset,
/// which the UI surfaces as "not yet polled".
static LAST_POLL_AT: AtomicU64 = AtomicU64::new(0);
static POLL_COUNT: AtomicU64 = AtomicU64::new(0);
static LAST_PROCESSED: AtomicU32 = AtomicU32::new(0);
static POLL_INTERVAL_SECS: AtomicU64 = AtomicU64::new(10);

/// Live Runner status exposed to the UI via `get_runner_runtime_status`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RunnerRuntimeStatus {
    /// Intent flag: is the Runner supposed to be running?
    pub running: bool,
    /// Is the polling thread actually alive? (false right after a crash)
    pub thread_alive: bool,
    /// Unix seconds of the last poll — or the last in-execution heartbeat
    /// (the thread beats every 10s while synchronously executing a package,
    /// since it can't return to the poll loop); None = never.
    pub last_poll_at: Option<u64>,
    /// Total number of polls since the thread started.
    pub poll_count: u64,
    /// Packages processed by the last poll (0 = idle, alive).
    pub last_processed: u32,
    /// Poll interval in seconds.
    pub poll_interval_secs: u64,
    /// Packages currently in `ready` state waiting for pickup.
    pub ready_packages: usize,
    /// Packages sealed but not enqueued — claimable the moment their project
    /// and epic switches are ON ("switch = authorization").
    pub sealed_packages: usize,
    /// Executions currently in flight (running/candidate/verifying/…).
    pub active_executions: usize,
}

/// Snapshot the Runner heartbeat for the UI.
pub fn runtime_status() -> RunnerRuntimeStatus {
    let last = LAST_POLL_AT.load(Ordering::SeqCst);
    // Live queue counts: packages waiting for pickup (by status) + executions
    // in flight. Read failures degrade to zero — the heartbeat is the liveness
    // signal; the counts are informational for the status indicator.
    let (ready_packages, sealed_packages) =
        crate::execution_store::count_claimable_by_status().unwrap_or((0, 0));
    let active_executions = crate::execution_store::list_executions()
        .map(|execs| {
            execs
                .iter()
                .filter(|e| {
                    matches!(
                        e.status,
                        ExecutionStatus::Running
                            | ExecutionStatus::Candidate
                            | ExecutionStatus::Verifying
                            | ExecutionStatus::CancelRequested
                            | ExecutionStatus::PauseRequested
                    )
                })
                .count()
        })
        .unwrap_or(0);
    RunnerRuntimeStatus {
        running: RUNNER_RUNNING.load(Ordering::SeqCst),
        thread_alive: RUNNER_THREAD_ALIVE.load(Ordering::SeqCst),
        last_poll_at: if last > 0 { Some(last) } else { None },
        poll_count: POLL_COUNT.load(Ordering::SeqCst),
        last_processed: LAST_PROCESSED.load(Ordering::SeqCst),
        poll_interval_secs: POLL_INTERVAL_SECS.load(Ordering::SeqCst),
        ready_packages,
        sealed_packages,
        active_executions,
    }
}

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
    POLL_INTERVAL_SECS.store(interval.as_secs(), Ordering::SeqCst);
    thread::spawn(move || {
        let mut runner = Runner::new(config);
        log::info!("QuestLog Runner started, polling every {:?}", interval);

        while RUNNER_RUNNING.load(Ordering::SeqCst) {
            match runner.poll_once() {
                Ok(n) => {
                    // Heartbeat: every successful poll proves liveness, even
                    // when there was nothing to process.
                    LAST_POLL_AT.store(
                        chrono::Utc::now().timestamp().max(0) as u64,
                        Ordering::SeqCst,
                    );
                    POLL_COUNT.fetch_add(1, Ordering::SeqCst);
                    LAST_PROCESSED.store(n as u32, Ordering::SeqCst);
                    if n > 0 {
                        log::info!("Runner processed {} package(s)", n);
                    }
                }
                Err(e) => {
                    // Poll failure is NOT a heartbeat — the UI will surface
                    // staleness so the user can inspect.
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
    // Read-only artifact contract: confirms the worktree is unchanged vs
    // base (tracked files). Untracked coverage comes from the Runner's own
    // changed-paths check; the real weight is the non-empty report.
    ("verify:readonly", "git diff --quiet HEAD", "."),
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
/// Create a DecisionRequest in the runtime store and mirror it into the
/// attempt bundle (manifest.decisions_ref). Shared by seal_bundle's pre-seal
/// decision path and the post-seal-failure conversion path.
fn create_bundle_decision(
    bundle_dir: &Path,
    manifest: &mut AttemptManifest,
    pkg: &TaskPackageSummary,
    kind: &str,
    question: &str,
    options: Vec<String>,
) {
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
            if let Ok(reference) = attempt_bundle::write_decision_request(bundle_dir, &request) {
                manifest.decisions_ref = reference;
            }
        }
        Err(error) => log::warn!("Failed to create DecisionRequest: {}", error),
    }
}

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

    /// Network-class failure detection: connection/DNS/rate-limit signals
    /// are retryable; genuine code errors are not.
    #[test]
    fn network_failure_detection() {
        assert!(is_network_failure("fetch failed: Connection refused"));
        assert!(is_network_failure("Error: getaddrinfo ENOTFOUND api.example.com"));
        assert!(is_network_failure("request failed: ECONNRESET"));
        assert!(is_network_failure("Network is unreachable"));
        assert!(is_network_failure("429 rate limit exceeded"));
        assert!(is_network_failure("Failed to fetch"));
        assert!(!is_network_failure("exit code 1: cargo test failed"));
        assert!(!is_network_failure("panicked at 'index out of bounds'"));
        assert!(!is_network_failure("syntax error in src/main.rs"));
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
    fn poll_blocked_by_global_kill_switch() {
        let (_dir, _guard) = setup();
        let runtime = std::env::var("QUESTLOG_RUNTIME").unwrap();
        std::fs::create_dir_all(&runtime).unwrap();
        let project = tempfile::tempdir().unwrap();
        let project_path = project.path().to_string_lossy().to_string();
        write_lineage_epics(project.path(), &[("s1", 0, "pending")]);
        write_package_fixture(
            std::path::Path::new(&runtime),
            &project_path,
            "sealed",
            "zcode",
            "s1",
        );
        crate::runner_config::set_project_enabled(&project_path, true).unwrap();
        // Migrate to v2 (project enabled → global auto-ON once), then
        // deliberately kill the switch: v2 + global OFF must hold.
        crate::runner_config::set_globally_enabled(false).unwrap();

        let mut runner = new_runner();
        let count = runner.poll_once().unwrap();
        assert_eq!(count, 0, "global kill switch blocks every claim");
        assert!(execution_store::list_executions().unwrap().is_empty());
        let (_ready, sealed) = execution_store::count_claimable_by_status().unwrap();
        assert_eq!(sealed, 1, "package must stay sealed under the kill switch");
    }

    #[test]
    fn runtime_status_reports_heartbeat_state() {
        let (_dir, _guard) = setup();
        // Other tests in this process may have started a real polling thread
        // that updates the heartbeat — snapshot before, restore after, so the
        // test is order-independent.
        let before = runtime_status();

        // Simulate what the polling loop writes after a successful (idle) poll.
        LAST_POLL_AT.store(1_700_000_000, Ordering::SeqCst);
        POLL_COUNT.store(42, Ordering::SeqCst);
        LAST_PROCESSED.store(0, Ordering::SeqCst);
        let beat = runtime_status();
        assert_eq!(beat.running, RUNNER_RUNNING.load(Ordering::SeqCst));
        assert_eq!(
            beat.thread_alive,
            RUNNER_THREAD_ALIVE.load(Ordering::SeqCst)
        );
        assert_eq!(beat.last_poll_at, Some(1_700_000_000));
        assert_eq!(beat.poll_count, 42);
        assert_eq!(
            beat.last_processed, 0,
            "idle poll → 0 processed, still alive"
        );
        assert_eq!(beat.poll_interval_secs, 10);

        // An empty poll must count as a heartbeat — the UI uses last_poll_at
        // staleness (not processed>0) to tell "idle" from "dead".
        POLL_COUNT.store(43, Ordering::SeqCst);
        assert_eq!(runtime_status().poll_count, 43);

        // Restore the snapshot so concurrent threads' history stays intact.
        LAST_POLL_AT.store(before.last_poll_at.unwrap_or(0), Ordering::SeqCst);
        POLL_COUNT.store(before.poll_count, Ordering::SeqCst);
        LAST_PROCESSED.store(before.last_processed, Ordering::SeqCst);
    }

    #[test]
    fn runtime_status_counts_queue_and_inflight() {
        let (dir, _guard) = setup();
        let runtime = dir.path().join("runtime");
        std::fs::create_dir_all(&runtime).unwrap();

        // packages.json: 2 ready + 1 sealed → ready=2, sealed=1 (split counts
        // feed the UI's queued vs sealed status display).
        // Full RawTaskPackage shape (the ready filter parses every ready
        // package before counting).
        let package = |id: &str, status: &str| {
            serde_json::json!({
                "schema_version": 1,
                "package_id": id,
                "epic_id": "e",
                "step_id": "s",
                "plan_revision": 1,
                "project_path": "/tmp/p",
                "repo_root": "/tmp/p",
                "base_sha": "0".repeat(40),
                "objective": "x",
                "context_refs": [],
                "scope": {"allowed_paths": ["src/**"], "forbidden_paths": [], "max_changed_files": 5},
                "executor": {"preferred": "zcode", "fallbacks": [], "sandbox": "workspace-write", "timeout_seconds": 60, "max_attempts": 2},
                "acceptance": [{"type": "command", "command_id": "verify:candidate"}],
                "preconditions": [],
                "authority": {"auto_edit_worktree": true, "auto_commit_candidate": true, "auto_merge": false, "auto_push": false, "auto_deploy": false},
                "status": status,
                "sealed_at": "2026-08-11T00:00:00Z",
                "package_hash": "h"
            })
        };
        let packages = serde_json::json!({
            "version": 1,
            "packages": [
                package("p1", "ready"),
                package("p2", "ready"),
                package("p3", "sealed")
            ]
        });
        std::fs::write(runtime.join("packages.json"), packages.to_string()).unwrap();

        // executions.json: 1 running + 1 passed → active_executions must be 1.
        let exec = |id: &str, status: &str| {
            serde_json::json!({
                "id": id,
                "package_id": "pkg_x",
                "epic_id": "e",
                "step_id": "s",
                "project_path": "/tmp/p",
                "status": status,
                "lease": null,
                "cli_session": null,
                "worktree_path": null,
                "branch": null,
                "base_sha": "0".repeat(40),
                "candidate_sha": null,
                "attempts": 1,
                "last_error": null,
                "created_at": 1,
                "updated_at": 1,
                "attempt_no": 1,
                "bundle_path": null,
                "executor": null
            })
        };
        let executions = serde_json::json!({
            "version": 1,
            "revision": 0,
            "executions": [exec("e1", "running"), exec("e2", "passed")]
        });
        std::fs::write(runtime.join("executions.json"), executions.to_string()).unwrap();

        let status = runtime_status();
        assert_eq!(status.ready_packages, 2, "ready count stays ready-only");
        assert_eq!(
            status.sealed_packages, 1,
            "sealed packages are counted separately (claimable, not enqueued)"
        );
        assert_eq!(status.active_executions, 1, "only in-flight statuses count");
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
        // Epic authorization is fail-closed: the project must exist on disk
        // with .epics.json carrying auto_execute=true. The repo_root stays
        // bogus so preflight still rejects after the claim.
        let project = tempfile::tempdir().unwrap();
        let project_path = project.path().to_string_lossy().to_string();
        write_lineage_epics(project.path(), &[("s1", 0, "pending")]);
        crate::runner_config::set_project_enabled(&project_path, true).unwrap();
        // Write a fake packages.json with one ready package (nested format matching Node)
        let packages = serde_json::json!({
            "version": 1,
            "packages": [{
                "schema_version": 1,
                "package_id": "pkg_test1",
                "epic_id": "e1",
                "step_id": "s1",
                "plan_revision": 1,
                "project_path": project_path,
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

    /// Minimal .epics.json fixture: one epic (`auto_execute` configurable) and
    /// its steps. Used by the poll-gate tests below.
    fn write_epics_fixture(
        project: &std::path::Path,
        epic_auto_execute: Option<bool>,
        steps: serde_json::Value,
    ) {
        let epics = serde_json::json!({
            "version": 1,
            "project": "gate-test",
            "epics": [{
                "id": "epic_gate",
                "title": "gate test",
                "status": "active",
                "priority": "P1",
                "created_at": "2026-08-11T00:00:00Z",
                "updated_at": "2026-08-11T00:00:00Z",
                "auto_execute": epic_auto_execute
            }],
            "steps": steps
        });
        std::fs::write(
            project.join(".epics.json"),
            serde_json::to_string_pretty(&epics).unwrap(),
        )
        .unwrap();
    }

    /// Minimal sealed/ready package fixture for epic_gate/`step` in `project`.
    fn write_package_fixture(
        runtime: &std::path::Path,
        project: &str,
        status: &str,
        executor: &str,
        step: &str,
    ) {
        let packages = serde_json::json!({
            "version": 1,
            "packages": [{
                "schema_version": 1,
                "package_id": "pkg_gate",
                "epic_id": "epic_gate",
                "step_id": step,
                "plan_revision": 1,
                "project_path": project,
                "repo_root": project,
                "base_sha": "abc",
                "objective": "gate test task",
                "context_refs": [],
                "scope": {
                    "allowed_paths": ["**"],
                    "forbidden_paths": [],
                    "max_changed_files": 10
                },
                "preconditions": [],
                "executor": {
                    "preferred": executor,
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
                "status": status,
                "sealed_at": "2026-08-11T00:00:00Z",
                "package_hash": "fake_hash"
            }]
        });
        std::fs::write(
            runtime.join("packages.json"),
            serde_json::to_string_pretty(&packages).unwrap(),
        )
        .unwrap();
    }

    fn new_runner() -> Runner {
        Runner::new(RunnerConfig {
            enabled: true,
            poll_interval: Duration::from_secs(1),
            runner_id: "test".to_string(),
        })
    }

    #[test]
    fn poll_picks_up_sealed_package_when_switches_allow() {
        let (_dir, _guard) = setup();
        let runtime = std::env::var("QUESTLOG_RUNTIME").unwrap();
        std::fs::create_dir_all(&runtime).unwrap();
        // Fail-closed epic authorization needs a real .epics.json with
        // auto_execute=true; the dir is still not a git repo, so preflight
        // rejects after the claim.
        let project = tempfile::tempdir().unwrap();
        let project_path = project.path().to_string_lossy().to_string();
        crate::runner_config::set_project_enabled(&project_path, true).unwrap();
        write_epics_fixture(
            project.path(),
            Some(true),
            serde_json::json!([{
                "id": "s1", "epic_id": "epic_gate", "title": "first", "order": 0,
                "depends_on": [], "stage": "pending",
                "created_at": "2026-08-11T00:00:00Z",
                "updated_at": "2026-08-11T00:00:00Z"
            }]),
        );
        write_package_fixture(
            std::path::Path::new(&runtime),
            &project_path,
            "sealed",
            "zcode",
            "s1",
        );

        // A sealed package must flow through the same claim path as ready.
        // (Preflight then fails because the project is not a repo — but the
        // package must be CLAIMED, not filtered.)
        let mut runner = new_runner();
        let count = runner.poll_once().unwrap();
        assert_eq!(
            count, 1,
            "sealed package must be claimed when switches allow"
        );
        let remaining = execution_store::read_ready_packages().unwrap();
        assert!(remaining.is_empty(), "claimed package must be consumed");
    }

    #[test]
    fn reanchor_package_rebases_to_new_head() {
        use execution_store::reanchor_package;

        let (dir, _guard) = setup();
        let runtime = std::env::var("QUESTLOG_RUNTIME").unwrap();
        std::fs::create_dir_all(&runtime).unwrap();
        let project = dir.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();

        // Sealed package at a stale base + step summary pointing at it
        write_package_fixture(
            std::path::Path::new(&runtime),
            project.to_str().unwrap(),
            "sealed",
            "zcode",
            "s1",
        );
        write_epics_fixture(
            &project,
            Some(true),
            serde_json::json!([{
                "id": "s1", "epic_id": "epic_gate", "title": "t", "order": 0,
                "depends_on": [], "stage": "pending",
                "created_at": "2026-08-11T00:00:00Z",
                "updated_at": "2026-08-11T00:00:00Z",
                "package": {
                    "package_id": "pkg_gate",
                    "plan_revision": 1,
                    "package_hash": "fake_hash",
                    "objective": "gate test task",
                    "executor": "zcode",
                    "status": "sealed"
                }
            }]),
        );

        let summary =
            reanchor_package("pkg_gate", "newhead123", project.to_str().unwrap(), "s1").unwrap();
        assert_eq!(summary.base_sha, "newhead123");
        assert_eq!(summary.plan_revision, 2);
        assert_eq!(summary.package_hash, summary.computed_hash);

        // Old row superseded, exactly one claimable row remains
        let raw =
            std::fs::read_to_string(std::path::Path::new(&runtime).join("packages.json")).unwrap();
        let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let rows: Vec<&serde_json::Value> = json["packages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["package_id"].as_str() == Some("pkg_gate"))
            .collect();
        assert_eq!(rows.len(), 2, "append-only: old row + new revision");
        assert_eq!(rows[0]["status"].as_str(), Some("superseded"));
        assert_eq!(rows[1]["status"].as_str(), Some("sealed"));
        assert_eq!(rows[1]["base_sha"].as_str(), Some("newhead123"));

        // Step summary kept in sync
        let epics = crate::epic_store::read_epics(&project).unwrap();
        let step = epics.steps.iter().find(|s| s.id == "s1").unwrap();
        let pkg = step.package.as_ref().unwrap();
        assert_eq!(pkg.plan_revision, 2);
        assert_eq!(pkg.package_hash, summary.package_hash);

        // Claimable set reflects the re-anchored revision
        let ready = execution_store::read_ready_packages().unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].base_sha, "newhead123");
    }

    #[test]
    fn reanchor_refuses_packages_with_history_or_terminal_status() {
        let (dir, _guard) = setup();
        let runtime = std::env::var("QUESTLOG_RUNTIME").unwrap();
        std::fs::create_dir_all(&runtime).unwrap();
        let project = dir.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        write_package_fixture(
            std::path::Path::new(&runtime),
            project.to_str().unwrap(),
            "blocked",
            "zcode",
            "s1",
        );
        let err =
            execution_store::reanchor_package("pkg_gate", "x", project.to_str().unwrap(), "s1")
                .unwrap_err();
        assert!(err.contains("only sealed/ready"), "got: {err}");
    }

    #[test]
    fn poll_skips_manual_executor_package_quietly() {
        let (_dir, _guard) = setup();
        let runtime = std::env::var("QUESTLOG_RUNTIME").unwrap();
        std::fs::create_dir_all(&runtime).unwrap();
        let project = tempfile::tempdir().unwrap();
        let project_path = project.path().to_string_lossy().to_string();
        crate::runner_config::set_project_enabled(&project_path, true).unwrap();
        write_epics_fixture(
            project.path(),
            Some(true),
            serde_json::json!([{
                "id": "s1", "epic_id": "epic_gate", "title": "manual step", "order": 0,
                "depends_on": [], "stage": "pending",
                "created_at": "2026-08-11T00:00:00Z",
                "updated_at": "2026-08-11T00:00:00Z"
            }]),
        );
        write_package_fixture(
            std::path::Path::new(&runtime),
            &project_path,
            "sealed",
            "manual",
            "s1",
        );

        let mut runner = new_runner();
        let count = runner.poll_once().unwrap();
        assert_eq!(count, 0, "manual packages are tracking-only, never claimed");
        let (ready, sealed) = execution_store::count_claimable_by_status().unwrap();
        assert_eq!((ready, sealed), (0, 1), "package must stay sealed");
        assert!(
            execution_store::list_executions().unwrap().is_empty(),
            "no execution may be created for a manual package"
        );
    }

    #[test]
    fn poll_skips_unmet_dependency_and_waits_quietly() {
        let (_dir, _guard) = setup();
        let runtime = std::env::var("QUESTLOG_RUNTIME").unwrap();
        std::fs::create_dir_all(&runtime).unwrap();
        let project = tempfile::tempdir().unwrap();
        let project_path = project.path().to_string_lossy().to_string();
        crate::runner_config::set_project_enabled(&project_path, true).unwrap();
        let steps_with = |s1_stage: &str| {
            serde_json::json!([
                {"id": "s1", "epic_id": "epic_gate", "title": "first", "order": 0,
                 "depends_on": [], "stage": s1_stage,
                 "created_at": "2026-08-11T00:00:00Z",
                 "updated_at": "2026-08-11T00:00:00Z"},
                {"id": "s2", "epic_id": "epic_gate", "title": "second", "order": 1,
                 "depends_on": [], "stage": "pending",
                 "created_at": "2026-08-11T00:00:00Z",
                 "updated_at": "2026-08-11T00:00:00Z"}
            ])
        };
        write_epics_fixture(project.path(), Some(true), steps_with("pending"));
        write_package_fixture(
            std::path::Path::new(&runtime),
            &project_path,
            "sealed",
            "zcode",
            "s2",
        );

        // s2's order-predecessor s1 is pending (< coded) → wait quietly,
        // keeping the sealed status instead of being claimed and blocked.
        let mut runner = new_runner();
        let count = runner.poll_once().unwrap();
        assert_eq!(count, 0, "dependency-unready package must not be claimed");
        let (ready, sealed) = execution_store::count_claimable_by_status().unwrap();
        assert_eq!(
            (ready, sealed),
            (0, 1),
            "package must stay sealed while waiting"
        );
        assert!(execution_store::list_executions().unwrap().is_empty());

        // Advance s1 to observed → the next poll claims s2 (consumed via
        // preflight-block because the fixture project is not a real repo).
        // (Unified semantics: order-predecessors must be OBSERVED — coded is
        // no longer enough, a failed-acceptance step must not release its
        // successor.)
        write_epics_fixture(project.path(), Some(true), steps_with("observed"));
        let count = runner.poll_once().unwrap();
        assert_eq!(count, 1, "satisfied dependency must release the package");
        assert!(execution_store::read_ready_packages().unwrap().is_empty());
    }

    #[test]
    fn poll_skips_epic_without_explicit_auto_execute() {
        let (_dir, _guard) = setup();
        let runtime = std::env::var("QUESTLOG_RUNTIME").unwrap();
        std::fs::create_dir_all(&runtime).unwrap();
        let project = tempfile::tempdir().unwrap();
        let project_path = project.path().to_string_lossy().to_string();
        crate::runner_config::set_project_enabled(&project_path, true).unwrap();
        // Epic has NO auto_execute field → fail-closed: the project switch
        // alone must not release sealed packages (authorization is epic-level).
        write_epics_fixture(
            project.path(),
            None,
            serde_json::json!([{
                "id": "s1", "epic_id": "epic_gate", "title": "waiting", "order": 0,
                "depends_on": [], "stage": "pending",
                "created_at": "2026-08-11T00:00:00Z",
                "updated_at": "2026-08-11T00:00:00Z"
            }]),
        );
        write_package_fixture(
            std::path::Path::new(&runtime),
            &project_path,
            "sealed",
            "zcode",
            "s1",
        );

        let mut runner = new_runner();
        let count = runner.poll_once().unwrap();
        assert_eq!(
            count, 0,
            "epic without explicit auto_execute=true must not release packages"
        );
        let (ready, sealed) = execution_store::count_claimable_by_status().unwrap();
        assert_eq!((ready, sealed), (0, 1));
        assert!(execution_store::list_executions().unwrap().is_empty());
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
            display_no: None,
            allowed_paths: vec!["src/**".to_string()],
            create_paths: vec![],
            forbidden_paths: vec![],
            max_changed_files: 1,
            expected_changed_files: None,
            warn_changed_files: None,
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

    /// S1: seal_bundle with a live worktree captures the scene into the
    /// bundle AND writes scene_preserved back onto the execution row — the
    /// two facts the worktree-cleanup gate relies on.
    #[test]
    fn seal_bundle_captures_scene_and_flags_execution() {
        let _guard = crate::execution_store::TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("QUESTLOG_RUNTIME", dir.path());
        std::fs::create_dir_all(crate::execution_store::runtime_dir()).unwrap();

        // A real repo with a change outside the sealed scope (src/** only).
        let repo = tempfile::tempdir().unwrap();
        let repo_path = repo.path();
        git(repo_path, &["init", "-q"]);
        std::fs::write(repo_path.join("README.md"), "base\n").unwrap();
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
        std::fs::write(repo_path.join("outside.txt"), "out of scope change\n").unwrap();
        std::fs::write(repo_path.join("README.md"), "tracked edit too\n").unwrap();

        let exec_id = crate::execution_store::enqueue_execution(
            "pkg_seal",
            "e1",
            "s1",
            "/tmp/proj",
            &base_sha,
        )
        .unwrap();

        let runner = Runner::new(RunnerConfig::default());
        let mut pkg = seal_test_pkg();
        pkg.base_sha = base_sha;
        let mut manifest =
            AttemptManifest::new(&exec_id, 1, 0, None, &pkg);
        let bundle_dir = tempfile::tempdir().unwrap();

        let result = runner.seal_bundle(
            bundle_dir.path(),
            &mut manifest,
            &pkg,
            RunResult::ScopeViolation {
                execution_id: exec_id.clone(),
                violation: runner_git::ScopeViolation::UnauthorizedCreate {
                    path: "outside.txt".to_string(),
                    create: pkg.allowed_paths.clone(),
                },
            },
            None,
            &[],
            Some(repo_path),
        );
        match result {
            RunResult::ScopeViolation { .. } => {}
            other => panic!("expected ScopeViolation, got {other:?}"),
        }

        // Scene captured and referenced in the (sealed) manifest.
        assert_eq!(manifest.scene_ref.as_deref(), Some("scene"));
        assert!(bundle_dir.path().join("scene/diff-full.patch").exists());
        assert!(bundle_dir
            .path()
            .join("scene/untracked/outside.txt")
            .exists());
        let classes =
            std::fs::read_to_string(bundle_dir.path().join("scene/changed-paths.json"))
                .unwrap();
        assert!(classes.contains("unauthorized_create"), "classes: {classes}");
        // Execution row flagged preserved → the cleanup gate may clear the
        // worktree (evidence is complete).
        let exec = crate::execution_store::get_execution(&exec_id).unwrap();
        assert_eq!(exec.scene_preserved, Some(true));
    }

    fn seal_test_pkg() -> TaskPackageSummary {
        TaskPackageSummary {
            package_id: "pkg_seal".to_string(),
            epic_id: "e1".to_string(),
            step_id: "s1".to_string(),
            plan_revision: 1,
            project_path: "/tmp/proj".to_string(),
            repo_root: "/tmp/proj".to_string(),
            base_sha: "abc".to_string(),
            objective: "test".to_string(),
            display_no: None,
            allowed_paths: vec!["src/**".to_string()],
            create_paths: vec![],
            forbidden_paths: vec![],
            max_changed_files: 1,
            expected_changed_files: None,
            warn_changed_files: None,
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
            package_hash: "h".to_string(),
            computed_hash: "h".to_string(),
        }
    }

    #[test]
    fn seal_failure_converts_success_to_evidence_seal_failed() {
        let (dir, _guard) = setup();
        std::fs::create_dir_all(dir.path().join("runtime")).unwrap();
        let runner = Runner::new(RunnerConfig::default());
        let pkg = seal_test_pkg();
        let mut manifest = AttemptManifest::new("exec_sealfail", 1, 0, None, &pkg);
        let bundle_dir = tempfile::tempdir().unwrap();

        // Read-only bundle dir → write_outcome_and_seal must fail.
        let mut perms = std::fs::metadata(bundle_dir.path()).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(bundle_dir.path(), perms).unwrap();

        let result = runner.seal_bundle(
            bundle_dir.path(),
            &mut manifest,
            &pkg,
            RunResult::Passed {
                execution_id: "exec_sealfail".to_string(),
                candidate_sha: "abc".to_string(),
            },
            Some("abc"),
            &[],
            None,
        );

        // Restore writability so the tempdir can be cleaned up.
        let mut perms = std::fs::metadata(bundle_dir.path()).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(bundle_dir.path(), perms).unwrap();

        assert!(
            matches!(result, RunResult::EvidenceSealFailed { .. }),
            "unsealed success must convert to EvidenceSealFailed"
        );
        // Fail-closed also means: a human is paged via DecisionRequest.
        let reqs = crate::decision_store::list_requests(false).unwrap();
        assert!(
            reqs.iter().any(|r| r.execution_id == "exec_sealfail"),
            "seal failure must create a DecisionRequest"
        );
    }

    #[test]
    fn seal_failure_keeps_failure_outcomes_unchanged() {
        let (_dir, _guard) = setup();
        let runner = Runner::new(RunnerConfig::default());
        let pkg = seal_test_pkg();
        let mut manifest = AttemptManifest::new("exec_sealfail2", 1, 0, None, &pkg);
        let bundle_dir = tempfile::tempdir().unwrap();

        let mut perms = std::fs::metadata(bundle_dir.path()).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(bundle_dir.path(), perms).unwrap();

        let result = runner.seal_bundle(
            bundle_dir.path(),
            &mut manifest,
            &pkg,
            RunResult::Timeout {
                execution_id: "exec_sealfail2".to_string(),
            },
            None,
            &[],
            None,
        );

        let mut perms = std::fs::metadata(bundle_dir.path()).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(bundle_dir.path(), perms).unwrap();

        // Already a deviation — passes through (its own DR logic applies).
        assert!(matches!(result, RunResult::Timeout { .. }));
    }

    #[test]
    fn successful_seal_passes_result_through() {
        let (_dir, _guard) = setup();
        let runner = Runner::new(RunnerConfig::default());
        let pkg = seal_test_pkg();
        let mut manifest = AttemptManifest::new("exec_sealok", 1, 0, None, &pkg);
        let bundle_dir = tempfile::tempdir().unwrap();

        let result = runner.seal_bundle(
            bundle_dir.path(),
            &mut manifest,
            &pkg,
            RunResult::Passed {
                execution_id: "exec_sealok".to_string(),
                candidate_sha: "abc".to_string(),
            },
            Some("abc"),
            &[],
            None,
        );
        assert!(matches!(result, RunResult::Passed { .. }));
        // Sealed bundle carries an integrity hash.
        assert!(manifest.integrity_hash.is_some());
    }

    /// Minimal .epics.json with one active epic and the given steps
    /// (id, order, stage). Step ids double as predecessor references.
    fn write_lineage_epics(repo: &std::path::Path, steps: &[(&str, u32, &str)]) {
        let steps_json: Vec<serde_json::Value> = steps
            .iter()
            .map(|(id, order, stage)| {
                serde_json::json!({
                    "id": id,
                    "epic_id": "e1",
                    "title": id,
                    "order": order,
                    "depends_on": [],
                    "stage": stage,
                    "created_at": "2026-08-11T00:00:00Z",
                    "updated_at": "2026-08-11T00:00:00Z"
                })
            })
            .collect();
        let epics = serde_json::json!({
            "version": 1,
            "project": "lineage-test",
            "epics": [{
                "id": "e1",
                "title": "lineage test",
                "status": "active",
                "priority": "P1",
                // Poll-gate tests reuse this fixture: explicit authorization.
                "auto_execute": true,
                "created_at": "2026-08-11T00:00:00Z",
                "updated_at": "2026-08-11T00:00:00Z"
            }],
            "steps": steps_json
        });
        std::fs::write(
            repo.join(".epics.json"),
            serde_json::to_string_pretty(&epics).unwrap(),
        )
        .unwrap();
    }

    /// A passed-execution row: step `step_id` produced candidate `sha`.
    fn write_passed_execution(step_id: &str, sha: &str, created_at: u64) {
        let exec = serde_json::json!({
            "id": format!("exec_{}_{}", step_id, created_at),
            "package_id": format!("pkg_{}", step_id),
            "epic_id": "e1",
            "step_id": step_id,
            "project_path": "/tmp/unused",
            "status": "passed",
            "lease": null,
            "cli_session": null,
            "worktree_path": null,
            "branch": null,
            "base_sha": "whatever",
            "candidate_sha": sha,
            "attempts": 1,
            "last_error": null,
            "created_at": created_at,
            "updated_at": created_at
        });
        let data = serde_json::json!({
            "version": 1,
            "revision": 1,
            "executions": [exec]
        });
        std::fs::write(
            crate::execution_store::runtime_dir().join("executions.json"),
            serde_json::to_string_pretty(&data).unwrap(),
        )
        .unwrap();
    }

    fn lineage_pkg(repo: &std::path::Path, step_id: &str, base_sha: &str) -> TaskPackageSummary {
        let repo_path = repo.to_string_lossy().to_string();
        TaskPackageSummary {
            package_id: format!("pkg_{step_id}"),
            epic_id: "e1".to_string(),
            step_id: step_id.to_string(),
            plan_revision: 1,
            project_path: repo_path.clone(),
            repo_root: repo_path,
            base_sha: base_sha.to_string(),
            objective: "test".to_string(),
            display_no: None,
            allowed_paths: vec!["src/**".to_string()],
            create_paths: vec![],
            forbidden_paths: vec![],
            max_changed_files: 5,
            expected_changed_files: None,
            warn_changed_files: None,
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
            package_hash: "h".to_string(),
            computed_hash: "h".to_string(),
        }
    }

    /// Create an unmerged side commit on top of the repo's current HEAD and
    /// return its SHA (HEAD stays on the original branch).
    fn make_unmerged_candidate(repo: &std::path::Path, name: &str) -> String {
        let current = git(repo, &["branch", "--show-current"]);
        git(repo, &["checkout", "-q", "-b", name]);
        std::fs::write(repo.join(format!("{name}.txt")), name).unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-q", "-m", name]);
        let sha = git(repo, &["rev-parse", "HEAD"]);
        git(repo, &["checkout", "-q", &current]);
        sha
    }

    /// Create an unmerged side commit on top of an ARBITRARY base (e.g. a
    /// predecessor's candidate — the linear-chain shape).
    fn make_candidate_on(repo: &std::path::Path, name: &str, base: &str) -> String {
        let current = git(repo, &["branch", "--show-current"]);
        git(repo, &["checkout", "-q", "-b", name, base]);
        std::fs::write(repo.join(format!("{name}.txt")), name).unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-q", "-m", name]);
        let sha = git(repo, &["rev-parse", "HEAD"]);
        git(repo, &["checkout", "-q", &current]);
        sha
    }

    /// Append a second passed-execution row to the executions fixture.
    fn append_passed_execution(step_id: &str, sha: &str, created_at: u64) {
        let exec = serde_json::json!({
            "id": format!("exec_{}_{}", step_id, created_at),
            "package_id": format!("pkg_{}", step_id),
            "epic_id": "e1",
            "step_id": step_id,
            "project_path": "/tmp/unused",
            "status": "passed",
            "lease": null,
            "cli_session": null,
            "worktree_path": null,
            "branch": null,
            "base_sha": "whatever",
            "candidate_sha": sha,
            "attempts": 1,
            "last_error": null,
            "created_at": created_at,
            "updated_at": created_at
        });
        let path = crate::execution_store::runtime_dir().join("executions.json");
        let mut data: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        data["executions"].as_array_mut().unwrap().push(exec);
        std::fs::write(&path, serde_json::to_string_pretty(&data).unwrap()).unwrap();
    }

    #[test]
    fn effective_base_linear_chain_uses_tip_candidate() {
        // A→B→C, nothing merged: B's candidate descends from A's, so the
        // only valid base for C is the TIP (C2) — not a conflict.
        let (dir, _guard) = setup();
        std::fs::create_dir_all(dir.path().join("runtime")).unwrap();
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path();
        let head = init_repo(repo);
        let c1 = make_unmerged_candidate(repo, "c1");
        let c2 = make_candidate_on(repo, "c2", &c1);
        write_lineage_epics(
            repo,
            &[
                ("s1", 0, "observed"),
                ("s2", 1, "observed"),
                ("s3", 2, "pending"),
            ],
        );
        write_passed_execution("s1", &c1, 100);
        append_passed_execution("s2", &c2, 200);

        let pkg = lineage_pkg(repo, "s3", &head);
        let base = Runner::resolve_effective_base(&pkg).unwrap();
        assert!(
            matches!(base, EffectiveBase::PredecessorCandidate(_)),
            "linear chain must resolve to the tip candidate, not a conflict"
        );
        assert_eq!(base.sha(), c2, "tip candidate C2 already contains C1");
    }

    #[test]
    fn effective_base_chains_to_unmerged_predecessor_candidate() {
        let (dir, _guard) = setup();
        std::fs::create_dir_all(dir.path().join("runtime")).unwrap();
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path();
        let head = init_repo(repo);
        let candidate = make_unmerged_candidate(repo, "s1cand");
        write_lineage_epics(repo, &[("s1", 0, "observed"), ("s2", 1, "pending")]);
        write_passed_execution("s1", &candidate, 100);

        let pkg = lineage_pkg(repo, "s2", &head);
        let base = Runner::resolve_effective_base(&pkg).unwrap();
        assert!(
            matches!(base, EffectiveBase::PredecessorCandidate(_)),
            "unmerged predecessor candidate must become the successor's base"
        );
        assert_eq!(base.sha(), candidate);
    }

    #[test]
    fn effective_base_returns_head_once_predecessor_merged() {
        let (dir, _guard) = setup();
        std::fs::create_dir_all(dir.path().join("runtime")).unwrap();
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path();
        let head = init_repo(repo);
        let candidate = make_unmerged_candidate(repo, "s1cand");
        // Merge the candidate into the original branch — it becomes an
        // ancestor of HEAD, so no chaining is needed.
        git(repo, &["merge", "-q", "--no-ff", "s1cand", "-m", "merge"]);
        let head_after = git(repo, &["rev-parse", "HEAD"]);
        assert_ne!(head, head_after);
        write_lineage_epics(repo, &[("s1", 0, "observed"), ("s2", 1, "pending")]);
        write_passed_execution("s1", &candidate, 100);

        let pkg = lineage_pkg(repo, "s2", &head_after);
        let base = Runner::resolve_effective_base(&pkg).unwrap();
        assert!(matches!(base, EffectiveBase::Head(_)));
        assert_eq!(base.sha(), head_after);
    }

    #[test]
    fn effective_base_conflict_on_multiple_unmerged_candidates() {
        let (dir, _guard) = setup();
        std::fs::create_dir_all(dir.path().join("runtime")).unwrap();
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path();
        let head = init_repo(repo);
        let cand1 = make_unmerged_candidate(repo, "c1");
        let cand2 = make_unmerged_candidate(repo, "c2");
        write_lineage_epics(
            repo,
            &[
                ("s1", 0, "observed"),
                ("s2", 1, "observed"),
                ("s3", 2, "pending"),
            ],
        );
        write_passed_execution("s1", &cand1, 100);
        // A second passed execution for the other predecessor step.
        let exec2 = serde_json::json!({
            "id": "exec_s2_200", "package_id": "pkg_s2", "epic_id": "e1",
            "step_id": "s2", "project_path": "/tmp/unused", "status": "passed",
            "lease": null, "cli_session": null, "worktree_path": null, "branch": null,
            "base_sha": "whatever", "candidate_sha": cand2, "attempts": 1,
            "last_error": null, "created_at": 200, "updated_at": 200
        });
        let path = crate::execution_store::runtime_dir().join("executions.json");
        let mut data: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        data["executions"].as_array_mut().unwrap().push(exec2);
        std::fs::write(&path, serde_json::to_string_pretty(&data).unwrap()).unwrap();

        let pkg = lineage_pkg(repo, "s3", &head);
        let error = Runner::resolve_effective_base(&pkg).unwrap_err();
        assert!(
            error.contains("multiple unmerged predecessor candidates"),
            "conflict must refuse auto-guessing, got: {error}"
        );
    }

    #[test]
    fn dependencies_require_observed_not_just_coded() {
        let (dir, _guard) = setup();
        std::fs::create_dir_all(dir.path().join("runtime")).unwrap();
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path();
        let head = init_repo(repo);
        // Predecessor whose acceptance FAILED mid-chain → stuck at coded.
        write_lineage_epics(repo, &[("s1", 0, "coded"), ("s2", 1, "pending")]);
        let pkg = lineage_pkg(repo, "s2", &head);
        assert!(
            !Runner::dependencies_ready(&pkg).unwrap(),
            "a coded (failed-acceptance) predecessor must not release the successor"
        );
        // Acceptance auto-advance / human confirmation to observed → released.
        write_lineage_epics(repo, &[("s1", 0, "observed"), ("s2", 1, "pending")]);
        assert!(Runner::dependencies_ready(&pkg).unwrap());
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

        // Lineage resolution reads the epics store: single step, no
        // predecessors → effective base is HEAD → the drift check fires.
        write_lineage_epics(repo, &[("s1", 0, "pending")]);

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
            display_no: None,
            allowed_paths: vec!["src/**".to_string()],
            create_paths: vec![],
            forbidden_paths: vec![],
            max_changed_files: 5,
            expected_changed_files: None,
            warn_changed_files: None,
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
            display_no: None,
            allowed_paths: vec!["src/**".to_string()],
            create_paths: vec![],
            forbidden_paths: vec![],
            max_changed_files: 5,
            expected_changed_files: None,
            warn_changed_files: None,
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

    #[test]
    fn effective_timeout_escalates_per_attempt_with_cap() {
        // G2: attempt 1 keeps the base budget; each retry doubles it; 7200 cap.
        assert_eq!(Runner::effective_timeout_secs(1800, 1), 1800);
        assert_eq!(Runner::effective_timeout_secs(1800, 2), 3600);
        assert_eq!(Runner::effective_timeout_secs(1800, 3), 7200);
        assert_eq!(Runner::effective_timeout_secs(1800, 9), 7200);
        // Large bases clamp immediately.
        assert_eq!(Runner::effective_timeout_secs(9000, 1), 7200);
        assert_eq!(Runner::effective_timeout_secs(0, 1), 0);
    }

    #[test]
    fn create_retry_attempt_is_idempotent_per_parent() {
        // G4: auto-retry and a human resolve("retry") can race — the second
        // caller must get the SAME child id, not a duplicate queued execution.
        let (_dir, _guard) = setup();
        let runtime = std::env::var("QUESTLOG_RUNTIME").unwrap();
        std::fs::create_dir_all(&runtime).unwrap();
        // create_retry_attempt reads the package (max_attempts) — provide it.
        let packages = serde_json::json!({
            "version": 1,
            "packages": [{
                "schema_version": 1,
                "package_id": "pkg-r",
                "epic_id": "e", "step_id": "s",
                "plan_revision": 1,
                "project_path": "/tmp/p", "repo_root": "/tmp/p", "base_sha": "abc",
                "objective": "t", "context_refs": [],
                "scope": {"allowed_paths": ["**"], "forbidden_paths": [], "max_changed_files": 5},
                "preconditions": [],
                "executor": {"preferred": "zcode", "fallbacks": [], "sandbox": "workspace-write", "timeout_seconds": 60, "max_attempts": 3},
                "acceptance": [], "authority": {"auto_edit_worktree": true, "auto_commit_candidate": true, "auto_merge": false, "auto_push": false, "auto_deploy": false},
                "status": "ready",
                "sealed_at": "2026-08-15T00:00:00Z", "package_hash": "h"
            }]
        });
        std::fs::write(
            std::path::Path::new(&runtime).join("packages.json"),
            packages.to_string(),
        )
        .unwrap();
        execution_store::enqueue_execution("pkg-r", "e", "s", "/tmp/p", "abc").unwrap();
        // Bring the parent to a retryable terminal state.
        let execs = execution_store::list_executions().unwrap();
        let parent = execs.iter().find(|e| e.package_id == "pkg-r").unwrap();
        execution_store::acquire_lease(&parent.id, "t").unwrap();
        execution_store::release_lease(&parent.id, ExecutionStatus::TimedOut, Some("timeout"))
            .unwrap();

        let first = execution_store::create_retry_attempt(&parent.id).unwrap();
        let second = execution_store::create_retry_attempt(&parent.id).unwrap();
        assert_eq!(
            first, second,
            "duplicate retry must return the existing child"
        );

        // And the queue holds exactly one queued child for this parent.
        let execs = execution_store::list_executions().unwrap();
        let queued = execs
            .iter()
            .filter(|e| {
                e.parent_attempt_id.as_deref() == Some(parent.id.as_str())
                    && e.status == ExecutionStatus::Queued
            })
            .count();
        assert_eq!(queued, 1);
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
