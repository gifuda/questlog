// Tauri commands take one argument per frontend-invoked field — that is the
// framework's standard shape (frontend calls `invoke("add_todo", {...})` by
// name). Refactoring to a struct argument would break every frontend call
// site, so the linter is disabled for this module rather than changing the API.
#![allow(clippy::too_many_arguments)]

use crate::epic_store;
use crate::models::{
    Epic, EpicStatus, ItemStatus, ItemType, Priority, Step, StepStage, TodoItem, TodosFile,
};
use crate::{discovery, store};
use std::path::PathBuf;
use tauri::{Manager, State};

pub struct AppState {
    pub projects: std::sync::Mutex<Vec<(String, PathBuf)>>,
}

/// Validate a project_path against an explicit registry allowlist:
/// canonicalize it, require it exists as a directory, and require it matches
/// a registered project (the UI only ever operates on registered projects).
/// Returns the canonical PathBuf on success.
pub fn validate_project_path_list(
    project_path: &str,
    projects: &[(String, PathBuf)],
) -> Result<PathBuf, String> {
    let raw = PathBuf::from(project_path);
    let canonical = raw
        .canonicalize()
        .map_err(|e| format!("invalid project_path '{}': {e}", raw.display()))?;
    if !canonical.is_dir() {
        return Err(format!(
            "invalid project_path '{}': not a directory",
            canonical.display()
        ));
    }
    let registered = projects
        .iter()
        .any(|(_, p)| p.canonicalize().map(|c| c == canonical).unwrap_or(false));
    if !registered {
        return Err(format!(
            "invalid project_path '{}': not a registered project",
            canonical.display()
        ));
    }
    Ok(canonical)
}

/// Forward webview JS errors to the stdout log. WKWebView console is
/// invisible in release builds; this bridge makes render-time crashes
/// diagnosable (see main.tsx error listeners).
#[tauri::command]
pub fn diag_report_error(msg: String) {
    eprintln!("[webview-error] {msg}");
}

#[tauri::command]
pub fn get_projects(state: State<AppState>) -> Result<Vec<serde_json::Value>, String> {
    if crate::remote_client::enabled() {
        // A1: counts/active_epics/last_active_at are assembled SERVER-side
        // (/api/projects-full runs get_projects_impl on authority data) —
        // no local mirror-file join for counts anymore.
        return crate::remote_client::get_into("/api/projects-full");
    }
    let projects = state.projects.lock().map_err(|e| e.to_string())?;
    get_projects_impl(&projects)
}

/// Plain core: build the project tab list (counts, active epics, last
/// activity) from an explicit registry allowlist.
pub fn get_projects_impl(
    projects: &[(String, std::path::PathBuf)],
) -> Result<Vec<serde_json::Value>, String> {
    let mut result = vec![];
    for (name, path) in projects.iter() {
        // A corrupt .todos.json must NOT hide the whole project from the board.
        // read_todos already salvages valid items when possible; if even that
        // fails, show the project tab with zero counts rather than dropping it.
        let data = match store::read_todos(path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!(
                    "[get_projects] failed to read '{name}' at {}: {e} — showing with empty data",
                    path.display()
                );
                crate::models::TodosFile {
                    version: 1,
                    project: name.clone(),
                    items: vec![],
                    revision: 0,
                }
            }
        };
        let (knowhow, todo, idea) = store::count_active(&data);
        // 方案追踪 tab 的徽标：未完成（非 done）的顶级方案数。
        // 读失败（缺文件/损坏）按 0 处理，不影响项目显示。
        let active_epics = match epic_store::read_epics(path) {
            Ok(d) => d
                .epics
                .iter()
                .filter(|e| e.parent_id.is_none() && e.status != EpicStatus::Done)
                .count(),
            Err(_) => 0,
        };
        let last_active_at = data
            .items
            .iter()
            .filter(|i| i.status == ItemStatus::Active)
            .map(|i| i.updated_at.as_str())
            .max()
            .unwrap_or("")
            .to_string();
        result.push(serde_json::json!({
            "name": name,
            "path": path.to_string_lossy(),
            "counts": { "knowhow": knowhow, "todo": todo, "idea": idea },
            "active_epics": active_epics,
            "last_active_at": last_active_at
        }));
    }
    Ok(result)
}

#[tauri::command]
pub fn get_todos(project_path: String, state: State<AppState>) -> Result<TodosFile, String> {
    if crate::remote_client::enabled() {
        return crate::remote_client::get_into(&format!(
            "/api/todos?project={}",
            crate::remote_client::urlencode(&project_path)
        ));
    }
    let projects = state.projects.lock().map_err(|e| format!("state: {e}"))?.clone();
    get_todos_impl(&projects, project_path)
}

/// Plain core: validate against an explicit allowlist and read the board file.
pub fn get_todos_impl(
    projects: &[(String, std::path::PathBuf)],
    project_path: String,
) -> Result<TodosFile, String> {
    let path = validate_project_path_list(&project_path, projects)?;
    store::read_todos(&path)
}

/// Retyping an opted-in item away from "todo" would strand an authorization
/// flag on a non-todo item — rejected (G1 audit BLOCK-1). Mirrors the MCP-side
/// guard in store.ts pushTodo/updateAndSave.
fn ensure_type_change_allowed(auto_process: bool, new_type: &ItemType) -> Result<(), String> {
    if auto_process && *new_type != ItemType::Todo {
        return Err(
            "cannot change type of an auto_process item — turn off auto_process first".into(),
        );
    }
    Ok(())
}

#[tauri::command]
pub fn update_todo(
    project_path: String,
    state: State<AppState>,
    id: String,
    status: Option<String>,
    content: Option<String>,
    tags: Option<Vec<String>>,
    priority: Option<String>,
    title: Option<String>,
    item_type: Option<String>,
    processing: Option<bool>,
    auto_process: Option<bool>,
    auto_ack: Option<bool>,
) -> Result<(), String> {
    if crate::remote_client::enabled() {
        let body = serde_json::json!({
            "project_path": project_path,
            "id": id,
            "status": status,
            "content": content,
            "tags": tags,
            "priority": priority,
            "title": title,
            "item_type": item_type,
            "processing": processing,
            "auto_process": auto_process,
            "auto_ack": auto_ack,
        });
        crate::remote_client::post_json("/api/todos/update", &body)?;
        return Ok(());
    }
    let projects = state.projects.lock().map_err(|e| format!("state: {e}"))?.clone();
    update_todo_impl(
        &projects,
        project_path,
        id,
        status,
        content,
        tags,
        priority,
        title,
        item_type,
        processing,
        auto_process,
        auto_ack,
    )
}

/// Plain core: projects = registry allowlist (API routes pass
/// discover_projects()). Body identical to the former command body.
pub fn update_todo_impl(
    projects: &[(String, std::path::PathBuf)],
    project_path: String,
    id: String,
    status: Option<String>,
    content: Option<String>,
    tags: Option<Vec<String>>,
    priority: Option<String>,
    title: Option<String>,
    item_type: Option<String>,
    processing: Option<bool>,
    auto_process: Option<bool>,
    auto_ack: Option<bool>,
) -> Result<(), String> {
    let path = validate_project_path_list(&project_path, projects)?;
    let mut archived_quickly = false;

    // G1: every .todos.json write goes through mutate_todos — cross-process
    // lock held for the whole read-modify-write, unknown fields preserved.
    store::mutate_todos(&path, |root| {
        let items = root
            .get_mut("items")
            .and_then(|v| v.as_array_mut())
            .ok_or_else(|| "items is not an array".to_string())?;
        let item_val = items
            .iter_mut()
            .find(|it| it.get("id").and_then(|v| v.as_str()) == Some(id.as_str()))
            .ok_or_else(|| format!("item '{}' not found", id))?;

        // Typed view for validation. The WRITE path never coerces: an item
        // that fails strict deserialization aborts the update instead of
        // being silently repaired into the file on disk.
        let mut typed: TodoItem = serde_json::from_value(item_val.clone())
            .map_err(|e| format!("item '{}' failed strict parse: {e}", id))?;

        if let Some(s) = &status {
            typed.status = ItemStatus::from_str(s)?;
            // Completing/archiving clears the in-progress flag
            if typed.status != ItemStatus::Active {
                typed.processing = false;
            }
        }
        if let Some(c) = &content {
            typed.content = c.clone();
        }
        if let Some(t) = &tags {
            typed.tags = t.clone();
        }
        if let Some(p) = &priority {
            typed.priority = Priority::from_str(p)?;
        }
        if let Some(t) = &title {
            typed.title = t.clone();
        }
        if let Some(ty) = &item_type {
            let new_type = ItemType::from_str(ty)?;
            ensure_type_change_allowed(typed.auto_process, &new_type)?;
            typed.item_type = new_type;
        }
        if let Some(pr) = processing {
            typed.processing = pr;
        }
        // 夜间自动处理开关（待办租户 §2.2）：off→on 递增授权修订——旧结果
        // 永不跨修订复用；开在非 todo 类型上直接拒绝（fail-closed）。
        // 夜间完成确认（review gate）：确认后卡片进入普通完成区
        if let Some(ack) = auto_ack {
            if ack {
                typed.auto_ack_at = Some(chrono::Utc::now().to_rfc3339());
            }
        }
        if let Some(ap) = auto_process {
            if ap && typed.item_type != ItemType::Todo {
                return Err("auto_process opt-in is only valid for type='todo' items".into());
            }
            if ap && !typed.auto_process {
                typed.auto_process_revision = typed.auto_process_revision.saturating_add(1).max(1);
            }
            typed.auto_process = ap;
        }

        // Quality feedback: if an item gets archived within 48h of creation,
        // record it as a low-value push signal for the MCP side to read.
        archived_quickly = typed.status == ItemStatus::Archived
            && chrono::DateTime::parse_from_rfc3339(&typed.created_at)
                .map(|created| (chrono::Utc::now() - created.to_utc()).num_hours() < 48)
                .unwrap_or(false);

        typed.updated_at = chrono::Utc::now().to_rfc3339();

        // Patch ONLY the serialized known fields back onto the raw item
        // object — every other key (unknown/future fields) survives as-is.
        let patched =
            serde_json::to_value(&typed).map_err(|e| format!("serialize error: {e}"))?;
        let obj = item_val
            .as_object_mut()
            .ok_or_else(|| "item is not an object".to_string())?;
        for (k, v) in patched.as_object().unwrap() {
            obj.insert(k.clone(), v.clone());
        }
        Ok(())
    })?;

    if archived_quickly {
        discovery::record_archived_soon(&project_path);
    }

    Ok(())
}

#[tauri::command]
pub fn add_todo(
    project_path: String,
    state: State<AppState>,
    item_type: String,
    title: String,
    content: Option<String>,
    suggestion: Option<String>,
    tags: Option<Vec<String>>,
    priority: Option<String>,
) -> Result<TodoItem, String> {
    if crate::remote_client::enabled() {
        let body = serde_json::json!({
            "project_path": project_path,
            "item_type": item_type,
            "title": title,
            "content": content,
            "suggestion": suggestion,
            "tags": tags,
            "priority": priority,
        });
        let v = crate::remote_client::post_json("/api/todos/add", &body)?;
        return serde_json::from_value(v).map_err(|e| format!("api shape mismatch: {e}"));
    }
    let projects = state.projects.lock().map_err(|e| format!("state: {e}"))?.clone();
    add_todo_impl(
        &projects,
        project_path,
        item_type,
        title,
        content,
        suggestion,
        tags,
        priority,
    )
}

/// Plain core: projects = registry allowlist (API routes pass
/// discover_projects()). Body identical to the former command body.
pub fn add_todo_impl(
    projects: &[(String, std::path::PathBuf)],
    project_path: String,
    item_type: String,
    title: String,
    content: Option<String>,
    suggestion: Option<String>,
    tags: Option<Vec<String>>,
    priority: Option<String>,
) -> Result<TodoItem, String> {
    let path = validate_project_path_list(&project_path, projects)?;

    let now = chrono::Utc::now().to_rfc3339();
    let item = TodoItem {
        id: uuid::Uuid::new_v4().to_string(),
        item_type: ItemType::from_str(&item_type)?,
        title,
        content: content.unwrap_or_default(),
        suggestion: suggestion.unwrap_or_default(),
        tags: tags.unwrap_or_default(),
        status: ItemStatus::Active,
        priority: priority
            .as_deref()
            .map(Priority::from_str)
            .transpose()?
            .unwrap_or(Priority::Medium),
        source_session: String::new(),
        processing: false,
        auto_process: false,
        auto_process_revision: 0,
        auto_result: None,
        auto_completed_at: None,
        auto_ack_at: None,
        created_at: now.clone(),
        updated_at: now,
    };

    let dir_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    store::mutate_todos(&path, |root| {
        let items = root
            .get_mut("items")
            .and_then(|v| v.as_array_mut())
            .ok_or_else(|| "items is not an array".to_string())?;
        items.push(
            serde_json::to_value(&item).map_err(|e| format!("serialize error: {e}"))?,
        );
        if let Some(obj) = root.as_object_mut() {
            obj.insert("project".into(), serde_json::Value::String(dir_name.clone()));
        }
        Ok(())
    })?;
    Ok(item)
}

#[tauri::command]
pub fn remove_project(name: String, state: State<AppState>) -> Result<(String, String), String> {
    let projects = state.projects.lock().map_err(|e| e.to_string())?;
    let path = projects
        .iter()
        .find(|(n, _)| n == &name)
        .map(|(_, p)| p.to_string_lossy().to_string())
        .ok_or_else(|| format!("project '{}' not found", name))?;
    drop(projects);

    discovery::remove_project(&name)?;
    let mut projects = state.projects.lock().map_err(|e| e.to_string())?;
    projects.retain(|(n, _)| n != &name);
    Ok((name, path))
}

#[tauri::command]
pub fn undo_remove_project(
    name: String,
    path: String,
    state: State<AppState>,
) -> Result<(), String> {
    discovery::add_project(&name, &path)?;
    let mut projects = state.projects.lock().map_err(|e| e.to_string())?;
    projects.push((name, PathBuf::from(&path)));
    Ok(())
}

#[tauri::command]
pub fn get_trash() -> Result<Vec<discovery::TrashEntry>, String> {
    if crate::remote_client::enabled() {
        return crate::remote_client::get_into("/api/trash");
    }
    discovery::list_trash()
}

#[tauri::command]
pub fn restore_project(name: String, state: State<AppState>) -> Result<(), String> {
    discovery::restore_project(&name)?;
    // Refresh in-memory list
    let new_projects = discovery::discover_projects();
    let mut projects = state.projects.lock().map_err(|e| e.to_string())?;
    *projects = new_projects;
    Ok(())
}

#[tauri::command]
pub fn permanently_delete_project(name: String) -> Result<(), String> {
    discovery::permanently_delete(&name)
}

#[tauri::command]
pub fn reorder_projects(names: Vec<String>, state: State<AppState>) -> Result<(), String> {
    discovery::reorder_projects(&names)?;
    let mut projects = state.projects.lock().map_err(|e| e.to_string())?;
    let owned: Vec<(String, PathBuf)> = projects.drain(..).collect();
    let mut reordered: Vec<(String, PathBuf)> = Vec::new();
    for name in &names {
        if let Some((n, p)) = owned.iter().find(|(n, _)| n == name) {
            reordered.push((n.clone(), p.clone()));
        }
    }
    for entry in &owned {
        if !names.iter().any(|n| n == &entry.0) {
            reordered.push(entry.clone());
        }
    }
    *projects = reordered;
    Ok(())
}

/// Get active items for a single project, grouped by type.
/// Used by the floating panel project detail view.
#[tauri::command]
pub fn get_project_items(
    project_path: String,
    state: State<AppState>,
) -> Result<serde_json::Value, String> {
    if crate::remote_client::enabled() {
        return crate::remote_client::get_into(&format!(
            "/api/project-items?project={}",
            crate::remote_client::urlencode(&project_path)
        ));
    }
    let projects = state.projects.lock().map_err(|e| format!("state: {e}"))?.clone();
    get_project_items_impl(&projects, project_path)
}

/// Plain core: active items for one project grouped by type.
pub fn get_project_items_impl(
    projects: &[(String, std::path::PathBuf)],
    project_path: String,
) -> Result<serde_json::Value, String> {
    let path = validate_project_path_list(&project_path, projects)?;
    let data = store::read_todos(&path)?;
    let project_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    let mut knowhow: Vec<serde_json::Value> = vec![];
    let mut todo: Vec<serde_json::Value> = vec![];
    let mut idea: Vec<serde_json::Value> = vec![];

    for item in data.items.iter().filter(|i| i.status == ItemStatus::Active) {
        let entry = serde_json::json!({
            "id": item.id,
            "type": item.item_type.as_str(),
            "title": item.title,
            "priority": item.priority.as_str(),
            "processing": item.processing,
            "updated_at": item.updated_at,
        });
        match item.item_type {
            ItemType::Knowhow => knowhow.push(entry),
            ItemType::Todo => todo.push(entry),
            ItemType::Idea => idea.push(entry),
        }
    }

    Ok(serde_json::json!({
        "project": project_name,
        "project_path": project_path,
        "knowhow": knowhow,
        "todo": todo,
        "idea": idea,
    }))
}

/// Get all active items across all projects, sorted by updated_at desc.
/// Used by the floating panel timeline view.
#[tauri::command]
pub fn get_all_active(state: State<AppState>) -> Result<Vec<serde_json::Value>, String> {
    if crate::remote_client::enabled() {
        return crate::remote_client::get_into("/api/all-active");
    }
    let projects = state.projects.lock().map_err(|e| e.to_string())?;
    get_all_active_impl(&projects)
}

/// Plain core: all active items across the given registry, newest first.
pub fn get_all_active_impl(
    projects: &[(String, std::path::PathBuf)],
) -> Result<Vec<serde_json::Value>, String> {
    let mut items: Vec<serde_json::Value> = vec![];
    for (name, path) in projects.iter() {
        let data = match store::read_todos(path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!(
                    "[get_all_active] failed to read '{name}' at {}: {e}",
                    path.display()
                );
                continue;
            }
        };
        for item in data.items.iter().filter(|i| i.status == ItemStatus::Active) {
            items.push(serde_json::json!({
                "id": item.id,
                "type": item.item_type.as_str(),
                "title": item.title,
                "priority": item.priority.as_str(),
                "processing": item.processing,
                "updated_at": item.updated_at,
                "project": name,
                "project_path": path.to_string_lossy(),
            }));
        }
    }
    items.sort_by(|a, b| {
        let a_time = a.get("updated_at").and_then(|v| v.as_str()).unwrap_or("");
        let b_time = b.get("updated_at").and_then(|v| v.as_str()).unwrap_or("");
        b_time.cmp(a_time)
    });
    Ok(items)
}

#[tauri::command]
pub fn show_main_window(app: tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[tauri::command]
pub fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}

// ════════════════════════════════════════════════════════════
// 方案追踪模块 (Epic / Step) — 与待办完全隔离的命令集
// ════════════════════════════════════════════════════════════

/// Get epics + steps + derived progress for a project.
/// Returns a JSON structure that the frontend can render directly.
#[tauri::command]
pub fn get_epics(
    project_path: String,
    state: State<AppState>,
) -> Result<serde_json::Value, String> {
    if crate::remote_client::enabled() {
        // /api/epics returns the RAW EpicsFile; the frontend shape is built
        // by the same local pure function so remote == local output.
        let data: crate::models::EpicsFile = crate::remote_client::get_into(&format!(
            "/api/epics?project={}",
            crate::remote_client::urlencode(&project_path)
        ))?;
        return Ok(epics_to_json(&data, &project_path));
    }
    let path = {
        let projects = state.projects.lock().map_err(|e| e.to_string())?;
        validate_project_path_list(&project_path, &projects)?
    };
    let data = epic_store::read_epics(&path)?;
    Ok(epics_to_json(&data, &project_path))
}

/// Global summary across ALL registered projects (aggregate dashboard).
#[tauri::command]
pub fn get_epics_global(state: State<AppState>) -> Result<serde_json::Value, String> {
    if crate::remote_client::enabled() {
        return crate::remote_client::get_into("/api/epics-global");
    }
    let projects = state.projects.lock().map_err(|e| e.to_string())?;
    get_epics_global_impl(&projects)
}

/// Plain core: aggregate epic summary across the given registry.
pub fn get_epics_global_impl(
    projects: &[(String, std::path::PathBuf)],
) -> Result<serde_json::Value, String> {
    let mut all_epics: Vec<serde_json::Value> = vec![];
    for (name, path) in projects.iter() {
        let data = match epic_store::read_epics(path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!(
                    "[get_epics_global] failed to read '{name}' at {}: {e}",
                    path.display()
                );
                continue;
            }
        };
        for epic in &data.epics {
            let progress = epic_store::epic_progress(&epic.id, &data);
            let step_count = data.steps.iter().filter(|s| s.epic_id == epic.id).count();
            all_epics.push(serde_json::json!({
                "id": epic.id,
                "title": epic.title,
                "description": epic.description,
                "status": epic.status.as_str(),
                "status_label": epic.status.label(),
                "priority": epic.priority,
                "depends_on": epic.depends_on,
                "blocks": epic.blocks,
                "progress": progress,
                "step_count": step_count,
                "project": name,
                "project_path": path.to_string_lossy(),
            }));
        }
    }
    // Sort: active first, then by priority
    all_epics.sort_by(|a, b| {
        let sa = a.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let sb = b.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let rank = |s: &str| match s {
            "active" => 0,
            "verifying" => 1,
            "observing" => 2,
            "blocked" => 3,
            "draft" => 4,
            "paused" => 5,
            "done" => 6,
            _ => 9,
        };
        rank(sa).cmp(&rank(sb)).then_with(|| {
            let pa = a.get("priority").and_then(|v| v.as_str()).unwrap_or("P9");
            let pb = b.get("priority").and_then(|v| v.as_str()).unwrap_or("P9");
            pa.cmp(pb)
        })
    });
    Ok(serde_json::json!({ "epics": all_epics }))
}

/// Add a new epic to a project.
/// `parent_id`: 可选。填了则作为子方案挂到父方案下（最多三层，子方案不能再有子方案）。
#[tauri::command]
pub fn add_epic(
    project_path: String,
    state: State<AppState>,
    title: String,
    description: Option<String>,
    priority: Option<String>,
    depends_on: Option<Vec<String>>,
    parent_id: Option<String>,
    acceptance: Option<String>,
) -> Result<serde_json::Value, String> {
    if crate::remote_client::enabled() {
        let body = serde_json::json!({
            "project_path": project_path,
            "title": title,
            "description": description,
            "priority": priority,
            "depends_on": depends_on,
            "parent_id": parent_id,
            "acceptance": acceptance,
        });
        return crate::remote_client::post_json("/api/epics/add", &body);
    }
    let projects = state.projects.lock().map_err(|e| format!("state: {e}"))?.clone();
    add_epic_impl(
        &projects,
        project_path,
        title,
        description,
        priority,
        depends_on,
        parent_id,
        acceptance,
    )
}

/// Plain core: projects = registry allowlist. Body identical to the former
/// command body.
pub fn add_epic_impl(
    projects: &[(String, std::path::PathBuf)],
    project_path: String,
    title: String,
    description: Option<String>,
    priority: Option<String>,
    depends_on: Option<Vec<String>>,
    parent_id: Option<String>,
    acceptance: Option<String>,
) -> Result<serde_json::Value, String> {
    let path = validate_project_path_list(&project_path, projects)?;
    let now = chrono::Utc::now().to_rfc3339();
    let id = format!("epic_{}", uuid::Uuid::new_v4().simple());

    // 层级校验：parent_id 必须指向一个存在的顶级方案（三层：顶级→子→步骤）
    if let Some(pid) = &parent_id {
        let existing = epic_store::read_epics(&path)?;
        let parent = existing
            .epics
            .iter()
            .find(|e| e.id == *pid)
            .ok_or_else(|| format!("parent epic '{}' not found", pid))?;
        if parent.parent_id.is_some() {
            return Err("最多三层：子方案不能再有子方案。把步骤直接加到该子方案下。".to_string());
        }
    }

    let new_epic = Epic {
        id: id.clone(),
        title,
        description: description.unwrap_or_default(),
        status: EpicStatus::Draft,
        priority: priority.unwrap_or_else(|| "P1".to_string()),
        parent_id,
        depends_on: depends_on.unwrap_or_default(),
        blocks: vec![],
        links: Default::default(),
        max_active: None,
        auto_execute: None,
        acceptance: acceptance.unwrap_or_default(),
        close_reason: String::new(),
        created_at: now.clone(),
        updated_at: now,
    };

    let data = epic_store::mutate_epics(&path, |file| {
        file.epics.push(new_epic.clone());
        Ok(())
    })?;

    Ok(epics_to_json(&data, &project_path))
}

/// Update an epic's mutable fields (title, description, status, priority, depends_on, links).
#[tauri::command]
pub fn update_epic(
    project_path: String,
    state: State<AppState>,
    id: String,
    title: Option<String>,
    description: Option<String>,
    status: Option<String>,
    priority: Option<String>,
    depends_on: Option<Vec<String>>,
    links_doc: Option<String>,
    links_branch: Option<String>,
    links_note: Option<String>,
    acceptance: Option<String>,
    close_reason: Option<String>,
) -> Result<serde_json::Value, String> {
    if crate::remote_client::enabled() {
        let body = serde_json::json!({
            "project_path": project_path,
            "id": id,
            "title": title,
            "description": description,
            "status": status,
            "priority": priority,
            "depends_on": depends_on,
            "links_doc": links_doc,
            "links_branch": links_branch,
            "links_note": links_note,
            "acceptance": acceptance,
            "close_reason": close_reason,
        });
        return crate::remote_client::post_json("/api/epics/update", &body);
    }
    let projects = state.projects.lock().map_err(|e| format!("state: {e}"))?.clone();
    update_epic_impl(
        &projects,
        project_path,
        id,
        title,
        description,
        status,
        priority,
        depends_on,
        links_doc,
        links_branch,
        links_note,
        acceptance,
        close_reason,
    )
}

/// Plain core: projects = registry allowlist. Body identical to the former
/// command body.
pub fn update_epic_impl(
    projects: &[(String, std::path::PathBuf)],
    project_path: String,
    id: String,
    title: Option<String>,
    description: Option<String>,
    status: Option<String>,
    priority: Option<String>,
    depends_on: Option<Vec<String>>,
    links_doc: Option<String>,
    links_branch: Option<String>,
    links_note: Option<String>,
    acceptance: Option<String>,
    close_reason: Option<String>,
) -> Result<serde_json::Value, String> {
    let path = validate_project_path_list(&project_path, projects)?;
    let new_status = match &status {
        Some(s) => Some(EpicStatus::from_str(s)?),
        None => None,
    };

    let data = epic_store::mutate_epics(&path, |file| {
        // done 门禁：进度<100% 且 close_reason 空 → 拒绝。
        // 必须在设置 epic.status 之前算进度（progress 不受 status 影响，只受 step stage 影响，
        // 所以取 set 之前的进度值和最终的等价）。
        if new_status == Some(EpicStatus::Done) {
            let progress = epic_store::epic_progress(&id, file);
            if progress < 100 {
                let reason = close_reason.as_deref().unwrap_or("").trim();
                if reason.is_empty() {
                    return Err(format!(
                        "进度 {}% 未达 100%，提前收尾需填写 close_reason（收尾原因，如\"方向调整\"）",
                        progress
                    ));
                }
            }
        }

        let epic = file
            .epics
            .iter_mut()
            .find(|e| e.id == id)
            .ok_or_else(|| format!("epic '{}' not found", id))?;
        if let Some(t) = title {
            epic.title = t;
        }
        if let Some(d) = description {
            epic.description = d;
        }
        if let Some(s) = new_status {
            epic.status = s;
        }
        if let Some(p) = priority {
            epic.priority = p;
        }
        if let Some(d) = depends_on {
            epic.depends_on = d;
        }
        if let Some(v) = links_doc {
            epic.links.doc = if v.is_empty() { None } else { Some(v) };
        }
        if let Some(v) = links_branch {
            epic.links.branch = if v.is_empty() { None } else { Some(v) };
        }
        if let Some(v) = links_note {
            epic.links.note = if v.is_empty() { None } else { Some(v) };
        }
        if let Some(a) = acceptance {
            epic.acceptance = a;
        }
        if let Some(r) = close_reason {
            epic.close_reason = r;
        }
        epic.updated_at = chrono::Utc::now().to_rfc3339();
        Ok(())
    })?;

    Ok(epics_to_json(&data, &project_path))
}

/// Delete an epic and its steps.
#[tauri::command]
pub fn delete_epic(
    project_path: String,
    state: State<AppState>,
    id: String,
) -> Result<serde_json::Value, String> {
    if crate::remote_client::enabled() {
        let body = serde_json::json!({ "project_path": project_path, "id": id });
        return crate::remote_client::post_json("/api/epics/delete", &body);
    }
    let projects = state.projects.lock().map_err(|e| format!("state: {e}"))?.clone();
    delete_epic_impl(&projects, project_path, id)
}

/// Plain core: delete an epic and its steps (registry allowlist validated).
pub fn delete_epic_impl(
    projects: &[(String, std::path::PathBuf)],
    project_path: String,
    id: String,
) -> Result<serde_json::Value, String> {
    let path = validate_project_path_list(&project_path, projects)?;
    let data = epic_store::mutate_epics(&path, |file| {
        file.epics.retain(|e| e.id != id);
        file.steps.retain(|s| s.epic_id != id);
        // Also remove this epic from other epics' depends_on
        for e in file.epics.iter_mut() {
            e.depends_on.retain(|d| d != &id);
        }
        Ok(())
    })?;
    Ok(epics_to_json(&data, &project_path))
}

/// Add a step to an epic.
#[tauri::command]
pub fn add_step(
    project_path: String,
    state: State<AppState>,
    epic_id: String,
    title: String,
    description: Option<String>,
    depends_on: Option<Vec<String>>,
) -> Result<serde_json::Value, String> {
    if crate::remote_client::enabled() {
        let body = serde_json::json!({
            "project_path": project_path,
            "epic_id": epic_id,
            "title": title,
            "description": description,
            "depends_on": depends_on,
        });
        return crate::remote_client::post_json("/api/steps/add", &body);
    }
    let projects = state.projects.lock().map_err(|e| format!("state: {e}"))?.clone();
    add_step_impl(&projects, project_path, epic_id, title, description, depends_on)
}

/// Plain core: add a step (registry allowlist validated).
pub fn add_step_impl(
    projects: &[(String, std::path::PathBuf)],
    project_path: String,
    epic_id: String,
    title: String,
    description: Option<String>,
    depends_on: Option<Vec<String>>,
) -> Result<serde_json::Value, String> {
    let path = validate_project_path_list(&project_path, projects)?;
    let now = chrono::Utc::now().to_rfc3339();
    let id = format!("step_{}", uuid::Uuid::new_v4().simple());

    // Determine order = max existing order in this epic + 1
    let data = epic_store::read_epics(&path)?;
    let next_order = data
        .steps
        .iter()
        .filter(|s| s.epic_id == epic_id)
        .map(|s| s.order)
        .max()
        .unwrap_or(0)
        + 1;

    let new_step = Step {
        id: id.clone(),
        epic_id: epic_id.clone(),
        title,
        description: description.unwrap_or_default(),
        order: next_order,
        depends_on: depends_on.unwrap_or_default(),
        stage: StepStage::Pending,
        links: Default::default(),
        package: None,
        created_at: now.clone(),
        updated_at: now,
    };

    let data = epic_store::mutate_epics(&path, |file| {
        // Bump epic updated_at
        if let Some(epic) = file.epics.iter_mut().find(|e| e.id == epic_id) {
            epic.updated_at = chrono::Utc::now().to_rfc3339();
        }
        file.steps.push(new_step);
        Ok(())
    })?;

    Ok(epics_to_json(&data, &project_path))
}

/// Update a step's stage (the progress slider) or other fields.
#[tauri::command]
pub fn update_step(
    project_path: String,
    state: State<AppState>,
    id: String,
    title: Option<String>,
    description: Option<String>,
    stage: Option<String>,
    order: Option<u32>,
    depends_on: Option<Vec<String>>,
    todo_ref: Option<String>,
) -> Result<serde_json::Value, String> {
    if crate::remote_client::enabled() {
        let body = serde_json::json!({
            "project_path": project_path,
            "id": id,
            "title": title,
            "description": description,
            "stage": stage,
            "order": order,
            "depends_on": depends_on,
            "todo_ref": todo_ref,
        });
        return crate::remote_client::post_json("/api/steps/update", &body);
    }
    let projects = state.projects.lock().map_err(|e| format!("state: {e}"))?.clone();
    update_step_impl(
        &projects,
        project_path,
        id,
        title,
        description,
        stage,
        order,
        depends_on,
        todo_ref,
    )
}

/// Plain core: update a step (registry allowlist validated).
pub fn update_step_impl(
    projects: &[(String, std::path::PathBuf)],
    project_path: String,
    id: String,
    title: Option<String>,
    description: Option<String>,
    stage: Option<String>,
    order: Option<u32>,
    depends_on: Option<Vec<String>>,
    todo_ref: Option<String>,
) -> Result<serde_json::Value, String> {
    let path = validate_project_path_list(&project_path, projects)?;
    let new_stage = match &stage {
        Some(s) => Some(StepStage::from_str(s)?),
        None => None,
    };

    let data = epic_store::mutate_epics(&path, |file| {
        let step = file
            .steps
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or_else(|| format!("step '{}' not found", id))?;
        if let Some(t) = title {
            step.title = t;
        }
        if let Some(d) = description {
            step.description = d;
        }
        if let Some(s) = new_stage {
            step.stage = s;
        }
        if let Some(o) = order {
            step.order = o;
        }
        if let Some(d) = depends_on {
            step.depends_on = d;
        }
        if let Some(r) = todo_ref {
            step.links.todo_ref = if r.is_empty() { None } else { Some(r) };
        }
        step.updated_at = chrono::Utc::now().to_rfc3339();

        // Bump parent epic's updated_at
        let eid = step.epic_id.clone();
        if let Some(epic) = file.epics.iter_mut().find(|e| e.id == eid) {
            epic.updated_at = chrono::Utc::now().to_rfc3339();
        }
        Ok(())
    })?;

    Ok(epics_to_json(&data, &project_path))
}

/// Delete a step.
#[tauri::command]
pub fn delete_step(
    project_path: String,
    state: State<AppState>,
    id: String,
) -> Result<serde_json::Value, String> {
    if crate::remote_client::enabled() {
        let body = serde_json::json!({ "project_path": project_path, "id": id });
        return crate::remote_client::post_json("/api/steps/delete", &body);
    }
    let projects = state.projects.lock().map_err(|e| format!("state: {e}"))?.clone();
    delete_step_impl(&projects, project_path, id)
}

/// Plain core: delete a step (registry allowlist validated).
pub fn delete_step_impl(
    projects: &[(String, std::path::PathBuf)],
    project_path: String,
    id: String,
) -> Result<serde_json::Value, String> {
    let path = validate_project_path_list(&project_path, projects)?;
    let data = epic_store::mutate_epics(&path, |file| {
        file.steps.retain(|s| s.id != id);
        Ok(())
    })?;
    Ok(epics_to_json(&data, &project_path))
}

// ════════════════════════════════════════════════════════════
// 套利机会栏 (Arb Board) — GROS board.json 的只读视图
// App 级全局功能，不按项目区分。纯只读：SQLite 是 GROS 的真相源，
// app 不提供任何写操作。
// ════════════════════════════════════════════════════════════

/// Read the global GROS board export. Read-only. Returns a status envelope so
/// the frontend can distinguish "off" / "file missing" / "parse error" / "ok"
/// without guessing — an absent or broken file must degrade to a friendly
/// empty state, never break the panel.
#[tauri::command]
pub fn get_arb_board() -> Result<serde_json::Value, String> {
    // Off 时也带上已保存的路径，Settings 面板要显示当前配置。
    // A6: api 模式下配置来自权威 API（注册表在 m5p 侧，不再读本地镜像
    // 注册表）；board.json 本体仍读 M4 本地（设计内——M4 有本地 GROS 导出）。
    let (enabled, saved_path) = if crate::remote_client::enabled() {
        let cfg: discovery::ArbBoardConfig = crate::remote_client::get_into("/api/arb-config")?;
        (cfg.enabled, cfg.board_path)
    } else {
        match discovery::get_arb_board_config() {
            Some(c) => (c.enabled, c.board_path),
            None => (false, String::new()),
        }
    };
    if !enabled {
        return Ok(serde_json::json!({ "status": "off", "board_path": saved_path }));
    }
    let config_board_path = saved_path;

    let board_path = std::path::PathBuf::from(&config_board_path);
    if !board_path.is_absolute() {
        return Ok(serde_json::json!({
            "status": "missing",
            "board_path": config_board_path.clone(),
        }));
    }
    if !board_path.exists() {
        return Ok(serde_json::json!({
            "status": "missing",
            "board_path": config_board_path.clone(),
        }));
    }
    let raw = std::fs::read_to_string(&board_path).map_err(|e| format!("read error: {e}"))?;
    match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(mut data) => {
            // Attach the path so the frontend can show where data came from.
            if let Some(obj) = data.as_object_mut() {
                obj.insert("status".into(), serde_json::Value::String("ok".into()));
                obj.insert(
                    "board_path".into(),
                    serde_json::Value::String(config_board_path.clone()),
                );
            }
            Ok(data)
        }
        Err(e) => Ok(serde_json::json!({
            "status": "parse_error",
            "board_path": config_board_path.clone(),
            "error": e.to_string(),
        })),
    }
}

/// Enable/disable the global 套利机会栏 and set the board.json path (absolute).
/// This writes ONLY to the app's own registry — it never touches GROS data.
#[tauri::command]
pub fn set_arb_board(enabled: bool, board_path: Option<String>) -> Result<(), String> {
    discovery::set_arb_board_config(
        enabled,
        board_path.as_deref().unwrap_or("exports/board.json"),
    )
}

/// Submit an automation-control parameter card (契约 v3 B 控制区).
///
/// The card goes through the local thin-shell gros CLI into the ledger
/// (`chat-archive --conversation-id gros-config`); the collector-side
/// config-sync consumes it and the change echoes back via the next
/// board.json export. The app NEVER writes board.json itself — the control
/// section's echo is the single source of truth (anti-loop, 契约 v3).
///
/// Security: param is whitelist-mapped to the card text; values are validated
/// and only ever land in an argv slot (std::process::Command, no shell) —
/// model names come from board.json (external data) and must never reach a
/// shell string.
#[tauri::command]
pub fn submit_arb_control(param: String, value: String) -> Result<(), String> {
    let note = match param.as_str() {
        "current_model" => {
            if value.is_empty() || value.len() > 64 || value.contains(['\n', '\r', '|', '=']) {
                return Err(format!("非法模型名: {value:?}"));
            }
            format!("[gros-config] 雷达模型 = {value}")
        }
        "autonomy" => {
            let v = match value.as_str() {
                "true" | "1" => "开",
                "false" | "0" => "关",
                other => return Err(format!("非法总闸值: {other:?}")),
            };
            format!("[gros-config] 总闸 = {v}")
        }
        "leads_per_run" => {
            let n: u32 = value
                .parse()
                .map_err(|_| format!("线索上限必须是数字: {value:?}"))?;
            if !(1..=20).contains(&n) {
                return Err(format!("线索上限超出范围 1..=20: {n}"));
            }
            format!("[gros-config] 线索上限 = {n}")
        }
        "research_quota_per_week" => {
            let n: u32 = value
                .parse()
                .map_err(|_| format!("配额必须是数字: {value:?}"))?;
            if n > 50 {
                return Err(format!("每周配额超出范围 0..=50: {n}"));
            }
            format!("[gros-config] 自动研究配额 = {n}/周")
        }
        other => return Err(format!("未知参数: {other}")),
    };

    let home = dirs::home_dir().ok_or("no home dir")?;
    let gros = home.join(".local/bin/gros");
    if !gros.exists() {
        return Err(format!("gros CLI 不存在: {}", gros.display()));
    }
    let out = std::process::Command::new(&gros)
        .args(["chat-archive", "--conversation-id", "gros-config", "--note", &note])
        .output()
        .map_err(|e| format!("执行 gros 失败: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "gros chat-archive 失败: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// Execute a decision-item command draft (契约 v4 C 决策点击处置).
///
/// The draft is pre-generated by the GROS morning brief — the app NEVER
/// assembles a command, it only validates and executes. Hard whitelist:
/// the string must start with `bash ~/.local/bin/gros ` followed by one of
/// `research` / `lead-status` / `settle`; anything else is refused with
/// 「命令不在白名单」. A second guard rejects shell command-chaining
/// metacharacters so the whitelist prefix cannot be escaped mid-string.
///
/// Same single-channel discipline as the control bar: write goes through the
/// thin-shell (ledger receipt), the outcome is read back via the next
/// board.json export (handled items disappear on their own). Failure output
/// is returned verbatim; the frontend shows it without retrying.
#[tauri::command]
pub fn run_arb_decision(cmd: String) -> Result<String, String> {
    const PREFIX: &str = "bash ~/.local/bin/gros ";
    let body = cmd.strip_prefix(PREFIX).ok_or("命令不在白名单")?;
    match body.split_whitespace().next() {
        Some("research") | Some("lead-status") | Some("settle") | Some("chat-archive") => {}
        _ => return Err("命令不在白名单".into()),
    }
    // 防拼接：白名单管住了程序，还要管住同一行里再跑别的命令。
    // 引号感知——引号内是数据载荷（如 "[裁决|D1|不追]"），不算拼接。
    if shell_meta_outside_quotes(&cmd) {
        return Err("命令不在白名单".into());
    }
    let out = std::process::Command::new("bash")
        .arg("-c")
        .arg(&cmd)
        .output()
        .map_err(|e| format!("执行失败: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if out.status.success() {
        Ok(stdout.trim().to_string())
    } else {
        // 失败原样带回错误输出（stderr 优先，空则退 stdout），前端显示不重试。
        let err_out = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        // 幂等语义：服务端"状态流转不允许: X → X(...)"（同态流转）说明该决策
        // 已在账房消费——决策清单是晨报生成的快照，天然滞后于账房实况。此时
        // 按"已生效"处理（前端标记已处置），而不是让用户撞硬错误。
        if let Some(state) = same_state_transition(err_out) {
            return Ok(format!(
                "已在账房生效（当前状态: {state}）——决策清单为晨报快照，该项此前已处置"
            ));
        }
        Err(format!(
            "exit {}: {}",
            out.status.code().unwrap_or(-1),
            err_out
        ))
    }
}

/// 检测服务端同态流转错误「状态流转不允许: X → X(…)」——X→X 即目标态已是
/// 当前态，导出快照滞后的幂等信号。返回该状态名。
fn same_state_transition(err: &str) -> Option<String> {
    const MARKER: &str = "状态流转不允许: ";
    let rest = err.split(MARKER).nth(1)?;
    let pair = rest.split('(').next()?.trim();
    let mut parts = pair.split("→");
    let from = parts.next()?.trim();
    let to = parts.next()?.trim();
    if !from.is_empty() && from == to {
        Some(from.to_string())
    } else {
        None
    }
}

/// 引号感知的 shell 拼接符检查：只查引号外的 `; | & \` 换行 $( ——引号内
/// 是数据载荷（裁决文本里含 "|" 是合法的）。未闭合引号视为不安全，fail-closed。
fn shell_meta_outside_quotes(cmd: &str) -> bool {
    let mut in_single = false;
    let mut in_double = false;
    let mut iter = cmd.chars().peekable();
    while let Some(c) = iter.next() {
        if c == '\'' && !in_double {
            in_single = !in_single;
            continue;
        }
        if c == '"' && !in_single {
            in_double = !in_double;
            continue;
        }
        if in_single || in_double {
            continue;
        }
        if matches!(c, ';' | '|' | '&' | '`' | '\n' | '\r') {
            return true;
        }
        if c == '$' && iter.peek() == Some(&'(') {
            return true;
        }
    }
    in_single || in_double
}

/// Render the epics file as the JSON shape the frontend consumes.
/// Each epic carries its derived progress + step list + summary.
fn epics_to_json(data: &crate::models::EpicsFile, project_path: &str) -> serde_json::Value {
    let project_name = PathBuf::from(project_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    // 顶层方案按创建时间倒序（最新在前）；子方案在其父下也按创建时间倒序
    let mut top_order: Vec<&crate::models::Epic> = data
        .epics
        .iter()
        .filter(|e| e.parent_id.is_none())
        .collect();
    top_order.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    let mut child_order: Vec<&crate::models::Epic> = data
        .epics
        .iter()
        .filter(|e| e.parent_id.is_some())
        .collect();
    child_order.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    let epic_json_map: std::collections::HashMap<String, serde_json::Value> = data
        .epics
        .iter()
        .map(|e| {
            let progress = epic_store::epic_progress(&e.id, data);
            // 当前可做步骤 id 集合（未完成 + 前置已观察）
            let actionable_ids: Vec<String> = epic_store::actionable_steps(&e.id, &data.steps)
                .iter()
                .map(|s| s.id.clone())
                .collect();
            // S5: stable (order, created_at) ordering — the UI list, focus
            // and next-step hints must never follow JSON insertion order
            // (a step inserted later with a smaller order shows first).
            let mut ordered_steps: Vec<&crate::models::Step> = data
                .steps
                .iter()
                .filter(|s| s.epic_id == e.id)
                .collect();
            ordered_steps.sort_by(|a, b| {
                a.order
                    .cmp(&b.order)
                    .then_with(|| a.created_at.cmp(&b.created_at))
            });
            let epic_steps: Vec<serde_json::Value> = ordered_steps
                .into_iter()
                .map(|s| {
                    let depends_titles: Vec<String> = s
                        .depends_on
                        .iter()
                        .filter_map(|d| {
                            data.steps
                                .iter()
                                .find(|x| x.id == *d)
                                .map(|x| x.title.clone())
                        })
                        .collect();
                    serde_json::json!({
                        "id": s.id,
                        "epic_id": s.epic_id,
                        "title": s.title,
                        "description": s.description,
                        "order": s.order,
                        "depends_on": s.depends_on,
                        "depends_on_titles": depends_titles,
                        "stage": s.stage.as_str(),
                        "stage_label": s.stage.label(),
                        "stage_pct": s.stage.pct(),
                        "stage_icon": s.stage.icon(),
                        "actionable": actionable_ids.contains(&s.id),
                        "todo_ref": s.links.todo_ref,
                        "doc": s.links.doc,
                        "package": s.package,
                        "updated_at": s.updated_at,
                    })
                })
                .collect();

            let depends_titles: Vec<String> = e
                .depends_on
                .iter()
                .filter_map(|d| {
                    data.epics
                        .iter()
                        .find(|x| x.id == *d)
                        .map(|x| x.title.clone())
                })
                .collect();
            let blocks_titles: Vec<String> = e
                .blocks
                .iter()
                .filter_map(|b| {
                    data.epics
                        .iter()
                        .find(|x| x.id == *b)
                        .map(|x| x.title.clone())
                })
                .collect();

            (
                e.id.clone(),
                serde_json::json!({
                    "id": e.id,
                    "title": e.title,
                    "description": e.description,
                    "status": e.status.as_str(),
                    "status_label": e.status.label(),
                    "priority": e.priority,
                    "parent_id": e.parent_id,
                    "depends_on": e.depends_on,
                    "depends_on_titles": depends_titles,
                    "blocks": e.blocks,
                    "blocks_titles": blocks_titles,
                    "progress": progress,
                    "step_count": epic_steps.len(),
                    "child_count": 0, // 占位，下面按 children 实际数量覆盖
                    "links": {
                        "doc": e.links.doc,
                        "branch": e.links.branch,
                        "note": e.links.note,
                    },
                    "max_active": e.max_active,
                    "acceptance": e.acceptance,
                    "close_reason": e.close_reason,
                    "created_at": e.created_at,
                    "updated_at": e.updated_at,
                    "steps": epic_steps,
                    "children": [],
                }),
            )
        })
        .collect::<std::collections::HashMap<String, serde_json::Value>>();

    // 组装 children + child_count
    // 先克隆 child json（避免借用冲突），再逐项写回 parent
    let child_entries: Vec<(String, serde_json::Value)> = child_order
        .iter()
        .filter_map(|c| {
            c.parent_id.as_ref().map(|pid| {
                (
                    pid.clone(),
                    epic_json_map
                        .get(&c.id)
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                )
            })
        })
        .collect();

    let mut epic_json_map = epic_json_map;
    for (pid, child_json) in child_entries {
        if child_json.is_null() {
            continue;
        }
        if let Some(parent_json) = epic_json_map.get_mut(&pid) {
            if let Some(arr) = parent_json
                .get_mut("children")
                .and_then(|v| v.as_array_mut())
            {
                arr.push(child_json);
            }
            let len = arr_len(parent_json);
            if let Some(cnt) = parent_json.get_mut("child_count") {
                *cnt = serde_json::json!(len);
            }
        }
    }

    // 顶层按创建时间倒序输出
    let epics_json: Vec<serde_json::Value> = top_order
        .iter()
        .filter_map(|e| epic_json_map.get(&e.id).cloned())
        .collect();

    serde_json::json!({
        "version": data.version,
        "project": project_name,
        "project_path": project_path,
        "epics": epics_json,
        "summary": epic_store::build_summary(data),
    })
}

/// 读取 children 数组长度（辅助）
fn arr_len(v: &serde_json::Value) -> u64 {
    v.get("children")
        .and_then(|c| c.as_array())
        .map(|a| a.len() as u64)
        .unwrap_or(0)
}

// ════════════════════════════════════════════════════════════════
// Execution state commands (G2 — Runner state exposed to UI)
// These read from the Runtime Store, not .epics.json.
// ════════════════════════════════════════════════════════════════

#[tauri::command]
pub fn get_executions() -> Result<Vec<crate::execution_store::Execution>, String> {
    if crate::remote_client::enabled() {
        // A2: the API already returns view fields filled from authority-side
        // runtime data — no local mirror join here.
        return crate::remote_client::get_into("/api/executions");
    }
    let mut executions = crate::execution_store::list_executions()?;
    fill_execution_view_fields(&mut executions);
    Ok(executions)
}

/// S0-A/C: fill the view-only retries_remaining in ONE package read
/// (map join, not per-row reads) so the UI can disable the retry button
/// on an exhausted budget and explain why. Shared by the local read path
/// and the /api/executions server route so both return the same shape.
pub fn fill_execution_view_fields(executions: &mut [crate::execution_store::Execution]) {
    let caps = crate::execution_store::budget_caps_by_package().unwrap_or_default();
    let display_nos = crate::execution_store::display_nos_by_package().unwrap_or_default();
    for exec in executions.iter_mut() {
        if let Some(max_attempts) = caps.get(&exec.package_id) {
            let used = crate::execution_store::effective_budget_used(exec);
            exec.retries_remaining = Some(max_attempts.saturating_sub(used));
        }
        exec.package_display_no = display_nos.get(&exec.package_id).copied();
    }
}

/// What is the Runner working on right now — the freshest in-flight
/// execution joined with its human-readable package number and step/epic
/// titles, for the App's "currently running" banner.
#[tauri::command]
pub fn get_current_execution() -> Result<Option<serde_json::Value>, String> {
    if crate::remote_client::enabled() {
        // A3: banner payload (incl. step/epic title lookup against the
        // authority's board paths) is assembled SERVER-side.
        return crate::remote_client::get_into("/api/current-execution");
    }
    let all = crate::execution_store::list_executions()?;
    current_execution_payload(freshest_inflight(all))
}

/// Freshest queued/running/candidate/verifying execution, newest updated_at
/// first (the selection half of get_current_execution).
fn freshest_inflight(
    execs: Vec<crate::execution_store::Execution>,
) -> Option<crate::execution_store::Execution> {
    execs
        .into_iter()
        .filter(|e| {
            matches!(
                e.status,
                crate::execution_store::ExecutionStatus::Queued
                    | crate::execution_store::ExecutionStatus::Running
                    | crate::execution_store::ExecutionStatus::Candidate
                    | crate::execution_store::ExecutionStatus::Verifying
            )
        })
        .max_by_key(|e| e.updated_at)
}

/// Plain core: render the banner payload for the selected execution (body
/// identical to the former command body after the list read).
fn current_execution_payload(
    inflight: Option<crate::execution_store::Execution>,
) -> Result<Option<serde_json::Value>, String> {
    let Some(exec) = inflight else {
        return Ok(None);
    };
    let display_no = crate::execution_store::display_nos_by_package()
        .ok()
        .and_then(|m| m.get(&exec.package_id).copied());
    // Titles from the owning project's board (best-effort — missing board
    // degrades to ids, never fails the banner).
    let (epic_title, step_title) = (|| {
        let board = std::fs::read_to_string(
            std::path::Path::new(&exec.project_path).join(".epics.json"),
        )
        .ok()?;
        let v: serde_json::Value = serde_json::from_str(&board).ok()?;
        let step_title = v["steps"]
            .as_array()?
            .iter()
            .find(|s| s["id"] == exec.step_id.as_str())
            .and_then(|s| s["title"].as_str())
            .map(str::to_string)?;
        let epic_title = v["epics"]
            .as_array()?
            .iter()
            .find(|e| e["id"] == exec.epic_id.as_str())
            .and_then(|e| e["title"].as_str())
            .map(str::to_string)
            .unwrap_or_else(|| exec.epic_id.clone());
        Some((epic_title, step_title))
    })()
        .unwrap_or((exec.epic_id.clone(), exec.step_id.clone()));
    Ok(Some(serde_json::json!({
        "execution_id": exec.id,
        "package_id": exec.package_id,
        "package_display_no": display_no,
        "epic_title": epic_title,
        "step_title": step_title,
        "status": format!("{:?}", exec.status).to_lowercase(),
        "attempt_no": exec.attempt_no,
        "scene_recovered_from": exec.scene_recovered_from,
        "updated_at": exec.updated_at,
    })))
}

#[tauri::command]
pub fn cancel_execution(id: String) -> Result<(), String> {
    if crate::remote_client::enabled() {
        let body = serde_json::json!({ "id": id, "pause": false });
        crate::remote_client::post_json("/api/executions/control", &body)?;
        return Ok(());
    }
    crate::execution_store::request_control(&id, false).map(|_| ())
}

#[tauri::command]
pub fn retry_execution(id: String) -> Result<String, String> {
    // Evidence-aware retry: create a NEW attempt preserving the parent's evidence.
    // The parent execution row is never modified — its bundle stays sealed.
    if crate::remote_client::enabled() {
        let body = serde_json::json!({ "id": id });
        let v = crate::remote_client::post_json("/api/executions/retry", &body)?;
        return serde_json::from_value(v).map_err(|e| format!("api shape mismatch: {e}"));
    }
    crate::execution_store::create_retry_attempt(&id)
}

#[tauri::command]
pub fn pause_execution(id: String) -> Result<(), String> {
    if crate::remote_client::enabled() {
        let body = serde_json::json!({ "id": id, "pause": true });
        crate::remote_client::post_json("/api/executions/control", &body)?;
        return Ok(());
    }
    crate::execution_store::request_control(&id, true).map(|_| ())
}

/// Live Runner heartbeat status: intent flag, thread liveness, last poll time,
/// poll count, and last processed count. Lets the UI show "running & polling"
/// vs "switched on but not running" vs "stale (no poll in a while)".
#[tauri::command]
pub fn get_runner_runtime_status() -> Result<crate::runner::RunnerRuntimeStatus, String> {
    if crate::remote_client::enabled() {
        return crate::remote_client::get_into("/api/runner/status");
    }
    Ok(crate::runner::runtime_status())
}

/// A model entry under a provider in the ZCode CLI config.
/// Deserialize added in S5 so the remote (API) branch can reuse this type.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExecutorModelOption {
    /// Full model id ("provider/model") to store in runner-config.
    pub id: String,
    /// Human-readable model name (e.g. "deepseek-v4-flash").
    pub name: String,
    /// Thinking levels declared for this model, following the user's ZCode
    /// config (reasoning.levels/variants) with CLI-builtin fallbacks. The UI
    /// lists these in the thinking selector; changing the ZCode config
    /// automatically changes this list.
    pub thinking_levels: Vec<String>,
    /// Default thinking level from the config's reasoning spec, if any.
    pub thinking_default: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExecutorProviderOption {
    pub name: String,
    pub display_name: String,
    pub models: Vec<ExecutorModelOption>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExecutorOptions {
    pub providers: Vec<ExecutorProviderOption>,
    /// Currently selected "provider/model"; None = CLI default.
    pub current_model: Option<String>,
    /// Currently selected thinking level; None = CLI/runtime defaults.
    pub current_thinking: Option<String>,
}

/// Migrate a stored executor_model id from a legacy config generation to the
/// current options list. When the stored id (e.g. "deepseek/deepseek-v4-flash",
/// a v1-format key) is not in the list but a provider offers the same model
/// NAME (e.g. "e7a18242…/deepseek-v4-flash" from the v2 config), return the
/// new id so the selector can resolve the current model — and its thinking
/// levels — instead of showing a detached selection.
fn migrate_executor_model(current: &str, providers: &[ExecutorProviderOption]) -> Option<String> {
    if providers
        .iter()
        .any(|p| p.models.iter().any(|m| m.id == current))
    {
        return None; // still valid, no migration needed
    }
    let model_name = current.split('/').nth(1)?;
    providers
        .iter()
        .flat_map(|p| p.models.iter())
        .find(|m| m.name == model_name)
        .map(|m| m.id.clone())
}

/// Parse one provider's models into ExecutorModelOption entries. Shared by
/// the v2 and v1 config readers.
fn executor_models_from_provider(
    provider_key: &str,
    def: &serde_json::Value,
) -> Vec<ExecutorModelOption> {
    let mut models = Vec::new();
    if let Some(model_map) = def.get("models").and_then(|m| m.as_object()) {
        for (model_name, model_def) in model_map {
            let reasoning = model_def.get("reasoning");
            let thinking_default = ["defaultLevel", "defaultVariant"].iter().find_map(|key| {
                reasoning
                    .and_then(|r| r.get(key))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            });
            let thinking_levels = crate::runner_git::thinking_levels_for(model_name, model_def);
            models.push(ExecutorModelOption {
                id: format!("{}/{}", provider_key, model_name),
                name: model_name.clone(),
                thinking_levels,
                thinking_default,
            });
        }
    }
    models
}

/// List the executor model options from the user's ZCode configs plus the
/// currently selected model.
///
/// Primary source: `~/.zcode/v2/config.json` (where the ZCode desktop app
/// manages models) — custom providers plus builtin providers that are BOTH
/// enabled and self-contained (apiKey + baseURL present — verified
/// standalone-usable by the headless CLI, e.g. builtin:bigmodel-coding-plan).
/// The legacy `~/.zcode/cli/config.json` is used only as a whole-list
/// fallback when the v2 config offers nothing — merging it model-by-model
/// resurfaces retired models (e.g. GLM-5.2) the user already removed.
///
/// A stored selection from a previous config generation (v1-format id) is
/// migrated to the same-named model in the current list and persisted, so the
/// selector (and its thinking levels) resolve the current model.
///
/// Read failures degrade to empty options — the UI shows "no models
/// configured".
#[tauri::command]
pub fn get_executor_options() -> Result<ExecutorOptions, String> {
    if crate::remote_client::enabled() {
        return crate::remote_client::get_into("/api/executor-options");
    }
    let mut providers: Vec<ExecutorProviderOption> = Vec::new();

    let provider_self_contained = |def: &serde_json::Value| -> bool {
        let opts = def.get("options");
        opts.and_then(|o| o.get("apiKey"))
            .and_then(|k| k.as_str())
            .map(|k| !k.is_empty())
            .unwrap_or(false)
            && opts
                .and_then(|o| o.get("baseURL"))
                .and_then(|u| u.as_str())
                .is_some()
    };

    if let Some(home) = dirs::home_dir() {
        let v2_path = home.join(".zcode").join("v2").join("config.json");
        if let Ok(raw) = std::fs::read_to_string(&v2_path) {
            if let Ok(config) = serde_json::from_str::<serde_json::Value>(&raw) {
                if let Some(provider_map) = config.get("provider").and_then(|p| p.as_object()) {
                    for (key, def) in provider_map {
                        let is_builtin = key.starts_with("builtin:");
                        // Custom providers pass as-is; builtin ones must be
                        // enabled + self-contained (OAuth-less key present).
                        if is_builtin
                            && (def.get("enabled").and_then(|e| e.as_bool()) != Some(true)
                                || !provider_self_contained(def))
                        {
                            continue;
                        }
                        if !is_builtin && !provider_self_contained(def) {
                            continue;
                        }
                        let display_name = def
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or(key)
                            .to_string();
                        let models = executor_models_from_provider(key, def);
                        if !models.is_empty() {
                            providers.push(ExecutorProviderOption {
                                name: key.clone(),
                                display_name,
                                models,
                            });
                        }
                    }
                }
            }
        }

        // v1 fallback: only when the v2 config offered nothing at all.
        if providers.is_empty() {
            let path = home.join(".zcode").join("cli").join("config.json");
            if let Ok(raw) = std::fs::read_to_string(&path) {
                if let Ok(config) = serde_json::from_str::<serde_json::Value>(&raw) {
                    if let Some(provider_map) = config.get("provider").and_then(|p| p.as_object()) {
                        for (name, def) in provider_map {
                            if !provider_self_contained(def) {
                                continue;
                            }
                            let display_name = def
                                .get("name")
                                .and_then(|n| n.as_str())
                                .unwrap_or(name)
                                .to_string();
                            let models = executor_models_from_provider(name, def);
                            if !models.is_empty() {
                                providers.push(ExecutorProviderOption {
                                    name: name.clone(),
                                    display_name,
                                    models,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    // Migrate a legacy-generation selection to the same-named model in the
    // current list so the selector resolves it (incl. thinking levels).
    let mut current_model = crate::runner_config::get_executor_model();
    if let Some(current) = &current_model {
        if let Some(migrated) = migrate_executor_model(current, &providers) {
            log::info!(
                "executor model migrated: '{}' → '{}' (config generation change)",
                current,
                migrated
            );
            if crate::runner_config::set_executor_model(Some(migrated.clone())).is_ok() {
                current_model = Some(migrated);
            }
        }
    }

    Ok(ExecutorOptions {
        providers,
        current_model,
        current_thinking: crate::runner_config::get_executor_thinking(),
    })
}

/// Set which thinking level the Runner's executor uses (e.g. "high", "max",
/// "off"), or None to use CLI/runtime defaults. Injected into each worktree
/// by normalizing the model's providerOptionsByLevel. Takes effect on the
/// next worktree.
#[tauri::command]
pub fn set_executor_thinking(thinking: Option<String>) -> Result<(), String> {
    crate::runner_config::set_executor_thinking(thinking)
}

/// Set which model the Runner's executor uses ("provider/model", or None to
/// use the ZCode CLI's own default). Takes effect on the next worktree.
#[tauri::command]
pub fn set_executor_model(model: Option<String>) -> Result<(), String> {
    crate::runner_config::set_executor_model(model)
}

/// 待办租户 G4: pipeline status for the UI — schedule, last sweep, ledger
/// entries, pending finalizations, outbox size.
/// 带 project_path 时额外返回 nightly_scheduled：该项目是否在夜间调度的
/// projects 名单里——不在名单的项目上开"自动"永远不会被处理。
#[tauri::command]
pub fn get_todo_status(
    project_path: Option<String>,
    state: State<AppState>,
) -> Result<serde_json::Value, String> {
    if crate::remote_client::enabled() {
        let query = project_path
            .as_ref()
            .map(|p| format!("?project={}", crate::remote_client::urlencode(p)))
            .unwrap_or_default();
        return crate::remote_client::get_into(&format!("/api/todo-status{query}"));
    }
    let projects = state.projects.lock().map_err(|e| e.to_string())?.clone();
    get_todo_status_impl(&projects, project_path)
}

/// Plain core: todo pipeline status; projects = registry allowlist. Body
/// identical to the former command body (the state lock replaced by the
/// explicit list).
pub fn get_todo_status_impl(
    projects: &[(String, std::path::PathBuf)],
    project_path: Option<String>,
) -> Result<serde_json::Value, String> {
    let schedule = crate::runner_config::get_todo_schedule();
    let sweep_state = crate::todo_ledger::read_sweep_state();
    let ledger = crate::todo_ledger::read_ledger()?;
    let pending: Vec<serde_json::Value> = ledger
        .entries
        .values()
        .filter(|e| {
            e.finalization
                .as_ref()
                .map(|f| f.state == "pending")
                .unwrap_or(false)
        })
        .map(|e| serde_json::json!({ "task_key": e.task_key, "attempt_id": e.finalization.as_ref().map(|f| f.attempt_id.clone()) }))
        .collect();
    let outbox = std::fs::read_dir(crate::todo_ledger::outbox_dir())
        .map(|rd| rd.filter_map(|e| e.ok()).count())
        .unwrap_or(0);
    // 项目名按注册表路径匹配（realpath 对齐，兼容软链/尾斜杠差异）。
    let project_ref = match &project_path {
        Some(pp) => {
            let target = std::fs::canonicalize(pp).unwrap_or_else(|_| std::path::PathBuf::from(pp));
            projects
                .iter()
                .find(|(_, p)| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()) == target)
                .map(|(n, p)| (n.clone(), p.clone()))
        }
        None => None,
    };
    // 语义与 todo_sweep 对齐：allowlist 缺省(None)=所有启用项目都覆盖；
    // Some(list) 按名称或路径命中；schedule 整体未配置时返回 Null（区别于
    // "启用但不在名单"的 false，UI 需要分两种文案）。
    let nightly_scheduled = match (&project_ref, &schedule) {
        (Some((name, path)), Some(sched)) => match &sched.projects {
            None => serde_json::json!(true),
            Some(allowed) => serde_json::json!(
                allowed.iter().any(|a| a == name || a == &path.to_string_lossy())
            ),
        },
        (Some(_), None) => serde_json::Value::Null,
        (None, _) => serde_json::Value::Null,
    };
    // 待办 ↔ 夜间任务链路（todo_id → 方案关联/状态/结果），供卡片显示"它去哪了"。
    // 账本 tenant_id 即项目名。epic_id 仅作"有无关联方案"的信号，不下发 step_id。
    let mut links = serde_json::Map::new();
    if let Some((name, _)) = &project_ref {
        for e in ledger.entries.values().filter(|e| &e.tenant_id == name) {
            links.insert(
                e.todo_id.clone(),
                serde_json::json!({
                    "state": e.state,
                    "has_epic": !e.epic_id.is_empty(),
                    "outcome": e.finalization.as_ref().map(|f| f.outcome.clone()),
                }),
            );
        }
    }
    Ok(serde_json::json!({
        "schedule": schedule,
        "sweep_state": sweep_state,
        "open_tasks": ledger.entries.values().filter(|e| crate::todo_ledger::entry_is_open(e)).count(),
        "pending_finalizations": pending,
        "outbox": outbox,
        "schedule_enabled": schedule.is_some(),
        "nightly_scheduled": nightly_scheduled,
        "links": links,
    }))
}

/// 跨机链路健康（只读聚合）：outbox 积压、回执滞留/未匹配、窗口漏跑、
/// 最近一夜的结果分布、待确认计数。全部从 runtime/项目目录既有文件读时聚合，
/// 无新写路径。
#[tauri::command]
pub fn get_fleet_health(state: State<AppState>) -> Result<serde_json::Value, String> {
    if crate::remote_client::enabled() {
        return crate::remote_client::get_into("/api/fleet-health");
    }
    let projects = state.projects.lock().map_err(|e| e.to_string())?.clone();
    get_fleet_health_impl(&projects)
}

/// Plain core: fleet health snapshot for an explicit registry allowlist.
pub fn get_fleet_health_impl(
    projects: &[(String, std::path::PathBuf)],
) -> Result<serde_json::Value, String> {
    crate::fleet_health::collect(projects)
}

/// Get the Runner status for a specific project (global + project + effective).
#[tauri::command]
pub fn get_runner_project_status(
    project_path: String,
) -> Result<crate::runner_config::ProjectRunnerStatus, String> {
    if crate::remote_client::enabled() {
        return crate::remote_client::get_into(&format!(
            "/api/runner-project-status?project={}",
            crate::remote_client::urlencode(&project_path)
        ));
    }
    Ok(crate::runner_config::get_project_status(&project_path))
}

/// Enable or disable Runner execution for a specific project.
#[tauri::command]
pub fn set_runner_project_enabled(
    project_path: String,
    enabled: bool,
) -> Result<crate::runner_config::ProjectRunnerStatus, String> {
    if crate::remote_client::enabled() {
        let body = serde_json::json!({ "project_path": project_path, "enabled": enabled });
        crate::remote_client::post_json("/api/runner/project-enabled", &body)?;
        // The control endpoint echoes the request; the command contract is
        // the full status — read it back from the status endpoint.
        return crate::remote_client::get_into(&format!(
            "/api/runner-project-status?project={}",
            crate::remote_client::urlencode(&project_path)
        ));
    }
    crate::runner_config::set_project_enabled(&project_path, enabled)?;
    Ok(crate::runner_config::get_project_status(&project_path))
}

/// Set the master kill switch. OFF stops the Runner thread and blocks every
/// poll cycle, regardless of per-project switches.
#[tauri::command]
pub fn set_runner_global_enabled(enabled: bool) -> Result<bool, String> {
    if crate::remote_client::enabled() {
        let body = serde_json::json!({ "enabled": enabled });
        let v = crate::remote_client::post_json("/api/runner/global-enabled", &body)?;
        return v
            .get("enabled")
            .and_then(|x| x.as_bool())
            .ok_or_else(|| "api shape mismatch: missing 'enabled'".to_string());
    }
    crate::runner_config::set_globally_enabled(enabled)?;
    Ok(crate::runner_config::is_globally_enabled())
}

/// Get the auto_execute setting for a specific epic.
#[tauri::command]
pub fn get_epic_auto_execute(
    epic_id: String,
    project_path: String,
) -> Result<Option<bool>, String> {
    if crate::remote_client::enabled() {
        return crate::remote_client::get_into(&format!(
            "/api/epic-auto-execute?epic_id={}&project={}",
            crate::remote_client::urlencode(&epic_id),
            crate::remote_client::urlencode(&project_path)
        ));
    }
    let dir = std::path::PathBuf::from(&project_path);
    let data = crate::epic_store::read_epics(&dir)?;
    let epic = data
        .epics
        .iter()
        .find(|e| e.id == epic_id)
        .ok_or_else(|| format!("epic '{}' not found", epic_id))?;
    Ok(epic.auto_execute)
}

/// Set the auto_execute setting for a specific epic.
/// None = inherit project setting, Some(true/false) = explicit override.
#[tauri::command]
pub fn set_epic_auto_execute(
    epic_id: String,
    project_path: String,
    auto_execute: Option<bool>,
) -> Result<(), String> {
    if crate::remote_client::enabled() {
        let body = serde_json::json!({
            "epic_id": epic_id,
            "project_path": project_path,
            "auto_execute": auto_execute,
        });
        crate::remote_client::post_json("/api/epic-auto-execute", &body)?;
        return Ok(());
    }
    let dir = std::path::PathBuf::from(&project_path);
    crate::epic_store::mutate_epics(&dir, |data| {
        let epic = data
            .epics
            .iter_mut()
            .find(|e| e.id == epic_id)
            .ok_or_else(|| format!("epic '{}' not found", epic_id))?;
        epic.auto_execute = auto_execute;
        let now = chrono::Utc::now().to_rfc3339();
        epic.updated_at = now;
        Ok(())
    })?;
    Ok(())
}

#[tauri::command]
pub fn get_decision_requests(
    include_resolved: Option<bool>,
) -> Result<Vec<crate::decision_store::DecisionRequest>, String> {
    if crate::remote_client::enabled() {
        let qs = if include_resolved.unwrap_or(false) {
            "?include_resolved=true"
        } else {
            ""
        };
        return crate::remote_client::get_into(&format!("/api/decision-requests{qs}"));
    }
    crate::decision_store::list_requests(include_resolved.unwrap_or(false))
}

#[tauri::command]
pub fn resolve_decision_request(
    request_id: String,
    resolution: String,
) -> Result<crate::decision_store::DecisionRequest, String> {
    if crate::remote_client::enabled() {
        let body = serde_json::json!({ "id": request_id, "resolution": resolution });
        let v = crate::remote_client::post_json("/api/decision-requests/resolve", &body)?;
        return serde_json::from_value(v).map_err(|e| format!("api shape mismatch: {e}"));
    }
    let request = crate::decision_store::resolve_request(&request_id, &resolution, "ui-user")?;

    // §5.1: resolve triggers a legal state transition, not a direct pass.
    // S0-B fail-closed: an action that FAILS rolls the decision back to
    // unresolved — a resolution must never stand resolved without its effect
    // (the failure reason is preserved in last_failed_resolution).
    //
    // CONTRACT (mirror in mcp-server/src/decision-actions.ts, same commit):
    //   retry | resume        → schedule a child attempt (budget-capped)
    //   cancel               → cancel the execution
    //   revise_package | transfer_manual | manual | reject
    //                        → cancel the execution (human work follows)
    //   revise_scope         → cancel the execution + keep the package
    //                           blocked until a NEW sealed revision (fresh
    //                           budget); scene salvage is S1/S2
    //   anything else        → record only (custom free text)
    let action = resolution.trim().to_lowercase();
    let action_result: Result<(), String> = match action.as_str() {
        "retry" | "resume" => {
            // resume == controlled retry: the attempt chain's resume logic
            // (same base + intact worktree + CLI session) reuses the
            // interrupted scene. The Runner picks the child up next poll.
            crate::execution_store::create_retry_attempt(&request.execution_id).map(|_| ())
        }
        "cancel" => {
            cancel_execution_for_decision(&request.execution_id, "cancelled via decision request")
        }
        "revise_package" | "transfer_manual" | "manual" | "reject" => {
            // These require human action outside the Runner — mark blocked
            // execution as cancelled so it doesn't linger in the queue.
            cancel_execution_for_decision(
                &request.execution_id,
                &format!("resolved as '{action}'"),
            )
        }
        "revise_scope" => {
            // Safe closure for a ScopeViolation: cancel the lingering
            // execution and keep the package blocked until the operator
            // seals a new revision (prepare → validate → seal — its retry
            // budget is independent of the old chain). S2: the new
            // revision's first attempt automatically recovers the captured
            // scene (same base) — the work is reused, not redone.
            cancel_execution_for_decision(
                &request.execution_id,
                "resolved as revise_scope — awaiting a new package revision",
            )?;
            let package_id = crate::execution_store::get_execution(&request.execution_id)
                .map(|exec| exec.package_id)
                .map_err(|e| format!("cannot resolve package for the blocked execution: {e}"))?;
            crate::execution_store::mark_package_status(&package_id, "blocked")
                .map_err(|e| format!("failed to keep package blocked: {e}"))
        }
        _ => {
            // Custom resolution text — no automatic action.
            Ok(())
        }
    };

    match action_result {
        Ok(()) => Ok(request),
        Err(action_error) => {
            // Roll the decision back so the operator can re-answer; the
            // failed choice stays visible via last_failed_resolution.
            match crate::decision_store::unresolve_request(&request_id, &action_error) {
                Ok(_) => Err(format!(
                    "action failed — decision rolled back to unresolved: {action_error}"
                )),
                Err(rollback_error) => {
                    log::error!(
                        "resolve_decision: action failed ({action_error}) and rollback failed ({rollback_error}) — request {request_id} left resolved"
                    );
                    Err(format!(
                        "action failed ({action_error}) and rollback failed ({rollback_error}) — decision left resolved; inspect {request_id} manually"
                    ))
                }
            }
        }
    }
}

/// Cancel a DecisionRequest's execution with honest feedback: a terminal
/// execution is a no-op (the evidence guard refuses the overwrite — nothing
/// to cancel), while a REAL failure is surfaced to the operator.
fn cancel_execution_for_decision(execution_id: &str, message: &str) -> Result<(), String> {
    let terminal = crate::execution_store::get_execution(execution_id)
        .map(|exec| {
            matches!(
                exec.status,
                crate::execution_store::ExecutionStatus::Passed
                    | crate::execution_store::ExecutionStatus::Failed
                    | crate::execution_store::ExecutionStatus::Cancelled
                    | crate::execution_store::ExecutionStatus::TimedOut
            )
        })
        .unwrap_or(false);
    if terminal {
        log::info!(
            "resolve_decision: execution {} already terminal — nothing to cancel",
            execution_id
        );
        return Ok(());
    }
    crate::execution_store::release_lease(
        execution_id,
        crate::execution_store::ExecutionStatus::Cancelled,
        Some(message),
    )
    .map_err(|e| format!("decision recorded, but the cancel action failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // arr_len: edge cases for the JSON helper used by epic rendering.
    // S5: epics_to_json's step ORDERING is pinned directly here (it is the
    // App's single data source — acceptance 10).

    /// S5 (acceptance 10): the serialized step list is ordered by
    /// (order, created_at) — a step inserted LATER with the SMALLEST order
    /// must serialize first, matching the Runner's order semantics. The
    /// JSON insertion order must never leak through.
    #[test]
    fn epic_steps_ordered_by_order_not_insertion() {
        let data: crate::models::EpicsFile = serde_json::from_value(serde_json::json!({
            "version": 1,
            "project": "p",
            "epics": [{"id": "e1", "title": "T", "status": "active",
                       "created_at": "2026-08-17T00:00:00Z",
                       "updated_at": "2026-08-17T00:00:00Z"}],
            "steps": [
                {"id": "s3", "epic_id": "e1", "title": "C", "stage": "pending",
                 "order": 3, "created_at": "2026-08-17T03:00:00Z",
                 "updated_at": "2026-08-17T03:00:00Z"},
                {"id": "s1", "epic_id": "e1", "title": "A", "stage": "pending",
                 "order": 1, "created_at": "2026-08-17T01:00:00Z",
                 "updated_at": "2026-08-17T01:00:00Z"},
                {"id": "s0", "epic_id": "e1", "title": "LATE-SMALLEST", "stage": "pending",
                 "order": 0, "created_at": "2026-08-17T09:00:00Z",
                 "updated_at": "2026-08-17T09:00:00Z"},
                {"id": "s2", "epic_id": "e1", "title": "B", "stage": "pending",
                 "order": 1, "created_at": "2026-08-17T00:00:00Z",
                 "updated_at": "2026-08-17T00:00:00Z"}
            ]
        }))
        .unwrap();
        let out = epics_to_json(&data, "/tmp/proj");
        let titles: Vec<&str> = out["epics"][0]["steps"]
            .as_array()
            .expect("epics array with steps")
            .iter()
            .map(|s| s["title"].as_str().unwrap())
            .collect();
        // order 0 (late-inserted) first; the order-1 tie breaks on created_at
        // ascending (B at 00:00 before A at 01:00); then order 3.
        assert_eq!(titles, vec!["LATE-SMALLEST", "B", "A", "C"]);
    }

    #[test]
    fn arr_len_counts_children_array() {
        let v = serde_json::json!({ "children": [1, 2, 3] });
        assert_eq!(arr_len(&v), 3);
    }

    #[test]
    fn arr_len_zero_when_children_missing() {
        let v = serde_json::json!({ "other": "x" });
        assert_eq!(arr_len(&v), 0);
    }

    #[test]
    fn arr_len_zero_when_children_not_array() {
        let v = serde_json::json!({ "children": "not-an-array" });
        assert_eq!(arr_len(&v), 0);
    }

    #[test]
    fn arr_len_zero_for_empty_object() {
        let v = serde_json::json!({});
        assert_eq!(arr_len(&v), 0);
    }

    fn seed_retryable_fixture() -> (String, String) {
        let package = serde_json::json!({
            "version": 1,
            "packages": [{
                "package_id": "pkg1", "epic_id": "e1", "step_id": "s1",
                "plan_revision": 1, "project_path": "/tmp/proj", "repo_root": "/tmp/proj",
                "base_sha": "abc123", "objective": "t",
                "scope": {"allowed_paths": ["**"], "forbidden_paths": [], "max_changed_files": 10},
                "executor": {"preferred": "zcode", "fallbacks": [], "sandbox": "workspace-write",
                             "timeout_seconds": 3600, "max_attempts": 3},
                "acceptance": [], "status": "blocked",
                "sealed_at": "2026-08-11T00:00:00Z", "package_hash": "fixture"
            }]
        });
        std::fs::create_dir_all(crate::execution_store::runtime_dir()).unwrap();
        std::fs::write(
            crate::execution_store::runtime_dir().join("packages.json"),
            serde_json::to_string_pretty(&package).unwrap(),
        )
        .unwrap();
        let parent =
            crate::execution_store::enqueue_execution("pkg1", "e1", "s1", "/tmp/proj", "abc123")
                .unwrap();
        crate::execution_store::acquire_lease(&parent, "runner-1").unwrap();
        crate::execution_store::finish_execution(
            &parent,
            crate::execution_store::ExecutionStatus::Blocked,
            Some("paused by user"),
            None,
        )
        .unwrap();
        (parent, "pkg1".to_string())
    }

    #[test]
    fn resolve_resume_creates_child_attempt() {
        let _guard = crate::execution_store::TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("QUESTLOG_RUNTIME", dir.path());
        let (parent, _pkg) = seed_retryable_fixture();
        let request =
            crate::decision_store::create_request(crate::decision_store::NewDecisionRequest {
                epic_id: "e1",
                step_id: "s1",
                execution_id: &parent,
                kind: "approval",
                question: "resume?",
                options: vec!["resume".to_string(), "cancel".to_string()],
                evidence_refs: vec![],
                package_revision: 1,
            })
            .unwrap();

        resolve_decision_request(request.request_id, "resume".to_string()).unwrap();

        let execs = crate::execution_store::list_executions().unwrap();
        let child = execs
            .iter()
            .find(|e| e.parent_attempt_id.as_deref() == Some(parent.as_str()))
            .expect("resume must schedule a controlled retry child");
        assert_eq!(
            child.status,
            crate::execution_store::ExecutionStatus::Queued
        );
        assert_eq!(child.attempt_no, 2);
    }

    /// S0-B: resolving a ScopeViolation DR as revise_scope performs the safe
    /// closure — the lingering execution is cancelled, the package stays
    /// blocked until a new sealed revision, and the decision stands resolved.
    #[test]
    fn resolve_revise_scope_cancels_execution_and_keeps_package_blocked() {
        let _guard = crate::execution_store::TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("QUESTLOG_RUNTIME", dir.path());
        std::fs::create_dir_all(crate::execution_store::runtime_dir()).unwrap();

        let packages = serde_json::json!({
            "version": 1,
            "packages": [{
                "package_id": "pkg1", "epic_id": "e1", "step_id": "s1",
                "plan_revision": 1, "project_path": "/tmp/proj", "repo_root": "/tmp/proj",
                "base_sha": "abc123", "objective": "revise scope closure",
                "scope": {"allowed_paths": ["src/**"], "forbidden_paths": [], "max_changed_files": 4},
                "executor": {"preferred": "zcode", "fallbacks": [], "sandbox": "workspace-write",
                             "timeout_seconds": 3600, "max_attempts": 2},
                "acceptance": [], "status": "blocked",
                "sealed_at": "2026-08-17T00:00:00Z", "package_hash": "fixture"
            }]
        });
        let runtime = crate::execution_store::runtime_dir().join("packages.json");
        std::fs::write(&runtime, serde_json::to_string_pretty(&packages).unwrap()).unwrap();

        let exec_id = crate::execution_store::enqueue_execution(
            "pkg1", "e1", "s1", "/tmp/proj", "abc123",
        )
        .unwrap();
        let _ = crate::execution_store::acquire_lease(&exec_id, "runner-1").unwrap();
        crate::execution_store::finish_execution(
            &exec_id,
            crate::execution_store::ExecutionStatus::Blocked,
            Some("changed paths outside the sealed scope"),
            Some("scope_violation"),
        )
        .unwrap();

        let request =
            crate::decision_store::create_request(crate::decision_store::NewDecisionRequest {
                epic_id: "e1",
                step_id: "s1",
                execution_id: &exec_id,
                kind: "blocked",
                question: "The candidate changed paths outside the sealed scope. Revise scope or cancel?",
                options: vec!["revise_scope".to_string(), "cancel".to_string()],
                evidence_refs: vec![],
                package_revision: 1,
            })
            .unwrap();

        let resolved =
            resolve_decision_request(request.request_id.clone(), "revise_scope".to_string())
                .expect("revise_scope must resolve cleanly");
        assert!(resolved.resolved);

        let exec = crate::execution_store::get_execution(&exec_id).unwrap();
        assert_eq!(
            exec.status,
            crate::execution_store::ExecutionStatus::Cancelled,
            "lingering execution must be closed"
        );
        assert!(exec
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("awaiting a new package revision")));

        // The package must stay blocked so the Runner cannot re-claim the
        // old scope while a revision is being prepared. (read_ready_packages
        // filters blocked rows out, so assert against the raw store.)
        let raw = std::fs::read_to_string(
            crate::execution_store::runtime_dir().join("packages.json"),
        )
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let status = parsed["packages"][0]["status"].as_str().unwrap();
        assert_eq!(status, "blocked");
    }

    #[test]
    fn resolve_cancel_failure_is_visible_not_silent() {
        let _guard = crate::execution_store::TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("QUESTLOG_RUNTIME", dir.path());
        std::fs::create_dir_all(crate::execution_store::runtime_dir()).unwrap();
        // DR references a nonexistent execution → cancel genuinely fails.
        let request =
            crate::decision_store::create_request(crate::decision_store::NewDecisionRequest {
                epic_id: "e1",
                step_id: "s1",
                execution_id: "exec_ghost",
                kind: "blocked",
                question: "cancel?",
                options: vec!["cancel".to_string()],
                evidence_refs: vec![],
                package_revision: 1,
            })
            .unwrap();

        let request_id = request.request_id.clone();
        let result = resolve_decision_request(request.request_id, "cancel".to_string());
        let error = result.expect_err("a real action failure must surface");
        assert!(
            error.contains("rolled back"),
            "honest framing: {error}"
        );
        // S0-B fail-closed: the decision does NOT stand resolved when its
        // action failed — it rolls back to unresolved with the failed choice
        // preserved in last_failed_resolution for the human.
        let stored = crate::decision_store::list_requests(true).unwrap();
        let rolled = stored
            .iter()
            .find(|r| r.request_id == request_id)
            .expect("request must exist");
        assert!(!rolled.resolved, "rolled back to unresolved");
        assert!(rolled
            .last_failed_resolution
            .as_deref()
            .is_some_and(|trail| trail.contains("cancel")));
    }

    fn opt_model(id: &str, name: &str) -> ExecutorModelOption {
        ExecutorModelOption {
            id: id.to_string(),
            name: name.to_string(),
            thinking_levels: vec![],
            thinking_default: None,
        }
    }

    #[test]
    fn migrate_executor_model_moves_legacy_id_to_same_named_model() {
        let providers = vec![ExecutorProviderOption {
            name: "uuid-1".into(),
            display_name: "Deepseek".into(),
            models: vec![
                opt_model("uuid-1/deepseek-v4-flash", "deepseek-v4-flash"),
                opt_model("uuid-1/deepseek-v4-pro", "deepseek-v4-pro"),
            ],
        }];
        // v1-format id not in list, same model NAME exists → migrate
        assert_eq!(
            migrate_executor_model("deepseek/deepseek-v4-flash", &providers),
            Some("uuid-1/deepseek-v4-flash".to_string())
        );
        // Already-current id → no migration
        assert_eq!(
            migrate_executor_model("uuid-1/deepseek-v4-pro", &providers),
            None
        );
        // Unknown model with no same-named match → no migration
        assert_eq!(migrate_executor_model("deepseek/ghost", &providers), None);
    }

    #[test]
    fn arb_decision_guard_allows_quoted_payload_but_blocks_chaining() {
        // 引号内的 | 是裁决文本载荷，合法（D1 cmd_reject 的真实形态）。
        assert!(!shell_meta_outside_quotes(
            r#"bash ~/.local/bin/gros chat-archive --conversation-id decision-log --text "[裁决|D1|不追]""#
        ));
        // 引号内分号同理（D1 cmd_approve 的 --support 载荷）。
        assert!(!shell_meta_outside_quotes(
            r#"bash ~/.local/bin/gros research "x" --support "a;b""#
        ));
        // 引号外的拼接符一律拒绝。
        assert!(shell_meta_outside_quotes("bash ~/.local/bin/gros research x; rm -rf /"));
        assert!(shell_meta_outside_quotes("bash ~/.local/bin/gros research x | curl evil"));
        assert!(shell_meta_outside_quotes("bash ~/.local/bin/gros research x $(whoami)"));
        assert!(shell_meta_outside_quotes("bash ~/.local/bin/gros research `id`"));
        // 未闭合引号 fail-closed。
        assert!(shell_meta_outside_quotes("bash ~/.local/bin/gros research \"unclosed"));
    }

    #[test]
    fn type_change_guard_protects_auto_process_invariant() {        // Opted-in item: retyping to knowhow/idea rejected.
        assert!(ensure_type_change_allowed(true, &ItemType::Knowhow).is_err());
        assert!(ensure_type_change_allowed(true, &ItemType::Idea).is_err());
        // Staying a todo is fine.
        assert!(ensure_type_change_allowed(true, &ItemType::Todo).is_ok());
        // Non-opted-in items can be retyped freely.
        assert!(ensure_type_change_allowed(false, &ItemType::Knowhow).is_ok());
    }

    #[test]
    fn same_state_transition_detects_idempotent_rejects_real_conflict() {
        // 同态（快照滞后）：dismissed → dismissed —— 幂等信号。
        assert_eq!(
            same_state_transition("Error: 状态流转不允许: dismissed → dismissed(仅 needs_review → promoted/dismissed)"),
            Some("dismissed".to_string())
        );
        // 真冲突（不同态）：不是幂等信号，仍应走硬错误路径。
        assert_eq!(same_state_transition("状态流转不允许: open → dismissed(仅 needs_review → promoted/dismissed)"), None);
        // 无关文案。
        assert_eq!(same_state_transition("exit 1: something else"), None);
    }
}
