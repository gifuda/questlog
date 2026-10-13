//! Remote-mirror data root (GUI bridge mode — S3/S4 transition).
//!
//! When the marker file exists, this process is a READ-ONLY mirror client:
//! board/runtime reads are re-rooted into the local mirror directory and
//! every write path fails closed with an explicit error. The marker is
//! created/removed by the S4 bridge rsync job; its absence is the normal
//! local-authority mode with zero behavior change.
//!
//! Mirror layout contract (produced by the bridge job):
//!   <mirror_root>/.todo-board.json          ← authority registry
//!   <mirror_root>/questlog-runtime/…        ← authority runtime snapshot
//!   <mirror_root>/<suffix-after-prefix>/…   ← project dirs, board files inside
//! e.g. prefix_from=<authority home> and mirror_root=<local mirror root>:
//! the authority project <prefix>/dev/demo-project reads as
//! <mirror_root>/dev/demo-project.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Debug, Clone, serde::Deserialize)]
pub struct RemoteConfig {
    /// Absolute home prefix on the authority machine.
    pub prefix_from: String,
    /// Local mirror root (absolute).
    pub mirror_root: PathBuf,
}

static REMOTE: OnceLock<Option<RemoteConfig>> = OnceLock::new();

/// Marker file inside the mirror root; QUESTLOG_REMOTE_CONFIG overrides the
/// location for tests. Content = RemoteConfig JSON.
fn marker_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("QUESTLOG_REMOTE_CONFIG") {
        return Some(PathBuf::from(p));
    }
    dirs::home_dir().map(|h| h.join("questlog-mirror").join("REMOTE_ACTIVE"))
}

fn load() -> Option<RemoteConfig> {
    let path = marker_path()?;
    let raw = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str::<RemoteConfig>(&raw) {
        Ok(cfg) if cfg.prefix_from.starts_with('/') && cfg.mirror_root.is_absolute() => Some(cfg),
        _ => None,
    }
}

/// Read the marker once at process start (lib.rs setup / headless main).
pub fn init() {
    let _ = REMOTE.set(load());
}

pub fn is_remote() -> bool {
    REMOTE.get().map(|o| o.is_some()).unwrap_or(false)
}

fn cfg() -> Option<&'static RemoteConfig> {
    REMOTE.get().unwrap_or(&None).as_ref()
}

/// Re-root an authority-machine path into the local mirror for READS.
/// Paths outside `prefix_from` (e.g. the local gros board.json) pass
/// through unchanged. Identity when not in remote mode.
pub fn map_read(path: &Path) -> PathBuf {
    map_read_with(cfg(), path)
}

fn map_read_with(remote: Option<&RemoteConfig>, path: &Path) -> PathBuf {
    match remote {
        Some(c) => {
            let s = path.to_string_lossy();
            let prefix = c.prefix_from.trim_end_matches('/');
            if s == prefix {
                c.mirror_root.clone()
            } else if let Some(suffix) = s.strip_prefix(&format!("{prefix}/")) {
                c.mirror_root.join(suffix)
            } else {
                path.to_path_buf()
            }
        }
        None => path.to_path_buf(),
    }
}

/// Fail-closed write guard. Called first in every mutating entry point;
/// the authority machine (headless on m5p) never sets the marker, so the
/// guard is a no-op there.
pub fn guard_write() -> Result<(), String> {
    guard_write_with(cfg())
}

fn guard_write_with(remote: Option<&RemoteConfig>) -> Result<(), String> {
    if remote.is_some() {
        Err(
            "remote mirror mode: this client is read-only — writes live on the authority machine"
                .to_string(),
        )
    } else {
        Ok(())
    }
}

/// Registry override: the mirror carries the authority registry at a fixed
/// spot, so the local home-derived path must not be used in remote mode.
pub fn registry_override() -> Option<PathBuf> {
    cfg().map(|c| c.mirror_root.join(".todo-board.json"))
}

/// Runtime override: same reasoning as the registry — the local
/// ~/questlog-runtime must not shadow the mirrored authority snapshot.
pub fn runtime_override() -> Option<PathBuf> {
    cfg().map(|c| c.mirror_root.join("questlog-runtime"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> RemoteConfig {
        RemoteConfig {
            prefix_from: "/data/auth".to_string(),
            mirror_root: PathBuf::from("/mirror/questlog"),
        }
    }

    #[test]
    fn identity_without_remote_config() {
        let p = Path::new("/workspace/demo-todos/.todos.json");
        assert_eq!(map_read_with(None, p), p);
    }

    #[test]
    fn maps_authority_prefix_into_mirror() {
        assert_eq!(
            map_read_with(Some(&cfg()), Path::new("/data/auth/dev/demo-project")),
            PathBuf::from("/mirror/questlog/dev/demo-project")
        );
        // Deep board file path maps component-preserving.
        assert_eq!(
            map_read_with(
                Some(&cfg()),
                Path::new("/data/auth/projects/demo/.epics.json")
            ),
            PathBuf::from("/mirror/questlog/projects/demo/.epics.json")
        );
    }

    #[test]
    fn prefix_boundary_is_exact_not_string_contains() {
        // A longer path that merely starts with the prefix STRING must not
        // match — the boundary is end-of-path or '/'.
        let p = Path::new("/data/authority-elsewhere/x");
        assert_eq!(map_read_with(Some(&cfg()), p), p);
    }

    #[test]
    fn local_paths_pass_through_in_remote_mode() {
        let p = Path::new("/workspace/local-board/exports/board.json");
        assert_eq!(map_read_with(Some(&cfg()), p), p);
    }

    #[test]
    fn write_guard_fail_closed_only_in_remote_mode() {
        assert!(guard_write_with(None).is_ok());
        assert!(guard_write_with(Some(&cfg())).is_err());
    }
}
