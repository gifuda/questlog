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

pub fn start_watching(app: &AppHandle) {
    let state = app.state::<AppState>();
    let projects = match state.projects.lock() {
        Ok(p) => p.clone(),
        Err(_) => return,
    };

    let registry_path = dirs::home_dir().map(|h| h.join(REGISTRY_FILENAME));

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
    for (name, path) in &projects {
        if let Err(e) = watcher.watch(path, RecursiveMode::NonRecursive) {
            log::warn!("failed to watch parent dir for {name}: {e}");
        } else {
            log::info!("watching {name}: {}", path.display());
        }
    }

    // Watch the central registry for new/deleted projects
    if let Some(ref rp) = registry_path {
        if rp.exists() {
            if let Err(e) = watcher.watch(rp, RecursiveMode::NonRecursive) {
                log::warn!("failed to watch registry: {e}");
            } else {
                log::info!("watching registry: {}", rp.display());
            }
        }
    }

    // Watch the Runtime Store directory for execution/package state changes.
    // This lets the UI refresh when the Runner processes a package.
    let runtime_dir = crate::execution_store::runtime_dir();
    if runtime_dir.exists() {
        if let Err(e) = watcher.watch(&runtime_dir, RecursiveMode::NonRecursive) {
            log::warn!("failed to watch runtime dir: {e}");
        } else {
            log::info!("watching runtime: {}", runtime_dir.display());
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
                    }
                }
                _ => {}
            }
        }
        drop(watcher);
    });
}
