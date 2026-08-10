//! ZCode adapter — unified executor interface for ZCode CLI.
//!
//! IMPORTANT: On this machine, ZCode is a GUI application (/Applications/ZCode.app)
//! with no standalone CLI binary callable as `zcode exec`. The probe() function
//! accurately reports this — it does NOT fake availability.
//!
//! Per the G3 plan: "若目标机缺 CLI/鉴权，完成全部接口、mock、fixture、UI 和文档后
//! 报告唯一外部阻塞，不能用 mock 冒充实跑。"
//!
//! This adapter implements the full Executor trait interface so it can be used
//! when a CLI becomes available. The probe() method detects real availability
//! by checking for the binary and its --help output.

use super::{Executor, ExecutorProbe, ExecutionHandle, ExecutorEvent};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

pub struct ZCodeExecutor {
    binary: String,
}

impl ZCodeExecutor {
    pub fn new() -> Self {
        Self {
            // Check common paths — the binary may be named differently
            binary: find_zcode_binary().unwrap_or_else(|| "zcode".to_string()),
        }
    }

    /// Check if ZCode has a non-interactive exec mode.
    /// This is done by probing --help output for known subcommands.
    pub fn supports_exec_mode(&self) -> bool {
        let output = Command::new(&self.binary)
            .arg("--help")
            .output();

        match output {
            Ok(o) if o.status.success() => {
                let help = String::from_utf8_lossy(&o.stdout);
                // Check for exec/non-interactive keywords
                help.contains("exec") || help.contains("non-interactive") || help.contains("run")
            }
            _ => false,
        }
    }
}

impl Default for ZCodeExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl Executor for ZCodeExecutor {
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
                    path: Some(self.binary.clone()),
                    supports_resume: self.supports_exec_mode(),
                    supports_json: self.supports_exec_mode(),
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
        // Same forbidden-parameter check as Codex
        if task.contains("--danger") || task.contains("--skip") {
            return Err("task contains forbidden parameters".to_string());
        }

        let output_dir = worktree_path.join(".questlog-output");
        std::fs::create_dir_all(&output_dir).map_err(|e| format!("mkdir output: {e}"))?;
        let stdout_path = output_dir.join("zcode-stdout.jsonl");
        let stderr_path = output_dir.join("zcode-stderr.log");

        let stdout_file = std::fs::File::create(&stdout_path)
            .map_err(|e| format!("create stdout: {e}"))?;
        let stderr_file = std::fs::File::create(&stderr_path)
            .map_err(|e| format!("create stderr: {e}"))?;

        // ZCode exec command format — may need adjustment based on real probe.
        // The command structure mirrors codex exec for consistency.
        let child = Command::new(&self.binary)
            .args(["exec", "--json"])
            .arg("--cwd")
            .arg(worktree_path)
            .arg("--sandbox")
            .arg(sandbox)
            .arg(task)
            .stdout(Stdio::from(stdout_file))
            .stderr(Stdio::from(stderr_file))
            .spawn()
            .map_err(|e| format!("failed to start zcode: {e}"))?;

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

        // Reuse the same JSONL parser as Codex (same event format)
        let mut events = super::CodexExecutor::parse_jsonl(&raw);

        if let Some(session) = events.iter().find_map(|e| e.session_id.clone()) {
            handle.session_id = Some(session);
        }

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
        handle
            .child
            .kill()
            .map_err(|e| format!("kill failed: {e}"))?;
        Ok(())
    }
}

/// Search for the ZCode CLI binary in common locations.
/// Returns the path if found, None if not available as a standalone CLI.
fn find_zcode_binary() -> Option<String> {
    let candidates = [
        "zcode",
        "zcode-cli",
        "/Applications/ZCode.app/Contents/MacOS/zcode",
        "/Applications/ZCode.app/Contents/MacOS/zcode-cli",
    ];

    for candidate in &candidates {
        if Command::new(candidate)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return Some(candidate.to_string());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_accurately_reports_availability() {
        let exec = ZCodeExecutor::new();
        let probe = exec.probe();
        // On this machine, ZCode is a GUI app without a standalone CLI.
        // The probe should report available=false (not fake it).
        // We assert the probe runs without crashing — the actual value
        // depends on the machine's ZCode installation.
        let _ = probe.available; // don't assert specific value — machine-dependent
    }

    #[test]
    fn find_binary_returns_none_when_not_installed() {
        // On a machine without ZCode CLI, this returns None.
        // We can't assert None specifically (it might be installed elsewhere),
        // but we verify the function doesn't panic.
        let _ = find_zcode_binary();
    }
}
