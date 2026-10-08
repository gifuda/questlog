pub mod attempt_bundle;
mod commands;
pub mod decision_store;
mod discovery;
pub mod epic_store;
pub mod execution_store;
pub mod executor_sandbox;
pub mod executors;
pub mod file_lock;
pub mod fleet_health;
mod models;
pub mod runner;
pub mod runner_config;
pub mod runner_git;
pub mod todo_finalization;
pub mod todo_ledger;
pub mod todo_sweep;
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
            commands::diag_report_error,
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
            // 套利机会栏 (Arb Board — GROS board.json 只读视图)
            commands::get_arb_board,
            commands::set_arb_board,
            commands::submit_arb_control,
            commands::run_arb_decision,
            // 执行状态模块 (G2 — Runner state)
            commands::get_executions,
            commands::get_current_execution,
            commands::get_execution,
            commands::cancel_execution,
            commands::retry_execution,
            commands::pause_execution,
            commands::cleanup_stale_executions,
            commands::set_runner_enabled,
            commands::is_runner_running,
            commands::get_runner_runtime_status,
            commands::get_executor_options,
            commands::set_executor_model,
            commands::set_todo_schedule,
            commands::get_todo_status,
            commands::set_executor_thinking,
            commands::get_runner_project_status,
            commands::set_runner_project_enabled,
            commands::set_runner_global_enabled,
            commands::get_epic_auto_execute,
            commands::set_epic_auto_execute,
            commands::get_decision_requests,
            commands::resolve_decision_request,
            // 跨机链路健康（只读聚合）
            commands::get_fleet_health,
            // 执行记忆 (AttemptBundle Trace Query)
            commands::get_attempt_bundles,
            commands::get_attempt_detail,
            commands::get_attempt_file,
        ])
        .setup(|app| {
            tray::setup_tray(app);
            let handle = app.handle().clone();
            watcher::start_watching(&handle);

            // Runner auto-start: gated by BOTH the master kill switch and at
            // least one enabled project. Global OFF keeps the thread down
            // even with projects enabled.
            if crate::runner_config::is_globally_enabled()
                && crate::runner_config::any_project_enabled()
            {
                log::info!("Runner: auto-starting on app launch (project(s) enabled)");
                crate::runner::ensure_running();
            } else {
                log::info!("Runner: staying off (global switch off or no projects enabled)");
            }
            // 待办租户 G4: the scheduler tick runs UNCONDITIONALLY (it idles
            // cheaply with no schedule configured; its own checks gate every
            // action) — its lifecycle must not depend on the Runner switches
            // being ON at app start (audit B-3).
            crate::todo_sweep::start_tick_thread();

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
