use crate::models::{ItemStatus, TodoItem, TodosFile};
use std::path::Path;

const FILENAME: &str = ".todos.json";

/// Monotonic counter so temp names stay unique even within one nanosecond.
static UNIQUE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Read .todos.json from a project directory.
/// Returns default empty structure if file doesn't exist.
///
/// Resilience: if strict deserialization fails (e.g. one item has an unknown
/// status value like `"pending"`), the whole file normally becomes unparseable
/// and the entire project vanishes from the board. To prevent one bad item from
/// nuking a whole project, we fall back to per-item parsing: keep the items that
/// deserialize cleanly, drop the ones that don't (logged to stderr). This way a
/// single corrupt item costs one todo, not the whole project.
pub fn read_todos(project_dir: &Path) -> Result<TodosFile, String> {
    let path = project_dir.join(FILENAME);
    if !path.exists() {
        return Ok(TodosFile {
            version: 1,
            project: project_dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            items: vec![],
        });
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("read error: {e}"))?;
    // Fast path: strict parse succeeds → return as-is.
    match serde_json::from_str::<TodosFile>(&raw) {
        Ok(d) => Ok(d),
        Err(strict_err) => {
            // Slow path: try to salvage items individually so one bad record
            // doesn't hide the whole project.
            let salvaged = salvage_todos(&raw, project_dir);
            match salvaged {
                Some(d) if !d.items.is_empty() => {
                    eprintln!(
                        "[store] {} strict parse failed ({}); recovered {}/{} items",
                        path.display(),
                        strict_err,
                        d.items.len(),
                        count_items_in_raw(&raw),
                    );
                    Ok(d)
                }
                _ => Err(format!("parse error: {strict_err}")),
            }
        }
    }
}

/// Count how many items the raw JSON claims to have (best-effort, for logging).
fn count_items_in_raw(raw: &str) -> usize {
    serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|v| v.get("items").and_then(|i| i.as_array()).map(|a| a.len()))
        .unwrap_or(0)
}

/// Try to parse a TodosFile loosely: parse the JSON as a generic Value, then
/// deserialize each item individually, keeping only the valid ones. Returns None
/// if the file isn't even valid JSON (truly corrupt, not just one bad field).
///
/// Before per-item deserialization, we coerce bad enum values to safe defaults
/// (e.g. status "pending" → "active"). This means a bad enum value — the most
/// common real-world data corruption — is fixed in place rather than dropping
/// the item. Only items with structural damage (missing required fields, wrong
/// JSON types) are dropped.
fn salvage_todos(raw: &str, project_dir: &Path) -> Option<TodosFile> {
    let root: serde_json::Value = serde_json::from_str(raw).ok()?;
    let project = root
        .get("project")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| {
            project_dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()
        });
    let items_arr = root.get("items").and_then(|v| v.as_array())?;
    let mut items: Vec<TodoItem> = Vec::new();
    for item_val in items_arr {
        let id = item_val
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        let title = item_val
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();

        // Coerce bad enum values to safe defaults so the item survives.
        // Without this, a single unknown variant (e.g. status="pending") would
        // fail deserialization and the item would be dropped.
        let coerced = coerce_enum_fields(item_val.clone());

        match serde_json::from_value::<TodoItem>(coerced) {
            Ok(item) => items.push(item),
            Err(e) => {
                // Even after coercion the item is structurally broken (missing
                // required field, wrong type). Drop it but log so it's traceable.
                eprintln!(
                    "[store] dropped bad todo item id={} title=\"{}\": {}",
                    id, title, e
                );
            }
        }
    }
    Some(TodosFile {
        version: root
            .get("version")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
            .unwrap_or(1),
        project,
        items,
    })
}

/// Coerce unknown enum values in a todo item's JSON to safe defaults.
/// - status: unknown → "active" (the item stays visible and actionable)
/// - type:   unknown → "todo"
/// - priority: unknown → "medium"
/// Returns the value unchanged if it's not an object or the fields are absent.
fn coerce_enum_fields(mut val: serde_json::Value) -> serde_json::Value {
    let obj = match val.as_object_mut() {
        Some(o) => o,
        None => return val,
    };
    if let Some(status) = obj.get("status").and_then(|v| v.as_str()) {
        if !["active", "completed", "archived", "converted"].contains(&status) {
            obj.insert("status".into(), serde_json::Value::String("active".into()));
        }
    }
    if let Some(t) = obj.get("type").and_then(|v| v.as_str()) {
        if !["knowhow", "todo", "idea"].contains(&t) {
            obj.insert("type".into(), serde_json::Value::String("todo".into()));
        }
    }
    if let Some(p) = obj.get("priority").and_then(|v| v.as_str()) {
        if !["high", "medium", "low"].contains(&p) {
            obj.insert("priority".into(), serde_json::Value::String("medium".into()));
        }
    }
    val
}

/// Write .todos.json atomically to a project directory.
/// Temp name is unique per process+time so concurrent writers never clobber
/// each other's temp file; the last rename wins but the file is never
/// observed half-written.
pub fn write_todos(project_dir: &Path, data: &TodosFile) -> Result<(), String> {
    let path = project_dir.join(FILENAME);
    let tmp = path.with_extension(format!(
        "tmp-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        UNIQUE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let json = serde_json::to_string_pretty(data).map_err(|e| format!("serialize error: {e}"))?;
    std::fs::write(&tmp, &json).map_err(|e| format!("write tmp error: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename error: {e}"))?;
    Ok(())
}

/// Count active items by type.
pub fn count_active(data: &TodosFile) -> (usize, usize, usize) {
    let mut knowhow = 0;
    let mut todo = 0;
    let mut idea = 0;
    for item in &data.items {
        if item.status != ItemStatus::Active {
            continue;
        }
        match item.item_type {
            crate::models::ItemType::Knowhow => knowhow += 1,
            crate::models::ItemType::Todo => todo += 1,
            crate::models::ItemType::Idea => idea += 1,
        }
    }
    (knowhow, todo, idea)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ItemStatus, ItemType, Priority, TodoItem};

    fn item(id: &str, t: ItemType, status: ItemStatus) -> TodoItem {
        TodoItem {
            id: id.into(),
            item_type: t,
            title: format!("T{id}"),
            content: "c".into(),
            suggestion: "s".into(),
            tags: vec![],
            status,
            priority: Priority::Medium,
            source_session: "".into(),
            processing: false,
            created_at: "2026".into(),
            updated_at: "2026".into(),
        }
    }

    #[test]
    fn count_active_splits_by_type_and_skips_non_active() {
        let data = TodosFile {
            version: 1,
            project: "p".into(),
            items: vec![
                item("1", ItemType::Knowhow, ItemStatus::Active),
                item("2", ItemType::Knowhow, ItemStatus::Archived), // skipped
                item("3", ItemType::Todo, ItemStatus::Active),
                item("4", ItemType::Todo, ItemStatus::Completed), // skipped
                item("5", ItemType::Idea, ItemStatus::Active),
            ],
        };
        let (k, t, i) = count_active(&data);
        assert_eq!((k, t, i), (1, 1, 1));
    }

    #[test]
    fn count_active_returns_zeros_for_empty() {
        let data = TodosFile {
            version: 1,
            project: "p".into(),
            items: vec![],
        };
        assert_eq!(count_active(&data), (0, 0, 0));
    }

    #[test]
    fn read_todos_defaults_when_file_absent_uses_dir_name() {
        let dir = tempfile::tempdir().unwrap();
        let data = read_todos(dir.path()).unwrap();
        assert!(data.items.is_empty());
        assert_eq!(
            data.project,
            dir.path()
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()
        );
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let original = TodosFile {
            version: 1,
            project: "round-trip".into(),
            items: vec![item("x", ItemType::Todo, ItemStatus::Active)],
        };
        write_todos(dir.path(), &original).unwrap();
        let reread = read_todos(dir.path()).unwrap();
        assert_eq!(reread.project, "round-trip");
        assert_eq!(reread.items.len(), 1);
        assert_eq!(reread.items[0].id, "x");
        assert_eq!(reread.items[0].item_type, ItemType::Todo);
    }

    #[test]
    fn corrupt_todos_json_returns_parse_error_not_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILENAME), "{ not valid json").unwrap();
        // Corrupt data must surface as an error, never as a silent empty file
        // that the next write would clobber.
        let err = read_todos(dir.path()).unwrap_err();
        assert!(err.starts_with("parse error"), "got: {err}");
    }

    #[test]
    fn write_todos_uses_unique_tmp_names() {
        let dir = tempfile::tempdir().unwrap();
        let data = TodosFile {
            version: 1,
            project: "unique-tmp".into(),
            items: vec![],
        };
        write_todos(dir.path(), &data).unwrap();
        write_todos(dir.path(), &data).unwrap();
        // No leftover temp files; the only entry is .todos.json itself.
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(entries, vec![FILENAME.to_string()]);
    }

    /// Regression: an item with an unknown status value (e.g. "pending") must
    /// NOT be dropped — it should be coerced to a safe default ("active") and
    /// kept. This was the root cause of the ymos project vanishing from the
    /// board: a single bad status made serde fail the entire file.
    #[test]
    fn read_todos_coerces_bad_enum_and_keeps_item() {
        let dir = tempfile::tempdir().unwrap();
        let raw = r#"{
            "version": 1,
            "project": "victim",
            "items": [
                {
                    "id": "good-1", "type": "todo", "title": "A", "content": "",
                    "suggestion": "", "tags": [], "status": "active",
                    "priority": "medium", "source_session": "",
                    "created_at": "2026", "updated_at": "2026"
                },
                {
                    "id": "bad-status", "type": "todo", "title": "B", "content": "",
                    "suggestion": "", "tags": [], "status": "pending",
                    "priority": "medium", "source_session": "",
                    "created_at": "2026", "updated_at": "2026"
                },
                {
                    "id": "bad-type", "type": "feature", "title": "C", "content": "",
                    "suggestion": "", "tags": [], "status": "active",
                    "priority": "high", "source_session": "",
                    "created_at": "2026", "updated_at": "2026"
                },
                {
                    "id": "bad-prio", "type": "idea", "title": "D", "content": "",
                    "suggestion": "", "tags": [], "status": "active",
                    "priority": "urgent", "source_session": "",
                    "created_at": "2026", "updated_at": "2026"
                }
            ]
        }"#;
        std::fs::write(dir.path().join(FILENAME), raw).unwrap();

        let data = read_todos(dir.path()).unwrap();
        // ALL 4 items survive — none are dropped for bad enum values.
        assert_eq!(data.items.len(), 4);
        assert_eq!(data.project, "victim");

        // The bad enum values are coerced to safe defaults.
        let by_id: std::collections::HashMap<&str, &TodoItem> =
            data.items.iter().map(|i| (i.id.as_str(), i)).collect();
        assert_eq!(by_id["bad-status"].status, ItemStatus::Active, "pending → active");
        assert_eq!(by_id["bad-type"].item_type, ItemType::Todo, "feature → todo");
        assert_eq!(by_id["bad-prio"].priority, Priority::Medium, "urgent → medium");
        // Good items are untouched.
        assert_eq!(by_id["good-1"].status, ItemStatus::Active);
    }

    /// Structurally broken items (missing a required field) are still dropped —
    /// coercion only fixes bad enum values, not missing data.
    #[test]
    fn read_todos_drops_structurally_broken_items() {
        let dir = tempfile::tempdir().unwrap();
        // "broken-1" is missing "title" (a required String field) — cannot be
        // salvaged, must be dropped. "ok-1" is fine and survives.
        let raw = r#"{
            "version": 1, "project": "mix",
            "items": [
                {
                    "id": "broken-1", "type": "todo", "content": "",
                    "suggestion": "", "tags": [], "status": "active",
                    "priority": "medium", "source_session": "",
                    "created_at": "2026", "updated_at": "2026"
                },
                {
                    "id": "ok-1", "type": "todo", "title": "Fine", "content": "",
                    "suggestion": "", "tags": [], "status": "active",
                    "priority": "medium", "source_session": "",
                    "created_at": "2026", "updated_at": "2026"
                }
            ]
        }"#;
        std::fs::write(dir.path().join(FILENAME), raw).unwrap();

        let data = read_todos(dir.path()).unwrap();
        assert_eq!(data.items.len(), 1, "broken item dropped, good one kept");
        assert_eq!(data.items[0].id, "ok-1");
    }

    /// Truly corrupt JSON (not even valid JSON) still returns an error, not a
    /// silent empty file — the salvage path only triggers on valid JSON with a
    /// bad item, not on garbage.
    #[test]
    fn read_todos_returns_error_for_truly_corrupt_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILENAME), "{ not valid json at all").unwrap();
        let err = read_todos(dir.path()).unwrap_err();
        assert!(err.starts_with("parse error"), "got: {err}");
    }
}
