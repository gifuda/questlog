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
    crate::execution_store::list_executions()
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

/// Parse one provider's models into ExecutorModelOption entries. Shared by
/// the v2 and v1 config readers.
fn executor_models_from_provider(provider_key: &str, def: &serde_json::Value) -> Vec<ExecutorModelOption> {
    let mut models = Vec::new();
    if let Some(model_map) = def.get("models").and_then(|m| m.as_object()) {
        for (model_name, model_def) in model_map {
            let reasoning = model_def.get("reasoning");
            let thinking_default = ["defaultLevel", "defaultVariant"]
                .iter()
                .find_map(|key| {
                    reasoning
                        .and_then(|r| r.get(key))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                });
            let thinking_levels =
                crate::runner_git::thinking_levels_for(model_name, model_def);
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
/// Sources, in priority order:
/// 1. `~/.zcode/v2/config.json` (where the ZCode desktop app manages models):
///    custom providers plus builtin providers that are BOTH enabled and
///    self-contained (apiKey + baseURL present — verified standalone-usable
///    by the headless CLI, e.g. builtin:bigmodel-coding-plan/GLM-5.3).
/// 2. `~/.zcode/cli/config.json` (legacy headless config): providers with
///    apiKey merged in, deduped by model name (e.g. v1 bigmodel still offers
///    GLM-5.2 when v2's coding-plan only has GLM-5.3).
///
/// Read failures degrade to empty options — the UI shows "no models
/// configured".
#[tauri::command]
pub fn get_executor_options() -> Result<ExecutorOptions, String> {
    let mut providers: Vec<ExecutorProviderOption> = Vec::new();
    let mut offered_models: std::collections::HashSet<String> = std::collections::HashSet::new();

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
                        offered_models.extend(models.iter().map(|m| m.name.clone()));
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

        // v1 merge: legacy config providers add models not already offered
        // (e.g. GLM-5.2 when only GLM-5.3 came from v2).
        let path = home.join(".zcode").join("cli").join("config.json");
        if let Ok(raw) = std::fs::read_to_string(&path) {
            if let Ok(config) = serde_json::from_str::<serde_json::Value>(&raw) {
                if let Some(provider_map) = config.get("provider").and_then(|p| p.as_object()) {
                    for (name, def) in provider_map {
                        if !provider_self_contained(def) {
                            continue;
                        }
                        let all = executor_models_from_provider(name, def);
                        let models: Vec<_> = all
                            .into_iter()
                            .filter(|m| !offered_models.contains(&m.name))
                            .collect();
                        if models.is_empty() {
                            continue;
                        }
                        offered_models.extend(models.iter().map(|m| m.name.clone()));
                        let display_name = def
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or(name)
                            .to_string();
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

    Ok(ExecutorOptions {
        providers,
        current_model: crate::runner_config::get_executor_model(),
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
    // The resolution determines what happens next.
    let action = resolution.trim().to_lowercase();
    match action.as_str() {
        "retry" => {
            // Create a new attempt for the same execution's package.
            // The Runner (if enabled) will pick it up on the next poll.
            if let Err(e) = crate::execution_store::create_retry_attempt(&request.execution_id) {
                log::warn!(
                    "resolve_decision(retry): failed to create new attempt for {}: {}",
                    request.execution_id,
                    e
                );
            }
        }
        "cancel" => {
            // Mark the execution as cancelled.
            let _ = crate::execution_store::release_lease(
                &request.execution_id,
                crate::execution_store::ExecutionStatus::Cancelled,
                Some("cancelled via decision request"),
            );
        }
        "revise_package" | "transfer_manual" | "reject" => {
            // These require human action outside the Runner — mark blocked execution
            // as cancelled so it doesn't linger in the queue.
            let _ = crate::execution_store::release_lease(
                &request.execution_id,
                crate::execution_store::ExecutionStatus::Cancelled,
                Some(&format!("resolved as '{}'", action)),
            );
        }
        _ => {
            // Custom resolution text — no automatic action.
        }
    }

    Ok(request)
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
}
