//! 跨机链路健康聚合（只读）。
//!
//! 把原本沉默的运行时信号——outbox 投递积压、回执滞留/未匹配、夜间窗口
//! 漏跑、每晚处理结果分布——从 `~/questlog-runtime/` 的既有文件聚合出来
//! 给面板展示。本模块只读，绝不写任何文件；digest 在读时从 ledger 聚合，
//! 因此没有新的写路径，也没有新的 schema 需要双端同步。

use crate::todo_ledger::{self, LedgerEntry};
use chrono::{DateTime, FixedOffset};
use std::collections::BTreeMap;

/// 业务时区固定 +08:00（与 todo_sweep::business_tz 一致，不跟随系统时区）。
fn business_tz() -> FixedOffset {
    FixedOffset::east_opt(8 * 3600).expect("+08:00 is a valid offset")
}

/// RFC3339 时间戳 → 业务日期 "YYYY-MM-DD"（+08:00）。解析失败返回 None。
pub fn business_date(iso: &str) -> Option<String> {
    let ts = DateTime::parse_from_rfc3339(iso).ok()?;
    Some(ts.with_timezone(&business_tz()).format("%Y-%m-%d").to_string())
}

/// 单个业务日期的结果分布。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct NightDigest {
    pub date: String,
    pub total: u64,
    /// accepted_report
    pub accepted: u64,
    /// needs_review + cannot_auto
    pub review: u64,
    /// execution_failed + timed_out
    pub failed: u64,
    /// stale / preflight_blocked / result_pending_writeback / 未知
    pub other: u64,
}

fn digest_bucket(outcome: &str) -> &'static str {
    match outcome {
        "accepted_report" => "accepted",
        "needs_review" | "cannot_auto" => "review",
        "execution_failed" | "timed_out" => "failed",
        _ => "other",
    }
}

/// 从 ledger 聚合"最近一个有终态结果的业务夜"的结果分布。
/// 只统计 finalization.state == "applied" 的条目（pending 还在流程中）。
/// 全部条目都无终态时返回 None。
pub fn digest_from_ledger(entries: &BTreeMap<String, LedgerEntry>) -> Option<NightDigest> {
    let mut by_date: BTreeMap<String, NightDigest> = BTreeMap::new();
    for e in entries.values() {
        let f = match &e.finalization {
            Some(f) if f.state == "applied" => f,
            _ => continue,
        };
        let date = match business_date(&f.updated_at) {
            Some(d) => d,
            None => continue,
        };
        let d = by_date.entry(date.clone()).or_insert_with(|| NightDigest {
            date: date.clone(),
            ..Default::default()
        });
        d.total += 1;
        match digest_bucket(&f.outcome) {
            "accepted" => d.accepted += 1,
            "review" => d.review += 1,
            "failed" => d.failed += 1,
            _ => d.other += 1,
        }
    }
    by_date.into_values().last()
}

/// 目录顶层 *.json 文件计数（archived/ 等子目录不计）。
fn count_json_files(dir: &std::path::Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| {
                    e.file_type().map(|t| t.is_file()).unwrap_or(false)
                        && e.path().extension().map(|x| x == "json").unwrap_or(false)
                })
                .count() as u64
        })
        .unwrap_or(0)
}

/// 最近一次 sweep 窗口是否漏跑：missed 日期比 run 日期新即为真。
/// 字符串日期同格式可直接字典序比较。
pub fn window_missed(last_run_date: &str, last_missed_date: &str) -> bool {
    !last_missed_date.is_empty() && last_missed_date > last_run_date
}

/// 汇总当前健康快照。ledger 损坏时 fail-closed 返回 Err（与 read_ledger 语义一致），
/// 前端 catch 后隐藏灯带——宁可不显示也不显示错的健康状态。
/// projects 用于跨项目统计"机器完成但人未确认"的待办（晨报首屏决断卡用）。
pub fn collect(projects: &[(String, std::path::PathBuf)]) -> Result<serde_json::Value, String> {
    let schedule_enabled = crate::runner_config::get_todo_schedule().is_some();
    let sweep_state = todo_ledger::read_sweep_state();
    let ledger = todo_ledger::read_ledger()?;

    // 待确认（review gate）：auto_completed_at 已写但 auto_ack_at 未写。
    let mut pending_ack_items: Vec<serde_json::Value> = vec![];
    for (name, path) in projects {
        let data = match crate::store::read_todos(path) {
            Ok(d) => d,
            Err(_) => continue, // 单项目读取失败不拖垮整体
        };
        for item in &data.items {
            if item.auto_completed_at.is_some() && item.auto_ack_at.is_none() {
                pending_ack_items.push(serde_json::json!({
                    "project": name,
                    "title": item.title,
                }));
            }
        }
    }
    let pending_ack_count = pending_ack_items.len();
    pending_ack_items.truncate(5);

    let entries: BTreeMap<String, LedgerEntry> = ledger.entries.into_iter().collect();
    let in_flight = entries
        .values()
        .filter(|e| todo_ledger::entry_is_open(e))
        .count() as u64;
    let last_applied_at = entries
        .values()
        .filter_map(|e| e.finalization.as_ref())
        .filter(|f| f.state == "applied")
        .map(|f| f.updated_at.clone())
        .max();
    let digest = digest_from_ledger(&entries);

    Ok(serde_json::json!({
        "schedule_enabled": schedule_enabled,
        "sweep_state": sweep_state,
        "window_missed": window_missed(&sweep_state.last_run_date, &sweep_state.last_missed_date),
        "outbox_pending": count_json_files(&todo_ledger::outbox_dir()),
        "receipts_pending": count_json_files(&todo_ledger::receipts_dir()),
        "receipts_unmatched": count_json_files(&todo_ledger::receipts_dir().join("unmatched")),
        "in_flight": in_flight,
        "last_applied_at": last_applied_at,
        "digest": digest,
        "pending_ack": { "count": pending_ack_count, "items": pending_ack_items },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::todo_ledger::{FinalizationRecord, LedgerState};
    use std::collections::HashMap;

    fn entry(outcome: &str, state: &str, updated_at: &str) -> LedgerEntry {
        LedgerEntry {
            task_key: format!("k-{outcome}-{updated_at}"),
            tenant_id: "t".into(),
            todo_id: "todo".into(),
            task_revision: "r".into(),
            consent_revision: 1,
            operation_id: "op".into(),
            state: LedgerState::Closed,
            epic_id: String::new(),
            step_id: String::new(),
            package_id: String::new(),
            created_at: updated_at.into(),
            updated_at: updated_at.into(),
            finalization: Some(FinalizationRecord {
                state: state.into(),
                attempt_id: "a".into(),
                report_hash: "h".into(),
                outcome: outcome.into(),
                updated_at: updated_at.into(),
            }),
        }
    }

    fn ledger_of(entries: Vec<LedgerEntry>) -> BTreeMap<String, LedgerEntry> {
        let map: HashMap<String, LedgerEntry> =
            entries.into_iter().map(|e| (e.task_key.clone(), e)).collect();
        map.into_iter().collect()
    }

    #[test]
    fn business_date_converts_utc_to_plus8() {
        // 20:09 UTC = 次日 04:09 +08:00 —— 跨日必须归入业务次日。
        assert_eq!(
            business_date("2026-09-17T20:09:38.646014+00:00"),
            Some("2026-09-18".to_string())
        );
        assert_eq!(
            business_date("2026-09-17T14:52:42.925Z"),
            Some("2026-09-17".to_string())
        );
        assert_eq!(business_date("not-a-date"), None);
    }

    #[test]
    fn digest_picks_latest_date_and_buckets_outcomes() {
        let entries = ledger_of(vec![
            entry("accepted_report", "applied", "2026-09-17T20:00:00+00:00"), // 业务日 09-18
            entry("needs_review", "applied", "2026-09-16T20:00:00+00:00"),   // 业务日 09-17
            entry("timed_out", "applied", "2026-09-16T21:00:00+00:00"),      // 业务日 09-17
            entry("accepted_report", "pending", "2026-09-18T20:00:00+00:00"), // pending 不计
        ]);
        let d = digest_from_ledger(&entries).expect("has applied entries");
        // 最新结果夜是 09-18（只有 1 条 accepted）。
        assert_eq!(d.date, "2026-09-18");
        assert_eq!((d.total, d.accepted, d.review, d.failed), (1, 1, 0, 0));
    }

    #[test]
    fn digest_none_when_no_applied() {
        let entries = ledger_of(vec![entry(
            "accepted_report",
            "pending",
            "2026-09-17T20:00:00+00:00",
        )]);
        assert_eq!(digest_from_ledger(&entries), None);
    }

    #[test]
    fn window_missed_semantics() {
        assert!(window_missed("2026-09-20", "2026-09-21"));
        assert!(!window_missed("2026-09-24", "2026-09-21"));
        assert!(!window_missed("2026-09-24", ""));
        assert!(!window_missed("", ""));
    }
}
