//! Runner runtime configuration — per-project and global enable/disable.
//!
//! Stored in `<runtime>/questlog-runtime/runner-config.json`, shared between
//! Node MCP and Rust Runner via the same `runtimeDir()` resolution.
//!
//! Semantics:
//! - `global_enabled = false` → nothing runs (master kill switch)
//! - `global_enabled = true` → only projects with `enabled = true` are picked up
//! - Per-epic `auto_execute` in `.epics.json` provides a third layer:
//!   - `None` → inherit project setting
//!   - `Some(true)` → execute when project is enabled
//!   - `Some(false)` → never execute (even if project is enabled)

use crate::execution_store;
use crate::file_lock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

const CONFIG_FILENAME: &str = "runner-config.json";

// ─── Types ───

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectRunnerEntry {
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerRuntimeConfig {
    pub version: u32,
    pub global_enabled: bool,
    #[serde(default)]
    pub projects: HashMap<String, ProjectRunnerEntry>,
    /// Which model the Runner's executor should use ("provider/model", e.g.
    /// "deepseek/deepseek-v4-flash"). None = use the ZCode CLI's own default.
    /// Injected into each worktree as a project-level .zcode/config.json.
    #[serde(default)]
    pub executor_model: Option<String>,
    #[serde(default)]
    pub revision: u64,
}

impl Default for RunnerRuntimeConfig {
    fn default() -> Self {
        Self {
            version: 1,
            global_enabled: false,
            projects: HashMap::new(),
            executor_model: None,
            revision: 0,
        }
    }
}

// ─── Path resolution ───

fn config_path() -> PathBuf {
    execution_store::runtime_dir().join(CONFIG_FILENAME)
}

fn ensure_runtime_dir() -> Result<(), String> {
    std::fs::create_dir_all(execution_store::runtime_dir())
        .map_err(|e| format!("mkdir runtime: {e}"))
}

// ─── Read / Write ───

/// Read the runner config. Returns default (all OFF) if file doesn't exist.
/// Fail-closed on corrupt JSON: returns default rather than crashing.
pub fn read_config() -> RunnerRuntimeConfig {
    let path = config_path();
    if !path.exists() {
        return RunnerRuntimeConfig::default();
    }
    match std::fs::read_to_string(&path) {
        Ok(raw) => match serde_json::from_str::<RunnerRuntimeConfig>(&raw) {
            Ok(config) => config,
            Err(e) => {
                log::warn!("runner-config.json corrupt, using safe defaults: {}", e);
                RunnerRuntimeConfig::default()
            }
        },
        Err(_) => RunnerRuntimeConfig::default(),
    }
}

fn write_config(config: &mut RunnerRuntimeConfig) -> Result<(), String> {
    ensure_runtime_dir()?;
    let path = config_path();
    config.revision = config.revision.saturating_add(1);

    file_lock::with_file_lock(&path, || {
        let json =
            serde_json::to_string_pretty(config).map_err(|e| format!("serialize config: {e}"))?;
        let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
        std::fs::write(&tmp, &json).map_err(|e| format!("write tmp: {e}"))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
        Ok(())
    })
}

// ─── Global toggle ───

pub fn is_globally_enabled() -> bool {
    read_config().global_enabled
}

pub fn set_globally_enabled(enabled: bool) -> Result<(), String> {
    let mut config = read_config();
    config.global_enabled = enabled;
    write_config(&mut config)
}

// ─── Per-project toggle ───

/// Check if a specific project is enabled for Runner execution.
/// Returns false if not explicitly enabled (safe default).
pub fn is_project_enabled(project_path: &str) -> bool {
    let config = read_config();
    config
        .projects
        .get(project_path)
        .map(|e| e.enabled)
        .unwrap_or(false)
}

/// Check if any project is enabled. Used at app startup to decide
/// whether to auto-start the Runner thread.
pub fn any_project_enabled() -> bool {
    let config = read_config();
    config.projects.values().any(|e| e.enabled)
}

/// Enable or disable a project for Runner execution.
/// When at least one project is enabled, starts the Runner thread.
/// When no projects are enabled, stops the Runner thread.
pub fn set_project_enabled(project_path: &str, enabled: bool) -> Result<(), String> {
    let mut config = read_config();
    config
        .projects
        .insert(project_path.to_string(), ProjectRunnerEntry { enabled });
    write_config(&mut config)?;

    // Manage the Runner thread based on whether any project is enabled
    let any_enabled = config.projects.values().any(|e| e.enabled);
    if any_enabled {
        crate::runner::ensure_running();
    } else {
        crate::runner::stop_runner();
    }

    Ok(())
}

/// Get the status for a project.
pub fn get_project_status(project_path: &str) -> ProjectRunnerStatus {
    let config = read_config();
    let project_enabled = config
        .projects
        .get(project_path)
        .map(|e| e.enabled)
        .unwrap_or(false);
    ProjectRunnerStatus { project_enabled }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectRunnerStatus {
    pub project_enabled: bool,
}

// ─── Executor model selection ───

/// Get the configured executor model ("provider/model"), or None to use the
/// ZCode CLI's own default.
pub fn get_executor_model() -> Option<String> {
    read_config().executor_model
}

/// Set the executor model. Does NOT restart the Runner thread — the value is
/// picked up on the next worktree creation.
pub fn set_executor_model(model: Option<String>) -> Result<(), String> {
    let mut config = read_config();
    config.executor_model = model;
    write_config(&mut config)
}

// ─── Per-epic auto_execute ───

/// Check if a specific epic should auto-execute.
/// Reads `auto_execute` from `.epics.json`:
/// - `None` → inherit project setting (return project status)
/// - `Some(false)` → always false
/// - `Some(true)` → defer to project setting
///
/// If `.epics.json` doesn't exist or the epic isn't found, returns Ok(true)
/// (no epic-level restriction → defer to project setting).
pub fn is_epic_auto_execute(project_path: &str, epic_id: &str) -> Result<bool, String> {
    let project_enabled = is_project_enabled(project_path);
    if !project_enabled {
        return Ok(false);
    }

    // Read .epics.json to check epic's auto_execute field.
    // If the file doesn't exist or epic isn't found, treat as no restriction.
    let dir = PathBuf::from(project_path);
    let data = match crate::epic_store::read_epics(&dir) {
        Ok(d) => d,
        Err(_) => return Ok(true), // no .epics.json → no epic restriction
    };
    let epic = match data.epics.iter().find(|e| e.id == epic_id) {
        Some(e) => e,
        None => return Ok(true), // epic not found → no restriction
    };

    match epic.auto_execute {
        None => Ok(true),         // inherit project (already checked)
        Some(false) => Ok(false), // explicitly disabled
        Some(true) => Ok(true),   // explicitly enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution_store::TEST_LOCK;

    fn setup_temp_runtime() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        let guard = TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("QUESTLOG_RUNTIME", dir.path());
        (dir, guard)
    }

    #[test]
    fn default_config_all_off() {
        let (_dir, _guard) = setup_temp_runtime();
        let config = read_config();
        assert!(config.projects.is_empty());
    }

    #[test]
    fn project_enable_persists() {
        let (_dir, _guard) = setup_temp_runtime();
        assert!(!is_project_enabled("/tmp/proj1"));

        set_project_enabled("/tmp/proj1", true).unwrap();
        assert!(is_project_enabled("/tmp/proj1"));
        assert!(!is_project_enabled("/tmp/proj2"));
    }

    #[test]
    fn project_status_reflects_enable() {
        let (_dir, _guard) = setup_temp_runtime();

        // Default → off
        let status = get_project_status("/tmp/proj1");
        assert!(!status.project_enabled);

        // Enable → on
        set_project_enabled("/tmp/proj1", true).unwrap();
        let status = get_project_status("/tmp/proj1");
        assert!(status.project_enabled);
    }

    #[test]
    fn corrupt_config_fails_closed() {
        let (_dir, _guard) = setup_temp_runtime();
        ensure_runtime_dir().unwrap();
        std::fs::write(config_path(), "{ corrupt json").unwrap();

        let config = read_config();
        assert!(config.projects.is_empty()); // safe default
    }

    #[test]
    fn global_toggle_persists_across_reads() {
        let (_dir, _guard) = setup_temp_runtime();

        set_globally_enabled(true).unwrap();
        assert!(is_globally_enabled());

        set_globally_enabled(false).unwrap();
        assert!(!is_globally_enabled());
    }

    #[test]
    fn executor_model_roundtrip_and_backward_compat() {
        let (_dir, _guard) = setup_temp_runtime();

        // Default: None (use CLI default)
        assert!(get_executor_model().is_none());

        // Set → read back
        set_executor_model(Some("bigmodel/GLM-5.2".to_string())).unwrap();
        assert_eq!(get_executor_model().as_deref(), Some("bigmodel/GLM-5.2"));

        // Clear → None again
        set_executor_model(None).unwrap();
        assert!(get_executor_model().is_none());

        // Old config without the field must deserialize (serde default)
        let raw = serde_json::json!({
            "version": 1,
            "global_enabled": false,
            "projects": {},
            "revision": 1
        });
        let parsed: RunnerRuntimeConfig = serde_json::from_value(raw).unwrap();
        assert!(parsed.executor_model.is_none());
    }
}
