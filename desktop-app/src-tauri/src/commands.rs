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
    crate::execution_store::release_lease(
        &id,
        crate::execution_store::ExecutionStatus::Cancelled,
        Some("cancelled by user"),
    )
}

#[tauri::command]
pub fn retry_execution(id: String) -> Result<(), String> {
    // Move the execution back to queued for retry
    crate::execution_store::update_execution(
        &id,
        crate::execution_store::ExecutionUpdates {
            status: Some(crate::execution_store::ExecutionStatus::Queued),
            ..Default::default()
        },
    )
}

#[tauri::command]
pub fn pause_execution(id: String) -> Result<(), String> {
    // Release the lease and mark as blocked (paused by user)
    crate::execution_store::release_lease(
        &id,
        crate::execution_store::ExecutionStatus::Blocked,
        Some("paused by user"),
    )
}

#[tauri::command]
pub fn cleanup_stale_executions() -> Result<Vec<String>, String> {
    crate::execution_store::cleanup_stale_leases()
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
