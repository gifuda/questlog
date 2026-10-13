//! Executor interface and the ZCode CLI adapter.
//!
//! The executor trait defines a uniform interface for running AI CLI tools
//! in a controlled worktree. The Runner uses this interface to start,
//! stream events, resume, and cancel executions.
//!
//! ZCode adapter:
//! - Implements the same Executor trait
//! - Probes for the ZCode CLI binary and its exec mode
//! - On machines where ZCode is only a GUI app (no standalone CLI),
//!   probe() accurately reports unavailable (never fakes availability)
//! - When a CLI becomes available, the adapter works without changes
//!
//! History: a Codex CLI adapter existed here (codex exec --json, JSONL
//! event parsing, forbidden-param screening). It was removed in the S2
//! audit — production is hard-gated to the ZCode executor (runner.rs
//! rejects any non-"zcode" package executor), and 447 historical sealed
//! packages contain zero codex executors. The "codex" value remains legal
//! in the TS task-package type union for MCP-sealed manual packages.

pub mod zcode;
pub use zcode::ZCodeExecutor;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Result of probing for an executor.
#[derive(Debug, Clone, Serialize)]
pub struct ExecutorProbe {
    pub available: bool,
    pub version: Option<String>,
    pub path: Option<String>,
    pub supports_resume: bool,
    pub supports_json: bool,
}

/// Result of starting an execution.
pub struct ExecutionHandle {
    pub child: Child,
    pub session_id: Option<String>,
    pub stdout_path: PathBuf,
    pub stderr_path: PathBuf,
}

/// A parsed JSONL event from the executor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutorEvent {
    pub event_type: String,
    pub session_id: Option<String>,
    pub message: Option<String>,
    pub success: Option<bool>,
    pub raw: serde_json::Value,
}

// ─── Unified trait ───

pub trait Executor {
    /// Probe for the CLI's existence and capabilities.
    fn probe(&self) -> ExecutorProbe;

    /// Start a non-interactive execution in the given worktree.
    fn start(
        &mut self,
        task: &str,
        worktree_path: &Path,
        sandbox: &str,
        timeout: Duration,
    ) -> Result<ExecutionHandle, String>;

    /// Start a continuation of a previous CLI session (timeout fallback):
    /// resume the session's context instead of re-running the analysis from
    /// scratch. Default: no resume support — delegates to `start`.
    fn start_with_resume(
        &mut self,
        task: &str,
        session_id: &str,
        worktree_path: &Path,
        sandbox: &str,
        timeout: Duration,
    ) -> Result<ExecutionHandle, String> {
        let _ = session_id;
        self.start(task, worktree_path, sandbox, timeout)
    }

    /// Wait for the execution to complete and parse events.
    fn wait_and_collect(&mut self, handle: ExecutionHandle) -> Result<Vec<ExecutorEvent>, String>;

    /// Cancel a running execution.
    fn cancel(&mut self, handle: &mut ExecutionHandle) -> Result<(), String>;
}

/// Terminate the whole executor process group so child tool processes do not
/// survive a pause, cancellation, or timeout. Falls back to killing the direct
/// child when process-group signalling is unavailable.
pub fn terminate_process_tree(handle: &mut ExecutionHandle) -> Result<(), String> {
    #[cfg(unix)]
    {
        let group = format!("-{}", handle.child.id());
        let _ = Command::new("kill")
            .args(["-TERM", &group])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        std::thread::sleep(Duration::from_millis(150));
        if handle.child.try_wait().ok().flatten().is_none() {
            let _ = Command::new("kill")
                .args(["-KILL", &group])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
    handle
        .child
        .kill()
        .or_else(|e| {
            if handle.child.try_wait().ok().flatten().is_some() {
                Ok(())
            } else {
                Err(e)
            }
        })
        .map_err(|e| format!("kill process tree failed: {e}"))?;

    // Reap the child and confirm the kill was effective. The group was already
    // TERM→KILLed, so this never fails hard: a timeout only means the child's
    // exit hasn't been observed yet, which we surface as a warning.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        if handle.child.try_wait().ok().flatten().is_some() {
            break; // reaped — the kill was effective
        }
        if std::time::Instant::now() >= deadline {
            log::warn!(
                "terminate_process_tree: child {} did not exit within 2s of KILL",
                handle.child.id()
            );
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}
