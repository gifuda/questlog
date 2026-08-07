use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::TrayIconId,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    App, Manager, WebviewUrl, WebviewWindowBuilder,
};

const POPUP_W: f64 = 340.0;
const POPUP_H: f64 = 800.0;

/// Bilingual tray labels (MVP): the menu is built once at startup and not yet
/// wired to the in-app language switcher. Menu ids stay stable — the
/// on_menu_event handler matches "show"/"quit".
const MENU_SHOW: &str = "显示面板 / Show Panel";
const MENU_QUIT: &str = "退出 / Quit";
const TRAY_TOOLTIP: &str = "桌面清单贴 / QuestLog";

pub fn setup_tray(app: &App) {
    let show = MenuItemBuilder::with_id("show", MENU_SHOW)
        .build(app)
        .unwrap_or_else(|e| {
            eprintln!("[tray] failed to build show menu item: {e}");
            panic!("menu build failed: {e}");
        });
    let quit = MenuItemBuilder::with_id("quit", MENU_QUIT)
        .build(app)
        .unwrap_or_else(|e| {
            eprintln!("[tray] failed to build quit menu item: {e}");
            panic!("menu build failed: {e}");
        });

    let menu = MenuBuilder::new(app)
        .items(&[&show, &quit])
        .build()
        .unwrap_or_else(|e| {
            eprintln!("[tray] failed to build menu: {e}");
            panic!("menu build failed: {e}");
        });

    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray/tray-44.png"))
        .unwrap_or_else(|e| {
            eprintln!("[tray] failed to load tray icon: {e}");
            app.default_window_icon()
                .cloned()
                .unwrap_or_else(|| tauri::image::Image::new_owned(vec![], 1, 1))
        });

    let _tray = TrayIconBuilder::with_id(TrayIconId::new("main"))
        .icon(icon)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip(TRAY_TOOLTIP)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            "quit" => {
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                rect: click_rect,
                ..
            } = event
            {
                let app = tray.app_handle();

                if let Some(popup) = app.get_webview_window("popup") {
                    if popup.is_visible().unwrap_or(false) {
                        let _ = popup.hide();
                    } else {
                        position_popup_from_rect(&click_rect, &popup);
                        let _ = popup.show();
                        let _ = popup.set_focus();
                    }
                } else {
                    let popup = match WebviewWindowBuilder::new(
                        app,
                        "popup",
                        WebviewUrl::App("/index.html#popup".into()),
                    )
                    .title("")
                    .inner_size(POPUP_W, POPUP_H)
                    .visible(false)
                    .transparent(true)
                    .decorations(false)
                    .always_on_top(true)
                    .skip_taskbar(true)
                    .resizable(false)
                    .build()
                    {
                        Ok(w) => w,
                        Err(e) => {
                            eprintln!("[tray] failed to create popup: {e}");
                            return;
                        }
                    };

                    position_popup_from_rect(&click_rect, &popup);

                    let popup_clone = popup.clone();
                    popup.on_window_event(move |event| {
                        if let tauri::WindowEvent::Focused(false) = event {
                            let _ = popup_clone.hide();
                        }
                    });

                    let _ = popup.show();
                    let _ = popup.set_focus();
                }
            }
        })
        .build(app);

    // Badge update timer
    let app_handle = app.handle().clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(30));
        let state = app_handle.state::<crate::commands::AppState>();
        let total = if let Ok(projects) = state.projects.lock() {
            projects
                .iter()
                .map(|(_, path)| {
                    crate::store::read_todos(path)
                        .map(|d| {
                            d.items
                                .iter()
                                .filter(|i| {
                                    i.status == crate::models::ItemStatus::Active
                                        && i.item_type == crate::models::ItemType::Todo
                                })
                                .count()
                        })
                        .unwrap_or(0)
                })
                .sum()
        } else {
            0
        };
        if let Some(tray) = app_handle.tray_by_id("main") {
            let badge = if total > 0 {
                total.to_string()
            } else {
                String::new()
            };
            let _ = tray.set_title(Some(&badge));
        }
    });
}

/// Position popup below the tray icon.
fn position_popup_from_rect(rect: &tauri::Rect, popup: &tauri::WebviewWindow) {
    let (px, py) = match rect.position {
        tauri::Position::Physical(p) => (p.x as f64, p.y as f64),
        tauri::Position::Logical(l) => (l.x, l.y),
    };
    let (sw, sh) = match rect.size {
        tauri::Size::Physical(s) => (s.width as f64, s.height as f64),
        tauri::Size::Logical(l) => (l.width, l.height),
    };

    let scale = popup.scale_factor().unwrap_or(2.0);
    let x = ((px + sw) - POPUP_W * scale).max(0.0);
    let y = py + sh + 4.0 * scale;

    let _ = popup.set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(
        x as i32, y as i32,
    )));
}

/// Request user attention (dock bounce) on macOS
pub fn bounce_notification(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.request_user_attention(Some(tauri::UserAttentionType::Informational));
    }
}
