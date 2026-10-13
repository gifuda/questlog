use crate::commands::AppState;
use crate::discovery;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::sync::mpsc;
use tauri::{AppHandle, Emitter, Manager};

const FILENAME: &str = ".todos.json";
const EPICS_FILENAME: &str = ".epics.json";
const REGISTRY_FILENAME: &str = ".todo-board.json";
const EXECUTIONS_FILENAME: &str = "executions.json";
const PACKAGES_FILENAME: &str = "packages.json";
const BOARD_FILENAME: &str = "board.json";

/// A6: arb board config source — API in remote mode (the registry lives on
/// the authority machine, not in the local mirror), local discovery
/// otherwise. The board.json file itself is still read/watched locally
/// (M4 keeps its own GROS export — by design).
fn arb_board_path() -> Option<String> {
    if crate::remote_client::enabled() {
        let cfg: discovery::ArbBoardConfig = crate::remote_client::get_into("/api/arb-config")
            .map_err(|e| {
                log::warn!("arb config fetch failed, arb watch skipped: {e}");
                e
            })
            .ok()?;
        cfg.enabled.then_some(cfg.board_path)
    } else {
        discovery::enabled_arb_board_path()
    }
}

pub fn start_watching(app: &AppHandle) {
    // S5 remote API mode: reads/writes live on the authority API — there is
    // no mirror to watch. Only the (local) arb board leg keeps its watch;
    // project/registry/runtime watches are skipped. The event loop itself
    // stays as-is (it simply receives no board events).
    let api_mode = crate::remote_client::enabled();

    let state = app.state::<AppState>();
    let projects = match state.projects.lock() {
        Ok(p) => p.clone(),
        Err(_) => return,
    };

    let registry_path = crate::data_root::registry_override()
        .or_else(|| dirs::home_dir().map(|h| h.join(REGISTRY_FILENAME)));

    let (tx, rx) = mpsc::channel();
    let mut watcher = match RecommendedWatcher::new(
        move |res: Result<Event, notify::Error>| {
            if let Ok(event) = res {
                let _ = tx.send(event);
            }
        },
        Config::default(),
    ) {
        Ok(w) => w,
        Err(e) => {
            log::error!("failed to create watcher: {e}");
            return;
        }
    };

    // Watch each project's .todos.json and .epics.json.
    // We watch the project directory itself (NonRecursive) so that we catch
    // creation of .todos.json / .epics.json when the MCP pushes for the first
    // time. Watching individual files misses the initial create.
    if !api_mode {
        for (name, path) in &projects {
            // Remote mirror mode: watch the local mirror copy, not the
            // authority path (which does not exist on this machine).
            let watch_path = crate::data_root::map_read(path);
            if let Err(e) = watcher.watch(&watch_path, RecursiveMode::NonRecursive) {
                log::warn!("failed to watch parent dir for {name}: {e}");
            } else {
                log::info!("watching {name}: {}", path.display());
            }
        }
    }

    // 套利机会栏: watch the global board.json PARENT directory. board.json
    // usually lives in a subdirectory of the GROS project, which no project-
    // root watch covers, so it needs its own watch. Config changes (enable +
    // path) take effect on next app start.
    if let Some(board_path) = arb_board_path() {
        let board = std::path::PathBuf::from(&board_path);
        match board.parent() {
            Some(parent) if parent.exists() => {
                if let Err(e) = watcher.watch(parent, RecursiveMode::NonRecursive) {
                    log::warn!("failed to watch arb board dir {parent:?}: {e}");
                } else {
                    log::info!("watching arb board dir: {}", parent.display());
                }
            }
            _ => log::warn!("arb board parent dir missing for {board_path}"),
        }
    }

    // Watch the central registry for new/deleted projects
    if !api_mode {
        if let Some(ref rp) = registry_path {
            if rp.exists() {
                if let Err(e) = watcher.watch(rp, RecursiveMode::NonRecursive) {
                    log::warn!("failed to watch registry: {e}");
                } else {
                    log::info!("watching registry: {}", rp.display());
                }
            }
        }
    }

    // Watch the Runtime Store directory for execution/package state changes.
    // This lets the UI refresh when the Runner processes a package.
    if !api_mode {
        let runtime_dir = crate::execution_store::runtime_dir();
        if runtime_dir.exists() {
            if let Err(e) = watcher.watch(&runtime_dir, RecursiveMode::NonRecursive) {
                log::warn!("failed to watch runtime dir: {e}");
            } else {
                log::info!("watching runtime: {}", runtime_dir.display());
            }
        }
    }

    let app_handle = app.clone();
    std::thread::spawn(move || {
        while let Ok(event) = rx.recv() {
            match event.kind {
                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {
                    for path in &event.paths {
                        let fname = path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();

                        // .todos.json changed → emit todos-changed.
                        // starts_with (not ==) so the atomic-write pattern
                        // (".todos.json.tmp-<uuid>" + rename) is caught even
                        // when fsevents reports only the tmp path.
                        if fname.starts_with(FILENAME) {
                            let project_dir = path.parent().map(|p| p.to_path_buf());
                            if let Some(dir) = project_dir {
                                let name = dir
                                    .file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_default();
                                let _ = app_handle.emit("todos-changed", &name);

                                if matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_))
                                {
                                    crate::tray::bounce_notification(&app_handle);
                                }
                            }
                        }

                        // .epics.json changed → emit epics-changed (方案追踪实时刷新).
                        // starts_with (not ==) for the same atomic-write reason.
                        if fname.starts_with(EPICS_FILENAME) {
                            let project_dir = path.parent().map(|p| p.to_path_buf());
                            if let Some(dir) = project_dir {
                                let name = dir
                                    .file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_default();
                                log::info!(
                                    "watcher: .epics.json activity in '{name}' ({}) → epics-changed",
                                    path.display()
                                );
                                let _ = app_handle.emit("epics-changed", &name);
                            }
                        }

                        // Registry changed → reload project list
                        if fname == REGISTRY_FILENAME {
                            let new_projects = discovery::discover_projects();
                            if let Ok(mut current) = app_handle.state::<AppState>().projects.lock()
                            {
                                *current = new_projects.clone();
                            }
                            let _ = app_handle.emit("projects-changed", ());
                            log::info!(
                                "registry changed, reloaded {} projects",
                                new_projects.len()
                            );
                        }

                        // executions.json or packages.json changed → emit executions-changed
                        // This lets the ExecutionPanel UI refresh when the Runner updates state.
                        if fname == EXECUTIONS_FILENAME || fname == PACKAGES_FILENAME {
                            let _ = app_handle.emit("executions-changed", ());
                        }

                        // board.json changed → emit arb-changed (套利机会栏刷新).
                        // GROS's board-export rewrites the file atomically-ish;
                        // any create/modify/remove triggers a panel refresh.
                        if fname == BOARD_FILENAME {
                            let _ = app_handle.emit("arb-changed", ());
                        }
                    }
                }
                _ => {}
            }
        }
        drop(watcher);
    });
}
