mod commands;
mod discovery;
mod epic_store;
mod models;
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
        ])
        .setup(|app| {
            tray::setup_tray(app);
            let handle = app.handle().clone();
            watcher::start_watching(&handle);

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
