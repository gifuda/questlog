mod commands;
mod discovery;
pub mod epic_store;
pub mod executors;
pub mod execution_store;
pub mod file_lock;
mod models;
pub mod runner;
pub mod runner_git;
mod store;
mod tray;
mod watcher;

use commands::AppState;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let project_list = discovery::discover_projects();

    tauri::Builder::default()
        .plugin(tauri_plugin_log::Builder::default().build())
        .manage(AppState {
            projects: std::sync::Mutex::new(project_list),
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_projects,
            commands::get_todos,
            commands::update_todo,
            commands::add_todo,
            commands::remove_project,
            commands::undo_remove_project,
            commands::get_trash,
            commands::restore_project,
            commands::permanently_delete_project,
            commands::reorder_projects,
            commands::get_all_active,
            commands::get_project_items,
            commands::show_main_window,
            commands::quit_app,
            // 方案追踪模块
            commands::get_epics,
            commands::get_epics_global,
            commands::add_epic,
            commands::update_epic,
            commands::delete_epic,
            commands::add_step,
            commands::update_step,
            commands::delete_step,
            // 执行状态模块 (G2 — Runner state)
            commands::get_executions,
            commands::get_execution,
            commands::cancel_execution,
            commands::retry_execution,
            commands::pause_execution,
            commands::cleanup_stale_executions,
        ])
        .setup(|app| {
            tray::setup_tray(app);
            let handle = app.handle().clone();
            watcher::start_watching(&handle);

            // Start the Runner background thread.
            // Enabled by config: QUESTLOG_RUNNER=1 to turn on.
            // When disabled, the Runner thread doesn't spawn — zero overhead.
            let runner_enabled = std::env::var("QUESTLOG_RUNNER")
                .map(|v| v == "1" || v == "true")
                .unwrap_or(false);
            if runner_enabled {
                runner::start_runner_thread(runner::RunnerConfig {
                    enabled: true,
                    poll_interval: std::time::Duration::from_secs(10),
                    runner_id: format!("runner-{}", std::process::id()),
                });
                log::info!("QuestLog Runner enabled via QUESTLOG_RUNNER=1");
            }

            // Hide main window on close instead of quitting (tray keeps app alive)
            if let Some(main_win) = app.get_webview_window("main") {
                let win = main_win.clone();
                main_win.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = win.hide();
                    }
                });
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
