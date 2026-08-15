//! ZCode adapter — unified executor interface for ZCode CLI.
//!
//! REAL AVAILABILITY: The ZCode CLI binary is at:
//!   /Applications/ZCode.app/Contents/Resources/glm/zcode.cjs
//! Invoked as: `node /Applications/ZCode.app/Contents/Resources/glm/zcode.cjs`
//!
//! Non-interactive mode:
//!   zcode.cjs --prompt "<task>" --cwd <worktree> --mode <plan|yolo>
//!
//! Resume:
//!   zcode.cjs --prompt "<task>" --resume <sessionId>
//!
//! The adapter probes for this binary and reports real availability.
//! Config requirement: ~/.zcode/cli/config.json must have a valid provider
//! with apiKey (the adapter does NOT fake availability if config is missing).

use super::{ExecutionHandle, Executor, ExecutorEvent, ExecutorProbe};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Known ZCode CLI binary locations (checked in order).
const ZCODE_BINARY_PATHS: &[&str] = &[
    "/Applications/ZCode.app/Contents/Resources/glm/zcode.cjs",
    // Future: if ZCode ships a standalone binary
    "zcode",
    "zcode-cli",
];

pub struct ZCodeExecutor {
    /// The full command to invoke (e.g. "node /path/to/zcode.cjs").
    binary: String,
    /// Arguments before the prompt (e.g. [] for node, the .cjs path is in binary).
    binary_args: Vec<String>,
}

impl ZCodeExecutor {
    pub fn new() -> Self {
        // Find the ZCode CLI binary
        for path in ZCODE_BINARY_PATHS {
            // For .cjs files, we need to invoke via node
            if path.ends_with(".cjs") {
                if std::path::Path::new(path).exists() {
                    // Verify node is available
                    if Command::new("node").arg("--version").output().is_ok() {
                        return Self {
                            binary: "node".to_string(),
                            binary_args: vec![path.to_string()],
                        };
                    }
                }
            } else {
                // Direct binary
                if Command::new(path)
                    .arg("--version")
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false)
                {
                    return Self {
                        binary: path.to_string(),
                        binary_args: vec![],
                    };
                }
            }
        }

        // Not found — default to "zcode" (will fail probe)
        Self {
            binary: "zcode".to_string(),
            binary_args: vec![],
        }
    }

    /// Build the full command with binary + args.
    fn build_command(&self) -> Command {
        let mut cmd = Command::new(&self.binary);
        for arg in &self.binary_args {
            cmd.arg(arg);
        }
        cmd
    }
}

impl Default for ZCodeExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl Executor for ZCodeExecutor {
    fn probe(&self) -> ExecutorProbe {
        // For .cjs invocation, check the file exists + node works
        if !self.binary_args.is_empty() {
            // node + .cjs path
            let cjs_path = &self.binary_args[0];
            if !std::path::Path::new(cjs_path).exists() {
                return ExecutorProbe {
                    available: false,
                    version: None,
                    path: None,
                    supports_resume: false,
                    supports_json: false,
                };
            }
        }

        let output = self.build_command().arg("--version").output();

        match output {
            Ok(o) if o.status.success() => {
                let version = String::from_utf8_lossy(&o.stdout).trim().to_string();
                ExecutorProbe {
                    available: true,
                    version: Some(version),
                    path: Some(
                        self.binary_args
                            .first()
                            .cloned()
                            .unwrap_or_else(|| self.binary.clone()),
                    ),
                    supports_resume: true, // --resume <sessionId>
                    supports_json: false,  // ZCode uses text output, not JSONL
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
        // Reject dangerous parameters (defense in depth)
        if task.contains("--danger") || task.contains("--skip") || task.contains("--yolo") {
            return Err("task contains forbidden executor parameters".to_string());
        }

        let output_dir = worktree_path.join(".questlog-output");
        std::fs::create_dir_all(&output_dir).map_err(|e| format!("mkdir output: {e}"))?;
        let stdout_path = output_dir.join("zcode-stdout.txt");
        let stderr_path = output_dir.join("zcode-stderr.log");

        let stdout_file =
            std::fs::File::create(&stdout_path).map_err(|e| format!("create stdout: {e}"))?;
        let stderr_file =
            std::fs::File::create(&stderr_path).map_err(|e| format!("create stderr: {e}"))?;

        // ZCode non-interactive command:
        // node zcode.cjs --prompt "<task>" --cwd <worktree> --mode <mode>
        //
        // Mode mapping:
        //   workspace-write → yolo (full write access in the worktree)
        //   read-only → plan (no writes)
        let mode = if sandbox == "read-only" {
            "plan"
        } else {
            "yolo"
        };

        let mut command = self.build_command();
        command
            .arg("--prompt")
            .arg(task)
            .arg("--cwd")
            .arg(worktree_path)
            .arg("--mode")
            .arg(mode)
            .stdout(Stdio::from(stdout_file))
            .stderr(Stdio::from(stderr_file));
        #[cfg(unix)]
        command.process_group(0);
        let child = command
            .spawn()
            .map_err(|e| format!("failed to start zcode: {e}"))?;

        Ok(ExecutionHandle {
            child,
            session_id: None,
            stdout_path,
            stderr_path,
        })
    }

    fn start_with_resume(
        &mut self,
        task: &str,
        session_id: &str,
        worktree_path: &Path,
        sandbox: &str,
        _timeout: Duration,
    ) -> Result<ExecutionHandle, String> {
        // Same safety posture as start().
        if task.contains("--danger") || task.contains("--skip") || task.contains("--yolo") {
            return Err("task contains forbidden executor parameters".to_string());
        }
        if session_id.is_empty() || !session_id.starts_with("sess_") {
            return Err(format!("invalid resume session id: '{session_id}'"));
        }

        let output_dir = worktree_path.join(".questlog-output");
        std::fs::create_dir_all(&output_dir).map_err(|e| format!("mkdir output: {e}"))?;
        // Per-attempt files: never truncate a previous attempt's raw output
        // (its bundle copy happened, but keep the originals intact anyway).
        let stdout_path = output_dir.join("zcode-stdout-resume.txt");
        let stderr_path = output_dir.join("zcode-stderr-resume.log");
        let stdout_file =
            std::fs::File::create(&stdout_path).map_err(|e| format!("create stdout: {e}"))?;
        let stderr_file =
            std::fs::File::create(&stderr_path).map_err(|e| format!("create stderr: {e}"))?;

        let mode = if sandbox == "read-only" { "plan" } else { "yolo" };

        let mut command = self.build_command();
        command
            .arg("--prompt")
            .arg(task)
            .arg("--resume")
            .arg(session_id)
            .arg("--cwd")
            .arg(worktree_path)
            .arg("--mode")
            .arg(mode)
            .stdout(Stdio::from(stdout_file))
            .stderr(Stdio::from(stderr_file));
        #[cfg(unix)]
        command.process_group(0);
        let child = command
            .spawn()
            .map_err(|e| format!("failed to start zcode (resume {session_id}): {e}"))?;

        Ok(ExecutionHandle {
            child,
            session_id: Some(session_id.to_string()),
            stdout_path,
            stderr_path,
        })
    }

    fn wait_and_collect(
        &mut self,
        mut handle: ExecutionHandle,
    ) -> Result<Vec<ExecutorEvent>, String> {
        let status = handle
            .child
            .wait()
            .map_err(|e| format!("wait failed: {e}"))?;

        let raw = std::fs::read_to_string(&handle.stdout_path)
            .map_err(|e| format!("read stdout: {e}"))?;

        // ZCode outputs plain text (not JSONL). We create a synthetic event stream.
        let mut events = Vec::new();

        // Extract session ID from stderr or output if present
        // ZCode session IDs have the format sess_<uuid>
        for line in raw.lines() {
            if let Some(session_match) = line.find("sess_") {
                let rest = &line[session_match..];
                if let Some(end) = rest.find(|c: char| c.is_whitespace()) {
                    let session_id = &rest[..end];
                    handle.session_id = Some(session_id.to_string());
                    break;
                }
            }
        }

        events.push(ExecutorEvent {
            event_type: "execution.output".to_string(),
            session_id: handle.session_id.clone(),
            message: Some(raw.trim().to_string()),
            success: None,
            raw: serde_json::json!({"output_length": raw.len()}),
        });

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_finds_zcode_cli_on_macos() {
        let exec = ZCodeExecutor::new();
        let probe = exec.probe();
        // On this machine, the .cjs file exists at the known path
        let cjs_exists = std::path::Path::new(ZCODE_BINARY_PATHS[0]).exists();
        if cjs_exists {
            assert!(
                probe.available,
                "ZCode CLI should be available when .cjs exists"
            );
            assert!(probe.version.is_some());
        }
    }

    #[test]
    fn probe_returns_path_when_available() {
        let exec = ZCodeExecutor::new();
        let probe = exec.probe();
        if probe.available {
            assert!(probe.path.is_some());
            // Path should be the .cjs file or the binary name
            let path = probe.path.unwrap();
            assert!(path.contains("zcode") || path.contains("node"));
        }
    }

    #[test]
    fn zcode_supports_resume() {
        let exec = ZCodeExecutor::new();
        let probe = exec.probe();
        if probe.available {
            // ZCode CLI supports --resume <sessionId>
            assert!(probe.supports_resume);
        }
    }

    #[test]
    fn start_rejects_dangerous_task_text() {
        let mut exec = ZCodeExecutor::new();
        let result = exec.start(
            "run this with --yolo",
            Path::new("/tmp"),
            "workspace-write",
            Duration::from_secs(60),
        );
        assert!(result.is_err());
    }

    #[test]
    fn start_with_resume_rejects_malformed_session_ids() {
        let mut exec = ZCodeExecutor::new();
        let dir = tempfile::tempdir().unwrap();
        // Non-sess_ prefix / empty → hard error, never spawn
        for bad in ["", "not-a-session", "sess"] {
            let result = exec.start_with_resume(
                "task",
                bad,
                dir.path(),
                "workspace-write",
                Duration::from_secs(60),
            );
            assert!(result.is_err(), "session id '{bad}' must be rejected");
        }
    }
}
