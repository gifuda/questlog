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

/// Validate a project_path coming from the frontend:
/// canonicalize it, require it exists as a directory, and require it matches a
/// registered project (the UI only ever operates on registered projects).
/// Returns the canonical PathBuf on success.
pub fn validate_project_path(
    project_path: &str,
    state: &State<AppState>,
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
    let projects = state.projects.lock().map_err(|e| e.to_string())?;
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

#[tauri::command]
pub fn get_projects(state: State<AppState>) -> Result<Vec<serde_json::Value>, String> {
    let projects = state.projects.lock().map_err(|e| e.to_string())?;
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
    let path = validate_project_path(&project_path, &state)?;
    store::read_todos(&path)
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
) -> Result<(), String> {
    let path = validate_project_path(&project_path, &state)?;
    let mut data = store::read_todos(&path)?;

    let item = data
        .items
        .iter_mut()
        .find(|i| i.id == id)
        .ok_or_else(|| format!("item '{}' not found", id))?;

    if let Some(s) = status {
        item.status = ItemStatus::from_str(&s)?;
        // Completing/archiving clears the in-progress flag
        if item.status != ItemStatus::Active {
            item.processing = false;
        }
    }
    if let Some(c) = content {
        item.content = c;
    }
    if let Some(t) = tags {
        item.tags = t;
    }
    if let Some(p) = priority {
        item.priority = Priority::from_str(&p)?;
    }
    if let Some(t) = title {
        item.title = t;
    }
    if let Some(ty) = item_type {
        item.item_type = ItemType::from_str(&ty)?;
    }
    if let Some(pr) = processing {
        item.processing = pr;
    }

    // Quality feedback: if an item gets archived within 48h of creation,
    // record it as a low-value push signal for the MCP side to read.
    let archived_quickly = item.status == ItemStatus::Archived
        && chrono::DateTime::parse_from_rfc3339(&item.created_at)
            .map(|created| (chrono::Utc::now() - created.to_utc()).num_hours() < 48)
            .unwrap_or(false);

    item.updated_at = chrono::Utc::now().to_rfc3339();

    store::write_todos(&path, &mut data)?;

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
    let path = validate_project_path(&project_path, &state)?;
    let mut data = store::read_todos(&path)?;

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
        created_at: now.clone(),
        updated_at: now,
    };

    data.items.push(item.clone());
    data.project = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    store::write_todos(&path, &mut data)?;
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
    let path = validate_project_path(&project_path, &state)?;
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
    let projects = state.projects.lock().map_err(|e| e.to_string())?;
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
    let path = validate_project_path(&project_path, &state)?;
    let data = epic_store::read_epics(&path)?;
    Ok(epics_to_json(&data, &project_path))
}

/// Global summary across ALL registered projects (aggregate dashboard).
#[tauri::command]
pub fn get_epics_global(state: State<AppState>) -> Result<serde_json::Value, String> {
    let projects = state.projects.lock().map_err(|e| e.to_string())?;
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
    let path = validate_project_path(&project_path, &state)?;
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
    let path = validate_project_path(&project_path, &state)?;
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
    let path = validate_project_path(&project_path, &state)?;
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
    let path = validate_project_path(&project_path, &state)?;
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
    let path = validate_project_path(&project_path, &state)?;
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
    let path = validate_project_path(&project_path, &state)?;
    let data = epic_store::mutate_epics(&path, |file| {
        file.steps.retain(|s| s.id != id);
        Ok(())
    })?;
    Ok(epics_to_json(&data, &project_path))
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
            let epic_steps: Vec<serde_json::Value> = data
                .steps
                .iter()
                .filter(|s| s.epic_id == e.id)
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
    let mut executions = crate::execution_store::list_executions()?;
    // S0-A/C: fill the view-only retries_remaining in ONE package read
    // (map join, not per-row reads) so the UI can disable the retry button
    // on an exhausted budget and explain why.
    let caps = crate::execution_store::budget_caps_by_package().unwrap_or_default();
    for exec in executions.iter_mut() {
        if let Some(max_attempts) = caps.get(&exec.package_id) {
            let used = crate::execution_store::effective_budget_used(exec);
            exec.retries_remaining = Some(max_attempts.saturating_sub(used));
        }
    }
    Ok(executions)
}

#[tauri::command]
pub fn get_execution(id: String) -> Result<crate::execution_store::Execution, String> {
    crate::execution_store::get_execution(&id)
}

#[tauri::command]
pub fn cancel_execution(id: String) -> Result<(), String> {
    crate::execution_store::request_control(&id, false).map(|_| ())
}

#[tauri::command]
pub fn retry_execution(id: String) -> Result<String, String> {
    // Evidence-aware retry: create a NEW attempt preserving the parent's evidence.
    // The parent execution row is never modified — its bundle stays sealed.
    crate::execution_store::create_retry_attempt(&id)
}

/// Get all attempt bundles for a given step (Trace Query).
/// Returns manifest-level summaries, sorted by attempt_no ascending.
#[tauri::command]
pub fn get_attempt_bundles(
    step_id: String,
) -> Result<Vec<crate::attempt_bundle::AttemptManifest>, String> {
    crate::attempt_bundle::list_bundles_for_step(&step_id)
}

/// Get a single attempt bundle's full manifest + available file names.
/// F8 audit fix: does NOT expose absolute bundle_dir to the frontend.
/// The frontend uses get_attempt_file(execution_id, filename) to read content.
#[tauri::command]
pub fn get_attempt_detail(execution_id: String) -> Result<AttemptDetail, String> {
    let (manifest, dir) = crate::attempt_bundle::read_bundle(&execution_id)?;
    Ok(AttemptDetail {
        manifest,
        execution_id: execution_id.clone(),
        files: list_bundle_files(&dir),
    })
}

/// Read a specific file from an attempt bundle by relative filename.
/// Validates the filename to prevent path traversal within the bundle.
#[tauri::command]
pub fn get_attempt_file(execution_id: String, filename: String) -> Result<String, String> {
    // Validate filename — only allow known bundle filenames, no path components
    const ALLOWED_FILES: &[&str] = &[
        "manifest.json",
        "task-package.json",
        "environment-snapshot.json",
        "events.jsonl",
        "stdout.log",
        "stderr.log",
        "diff.patch",
        "candidate.json",
        "verifier-results.json",
        "decisions.json",
        "outcome.json",
    ];
    if !ALLOWED_FILES.contains(&filename.as_str()) {
        return Err(format!(
            "filename '{}' is not a valid bundle file",
            filename
        ));
    }

    let dir = crate::attempt_bundle::bundle_dir(&execution_id)?;
    let path = dir.join(&filename);
    if !path.exists() {
        return Err(format!("file '{}' not found in bundle", filename));
    }
    std::fs::read_to_string(&path).map_err(|e| format!("read file: {e}"))
}

/// Response type for get_attempt_detail.
/// F8 audit fix: bundle_dir is NOT exposed — only execution_id + file names.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AttemptDetail {
    pub manifest: crate::attempt_bundle::AttemptManifest,
    pub execution_id: String,
    pub files: Vec<BundleFile>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct BundleFile {
    pub name: String,
    pub size: u64,
}

fn list_bundle_files(dir: &std::path::Path) -> Vec<BundleFile> {
    std::fs::read_dir(dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            // Skip temp files
            if name.contains("tmp-") {
                return None;
            }
            let size = e.metadata().ok()?.len();
            Some(BundleFile { name, size })
        })
        .collect()
}

#[tauri::command]
pub fn pause_execution(id: String) -> Result<(), String> {
    crate::execution_store::request_control(&id, true).map(|_| ())
}

#[tauri::command]
pub fn cleanup_stale_executions() -> Result<Vec<String>, String> {
    crate::execution_store::cleanup_stale_leases()
}

#[tauri::command]
pub fn set_runner_enabled(enabled: bool) -> Result<bool, String> {
    Ok(crate::runner::set_enabled(enabled))
}

#[tauri::command]
pub fn is_runner_running() -> Result<bool, String> {
    Ok(crate::runner::is_running())
}

/// Live Runner heartbeat status: intent flag, thread liveness, last poll time,
/// poll count, and last processed count. Lets the UI show "running & polling"
/// vs "switched on but not running" vs "stale (no poll in a while)".
#[tauri::command]
pub fn get_runner_runtime_status() -> Result<crate::runner::RunnerRuntimeStatus, String> {
    Ok(crate::runner::runtime_status())
}

/// A model entry under a provider in the ZCode CLI config.
#[derive(Debug, Clone, serde::Serialize)]
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

#[derive(Debug, Clone, serde::Serialize)]
pub struct ExecutorProviderOption {
    pub name: String,
    pub display_name: String,
    pub models: Vec<ExecutorModelOption>,
}

#[derive(Debug, Clone, serde::Serialize)]
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

/// Get the Runner status for a specific project (global + project + effective).
#[tauri::command]
pub fn get_runner_project_status(
    project_path: String,
) -> Result<crate::runner_config::ProjectRunnerStatus, String> {
    Ok(crate::runner_config::get_project_status(&project_path))
}

/// Enable or disable Runner execution for a specific project.
#[tauri::command]
pub fn set_runner_project_enabled(
    project_path: String,
    enabled: bool,
) -> Result<crate::runner_config::ProjectRunnerStatus, String> {
    crate::runner_config::set_project_enabled(&project_path, enabled)?;
    Ok(crate::runner_config::get_project_status(&project_path))
}

/// Set the master kill switch. OFF stops the Runner thread and blocks every
/// poll cycle, regardless of per-project switches.
#[tauri::command]
pub fn set_runner_global_enabled(enabled: bool) -> Result<bool, String> {
    crate::runner_config::set_globally_enabled(enabled)?;
    Ok(crate::runner_config::is_globally_enabled())
}

/// Get the auto_execute setting for a specific epic.
#[tauri::command]
pub fn get_epic_auto_execute(
    epic_id: String,
    project_path: String,
) -> Result<Option<bool>, String> {
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
    crate::decision_store::list_requests(include_resolved.unwrap_or(false))
}

#[tauri::command]
pub fn resolve_decision_request(
    request_id: String,
    resolution: String,
) -> Result<crate::decision_store::DecisionRequest, String> {
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
    // (epics_to_json itself is covered indirectly by the frontend; its
    // assembly logic is dense and couples to the full EpicsFile shape, so we
    // pin only the small pure helper here.)

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
}
