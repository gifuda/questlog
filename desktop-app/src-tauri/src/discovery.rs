use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const REGISTRY_FILE: &str = ".todo-board.json";

/// A project entry in the active list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    pub name: String,
    pub path: String,
}

/// A trashed project entry, with deletion timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrashEntry {
    #[serde(flatten)]
    pub entry: RegistryEntry,
    pub deleted_at: String,
}

/// Per-project push quality feedback (recorded by desktop + read by MCP).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectFeedback {
    #[serde(default)]
    pub pushes: u64,
    #[serde(default)]
    pub archived_soon: u64,
}

/// The full registry file structure.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Registry {
    #[serde(default)]
    projects: Vec<RegistryEntry>,
    #[serde(default)]
    trash: Vec<TrashEntry>,
    #[serde(default)]
    feedback: std::collections::HashMap<String, ProjectFeedback>,
}

fn registry_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("no home dir")?;
    Ok(home.join(REGISTRY_FILE))
}

fn read_registry() -> Registry {
    let path = match registry_path() {
        Ok(p) => p,
        Err(_) => return Registry::default(),
    };
    let raw = match std::fs::read_to_string(&path) {
        Ok(r) => r,
        Err(_) => return Registry::default(),
    };
    match serde_json::from_str(&raw) {
        Ok(reg) => reg,
        Err(_) => {
            // Corrupt registry — back it up so the original is never silently
            // overwritten, then start fresh.
            backup_corrupt_file(&path);
            Registry::default()
        }
    }
}

/// Rename a corrupt data file to `<name>.bak.<ts>` so the original survives.
fn backup_corrupt_file(path: &std::path::Path) {
    let ts = chrono::Utc::now().timestamp_millis();
    let bak = path.with_extension(format!("bak.{ts}"));
    if let Err(e) = std::fs::rename(path, &bak) {
        eprintln!(
            "[discovery] corrupt file {} could not be backed up: {e}",
            path.display()
        );
    } else {
        eprintln!(
            "[discovery] {} is corrupt — moved to {}; starting fresh. Recover data from the backup.",
            path.display(),
            bak.display()
        );
    }
}

/// Unique temp name for atomic writes (pid + nanos + counter) so concurrent
/// writers never clobber each other's temp file — even within the same
/// millisecond.
fn unique_tmp_path(path: &std::path::Path) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    path.with_extension(format!("tmp-{}-{}-{}", std::process::id(), nanos, seq))
}

/// Atomic write via temp file + rename. Concurrent writers keep their own temp
/// file; the last rename wins but the file is never observed half-written.
fn atomic_write(path: &std::path::Path, contents: &str) -> Result<(), String> {
    let tmp = unique_tmp_path(path);
    std::fs::write(&tmp, contents).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

fn write_registry(reg: &Registry) -> Result<(), String> {
    let path = registry_path()?;
    let json = serde_json::to_string_pretty(reg).map_err(|e| e.to_string())?;
    atomic_write(&path, &json)
}

/// Read projects from the central registry file (~/.todo-board.json).
pub fn discover_projects() -> Vec<(String, PathBuf)> {
    read_registry()
        .projects
        .into_iter()
        .filter(|e| PathBuf::from(&e.path).join(".todos.json").exists())
        .map(|e| (e.name, PathBuf::from(e.path)))
        .collect()
}

/// Soft-delete: move project from active list to trash.
pub fn remove_project(name: &str) -> Result<(), String> {
    let mut reg = read_registry();

    let idx = reg.projects.iter().position(|e| e.name == name);
    let removed = idx.map(|i| reg.projects.remove(i));

    if let Some(entry) = removed {
        reg.trash.push(TrashEntry {
            entry,
            deleted_at: Utc::now().to_rfc3339(),
        });
        write_registry(&reg)?;
    }
    Ok(())
}

/// Restore: move project from trash back to active list.
pub fn restore_project(name: &str) -> Result<(), String> {
    let mut reg = read_registry();

    let idx = reg.trash.iter().position(|t| t.entry.name == name);
    let restored = idx.map(|i| reg.trash.remove(i).entry);

    if let Some(entry) = restored {
        reg.projects.push(entry);
        write_registry(&reg)?;
    }
    Ok(())
}

/// Permanent delete: remove from trash AND delete .todos.json.
pub fn permanently_delete(name: &str) -> Result<(), String> {
    let mut reg = read_registry();

    let idx = reg.trash.iter().position(|t| t.entry.name == name);
    if let Some(i) = idx {
        let entry = reg.trash.remove(i);
        let todos_path = PathBuf::from(&entry.entry.path).join(".todos.json");
        let _ = std::fs::remove_file(todos_path);
        write_registry(&reg)?;
    }
    Ok(())
}

/// List trash entries.
pub fn list_trash() -> Result<Vec<TrashEntry>, String> {
    Ok(read_registry().trash)
}

/// Add a project back to the registry (for undo).
/// Also cleans it from trash if present.
pub fn add_project(name: &str, path: &str) -> Result<(), String> {
    let mut reg = read_registry();

    // Don't duplicate
    if !reg.projects.iter().any(|e| e.name == name) {
        reg.projects.push(RegistryEntry {
            name: name.to_string(),
            path: path.to_string(),
        });
    }

    // Remove from trash if present
    reg.trash.retain(|t| t.entry.name != name);

    write_registry(&reg)
}

/// Record that an item was archived quickly after creation (a low-value push signal).
/// Keyed by project path in the registry's feedback map. Read by MCP list_todos.
pub fn record_archived_soon(project_path: &str) {
    let mut reg = read_registry();
    let f = reg.feedback.entry(project_path.to_string()).or_default();
    f.archived_soon += 1;
    let _ = write_registry(&reg);
}

/// Reorder projects by the given name order.
pub fn reorder_projects(names: &[String]) -> Result<(), String> {
    let mut reg = read_registry();

    let existing = std::mem::take(&mut reg.projects);
    let mut by_name: std::collections::HashMap<&str, RegistryEntry> = existing
        .iter()
        .map(|e| (e.name.as_str(), e.clone()))
        .collect();

    let mut reordered = Vec::new();
    for name in names {
        if let Some(entry) = by_name.remove(name.as_str()) {
            reordered.push(entry);
        }
    }
    // Append any remaining entries not in the reorder list
    reordered.extend(by_name.into_values());

    reg.projects = reordered;
    write_registry(&reg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_file_without_leaving_temp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("registry.json");
        atomic_write(&path, "{\"projects\":[]}").unwrap();
        assert!(path.exists());
        // No temp leftovers.
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn backup_corrupt_file_renames_original_with_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("registry.json");
        std::fs::write(&path, "{ broken").unwrap();
        backup_corrupt_file(&path);
        assert!(!path.exists(), "original must be moved away");
        let baks: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with("registry.bak."))
            .collect();
        assert_eq!(baks.len(), 1, "one backup file expected, got {baks:?}");
    }

    #[test]
    fn unique_tmp_paths_differ() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("registry.json");
        let a = unique_tmp_path(&p);
        let b = unique_tmp_path(&p);
        assert_ne!(a, b, "tmp names must be unique across calls");
        assert!(!a.exists());
    }
}
