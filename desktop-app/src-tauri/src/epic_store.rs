//! 方案追踪模块的文件读写 + 进度派生 + 依赖联动。
//!
//! 数据存储在每个项目目录的 `.epics.json`，结构与 `.todos.json` 平行。
//! 进度从不手写：从步骤阶段自动派生。依赖关系自动派生 blocks 字段。

use crate::models::{Epic, EpicStatus, EpicsFile, Step, StepStage};
use std::path::Path;

const FILENAME: &str = ".epics.json";

/// Monotonic counter so temp names stay unique even within one nanosecond.
static UNIQUE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Read .epics.json from a project directory.
/// Returns default empty structure if file doesn't exist.
pub fn read_epics(project_dir: &Path) -> Result<EpicsFile, String> {
    let path = crate::data_root::map_read(&project_dir.join(FILENAME));
    if !path.exists() {
        return Ok(EpicsFile {
            version: 1,
            project: project_dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            epics: vec![],
            steps: vec![],
            revision: 0,
        });
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("read error: {e}"))?;
    let mut file: EpicsFile =
        serde_json::from_str(&raw).map_err(|e| format!("parse error: {e}"))?;
    // Normalize project name if missing
    if file.project.is_empty() {
        file.project = project_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
    }
    Ok(file)
}

/// Write .epics.json atomically. Bumps revision for CAS.
/// Temp name is unique per process+time so concurrent writers never clobber
/// each other's temp file; the last rename wins but the file is never
/// observed half-written.
pub fn write_epics(project_dir: &Path, data: &mut EpicsFile) -> Result<(), String> {
    crate::data_root::guard_write()?;
    let path = crate::data_root::map_read(&project_dir.join(FILENAME));
    data.revision = data.revision.saturating_add(1);
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

/// Calculate epic progress — hierarchical:
/// - If the epic has child epics: mean of child epics' progress (recursive).
/// - Otherwise: mean of its step stages.
///
/// Returns 0 if neither. Progress is derived, never stored.
pub fn epic_progress(epic_id: &str, data: &EpicsFile) -> u32 {
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    epic_progress_visited(epic_id, data, &mut visited)
}

/// Internal recursive impl carrying a `visited` set to defend against parent_id
/// cycles. `.epics.json` is a user-editable local file; a circular parent_id
/// chain must not crash the process. When a cycle is detected, the already-
/// visited node is treated as contributing 0 (the recursion is cut off, same
/// as if it had no children), so the caller still gets a finite answer.
fn epic_progress_visited(
    epic_id: &str,
    data: &EpicsFile,
    visited: &mut std::collections::HashSet<String>,
) -> u32 {
    if visited.contains(epic_id) {
        return 0; // cycle — treat as no contribution, cut the recursion
    }
    visited.insert(epic_id.to_string());
    let children: Vec<&Epic> = data
        .epics
        .iter()
        .filter(|e| e.parent_id.as_deref() == Some(epic_id))
        .collect();
    if !children.is_empty() {
        let total: u32 = children
            .iter()
            .map(|c| epic_progress_visited(&c.id, data, visited))
            .sum();
        return total / children.len() as u32;
    }
    let epic_steps: Vec<&Step> = data.steps.iter().filter(|s| s.epic_id == epic_id).collect();
    if epic_steps.is_empty() {
        return 0;
    }
    let total: u32 = epic_steps.iter().map(|s| s.stage.pct()).sum();
    total / epic_steps.len() as u32
}

/// Derive blocks (reverse of depends_on) for every epic and write to file.
/// Also enforces the dependency rule: if an epic's `depends_on` includes any
/// epic that is not `done`, the epic's status is forced to `blocked` — unless
/// it is already `done` / `paused`.
/// Returns the post-derivation file.
pub fn derive_dependencies(data: &mut EpicsFile) {
    // 1. Rebuild blocks from depends_on (reverse graph).
    //    Collect pairs first to avoid borrow conflicts.
    for epic in data.epics.iter_mut() {
        epic.blocks.clear();
    }
    // Pairs: (dependent_id, dep_id) — dependent depends on dep.
    let pairs: Vec<(String, String)> = data
        .epics
        .iter()
        .flat_map(|e| e.depends_on.iter().map(move |d| (e.id.clone(), d.clone())))
        .collect();
    for (dependent_id, dep_id) in &pairs {
        if let Some(dep) = data.epics.iter_mut().find(|e| e.id == *dep_id) {
            if !dep.blocks.contains(dependent_id) {
                dep.blocks.push(dependent_id.clone());
            }
        }
    }

    // 2. Auto-derive blocked status: epic with unmet dependencies → blocked
    //    (unless it is already done or paused).
    let done_ids: Vec<String> = data
        .epics
        .iter()
        .filter(|e| e.status == EpicStatus::Done)
        .map(|e| e.id.clone())
        .collect();

    for epic in data.epics.iter_mut() {
        if epic.status == EpicStatus::Done || epic.status == EpicStatus::Paused {
            continue;
        }
        let unmet: Vec<&String> = epic
            .depends_on
            .iter()
            .filter(|d| !done_ids.contains(d))
            .collect();
        if !unmet.is_empty() {
            epic.status = EpicStatus::Blocked;
        } else if epic.status == EpicStatus::Blocked {
            // Dependencies all met now — release back to draft (user picks next).
            epic.status = EpicStatus::Draft;
        }
    }
}

/// Convenience: read → mutate in closure → derive → write.
/// All steps run while holding a cross-process file lock, ensuring
/// Node MCP and Tauri UI never race on the same file.
pub fn mutate_epics<F>(project_dir: &Path, f: F) -> Result<EpicsFile, String>
where
    F: FnOnce(&mut EpicsFile) -> Result<(), String>,
{
    crate::data_root::guard_write()?;
    let path = crate::data_root::map_read(&project_dir.join(FILENAME));
    crate::file_lock::with_file_lock(&path, || {
        let mut data = read_epics(project_dir)?;
        f(&mut data)?;
        derive_dependencies(&mut data);
        write_epics(project_dir, &mut data)?;
        Ok(data)
    })
}

/// Build the global summary for a project's epics:
/// counts by status, current focus (active), next-up (draft / blocked-released).
/// Only TOP-LEVEL epics (parent_id=None) are counted — child epics roll up into their parent.
pub fn build_summary(data: &EpicsFile) -> serde_json::Value {
    let top_level: Vec<&Epic> = data
        .epics
        .iter()
        .filter(|e| e.parent_id.is_none())
        .collect();
    let total = top_level.len();
    let active = data
        .epics
        .iter()
        .filter(|e| e.status == EpicStatus::Active)
        .count();
    let observing = data
        .epics
        .iter()
        .filter(|e| e.status == EpicStatus::Observing)
        .count();
    let blocked = data
        .epics
        .iter()
        .filter(|e| e.status == EpicStatus::Blocked)
        .count();
    let done = data
        .epics
        .iter()
        .filter(|e| e.status == EpicStatus::Done)
        .count();

    let focus = data
        .epics
        .iter()
        .find(|e| e.parent_id.is_none() && e.status == EpicStatus::Active)
        .map(|e| {
            serde_json::json!({
                "id": e.id,
                "title": e.title,
                "progress": epic_progress(&e.id, data),
            })
        });

    // Next-up: first non-done, non-active TOP-LEVEL epic (draft preferred, then verifying/observing).
    let next = data
        .epics
        .iter()
        .filter(|e| {
            e.parent_id.is_none() && !matches!(e.status, EpicStatus::Done | EpicStatus::Active)
        })
        .min_by_key(|e| match e.status {
            EpicStatus::Draft => 0,
            EpicStatus::Blocked => 1,
            EpicStatus::Verifying => 2,
            EpicStatus::Observing => 3,
            EpicStatus::Paused => 4,
            _ => 9,
        })
        .map(|e| {
            let waiting_for: Vec<String> = e
                .depends_on
                .iter()
                .filter_map(|d| {
                    data.epics
                        .iter()
                        .find(|x| x.id == *d)
                        .map(|x| x.title.clone())
                })
                .collect();
            serde_json::json!({
                "id": e.id,
                "title": e.title,
                "status": e.status.as_str(),
                "waiting_for": waiting_for,
            })
        });

    serde_json::json!({
        "total": total,
        "active": active,
        "observing": observing,
        "blocked": blocked,
        "done": done,
        "focus": focus,
        "next": next,
    })
}

/// 计算「当前可做步骤」：未完成 + 所有前置（order 更小 ∪ depends_on）都已 observed。
/// 多个可做步骤表示可并行推进。
pub fn actionable_steps<'a>(epic_id: &str, steps: &'a [Step]) -> Vec<&'a Step> {
    let epic_steps: Vec<&Step> = steps.iter().filter(|s| s.epic_id == epic_id).collect();
    let observed_ids: std::collections::HashSet<&String> = epic_steps
        .iter()
        .filter(|s| s.stage == StepStage::Observed)
        .map(|s| &s.id)
        .collect();

    epic_steps
        .iter()
        .filter(|s| s.stage != StepStage::Observed)
        .filter(|s| {
            // 前置：order 更小的步骤 + 显式 depends_on
            let order_prereqs = epic_steps
                .iter()
                .filter(|p| p.order < s.order)
                .map(|p| &p.id);
            let explicit = s.depends_on.iter();
            order_prereqs
                .chain(explicit)
                .all(|id| observed_ids.contains(id))
        })
        .copied()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{EpicLinks, StepLinks};

    const BASE_TS: &str = "2026-01-01T00:00:00.000Z";

    // ── fixture builders — mirror the TS fixtures.ts data shapes ──

    fn epic(id: &str) -> Epic {
        Epic {
            id: id.into(),
            title: format!("Epic {id}"),
            description: "".into(),
            status: EpicStatus::Draft,
            priority: "P1".into(),
            parent_id: None,
            depends_on: vec![],
            blocks: vec![],
            links: EpicLinks::default(),
            max_active: None,
            auto_execute: None,
            acceptance: "".into(),
            close_reason: "".into(),
            created_at: BASE_TS.into(),
            updated_at: BASE_TS.into(),
        }
    }

    fn step(id: &str, epic_id: &str) -> Step {
        Step {
            id: id.into(),
            epic_id: epic_id.into(),
            title: format!("Step {id}"),
            description: "".into(),
            order: 0,
            depends_on: vec![],
            stage: StepStage::Pending,
            links: StepLinks::default(),
            package: None,
            created_at: BASE_TS.into(),
            updated_at: BASE_TS.into(),
        }
    }

    fn file_with(epics: Vec<Epic>, steps: Vec<Step>) -> EpicsFile {
        EpicsFile {
            version: 1,
            project: "test-project".into(),
            epics,
            steps,
            revision: 0,
        }
    }

    // ════════════════════════════════════════════════════════════
    // epic_progress — mirrors mcp-server/test/epic-progress.test.ts
    // ════════════════════════════════════════════════════════════

    #[test]
    fn progress_zero_for_epic_with_no_steps_or_children() {
        let data = file_with(vec![epic("E1")], vec![]);
        assert_eq!(epic_progress("E1", &data), 0);
    }

    #[test]
    fn progress_zero_when_only_step_is_pending() {
        let data = file_with(vec![epic("E1")], vec![step("S1", "E1")]);
        assert_eq!(epic_progress("E1", &data), 0);
    }

    #[test]
    fn progress_100_when_only_step_is_observed() {
        let mut s = step("S1", "E1");
        s.stage = StepStage::Observed;
        let data = file_with(vec![epic("E1")], vec![s]);
        assert_eq!(epic_progress("E1", &data), 100);
    }

    #[test]
    fn progress_averages_step_stages_and_truncates_50_75_to_62() {
        // Integer division: (50 + 75) / 2 = 62 (u32 truncation, matches TS Math.floor).
        let mut sa = step("S1", "E1");
        sa.stage = StepStage::Coded; // 50
        let mut sb = step("S2", "E1");
        sb.stage = StepStage::Verified; // 75
        let data = file_with(vec![epic("E1")], vec![sa, sb]);
        assert_eq!(epic_progress("E1", &data), 62);
    }

    #[test]
    fn progress_ignores_steps_of_other_epics() {
        let mut s1 = step("S1", "E1");
        s1.stage = StepStage::Observed;
        let s2 = step("S2", "E2"); // pending, different epic — ignored
        let data = file_with(vec![epic("E1"), epic("E2")], vec![s1, s2]);
        assert_eq!(epic_progress("E1", &data), 100);
    }

    #[test]
    fn parent_progress_is_mean_of_children() {
        // E1 → E1a (62) + E1b (100) → mean = 81
        let top = epic("E1");
        let mut ca = epic("E1a");
        ca.parent_id = Some("E1".into());
        let mut cb = epic("E1b");
        cb.parent_id = Some("E1".into());

        let mut sa = step("Sa1", "E1a");
        sa.stage = StepStage::Coded;
        let mut sa2 = step("Sa2", "E1a");
        sa2.stage = StepStage::Verified;
        let mut sb = step("Sb1", "E1b");
        sb.stage = StepStage::Observed;

        let data = file_with(vec![top, ca, cb], vec![sa, sa2, sb]);
        assert_eq!(epic_progress("E1a", &data), 62);
        assert_eq!(epic_progress("E1b", &data), 100);
        assert_eq!(epic_progress("E1", &data), 81);
    }

    #[test]
    fn child_with_no_steps_contributes_zero_to_parent_mean() {
        let top = epic("E1");
        let mut ca = epic("E1a");
        ca.parent_id = Some("E1".into());
        let mut cb = epic("E1b");
        cb.parent_id = Some("E1".into());

        let mut sa = step("Sa1", "E1a");
        sa.stage = StepStage::Observed;
        // E1b has no steps → contributes 0. Parent = (100 + 0) / 2 = 50.

        let data = file_with(vec![top, ca, cb], vec![sa]);
        assert_eq!(epic_progress("E1", &data), 50);
    }

    #[test]
    fn three_level_nesting_top_from_children_from_steps() {
        let top = epic("E1");
        let mut ca = epic("E1a");
        ca.parent_id = Some("E1".into());
        let mut cb = epic("E1b");
        cb.parent_id = Some("E1".into());

        let mut sa = step("Sa1", "E1a");
        sa.stage = StepStage::Observed; // E1a = 100
        let mut sb = step("Sb1", "E1b");
        sb.stage = StepStage::Coded; // E1b = 50

        let data = file_with(vec![top, ca, cb], vec![sa, sb]);
        assert_eq!(epic_progress("E1", &data), 75);
    }

    #[test]
    fn progress_for_nonexistent_epic_returns_zero() {
        let data = file_with(vec![epic("E1")], vec![]);
        assert_eq!(epic_progress("nonexistent", &data), 0);
    }

    // ── Cycle defense (M1): .epics.json is user-editable; a circular
    //    parent_id chain must NOT cause unbounded recursion / stack overflow. ──

    #[test]
    fn progress_survives_self_referential_parent_id() {
        // E1.parent_id = E1 — without cycle defense this recurses forever.
        let mut e1 = epic("E1");
        e1.parent_id = Some("E1".into());
        let mut s1 = step("S1", "E1");
        s1.stage = StepStage::Observed;
        let data = file_with(vec![e1], vec![s1]);
        // E1 is its own only child; the recursive call hits the visited guard
        // and returns 0. E1 never reaches its own steps (it has a "child").
        // The point is termination, not a meaningful value for malformed data.
        assert_eq!(epic_progress("E1", &data), 0);
    }

    #[test]
    fn progress_survives_two_node_parent_id_cycle() {
        // E1.parent_id = E2 and E2.parent_id = E1 — the exact crash fixture.
        let mut e1 = epic("E1");
        e1.parent_id = Some("E2".into());
        let mut e2 = epic("E2");
        e2.parent_id = Some("E1".into());
        let data = file_with(vec![e1, e2], vec![]);
        assert_eq!(epic_progress("E1", &data), 0);
        assert_eq!(epic_progress("E2", &data), 0);
    }

    #[test]
    fn progress_terminates_three_node_cycle_and_counts_valid_sibling() {
        // E1 → E2 → E3 → E1 (cycle), plus healthy child E1a of E1.
        // E1's children = [E2, E1a]. E2 → E3 → E1 (visited, cut, 0).
        // E1a observed → 100. E1 = mean(0, 100) = 50.
        let e1 = epic("E1");
        let mut e2 = epic("E2");
        e2.parent_id = Some("E1".into());
        let mut e3 = epic("E3");
        e3.parent_id = Some("E2".into());
        let mut e1a = epic("E1a");
        e1a.parent_id = Some("E1".into());
        let mut s1 = step("S1", "E1a");
        s1.stage = StepStage::Observed;
        let data = file_with(vec![e1, e2, e3, e1a], vec![s1]);
        assert_eq!(epic_progress("E1", &data), 50);
    }

    // ════════════════════════════════════════════════════════════
    // derive_dependencies — mirrors mcp-server/test/epic-dependencies.test.ts
    // ════════════════════════════════════════════════════════════

    #[test]
    fn blocks_rebuilt_as_reverse_of_depends_on() {
        let mut a = epic("A");
        a.status = EpicStatus::Done;
        let mut b = epic("B");
        b.depends_on = vec!["A".into()];
        let mut data = file_with(vec![a, b], vec![]);
        derive_dependencies(&mut data);
        let a = data.epics.iter().find(|e| e.id == "A").unwrap();
        assert!(a.blocks.contains(&"B".to_string()));
    }

    #[test]
    fn blocks_accumulate_multiple_dependents() {
        let mut a = epic("A");
        a.status = EpicStatus::Done;
        let mut b = epic("B");
        b.depends_on = vec!["A".into()];
        let mut c = epic("C");
        c.depends_on = vec!["A".into()];
        let mut data = file_with(vec![a, b, c], vec![]);
        derive_dependencies(&mut data);
        let a = data.epics.iter().find(|e| e.id == "A").unwrap();
        let mut blocks = a.blocks.clone();
        blocks.sort();
        assert_eq!(blocks, vec!["B".to_string(), "C".to_string()]);
    }

    #[test]
    fn blocks_cleared_before_rebuild() {
        let mut a = epic("A");
        a.status = EpicStatus::Done;
        a.blocks = vec!["STALE".into()];
        let b = epic("B");
        let mut data = file_with(vec![a, b], vec![]);
        derive_dependencies(&mut data);
        let a = data.epics.iter().find(|e| e.id == "A").unwrap();
        assert!(a.blocks.is_empty());
    }

    #[test]
    fn forces_blocked_when_dependency_not_done() {
        let mut a = epic("A");
        a.status = EpicStatus::Active;
        let mut b = epic("B");
        b.status = EpicStatus::Active;
        b.depends_on = vec!["A".into()];
        let mut data = file_with(vec![a, b], vec![]);
        derive_dependencies(&mut data);
        let b = data.epics.iter().find(|e| e.id == "B").unwrap();
        assert_eq!(b.status, EpicStatus::Blocked);
    }

    #[test]
    fn leaves_unblocked_when_dependency_done() {
        let mut a = epic("A");
        a.status = EpicStatus::Done;
        let mut b = epic("B");
        b.status = EpicStatus::Active;
        b.depends_on = vec!["A".into()];
        let mut data = file_with(vec![a, b], vec![]);
        derive_dependencies(&mut data);
        let b = data.epics.iter().find(|e| e.id == "B").unwrap();
        assert_eq!(b.status, EpicStatus::Active);
    }

    #[test]
    fn blocks_when_any_of_multiple_deps_not_done() {
        let mut a = epic("A");
        a.status = EpicStatus::Done;
        let mut b = epic("B");
        b.status = EpicStatus::Active;
        let mut c = epic("C");
        c.status = EpicStatus::Active;
        c.depends_on = vec!["A".into(), "B".into()];
        let mut data = file_with(vec![a, b, c], vec![]);
        derive_dependencies(&mut data);
        let c = data.epics.iter().find(|e| e.id == "C").unwrap();
        assert_eq!(c.status, EpicStatus::Blocked);
    }

    #[test]
    fn releases_blocked_to_draft_when_deps_satisfied() {
        let mut a = epic("A");
        a.status = EpicStatus::Done;
        let mut b = epic("B");
        b.status = EpicStatus::Blocked;
        b.depends_on = vec!["A".into()];
        let mut data = file_with(vec![a, b], vec![]);
        derive_dependencies(&mut data);
        let b = data.epics.iter().find(|e| e.id == "B").unwrap();
        assert_eq!(b.status, EpicStatus::Draft);
    }

    #[test]
    fn done_epic_immune_to_blocking() {
        let mut a = epic("A");
        a.status = EpicStatus::Active;
        let mut b = epic("B");
        b.status = EpicStatus::Done;
        b.depends_on = vec!["A".into()];
        let mut data = file_with(vec![a, b], vec![]);
        derive_dependencies(&mut data);
        let b = data.epics.iter().find(|e| e.id == "B").unwrap();
        assert_eq!(b.status, EpicStatus::Done);
    }

    #[test]
    fn paused_epic_immune_to_blocking() {
        let mut a = epic("A");
        a.status = EpicStatus::Active;
        let mut b = epic("B");
        b.status = EpicStatus::Paused;
        b.depends_on = vec!["A".into()];
        let mut data = file_with(vec![a, b], vec![]);
        derive_dependencies(&mut data);
        let b = data.epics.iter().find(|e| e.id == "B").unwrap();
        assert_eq!(b.status, EpicStatus::Paused);
    }

    #[test]
    fn paused_not_released_even_when_deps_met() {
        let mut a = epic("A");
        a.status = EpicStatus::Done;
        let mut b = epic("B");
        b.status = EpicStatus::Paused;
        b.depends_on = vec!["A".into()];
        let mut data = file_with(vec![a, b], vec![]);
        derive_dependencies(&mut data);
        let b = data.epics.iter().find(|e| e.id == "B").unwrap();
        assert_eq!(b.status, EpicStatus::Paused);
    }

    #[test]
    fn orphan_dep_id_does_not_crash_and_blocks() {
        let mut b = epic("B");
        b.status = EpicStatus::Active;
        b.depends_on = vec!["GHOST".into()];
        let mut data = file_with(vec![b], vec![]);
        // Should not panic.
        derive_dependencies(&mut data);
        let b = data.epics.iter().find(|e| e.id == "B").unwrap();
        assert_eq!(b.status, EpicStatus::Blocked);
    }

    // ── Cycle / self-reference (M4): depends_on cycles are malformed but must
    //    not crash. Pins the CURRENT behavior so future changes are intentional. ──

    #[test]
    fn self_referential_depends_on_blocks_and_never_releases() {
        // A depends on itself → A is never "done" → forced to Blocked, and
        // cannot self-release (its only dep is itself).
        let mut a = epic("A");
        a.status = EpicStatus::Active;
        a.depends_on = vec!["A".into()];
        let mut data = file_with(vec![a], vec![]);
        derive_dependencies(&mut data);
        let a = data.epics.iter().find(|e| e.id == "A").unwrap();
        assert_eq!(a.status, EpicStatus::Blocked);
    }

    #[test]
    fn two_node_cycle_blocks_both_then_releases_on_done() {
        // A→B, B→A: both active initially → both blocked. Mark A done and
        // re-derive → B's dep A is met → B releases to Draft.
        let mut a = epic("A");
        a.status = EpicStatus::Active;
        a.depends_on = vec!["B".into()];
        let mut b = epic("B");
        b.status = EpicStatus::Active;
        b.depends_on = vec!["A".into()];
        let mut data = file_with(vec![a, b], vec![]);

        derive_dependencies(&mut data);
        assert_eq!(
            data.epics.iter().find(|e| e.id == "A").unwrap().status,
            EpicStatus::Blocked
        );
        assert_eq!(
            data.epics.iter().find(|e| e.id == "B").unwrap().status,
            EpicStatus::Blocked
        );

        // Mark A done, re-derive.
        for e in data.epics.iter_mut() {
            if e.id == "A" {
                e.status = EpicStatus::Done;
            }
        }
        derive_dependencies(&mut data);
        assert_eq!(
            data.epics.iter().find(|e| e.id == "A").unwrap().status,
            EpicStatus::Done
        );
        assert_eq!(
            data.epics.iter().find(|e| e.id == "B").unwrap().status,
            EpicStatus::Draft
        );
    }

    #[test]
    fn independent_epics_left_untouched() {
        let mut a = epic("A");
        a.status = EpicStatus::Active;
        let mut b = epic("B");
        b.status = EpicStatus::Verifying;
        let mut data = file_with(vec![a, b], vec![]);
        derive_dependencies(&mut data);
        assert_eq!(
            data.epics.iter().find(|e| e.id == "A").unwrap().status,
            EpicStatus::Active
        );
        assert_eq!(
            data.epics.iter().find(|e| e.id == "B").unwrap().status,
            EpicStatus::Verifying
        );
    }

    // ════════════════════════════════════════════════════════════
    // actionable_steps — mirrors mcp-server/test/epic-actionable.test.ts
    // ════════════════════════════════════════════════════════════

    fn ids(steps: Vec<&Step>) -> Vec<String> {
        steps.iter().map(|s| s.id.clone()).collect()
    }

    #[test]
    fn first_step_actionable_when_nothing_observed() {
        let mut s1 = step("S1", "E1");
        s1.order = 0;
        let mut s2 = step("S2", "E1");
        s2.order = 1;
        let data = vec![s1, s2];
        let result = actionable_steps("E1", &data);
        assert_eq!(ids(result), vec!["S1".to_string()]);
    }

    #[test]
    fn next_step_unblocked_after_prior_observed() {
        let mut s1 = step("S1", "E1");
        s1.order = 0;
        s1.stage = StepStage::Observed;
        let mut s2 = step("S2", "E1");
        s2.order = 1;
        let data = vec![s1, s2];
        let result = actionable_steps("E1", &data);
        assert_eq!(ids(result), vec!["S2".to_string()]);
    }

    #[test]
    fn s2_blocked_while_s1_only_coded() {
        let mut s1 = step("S1", "E1");
        s1.order = 0;
        s1.stage = StepStage::Coded;
        let mut s2 = step("S2", "E1");
        s2.order = 1;
        let data = vec![s1, s2];
        let result = actionable_steps("E1", &data);
        // S1 not observed → it's the only actionable; S2 waits.
        assert_eq!(ids(result), vec!["S1".to_string()]);
    }

    #[test]
    fn parallel_steps_both_actionable_at_same_order() {
        let s1 = step("S1", "E1"); // order 0
        let s2 = step("S2", "E1"); // order 0
        let data = vec![s1, s2];
        let result = actionable_steps("E1", &data);
        let mut got = ids(result);
        got.sort();
        assert_eq!(got, vec!["S1".to_string(), "S2".to_string()]);
    }

    #[test]
    fn empty_when_all_observed() {
        let mut s1 = step("S1", "E1");
        s1.order = 0;
        s1.stage = StepStage::Observed;
        let mut s2 = step("S2", "E1");
        s2.order = 1;
        s2.stage = StepStage::Observed;
        let data = vec![s1, s2];
        let result = actionable_steps("E1", &data);
        assert!(result.is_empty());
    }

    #[test]
    fn ignores_steps_of_other_epics() {
        let s1 = step("S1", "E1");
        let s2 = step("S2", "E2");
        let data = vec![s1, s2];
        let result = actionable_steps("E1", &data);
        assert_eq!(ids(result), vec!["S1".to_string()]);
    }

    #[test]
    fn step_unblocked_when_order_prereqs_and_explicit_dep_observed() {
        // S1 order 0 observed, S2 order 1 observed, S3 order 2 depends_on S2.
        // S3's order-prereqs (S1, S2) and explicit dep (S2) all observed → actionable.
        let mut s1 = step("S1", "E1");
        s1.order = 0;
        s1.stage = StepStage::Observed;
        let mut s2 = step("S2", "E1");
        s2.order = 1;
        s2.stage = StepStage::Observed;
        let mut s3 = step("S3", "E1");
        s3.order = 2;
        s3.depends_on = vec!["S2".into()];
        let data = vec![s1, s2, s3];
        let result = actionable_steps("E1", &data);
        assert_eq!(ids(result), vec!["S3".to_string()]);
    }

    // ════════════════════════════════════════════════════════════
    // build_summary
    // ════════════════════════════════════════════════════════════

    #[test]
    fn summary_counts_top_level_and_picks_active_focus() {
        // A (active, top-level) has child C (active). Step S1 hangs on C.
        // epic_progress(A) recurses into its only child C → C has an observed
        // step → C = 100 → A = mean([100]) = 100.
        let mut a = epic("A");
        a.status = EpicStatus::Active;
        let mut b = epic("B");
        b.status = EpicStatus::Done;
        let mut child = epic("C");
        child.parent_id = Some("A".into());
        child.status = EpicStatus::Active;

        let mut s = step("S1", "C");
        s.stage = StepStage::Observed;

        let data = file_with(vec![a, b, child], vec![s]);
        let summary = build_summary(&data);
        assert_eq!(summary["total"], 2); // A + B (child C excluded from top-level)
        assert_eq!(summary["active"], 2); // A + child (active count is global)
        assert_eq!(summary["done"], 1);
        assert_eq!(summary["focus"]["id"], "A");
        assert_eq!(summary["focus"]["progress"], 100);
    }

    #[test]
    fn summary_next_prefers_draft_over_blocked() {
        let mut draft = epic("D");
        draft.status = EpicStatus::Draft;
        let mut blocked = epic("B");
        blocked.status = EpicStatus::Blocked;
        let data = file_with(vec![draft, blocked], vec![]);
        let summary = build_summary(&data);
        // Draft (rank 0) beats Blocked (rank 1).
        assert_eq!(summary["next"]["id"], "D");
    }

    #[test]
    fn summary_next_waiting_for_resolves_dep_titles() {
        let mut a = epic("A");
        a.title = "First".into();
        a.status = EpicStatus::Active; // not done, so B's dep is unmet
        let mut b = epic("B");
        b.status = EpicStatus::Draft;
        b.depends_on = vec!["A".into()];
        let data = file_with(vec![a, b], vec![]);
        let summary = build_summary(&data);
        assert_eq!(summary["next"]["id"], "B");
        assert_eq!(summary["next"]["waiting_for"][0], "First");
    }

    // ════════════════════════════════════════════════════════════
    // Data layer: read/write/mutate with tempfile
    // ════════════════════════════════════════════════════════════

    #[test]
    fn read_epics_defaults_when_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        let data = read_epics(dir.path()).unwrap();
        assert!(data.epics.is_empty());
        assert!(data.steps.is_empty());
        // project name falls back to the directory's basename.
        assert_eq!(
            data.project,
            dir.path()
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()
        );
    }

    #[test]
    fn mutate_epics_runs_closure_derives_and_writes() {
        let dir = tempfile::tempdir().unwrap();
        // Push one epic via the closure; derive + write should run automatically.
        let written = mutate_epics(dir.path(), |data| {
            data.epics.push(epic("E1"));
            Ok(())
        })
        .unwrap();
        assert_eq!(written.epics.len(), 1);

        // Re-read from disk to confirm it persisted.
        let reread = read_epics(dir.path()).unwrap();
        assert_eq!(reread.epics.len(), 1);
        assert_eq!(reread.epics[0].id, "E1");
    }

    #[test]
    fn corrupt_epics_json_returns_parse_error_not_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILENAME), "{ broken").unwrap();
        // Corrupt data must surface as an error, never as a silent empty file
        // that the next write would clobber.
        let err = read_epics(dir.path()).unwrap_err();
        assert!(err.starts_with("parse error"), "got: {err}");
    }

    #[test]
    fn mutate_epics_applies_dependency_derivation_on_write() {
        // Insert B depending on A, with A NOT done. After mutate_epics writes,
        // B should be persisted as Blocked (derivation applied before write).
        let dir = tempfile::tempdir().unwrap();
        let _ = mutate_epics(dir.path(), |data| {
            let mut a = epic("A");
            a.status = EpicStatus::Active;
            let mut b = epic("B");
            b.status = EpicStatus::Active;
            b.depends_on = vec!["A".into()];
            data.epics.push(a);
            data.epics.push(b);
            Ok(())
        })
        .unwrap();

        let reread = read_epics(dir.path()).unwrap();
        let b = reread.epics.iter().find(|e| e.id == "B").unwrap();
        assert_eq!(b.status, EpicStatus::Blocked);
    }
}
