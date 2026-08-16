//! Execution-time capability isolation for CLI executors.
//!
//! A task package's allowed/forbidden paths are verified AFTER execution
//! (the circuit breaker). This module constrains what the executor process
//! can do WHILE running, so the post-hoc check is a backstop, not the only
//! line of defense:
//!
//! 1. **Seatbelt profile** (macOS `sandbox-exec`, when available):
//!    - write access: the worktree + temp/cache dirs + `~/.zcode` (the CLI's
//!      own session DB) — nothing else; the ZCode user config files and hook
//!      dirs are explicitly carved out so an executor can't reprogram future
//!      sessions;
//!    - read access: everything EXCEPT credential dirs (`~/.ssh`, `~/.aws`,
//!      `~/.gnupg`, `~/.config/gh`, `~/.kube`, `~/.docker`);
//!    - exec of `ssh`/`scp`/`sftp`/`security`/`ssh-agent` denied outright;
//!    - network stays ALLOWED (the LLM API needs it) — documented residual:
//!      content visible to the executor could be exfiltrated over the
//!      network; secrets are protected by the read denies above.
//!    - `read-only` packages tighten further: the worktree itself is not
//!      writable — the FS enforces what the CLI mode only promises.
//! 2. **git shim** prepended to PATH: rejects `push/fetch/clone/remote/
//!    submodule` — the sealed authority declares `auto_push=false`; this
//!    enforces the network half at runtime.
//! 3. **Environment scrub**: no `SSH_AUTH_SOCK`, `credential.helper` emptied,
//!    `GIT_TERMINAL_PROMPT=0`, `GIT_SSH_COMMAND=false` — no silent credential
//!    pickup and no hanging auth prompts.
//!
//! Layer 1 auto-degrades to layers 2+3 on platforms without sandbox-exec,
//! with a warn log. If even the shim cannot be installed, the executor
//! refuses to start (fail-closed: never run silently unconstrained).

use std::path::{Path, PathBuf};
use std::process::Command;

/// Which isolation layers were applied to a spawn.
#[derive(Debug, Default)]
pub struct IsolationReport {
    pub seatbelt: bool,
    pub git_shim: bool,
    pub env_scrub: bool,
}

fn home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}

/// Build the Seatbelt profile for this execution. `write_worktree` is false
/// for read-only packages.
fn seatbelt_profile(worktree: &Path, write_worktree: bool) -> Option<String> {
    let home = home_dir()?;
    let home = home
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let worktree_str = worktree
        .canonicalize()
        .unwrap_or_else(|_| worktree.to_path_buf())
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");

    let mut write_allows = format!(
        "(allow file-write*\n  (subpath \"/private/tmp\")\n  (subpath \"/var/folders\")\n  (subpath \"{home}/.zcode\")\n  (subpath \"{home}/.npm\")\n  (subpath \"{home}/.cache\")\n  (subpath \"{home}/Library/Caches\"))"
    );
    if write_worktree {
        write_allows = format!(
            "(allow file-write*\n  (subpath \"{worktree_str}\")\n  (subpath \"/private/tmp\")\n  (subpath \"/var/folders\")\n  (subpath \"{home}/.zcode\")\n  (subpath \"{home}/.npm\")\n  (subpath \"{home}/.cache\")\n  (subpath \"{home}/Library/Caches\"))"
        );
    }

    Some(format!(
        r#"(version 1)
(deny default)
(allow process-exec process-fork)
(deny process-exec
  (literal "/usr/bin/ssh")
  (literal "/usr/bin/scp")
  (literal "/usr/bin/sftp")
  (literal "/usr/bin/ssh-agent")
  (literal "/usr/bin/security"))
(allow signal)
(allow sysctl-read)
(allow mach-lookup)
(allow ipc-posix-shm*)
(allow pseudo-tty)
(allow network*)
(allow file-read*)
(deny file-read*
  (subpath "{home}/.ssh")
  (subpath "{home}/.aws")
  (subpath "{home}/.gnupg")
  (subpath "{home}/.config/gh")
  (subpath "{home}/.kube")
  (subpath "{home}/.docker"))
{write_allows}
(deny file-write*
  (literal "{home}/.zcode/cli/config.json")
  (literal "{home}/.zcode/v2/config.json")
  (subpath "{home}/.zcode/cli/hooks")
  (subpath "{home}/.zcode/v2/hooks"))
"#
    ))
}

fn seatbelt_binary() -> Option<&'static str> {
    const PATH: &str = "/usr/bin/sandbox-exec";
    if Path::new(PATH).exists() {
        Some(PATH)
    } else {
        None
    }
}

/// Wrap `program + args` in a Seatbelt sandbox when available. Returns the
/// (possibly rewritten) program and args plus whether the wrap happened.
pub fn wrap_with_seatbelt(
    program: &str,
    args: &[String],
    worktree: &Path,
    sandbox: &str,
) -> (String, Vec<String>, bool) {
    let Some(seatbelt) = seatbelt_binary() else {
        log::warn!("executor isolation: sandbox-exec unavailable — shim+env only");
        return (program.to_string(), args.to_vec(), false);
    };
    let write_worktree = sandbox != "read-only";
    let Some(profile) = seatbelt_profile(worktree, write_worktree) else {
        log::warn!("executor isolation: no home dir — shim+env only");
        return (program.to_string(), args.to_vec(), false);
    };
    let mut new_args = vec!["-p".to_string(), profile, program.to_string()];
    new_args.extend(args.iter().cloned());
    (seatbelt.to_string(), new_args, true)
}

/// Ensure the git shim exists under the runtime dir and return its directory.
fn ensure_git_shim() -> Result<PathBuf, String> {
    let dir = crate::execution_store::runtime_dir().join("git-shim");
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir git-shim: {e}"))?;
    let shim = dir.join("git");
    let real_git = std::env::var("PATH")
        .ok()
        .and_then(|path| {
            path.split(':')
                .map(|entry| Path::new(entry).join("git"))
                .find(|candidate| candidate.is_file())
        })
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "/usr/bin/git".to_string());
    let script = format!(
        r#"#!/bin/sh
# QuestLog executor policy shim. The sealed package authority declares
# auto_push=false; this shim enforces the network half at runtime by refusing
# outbound/destructive git operations inside executions.
case "${{1:-}}" in
  push|fetch|clone|remote|ls-remote|submodule)
    echo "QuestLog executor policy: 'git $1' is forbidden inside executions" >&2
    exit 126
    ;;
esac
exec "{real_git}" "$@"
"#
    );
    // Rewrite only when content differs (keep mtime stable for debugging).
    let current = std::fs::read_to_string(&shim).unwrap_or_default();
    if current != script {
        std::fs::write(&shim, script).map_err(|e| format!("write git shim: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| format!("chmod git shim: {e}"))?;
        }
    }
    Ok(dir)
}

/// Apply the environment scrub + git shim to an already-built command.
/// Fail-closed: if the shim cannot be installed AND seatbelt is not active,
/// returns Err — the executor never runs silently unconstrained.
pub fn apply_isolation(
    command: &mut Command,
    seatbelt_active: bool,
) -> Result<IsolationReport, String> {
    let mut report = IsolationReport {
        seatbelt: seatbelt_active,
        ..Default::default()
    };

    // 3. Environment scrub — always applied.
    command.env_remove("SSH_AUTH_SOCK");
    command.env("GIT_TERMINAL_PROMPT", "0");
    command.env("GIT_SSH_COMMAND", "false");
    // Empty credential.helper: HTTPS git ops can't silently authenticate.
    command.env("GIT_CONFIG_COUNT", "1");
    command.env("GIT_CONFIG_KEY_0", "credential.helper");
    command.env("GIT_CONFIG_VALUE_0", "");
    report.env_scrub = true;

    // 2. git shim on PATH.
    match ensure_git_shim() {
        Ok(dir) => {
            let existing = std::env::var("PATH").unwrap_or_default();
            command.env("PATH", format!("{}:{}", dir.display(), existing));
            report.git_shim = true;
        }
        Err(e) => {
            if seatbelt_active {
                log::warn!("executor isolation: git shim unavailable ({e}) — seatbelt only");
            } else {
                return Err(format!(
                    "executor isolation: no seatbelt and no git shim ({e}) — refusing to spawn unconstrained"
                ));
            }
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seatbelt_profile_denies_credential_reads() {
        let profile = seatbelt_profile(Path::new("/tmp/wt"), true).unwrap();
        assert!(profile.contains("(deny file-read*"));
        assert!(profile.contains(".ssh"));
        assert!(profile.contains(".aws"));
        // Worktree writable in workspace-write mode
        assert!(profile.contains("/tmp/wt"));
        // User zcode configs carved out of the write allows
        assert!(profile.contains(".zcode/cli/config.json"));
        assert!(profile.contains(".zcode/v2/config.json"));
    }

    #[test]
    fn seatbelt_profile_read_only_mode_drops_worktree_write() {
        let profile = seatbelt_profile(Path::new("/tmp/wt"), false).unwrap();
        let write_section = profile
            .split("(allow file-write*")
            .nth(1)
            .expect("write allow block");
        assert!(
            !write_section.contains("/tmp/wt"),
            "read-only mode must not grant worktree write: {write_section}"
        );
    }

    #[test]
    fn git_shim_blocks_network_ops_but_allows_local() {
        let _guard = crate::execution_store::TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("QUESTLOG_RUNTIME", dir.path());
        let shim_dir = ensure_git_shim().unwrap();
        let git = shim_dir.join("git");

        for op in ["push", "fetch", "clone", "remote", "submodule"] {
            let output = std::process::Command::new(&git).arg(op).output().unwrap();
            assert_eq!(
                output.status.code(),
                Some(126),
                "git {op} must be refused inside executions"
            );
            assert!(String::from_utf8_lossy(&output.stderr).contains("forbidden"));
        }
        // A local read op passes through to real git.
        let output = std::process::Command::new(&git)
            .arg("--version")
            .output()
            .unwrap();
        assert!(output.status.success());
    }

    #[test]
    fn isolation_refuses_unconstrained_spawn() {
        // No seatbelt (simulated) + shim creation pointing at an unwritable
        // runtime → hard error, never a silent unconstrained spawn.
        let _guard = crate::execution_store::TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocked-runtime");
        // A FILE where the runtime dir should be → shim mkdir fails.
        std::fs::write(&blocker, "occupied").unwrap();
        std::env::set_var("QUESTLOG_RUNTIME", &blocker);
        let mut cmd = Command::new("true");
        let result = apply_isolation(&mut cmd, false);
        assert!(result.is_err(), "no isolation layers must be a hard error");
    }
}
