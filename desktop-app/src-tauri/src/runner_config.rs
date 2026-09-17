//! Runner runtime configuration — per-project and global enable/disable.
//!
//! Stored in `<runtime>/questlog-runtime/runner-config.json`, shared between
//! Node MCP and Rust Runner via the same `runtimeDir()` resolution.
//!
//! Semantics:
//! - `global_enabled = false` → nothing runs (master kill switch)
//! - `global_enabled = true` → only projects with `enabled = true` are picked up
//! - Per-epic `auto_execute` in `.epics.json` provides a third layer:
//!   - `None` → fail-closed (explicit ▶ required; the project switch alone
//!     must not batch-release every sealed package)
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

/// 待办租户 G4 (plan §6): nightly sweep schedule. `None` in the config =
/// not configured = the sweep never runs (§10: no ambiguous second master
/// switch — presence of a schedule does not replace the three-layer gates).
///
/// All times are evaluated on a FIXED +08:00 business timezone (Asia/
/// Shanghai) — the business date does not follow transient system timezone
/// changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoSchedule {
    /// Window start, HHMM in +08:00 (default 0300 = 03:00).
    #[serde(default = "default_start")]
    pub start_hhmm: u32,
    /// Window end, exclusive, HHMM in +08:00 (default 0600 = 06:00).
    #[serde(default = "default_end")]
    pub end_hhmm: u32,
    /// Per-night task cap across all tenants (plan §6 default 6).
    #[serde(default = "default_nightly_max")]
    pub nightly_max_tasks: u32,
    /// Per-night cap per tenant (plan §6 default 2).
    #[serde(default = "default_per_tenant_max")]
    pub per_tenant_max_tasks: u32,
    /// Wall-clock timeout for ONE task execution on the collector (§6: 15min).
    #[serde(default = "default_task_timeout")]
    pub per_task_timeout_secs: u64,
    /// Nightly cumulative execution budget (§6: 60min).
    #[serde(default = "default_nightly_budget")]
    pub nightly_budget_secs: u64,
    /// Absolute path to the node binary (GUI PATH is minimal — a bare "node"
    /// ENOENTs; the plan forbids relying on arbitrary PATH).
    pub node_bin: String,
    /// Absolute path to mcp-server/dist/todo-producer-cli.js.
    pub producer_cli: String,
    /// Absolute path to mcp-server/dist/todo-report-verify-cli.js.
    pub verify_cli: String,
    /// Folder-scope selection (user decision): when present, the sweep only
    /// covers tenants whose registry name OR path matches an entry. Absent =
    /// every enabled project. The project runner switch remains the master
    /// authorization either way.
    #[serde(default)]
    pub projects: Option<Vec<String>>,
    /// GLOBAL nightly-execution model preference ("provider/model", e.g.
    /// "bigmodel/GLM-5.3-Flash"). Absent = the collector zcode default.
    /// Injected per-task via a project-level .zcode/config.json in the
    /// snapshot (zero global mutation, no cross-task races).
    #[serde(default)]
    pub model: Option<String>,
    /// GLOBAL nightly thinking-level preference (e.g. "high" | "max" |
    /// "off"). Absent = CLI default. Applied by normalizing every
    /// providerOptionsByLevel entry to the chosen level's params.
    #[serde(default)]
    pub thinking: Option<String>,
}

fn default_start() -> u32 {
    300
}
fn default_end() -> u32 {
    600
}
fn default_nightly_max() -> u32 {
    6
}
fn default_per_tenant_max() -> u32 {
    2
}
fn default_task_timeout() -> u64 {
    900
}
fn default_nightly_budget() -> u64 {
    3600
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
    /// Which thinking level the executor should run at (e.g. "high", "max",
    /// "off"). None = don't inject thinking params (CLI/runtime defaults
    /// apply). Injected by normalizing every level's providerOptionsByLevel
    /// to the chosen level's params — decoupled from the model choice.
    #[serde(default)]
    pub executor_thinking: Option<String>,
    /// 待办租户 G4: nightly sweep schedule. Absent = sweep disabled.
    #[serde(default)]
    pub todo_schedule: Option<TodoSchedule>,
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
            executor_thinking: None,
            todo_schedule: None,
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
///
/// v1→v2 migration: v1 never enforced `global_enabled` (it was a dead field
/// and defaulted to false while projects were toggled directly). On first
/// read of a v1 config, if any project is enabled the global switch is
/// flipped ON (preserving the operator's evident intent) and the result is
/// persisted as v2. From then on `global_enabled = false` is a REAL kill
/// switch: nothing runs, regardless of project switches.
pub fn read_config() -> RunnerRuntimeConfig {
    let path = config_path();
    if !path.exists() {
        return RunnerRuntimeConfig::default();
    }
    let parsed = match std::fs::read_to_string(&path) {
        Ok(raw) => match serde_json::from_str::<RunnerRuntimeConfig>(&raw) {
            Ok(config) => Some(config),
            Err(e) => {
                log::warn!("runner-config.json corrupt, using safe defaults: {}", e);
                None
            }
        },
        Err(_) => None,
    };
    let mut config = match parsed {
        Some(config) => config,
        // Corrupt/unreadable: safe default view, and crucially NO migration
        // write — never overwrite a file we couldn't parse.
        None => return RunnerRuntimeConfig::default(),
    };
    if config.version < 2 {
        if config.projects.values().any(|e| e.enabled) && !config.global_enabled {
            config.global_enabled = true;
        }
        config.version = 2;
        // Persist at most once per process per path: read_config runs on
        // every poll cycle, and without this guard a persistently-failing
        // write (disk full, permissions) would be retried — and
        // lock-contended — on every single read.
        static MIGRATION_ATTEMPTED: std::sync::Mutex<
            Option<std::collections::HashSet<std::path::PathBuf>>,
        > = std::sync::Mutex::new(None);
        let first_attempt_for_path = {
            let mut guard = MIGRATION_ATTEMPTED
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard
                .get_or_insert_with(Default::default)
                .insert(path.clone())
        };
        if first_attempt_for_path {
            let mut persisted = config.clone();
            // Best-effort: if the write fails the in-memory migration still
            // applies, and the next setter persists it anyway.
            let _ = write_config(&mut persisted);
        }
    }
    config
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
    write_config(&mut config)?;

    // The kill switch controls the Runner thread directly: OFF stops it
    // immediately (in-flight executions are still supervised to their
    // terminal state by their own poll iteration ending — no new claims
    // happen because poll_once gates on the global switch first).
    if enabled && config.projects.values().any(|e| e.enabled) {
        crate::runner::ensure_running();
    } else if !enabled {
        crate::runner::stop_runner();
    }
    Ok(())
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

    // Manage the Runner thread: it runs only when the global kill switch is
    // ON and at least one project is enabled.
    let any_enabled = config.projects.values().any(|e| e.enabled);
    if any_enabled && config.global_enabled {
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
    ProjectRunnerStatus {
        project_enabled,
        global_enabled: config.global_enabled,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectRunnerStatus {
    pub project_enabled: bool,
    /// The master kill switch — surfaced so the UI can show "项目开关已开，
    /// 但总闸关闭" instead of letting the operator wonder why nothing runs.
    #[serde(default)]
    pub global_enabled: bool,
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

/// Get the configured executor thinking level, or None to use CLI/runtime
/// defaults (no thinking params injected).
pub fn get_executor_thinking() -> Option<String> {
    read_config().executor_thinking
}

/// 待办租户 G4: the nightly sweep schedule. None = not configured = the
/// sweep never runs.
pub fn get_todo_schedule() -> Option<TodoSchedule> {
    read_config().todo_schedule
}

pub fn set_todo_schedule(schedule: Option<TodoSchedule>) -> Result<(), String> {
    let mut config = read_config();
    config.todo_schedule = schedule;
    write_config(&mut config)
}

/// Set the executor thinking level. Takes effect on the next worktree.
pub fn set_executor_thinking(thinking: Option<String>) -> Result<(), String> {
    let mut config = read_config();
    config.executor_thinking = thinking;
    write_config(&mut config)
}

// ─── Per-epic auto_execute ───

/// Check if a specific epic should auto-execute.
/// Reads `auto_execute` from `.epics.json`:
/// - `Some(true)` → explicitly authorized
/// - `Some(false)` → explicitly paused
/// - `None` → fail-closed (explicit ▶ required; the project switch alone
///   must not batch-release every sealed package)
///
/// Fail-closed on missing data too: if `.epics.json` is unreadable or the
/// epic is unknown, returns Err — the poll loop quietly skips (no claim, no
/// burned attempt), and `verify_pickup_readiness` reports the matching
/// blocker so the gap is visible instead of silent.
pub fn is_epic_auto_execute(project_path: &str, epic_id: &str) -> Result<bool, String> {
    let project_enabled = is_project_enabled(project_path);
    if !project_enabled {
        return Ok(false);
    }

    let dir = PathBuf::from(project_path);
    let data = crate::epic_store::read_epics(&dir)
        .map_err(|e| format!("cannot verify epic authorization (.epics.json): {e}"))?;
    let epic = data
        .epics
        .iter()
        .find(|e| e.id == epic_id)
        .ok_or_else(|| format!("epic '{epic_id}' not found in .epics.json"))?;

    match epic.auto_execute {
        None => Ok(false),        // fail-closed: explicit ▶ required
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
    fn v1_config_with_enabled_projects_migrates_global_on() {
        let (_dir, _guard) = setup_temp_runtime();
        ensure_runtime_dir().unwrap();
        std::fs::write(
            config_path(),
            serde_json::json!({
                "version": 1,
                "global_enabled": false,
                "projects": {"/tmp/p": {"enabled": true}},
                "revision": 3
            })
            .to_string(),
        )
        .unwrap();

        let config = read_config();
        assert!(
            config.global_enabled,
            "v1 config with enabled projects must migrate the kill switch ON (legacy behavior had no global gate)"
        );
        assert_eq!(config.version, 2);
        // Migration is persisted — a later v2 read does not re-migrate.
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(config_path()).unwrap()).unwrap();
        assert_eq!(raw["version"], 2);
        assert_eq!(raw["global_enabled"], true);
    }

    #[test]
    fn v1_config_without_enabled_projects_stays_off() {
        let (_dir, _guard) = setup_temp_runtime();
        ensure_runtime_dir().unwrap();
        std::fs::write(
            config_path(),
            serde_json::json!({
                "version": 1,
                "global_enabled": false,
                "projects": {"/tmp/p": {"enabled": false}},
                "revision": 1
            })
            .to_string(),
        )
        .unwrap();

        let config = read_config();
        assert!(!config.global_enabled);
        assert_eq!(config.version, 2);
    }

    #[test]
    fn v2_kill_switch_is_not_re_migrated() {
        let (_dir, _guard) = setup_temp_runtime();
        ensure_runtime_dir().unwrap();
        // v2: operator deliberately killed the switch while a project is on.
        std::fs::write(
            config_path(),
            serde_json::json!({
                "version": 2,
                "global_enabled": false,
                "projects": {"/tmp/p": {"enabled": true}},
                "revision": 9
            })
            .to_string(),
        )
        .unwrap();

        let config = read_config();
        assert!(
            !config.global_enabled,
            "a deliberate v2 kill-switch OFF must survive reads"
        );
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

    #[test]
    fn executor_thinking_roundtrip_and_backward_compat() {
        let (_dir, _guard) = setup_temp_runtime();

        assert!(get_executor_thinking().is_none());

        set_executor_thinking(Some("high".to_string())).unwrap();
        assert_eq!(get_executor_thinking().as_deref(), Some("high"));

        set_executor_thinking(None).unwrap();
        assert!(get_executor_thinking().is_none());

        // Old config without the field must deserialize (serde default)
        let raw = serde_json::json!({
            "version": 1,
            "global_enabled": false,
            "projects": {},
            "revision": 1
        });
        let parsed: RunnerRuntimeConfig = serde_json::from_value(raw).unwrap();
        assert!(parsed.executor_thinking.is_none());
    }
}

