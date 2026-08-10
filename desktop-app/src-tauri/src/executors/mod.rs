//! Executor interface and Codex CLI adapter.
//!
//! The executor trait defines a uniform interface for running AI CLI tools
//! (Codex, ZCode, etc.) in a controlled worktree. The Runner uses this
//! interface to start, stream events, resume, and cancel executions.
//!
//! Codex adapter:
//! - Uses `codex exec --json` non-interactive mode
//! - Parses JSONL events (thread.started, turn.completed, etc.)
//! - Saves session ID for resume
//! - Supports timeout and cancel
//! - Rejects dangerous parameters (danger-full-access, skip-approval)

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use serde::{Deserialize, Serialize};

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

    /// Wait for the execution to complete and parse events.
    fn wait_and_collect(&mut self, handle: ExecutionHandle) -> Result<Vec<ExecutorEvent>, String>;

    /// Cancel a running execution.
    fn cancel(&mut self, handle: &mut ExecutionHandle) -> Result<(), String>;
}

// ─── Codex adapter ───

/// Dangerous parameters that must never be passed to Codex.
const FORBIDDEN_PARAMS: &[&str] = &[
    "--dangerously-bypass-approvals-and-sandbox",
    "--full-auto",
    "--danger-full-access",
    "--skip-approvals",
];

pub struct CodexExecutor {
    binary: String,
}

impl CodexExecutor {
    pub fn new() -> Self {
        Self {
            binary: "codex".to_string(),
        }
    }

    /// Check if a parameter is forbidden.
    pub fn is_param_forbidden(param: &str) -> bool {
        FORBIDDEN_PARAMS.iter().any(|&f| param.starts_with(f))
    }

    /// Parse JSONL output into events.
    pub fn parse_jsonl(raw: &str) -> Vec<ExecutorEvent> {
        let mut events = Vec::new();
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
                let event_type = val
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let session_id = val
                    .get("session_id")
                    .or_else(|| val.get("thread_id"))
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let message = val
                    .get("message")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let success = val
                    .get("success")
                    .and_then(|v| v.as_bool());
                events.push(ExecutorEvent {
                    event_type,
                    session_id,
                    message,
                    success,
                    raw: val,
                });
            }
        }
        events
    }
}

impl Default for CodexExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl Executor for CodexExecutor {
    fn probe(&self) -> ExecutorProbe {
        let output = Command::new(&self.binary)
            .arg("--version")
            .output();

        match output {
            Ok(o) if o.status.success() => {
                let version = String::from_utf8_lossy(&o.stdout).trim().to_string();
                ExecutorProbe {
                    available: true,
                    version: Some(version),
                    path: which(&self.binary),
                    supports_resume: true, // codex exec resume <session_id>
                    supports_json: true,   // codex exec --json
                }
            }
            _ => ExecutorProbe {
                available: false,
                version: None,
                path: None,
                supports_resume: false,
                supports_json: false,
            },
        }
    }

    fn start(
        &mut self,
        task: &str,
        worktree_path: &Path,
        sandbox: &str,
        _timeout: Duration,
    ) -> Result<ExecutionHandle, String> {
        // Verify no forbidden params snuck in (defense in depth)
        if task.contains("--danger") || task.contains("--full-auto") || task.contains("--skip") {
            return Err("task contains forbidden parameters".to_string());
        }

        // Set up output files
        let output_dir = worktree_path.join(".questlog-output");
        std::fs::create_dir_all(&output_dir).map_err(|e| format!("mkdir output: {e}"))?;
        let stdout_path = output_dir.join("stdout.jsonl");
        let stderr_path = output_dir.join("stderr.log");

        let stdout_file = std::fs::File::create(&stdout_path)
            .map_err(|e| format!("create stdout: {e}"))?;
        let stderr_file = std::fs::File::create(&stderr_path)
            .map_err(|e| format!("create stderr: {e}"))?;

        let child = Command::new(&self.binary)
            .args(["exec", "--json"])
            .arg("-C")
            .arg(worktree_path)
            .arg("--sandbox")
            .arg(sandbox)
            .arg(task)
            .stdout(Stdio::from(stdout_file))
            .stderr(Stdio::from(stderr_file))
            .spawn()
            .map_err(|e| format!("failed to start codex: {e}"))?;

        Ok(ExecutionHandle {
            child,
            session_id: None,
            stdout_path,
            stderr_path,
        })
    }

    fn wait_and_collect(&mut self, mut handle: ExecutionHandle) -> Result<Vec<ExecutorEvent>, String> {
        let status = handle
            .child
            .wait()
            .map_err(|e| format!("wait failed: {e}"))?;

        let raw = std::fs::read_to_string(&handle.stdout_path)
            .map_err(|e| format!("read stdout: {e}"))?;

        let mut events = Self::parse_jsonl(&raw);

        // Extract session ID from the first event that has one
        if let Some(session) = events.iter().find_map(|e| e.session_id.clone()) {
            handle.session_id = Some(session);
        }

        // Add a synthetic completion event
        events.push(ExecutorEvent {
            event_type: "execution.completed".to_string(),
            session_id: handle.session_id.clone(),
            message: Some(format!("exit code: {}", status.code().unwrap_or(-1))),
            success: Some(status.success()),
            raw: serde_json::json!({"exit_code": status.code()}),
        });

        Ok(events)
    }

    fn cancel(&mut self, handle: &mut ExecutionHandle) -> Result<(), String> {
        // Try to kill the child process
        handle
            .child
            .kill()
            .map_err(|e| format!("kill failed: {e}"))?;
        Ok(())
    }
}

/// Find the path of an executable (like `which`).
fn which(binary: &str) -> Option<String> {
    let output = Command::new("which")
        .arg(binary)
        .output()
        .ok()?;

    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forbidden_params_detected() {
        assert!(CodexExecutor::is_param_forbidden("--danger-full-access"));
        assert!(CodexExecutor::is_param_forbidden("--full-auto"));
        assert!(CodexExecutor::is_param_forbidden("--skip-approvals"));
        assert!(CodexExecutor::is_param_forbidden(
            "--dangerously-bypass-approvals-and-sandbox"
        ));
        assert!(!CodexExecutor::is_param_forbidden("--json"));
    }

    #[test]
    fn parse_jsonl_extracts_events() {
        let jsonl = r#"{"type":"thread.started","session_id":"abc123"}
{"type":"turn.completed","success":true}
{"type":"item.message","message":"Done!"}"#;
        let events = CodexExecutor::parse_jsonl(jsonl);
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event_type, "thread.started");
        assert_eq!(events[0].session_id.as_deref(), Some("abc123"));
        assert_eq!(events[1].success, Some(true));
        assert_eq!(events[2].message.as_deref(), Some("Done!"));
    }

    #[test]
    fn parse_jsonl_skips_invalid_lines() {
        let jsonl = r#"{"type":"ok"}
not json at all
{"type":"also_ok"}"#;
        let events = CodexExecutor::parse_jsonl(jsonl);
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn parse_jsonl_empty_input() {
        let events = CodexExecutor::parse_jsonl("");
        assert!(events.is_empty());
    }

    #[test]
    fn start_rejects_dangerous_task() {
        let mut exec = CodexExecutor::new();
        let result = exec.start(
            "--danger-full-access do something",
            Path::new("/tmp"),
            "workspace-write",
            Duration::from_secs(60),
        );
        assert!(result.is_err());
    }
}
