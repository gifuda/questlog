use crate::models::{ItemStatus, TodosFile};
use std::path::Path;

const FILENAME: &str = ".todos.json";

/// Monotonic counter so temp names stay unique even within one nanosecond.
static UNIQUE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Read .todos.json from a project directory.
/// Returns default empty structure if file doesn't exist.
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
    serde_json::from_str(&raw).map_err(|e| format!("parse error: {e}"))
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
}
