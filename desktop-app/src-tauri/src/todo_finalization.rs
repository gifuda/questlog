//! Todo finalization — receipt reconciliation on the manager (待办租户 G4,
//! plan §9.3).
//!
//! Persists-order discipline (crash recovery by construction):
//!   1. persist the report (report.md + manifest with sha256)
//!   2. ledger entry → finalization PENDING (attempt_id + report_hash)
//!   3. run the native verifier (Node todo-report-verify-cli)
//!   4. CAS write-back into .todos.json (re-check type/status/opt-in/consent/
//!      no-manual-takeover inside the lock; replay sees the same
//!      applied_attempt_id and no-ops)
//!   5. ledger finalization APPLIED + entry closed; receipt archived
//!
//! A crash anywhere before 5 → the same receipt is reprocessed on the next
//! tick: the report is re-persisted identically (same content), the verifier
//! is deterministic, and the write-back is idempotent by applied_attempt_id.
//! The AI is NEVER called again (execution already happened on the collector).

use crate::todo_ledger::*;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub enum VerifierOutcome {
    AcceptedReport,
    CannotAuto,
    NeedsReview,
    Rejected,
}

#[derive(Debug, Clone)]
pub struct VerifierOutput {
    pub outcome: VerifierOutcome,
    pub reasons: Vec<String>,
}

/// The verifier wrapper: spawns `node <verify_cli>` with structured stdin.
pub fn verify_via_cli(
    node_bin: &str,
    verify_cli: &str,
    receipt: &Receipt,
    snapshot_files: &[String],
) -> Result<VerifierOutput, String> {
    let payload = serde_json::json!({
        "stdout": receipt.report_stdout,
        "task_key": receipt.task_key,
        "snapshot_files": snapshot_files,
        "exit_ok": receipt.exit_ok,
        "timed_out": receipt.timed_out,
        "budget_exceeded": receipt.budget_exceeded,
        "changes_detected": receipt.changes_detected,
    });
    let (_ok, stdout, _stderr) =
        crate::todo_sweep::run_cli_with_timeout(node_bin, verify_cli, &payload, 60)
            .map_err(|e| format!("verify cli: {e}"))?;
    let v: serde_json::Value =
        serde_json::from_str(&stdout).map_err(|e| format!("parse verifier output: {e}"))?;
    let outcome = match v.get("outcome").and_then(|s| s.as_str()) {
        Some("accepted_report") => VerifierOutcome::AcceptedReport,
        Some("cannot_auto") => VerifierOutcome::CannotAuto,
        Some("needs_review") => VerifierOutcome::NeedsReview,
        _ => VerifierOutcome::Rejected,
    };
    let reasons = v
        .get("reasons")
        .and_then(|r| r.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    Ok(VerifierOutput { outcome, reasons })
}

/// Snapshot file list of a repo at a pinned SHA (`git ls-tree -r`). Empty on
/// failure → every evidence ref fails → needs_review (fail-closed, never
/// accepted).
pub fn snapshot_files_at(repo_root: &str, sha: &str) -> Vec<String> {
    let out = std::process::Command::new("git")
        .args(["-C", repo_root, "ls-tree", "-r", "--name-only", sha])
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect(),
        _ => vec![],
    }
}

/// Card outcome written into auto_result (plan §5.3).
fn card_outcome(v: &VerifierOutcome, receipt: &Receipt) -> &'static str {
    match v {
        VerifierOutcome::AcceptedReport => "accepted_report",
        VerifierOutcome::CannotAuto => "cannot_auto",
        VerifierOutcome::NeedsReview => "needs_review",
        VerifierOutcome::Rejected => {
            if receipt.timed_out {
                "timed_out"
            } else {
                "execution_failed"
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum FinalizeResult {
    Applied { task_key: String, outcome: String },
    /// Receipt belongs to an older authorization — report archived, card
    /// untouched (plan §5.3 stale row).
    StaleArchived { task_key: String, reason: String },
    /// No live ledger entry for this task_key.
    Unmatched { task_key: String, reason: String },
    /// Infrastructure failure — retry next tick (receipt stays in place).
    Error { task_key: String, error: String },
}

/// Process ONE receipt file through the full state machine.
/// `verify` is injectable for tests; `project_dir_of` maps tenant → project
/// directory (registry).
pub fn finalize_receipt(
    receipt_path: &Path,
    verify: &dyn Fn(&Receipt, &[String]) -> Result<VerifierOutput, String>,
    project_dir_of: &dyn Fn(&str) -> Option<PathBuf>,
    _node_cfg: Option<(&str, &str)>,
    snapshot_of: &dyn Fn(&str, &str) -> Vec<String>,
) -> FinalizeResult {
    let raw = match std::fs::read_to_string(receipt_path) {
        Ok(r) => r,
        Err(e) => {
            return FinalizeResult::Error {
                task_key: "?".into(),
                error: format!("read receipt: {e}"),
            }
        }
    };
    let receipt: Receipt = match serde_json::from_str(&raw) {
        Ok(r) => r,
        Err(e) => {
            return FinalizeResult::Error {
                task_key: "?".into(),
                error: format!("parse receipt: {e}"),
            }
        }
    };
    let tk = receipt.task_key.clone();

    // Locate the live ledger entry.
    let entry = {
        let ledger = match read_ledger() {
            Ok(l) => l,
            Err(e) => {
                return FinalizeResult::Error {
                    task_key: tk,
                    error: e,
                }
            }
        };
        match ledger.entries.get(&tk) {
            Some(e) if e.state == LedgerState::Published => e.clone(),
            Some(e) if e.finalization.as_ref().map(|f| f.state == "applied").unwrap_or(false) => {
                // Idempotent replay (§9.3 step 5): already applied.
                let _ = archive_receipt(receipt_path, "applied");
                return FinalizeResult::Applied {
                    task_key: tk,
                    outcome: e.finalization.as_ref().map(|f| f.outcome.clone()).unwrap_or_default(),
                };
            }
            Some(e) if e.state == LedgerState::Closed => {
                let _ = archive_receipt(receipt_path, "unmatched");
                return FinalizeResult::Unmatched {
                    task_key: tk,
                    reason: format!("ledger entry closed ({:?})", e.finalization.as_ref().map(|f| f.outcome.clone())),
                };
            }
            Some(e) => {
                let _ = archive_receipt(receipt_path, "unmatched");
                return FinalizeResult::Unmatched {
                    task_key: tk,
                    reason: format!("entry state {:?}", e.state),
                };
            }
            None => {
                let _ = archive_receipt(receipt_path, "unmatched");
                return FinalizeResult::Unmatched {
                    task_key: tk,
                    reason: "no ledger entry for task_key".into(),
                };
            }
        }
    };

    // 1. Persist the report FIRST (crash before write-back keeps evidence).
    let report_hash = match persist_report(&entry, &receipt) {
        Ok(h) => h,
        Err(e) => {
            return FinalizeResult::Error {
                task_key: tk,
                error: e,
            }
        }
    };

    // 2. Mark finalization pending (before any write-back).
    let consent_revision = entry.consent_revision;
    let todo_id = entry.todo_id.clone();
    let tenant_id = entry.tenant_id.clone();
    let attempt_id = receipt.attempt_id.clone();
    if let Err(e) = mutate_ledger(|entries| {
        let e = entries
            .get_mut(&tk)
            .ok_or_else(|| "entry vanished".to_string())?;
        e.finalization = Some(FinalizationRecord {
            state: "pending".into(),
            attempt_id: attempt_id.clone(),
            report_hash: report_hash.clone(),
            outcome: "pending".into(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        });
        e.updated_at = chrono::Utc::now().to_rfc3339();
        Ok(())
    }) {
        return FinalizeResult::Error {
            task_key: tk,
            error: format!("ledger pending: {e}"),
        };
    }

    // 3. Verify (native, deterministic).
    let package_repo_sha = package_repo_and_sha(&entry.package_id);
    let snapshot: Vec<String> = match &package_repo_sha {
        Some((repo, sha)) => snapshot_of(&repo, &sha),
        None => vec![],
    };
    let verified = match verify(&receipt, &snapshot) {
        Ok(v) => v,
        Err(e) => {
            return FinalizeResult::Error {
                task_key: tk,
                error: format!("verify: {e}"),
            }
        }
    };
    let outcome = card_outcome(&verified.outcome, &receipt).to_string();
    let reason_line = verified
        .reasons
        .iter()
        .take(2)
        .cloned()
        .collect::<Vec<_>>()
        .join("; ");

    // 4. CAS write-back into the authoritative card.
    let project_dir = project_dir_of(&tenant_id);
    let mut writeback = "no_project_dir".to_string();
    if let Some(dir) = &project_dir {
        writeback = match cas_write_back(dir, &todo_id, consent_revision, &attempt_id, &report_hash, &outcome, &receipt, &reason_line, &package_repo_sha) {
            Ok(w) => w,
            Err(e) => {
                return FinalizeResult::Error {
                    task_key: tk,
                    error: format!("write-back: {e}"),
                }
            }
        };
    }

    // 5. Finalize the ledger + package/step states + archive the receipt.
    let final_outcome = if writeback == "stale" {
        "stale".to_string()
    } else {
        outcome.clone()
    };
    if let Err(e) = mutate_ledger(|entries| {
        let e = entries
            .get_mut(&tk)
            .ok_or_else(|| "entry vanished".to_string())?;
        e.state = LedgerState::Closed;
        e.finalization = Some(FinalizationRecord {
            state: "applied".into(),
            attempt_id: attempt_id.clone(),
            report_hash: report_hash.clone(),
            outcome: final_outcome.clone(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        });
        e.updated_at = chrono::Utc::now().to_rfc3339();
        Ok(())
    }) {
        return FinalizeResult::Error {
            task_key: tk,
            error: format!("ledger applied: {e}"),
        };
    }
    // Package status + step stage: accepted → passed + verified (never
    // observed — the human observation gate stays human).
    let _ = crate::execution_store::mark_package_status(
        &entry.package_id,
        match verified_outcome_pkg(&outcome, &writeback) {
            "passed" => "passed",
            "failed" => "failed",
            _ => "blocked",
        },
    );
    if outcome == "accepted_report" && writeback != "stale" {
        if let Some(dir) = &project_dir {
            crate::runner::Runner::advance_step_stage(
                &dir.to_string_lossy(),
                &entry.step_id,
                crate::models::StepStage::Verified,
            );
        }
    }
    let _ = archive_receipt(receipt_path, "applied");

    if writeback == "stale" {
        return FinalizeResult::StaleArchived {
            task_key: tk,
            reason: "card state no longer matches the authorization (edited/completed/opt-in off/manual takeover)".into(),
        };
    }
    if writeback == "no_project_dir" {
        // Ledger closed and report archived, but the card could not be
        // touched (tenant vanished from the registry) — surfaced, not silent.
        return FinalizeResult::StaleArchived {
            task_key: tk,
            reason: "tenant no longer in registry — card untouched".into(),
        };
    }
    FinalizeResult::Applied {
        task_key: tk,
        outcome: final_outcome,
    }
}

fn verified_outcome_pkg<'a>(outcome: &'a str, writeback: &'a str) -> &'a str {
    if writeback == "stale" {
        return "blocked";
    }
    match outcome {
        "accepted_report" => "passed",
        "execution_failed" | "timed_out" => "failed",
        _ => "blocked",
    }
}

/// CAS write-back into .todos.json (G1 mutate_todos: lock + raw-JSON patch
/// + item-level re-check inside the closure).
///
/// Returns "applied" | "already" (replay) | "stale" (card state moved on).
#[allow(clippy::too_many_arguments)]
fn cas_write_back(
    project_dir: &Path,
    todo_id: &str,
    consent_revision: u64,
    attempt_id: &str,
    report_hash: &str,
    outcome: &str,
    receipt: &Receipt,
    reason_line: &str,
    _pkg: &Option<(String, String)>,
) -> Result<String, String> {
    crate::store::mutate_todos(project_dir, |root| {
        let items = root
            .get_mut("items")
            .and_then(|v| v.as_array_mut())
            .ok_or("items is not an array")?;
        let item = items
            .iter_mut()
            .find(|it| it.get("id").and_then(|v| v.as_str()) == Some(todo_id))
            .ok_or_else(|| format!("item '{todo_id}' not found"))?;

        // Replay check FIRST (§9.3 step 5): the same attempt already applied.
        let already = item
            .get("auto_result")
            .and_then(|r| r.get("attempt_id"))
            .and_then(|v| v.as_str())
            == Some(attempt_id);
        if already {
            return Ok("already".to_string());
        }

        // CAS re-checks: the card must still match the authorization this
        // receipt was produced under (plan §9.3 step 4).
        let stale_reasons = [
            (item.get("type").and_then(|v| v.as_str()) != Some("todo"), "type changed"),
            (
                item.get("status").and_then(|v| v.as_str()) != Some("active"),
                "status not active",
            ),
            (
                item.get("auto_process").and_then(|v| v.as_bool()) != Some(true),
                "opt-in turned off",
            ),
            (
                item.get("auto_process_revision").and_then(|v| v.as_u64())
                    != Some(consent_revision),
                "consent revision moved on",
            ),
            (
                item.get("processing").and_then(|v| v.as_bool()) == Some(true),
                "manual takeover (processing)",
            ),
        ];
        if stale_reasons.iter().any(|(hit, _)| *hit) {
            return Ok("stale".to_string());
        }

        // Apply: accepted → completed; everything else keeps the card active
        // with the structured result attached.
        apply_auto_result(
            item,
            outcome,
            attempt_id,
            report_hash,
            &receipt.received_at,
            reason_line,
        );
        Ok("applied".to_string())
    })
}

/// Field-level application onto the raw item object (G1 raw-patch style).
fn apply_auto_result(
    item: &mut serde_json::Value,
    outcome: &str,
    attempt_id: &str,
    report_hash: &str,
    finished_at: &str,
    reason_line: &str,
) {
    let now = chrono::Utc::now().to_rfc3339();
    let item_id = item
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let obj = item.as_object_mut().unwrap();
    if outcome == "accepted_report" {
        obj.insert("status".into(), serde_json::json!("completed"));
        obj.insert("auto_completed_at".into(), serde_json::json!(now));
    }
    obj.insert(
        "auto_result".into(),
        serde_json::json!({
            "outcome": outcome,
            "attempt_id": attempt_id,
            "report_ref": format!("{}/report.md", tenant_report_relpath(&item_id, attempt_id)),
            "report_hash": report_hash,
            "finished_at": finished_at,
            "reason_code": reason_line,
            "verification_level": "native_verifier",
        }),
    );
    obj.insert("processing".into(), serde_json::json!(false));
    obj.insert("updated_at".into(), serde_json::json!(now));
}

/// Card write-back for the recovery path (lost receipt): visible
/// needs_review result, idempotent by attempt_id, stale cards untouched.
pub(crate) fn write_auto_result(
    project_dir: &Path,
    todo_id: &str,
    consent_revision: u64,
    attempt_id: &str,
    report_hash: &str,
    outcome: &str,
    reason_line: &str,
    _finish: bool,
) -> Result<String, String> {
    crate::store::mutate_todos(project_dir, |root| {
        let items = root
            .get_mut("items")
            .and_then(|v| v.as_array_mut())
            .ok_or("items is not an array")?;
        let item = items
            .iter_mut()
            .find(|it| it.get("id").and_then(|v| v.as_str()) == Some(todo_id))
            .ok_or_else(|| format!("item '{todo_id}' not found"))?;
        if item
            .get("auto_result")
            .and_then(|r| r.get("attempt_id"))
            .and_then(|v| v.as_str())
            == Some(attempt_id)
        {
            return Ok("already".to_string());
        }
        let fresh = item.get("status").and_then(|v| v.as_str()) == Some("active")
            && item.get("type").and_then(|v| v.as_str()) == Some("todo")
            && item.get("auto_process").and_then(|v| v.as_bool()) == Some(true)
            && item.get("auto_process_revision").and_then(|v| v.as_u64())
                == Some(consent_revision);
        if !fresh {
            return Ok("stale".to_string());
        }
        apply_auto_result(item, outcome, attempt_id, report_hash, "", reason_line);
        Ok("applied".to_string())
    })
}

fn tenant_report_relpath(todo_id: &str, attempt_id: &str) -> String {
    format!("{todo_id}/{attempt_id}")
}

/// Persist report + manifest under todo-reports/<tenant>/<todo>/<attempt>/.
/// Idempotent: same content → same files, overwritten atomically.
fn persist_report(entry: &LedgerEntry, receipt: &Receipt) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let dir = reports_dir()
        .join(sanitize(&entry.tenant_id))
        .join(sanitize(&entry.todo_id))
        .join(sanitize(&receipt.attempt_id));
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir report dir: {e}"))?;
    let hash = hex(&Sha256::digest(receipt.report_stdout.as_bytes()));
    let tmp = dir.join("report.md.tmp");
    std::fs::write(&tmp, &receipt.report_stdout).map_err(|e| format!("write report: {e}"))?;
    std::fs::rename(&tmp, dir.join("report.md")).map_err(|e| format!("rename report: {e}"))?;
    let manifest = serde_json::json!({
        "task_key": entry.task_key,
        "attempt_id": receipt.attempt_id,
        "report_sha256": hash,
        "received_at": receipt.received_at,
        "persisted_at": chrono::Utc::now().to_rfc3339(),
    });
    let tmp = dir.join("manifest.json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?)
        .map_err(|e| format!("write manifest: {e}"))?;
    std::fs::rename(&tmp, dir.join("manifest.json")).map_err(|e| format!("rename manifest: {e}"))?;
    Ok(hash)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sanitize(seg: &str) -> String {
    seg.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

fn archive_receipt(path: &Path, sub: &str) -> Result<(), String> {
    let dest_dir = receipts_dir().join(sub);
    std::fs::create_dir_all(&dest_dir).map_err(|e| e.to_string())?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "receipt.json".into());
    std::fs::rename(path, dest_dir.join(name)).map_err(|e| e.to_string())
}

fn package_repo_and_sha(package_id: &str) -> Option<(String, String)> {
    let path = crate::execution_store::runtime_dir().join("packages.json");
    let raw = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let mut best: Option<(u64, String, String)> = None;
    for p in v.get("packages").and_then(|a| a.as_array()).into_iter().flatten() {
        if p.get("package_id").and_then(|s| s.as_str()) == Some(package_id) {
            let rev = p.get("plan_revision").and_then(|r| r.as_u64()).unwrap_or(0);
            if best.as_ref().map(|(r, _, _)| rev > *r).unwrap_or(true) {
                best = Some((
                    rev,
                    p.get("repo_root").and_then(|s| s.as_str())?.to_string(),
                    p.get("base_sha").and_then(|s| s.as_str())?.to_string(),
                ));
            }
        }
    }
    best.map(|(_, repo, sha)| (repo, sha))
}

/// One finalization pass over the receipts inbox + recovery of pending
/// finalizations whose receipt file was lost (re-finalizable from the ledger
/// only if the report exists on disk).
pub fn finalization_pass(
    node_cfg: Option<(&str, &str)>,
    project_dir_of: &dyn Fn(&str) -> Option<PathBuf>,
) -> Vec<FinalizeResult> {
    let mut results = Vec::new();
    // Recovery (audit B-2): a pending finalization whose receipt vanished
    // cannot be re-verified — the verdict is unknowable. The card gets a
    // VISIBLE needs_review result pointing at the archived report (never a
    // silent ledger-only close), the entry closes as needs_review.
    if let Ok(ledger) = read_ledger() {
        let tenants = crate::discovery::discover_projects();
        for (_, entry) in ledger.entries.iter() {
            let Some(f) = &entry.finalization else { continue };
            if f.state != "pending" || receipt_in_inbox(&entry.task_key) {
                continue;
            }
            let project_dir = tenants
                .iter()
                .find(|(name, _)| *name == entry.tenant_id)
                .map(|(_, p)| p.clone());
            // Card write-back: idempotent by attempt_id; stale cards stay
            // untouched (the report stays archived either way).
            if let Some(dir) = &project_dir {
                let _ = write_auto_result(
                    dir,
                    &entry.todo_id,
                    entry.consent_revision,
                    &f.attempt_id,
                    &f.report_hash,
                    "needs_review",
                    "receipt lost — report archived, verdict unknown, human review required",
                    false,
                );
            }
            let _ = mutate_ledger(|entries| {
                if let Some(e) = entries.get_mut(&entry.task_key) {
                    if e.finalization.as_ref().map(|x| x.state == "pending").unwrap_or(false) {
                        e.state = LedgerState::Closed;
                        e.finalization = Some(FinalizationRecord {
                            state: "applied".into(),
                            attempt_id: f.attempt_id.clone(),
                            report_hash: f.report_hash.clone(),
                            outcome: "needs_review".into(),
                            updated_at: chrono::Utc::now().to_rfc3339(),
                        });
                        e.updated_at = chrono::Utc::now().to_rfc3339();
                    }
                }
                Ok(())
            });
        }
    }
    // Inbox: any receipt file not yet archived.
    let dir = receipts_dir();
    let _ = std::fs::create_dir_all(&dir);
    let files = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false))
        .collect::<Vec<PathBuf>>();
    for file in files {
        let node_bin = node_cfg.map(|(n, _)| n).unwrap_or("node");
        let verify_cli = node_cfg.map(|(_, v)| v).unwrap_or("");
        let verify = |r: &Receipt, snap: &[String]| -> Result<VerifierOutput, String> {
            verify_via_cli(node_bin, verify_cli, r, snap)
        };
        let snapshot = |repo: &str, sha: &str| snapshot_files_at(repo, sha);
        results.push(finalize_receipt(
            &file,
            &verify,
            project_dir_of,
            node_cfg,
            &snapshot,
        ));
    }
    results
}

/// Strict presence check (audit B-2): a receipt is "in the inbox" when a
/// TOP-LEVEL file's stem starts with the task_key — the naming segment the
/// manager controls. Archived receipts live in subdirectories and do NOT
/// count (they are already processed).
fn receipt_in_inbox(task_key: &str) -> bool {
    std::fs::read_dir(receipts_dir())
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .any(|e| {
            e.path()
                .file_stem()
                .map(|s| s.to_string_lossy().starts_with(task_key))
                .unwrap_or(false)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::todo_ledger::{LedgerEntry, LedgerState};

    fn setup_runtime() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        let guard = crate::execution_store::TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("QUESTLOG_RUNTIME", dir.path().join("runtime"));
        (dir, guard)
    }

    struct Fixture {
        project_dir: PathBuf,
        task_key: String,
        #[allow(dead_code)]
        todo_id: String,
    }

    /// A git repo project with one opted-in todo + a published ledger entry +
    /// a sealed package row pointing at the repo HEAD.
    fn fixture(consent_revision: u64) -> Fixture {
        let root = std::env::temp_dir().join(format!("questlog-fin-{}", uuid::Uuid::new_v4()));
        let project = root.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        for cmd in [
            vec!["init"],
            vec!["config", "user.email", "t@t.com"],
            vec!["config", "user.name", "T"],
        ] {
            let _ = std::process::Command::new("git")
                .args(&cmd)
                .current_dir(&project)
                .output()
                .unwrap();
        }
        std::fs::write(project.join("src.rs"), "fn main() {}\n").unwrap();
        let _ = std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&project)
            .output()
            .unwrap();
        let _ = std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&project)
            .output()
            .unwrap();
        let sha = String::from_utf8(
            std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&project)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();

        std::fs::write(
            project.join(".todos.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "version": 1, "project": "proj", "revision": 1,
                "items": [{
                    "id": "td1", "type": "todo", "title": "研究任务",
                    "content": "目标与验收标准。", "suggestion": "报告",
                    "tags": [], "status": "active", "priority": "high",
                    "source_session": "", "created_at": "2026", "updated_at": "2026",
                    "auto_process": true, "auto_process_revision": consent_revision
                }]
            }))
            .unwrap(),
        )
        .unwrap();

        let task_key = format!("k_{}", uuid::Uuid::new_v4().simple());
        // packages.json with the sealed package.
        let runtime = crate::execution_store::runtime_dir();
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::write(
            runtime.join("packages.json"),
            serde_json::to_string(&serde_json::json!({
                "version": 1,
                "packages": [{
                    "package_id": "pkg_x", "plan_revision": 1, "status": "sealed",
                    "package_hash": "h", "objective": "o",
                    "repo_root": project.to_string_lossy(), "base_sha": sha,
                    "origin": {"type": "todo", "tenant_id": "proj", "todo_id": "td1",
                               "task_key": task_key, "task_revision": "r",
                               "consent_revision": consent_revision}
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        // .epics.json so advance_step_stage can find the step.
        std::fs::write(
            project.join(".epics.json"),
            serde_json::to_string(&serde_json::json!({
                "version": 1, "project": "proj", "revision": 1,
                "epics": [{"id": "ep1", "title": "E", "description": "", "status": "active",
                           "priority": "P2", "depends_on": [], "blocks": [], "links": {},
                           "created_at": "2026", "updated_at": "2026"}],
                "steps": [{"id": "st1", "epic_id": "ep1", "title": "S", "description": "",
                           "order": 1, "depends_on": [], "stage": "pending", "links": {},
                           "created_at": "2026", "updated_at": "2026"}]
            }))
            .unwrap(),
        )
        .unwrap();
        // Ledger entry published.
        crate::todo_ledger::mutate_ledger(|entries| {
            entries.insert(
                task_key.clone(),
                LedgerEntry {
                    task_key: task_key.clone(),
                    tenant_id: "proj".into(),
                    todo_id: "td1".into(),
                    task_revision: "r".into(),
                    consent_revision,
                    operation_id: "op".into(),
                    state: LedgerState::Published,
                    epic_id: "ep1".into(),
                    step_id: "st1".into(),
                    package_id: "pkg_x".into(),
                    created_at: "2026".into(),
                    updated_at: "2026".into(),
                    finalization: None,
                },
            );
            Ok(())
        })
        .unwrap();

        Fixture { project_dir: project, task_key, todo_id: "td1".into() }
    }

    fn report_stdout(task_key: &str, verdict: &str) -> String {
        let report = serde_json::json!({
            "schema_version": "questlog-todo-report",
            "task_key": task_key,
            "verdict": verdict,
            "summary": "分析完成。",
            "findings": ["发现一"],
            "evidence_refs": ["src.rs:1"],
            "unresolved": [],
            "limitations": []
        });
        let mut report = report;
        if verdict == "CANNOT_AUTO" {
            report["cannot_auto_reason"] = serde_json::json!("需改代码");
        }
        format!("进度输出…\n```json\n{}\n```\n", serde_json::to_string_pretty(&report).unwrap())
    }

    fn receipt_file(task_key: &str, flags: (&str, &str)) -> PathBuf {
        let (verdict, flag) = flags;
        let receipt = serde_json::json!({
            "version": 1,
            "task_key": task_key,
            "attempt_id": "att1",
            "exit_ok": flag != "timeout",
            "timed_out": flag == "timeout",
            "budget_exceeded": false,
            "changes_detected": false,
            "report_stdout": report_stdout(task_key, verdict),
            "received_at": "2026-09-18T03:30:00+08:00"
        });
        let path = receipts_dir().join(format!("{task_key}.json"));
        std::fs::create_dir_all(receipts_dir()).unwrap();
        std::fs::write(&path, receipt.to_string()).unwrap();
        path
    }

    /// Real verifier path is stubbed (Node tests cover it); the stub mirrors
    /// todo-report.ts semantics for DONE/CANNOT_AUTO with our fixtures.
    fn stub_verify(r: &Receipt, snap: &[String]) -> Result<VerifierOutput, String> {
        if r.timed_out {
            return Ok(VerifierOutput { outcome: VerifierOutcome::Rejected, reasons: vec!["execution timed out".into()] });
        }
        let accepted = r.report_stdout.contains("\"verdict\": \"DONE\"")
            || r.report_stdout.contains("\"verdict\":\"DONE\"");
        let cannot = r.report_stdout.contains("CANNOT_AUTO");
        let refs_ok = r.report_stdout.contains("src.rs") && snap.iter().any(|f| f.contains("src.rs"));
        if accepted && refs_ok {
            Ok(VerifierOutput { outcome: VerifierOutcome::AcceptedReport, reasons: vec![] })
        } else if cannot {
            Ok(VerifierOutput { outcome: VerifierOutcome::CannotAuto, reasons: vec![] })
        } else {
            Ok(VerifierOutput { outcome: VerifierOutcome::NeedsReview, reasons: vec!["stub".into()] })
        }
    }

    fn run(fx: &Fixture, path: &Path) -> FinalizeResult {
        let project_dir = fx.project_dir.clone();
        finalize_receipt(
            path,
            &stub_verify,
            &|_tenant| Some(project_dir.clone()),
            None,
            &|repo, sha| snapshot_files_at(repo, sha),
        )
    }

    fn read_item(fx: &Fixture) -> serde_json::Value {
        let raw = std::fs::read_to_string(fx.project_dir.join(".todos.json")).unwrap();
        serde_json::from_str::<serde_json::Value>(&raw).unwrap()["items"][0].clone()
    }

    fn ledger_entry(task_key: &str) -> LedgerEntry {
        read_ledger().unwrap().entries.get(task_key).cloned().unwrap()
    }

    #[test]
    fn accepted_report_completes_card_and_closes_task() {
        let (_dir, _g) = setup_runtime();
        let fx = fixture(1);
        let path = receipt_file(&fx.task_key, ("DONE", ""));
        let r = run(&fx, &path);
        assert!(matches!(r, FinalizeResult::Applied { .. }), "{r:?}");
        let item = read_item(&fx);
        assert_eq!(item["status"], "completed");
        assert_eq!(item["auto_result"]["outcome"], "accepted_report");
        assert_eq!(item["auto_result"]["attempt_id"], "att1");
        assert!(item["auto_completed_at"].is_string());
        let e = ledger_entry(&fx.task_key);
        assert_eq!(e.state, LedgerState::Closed);
        assert_eq!(e.finalization.unwrap().outcome, "accepted_report");
        // Receipt archived out of the inbox.
        assert!(!path.exists());
        // Report persisted with hash.
        let manifest = reports_dir().join("proj").join("td1").join("att1").join("manifest.json");
        assert!(manifest.exists());
    }

    #[test]
    fn replay_after_full_apply_is_a_noop() {
        let (_dir, _g) = setup_runtime();
        let fx = fixture(1);
        let path = receipt_file(&fx.task_key, ("DONE", ""));
        run(&fx, &path).unwrap_applied();
        // Re-drop the SAME receipt (idempotent replay, §9.3 step 5).
        let path2 = receipt_file(&fx.task_key, ("DONE", ""));
        let r = run(&fx, &path2);
        assert!(matches!(r, FinalizeResult::Applied { .. }), "{r:?}");
        let item = read_item(&fx);
        assert_eq!(item["status"], "completed");
        assert_eq!(item["auto_result"]["attempt_id"], "att1", "not overwritten");
    }

    #[test]
    fn crash_between_writeback_and_ledger_recovers_without_second_apply() {
        let (_dir, _g) = setup_runtime();
        let fx = fixture(1);
        let path = receipt_file(&fx.task_key, ("DONE", ""));
        run(&fx, &path).unwrap_applied();
        // Simulate the crash window: ledger finalization rewound to pending,
        // receipt restored (as if step 5 never happened).
        crate::todo_ledger::mutate_ledger(|entries| {
            let e = entries.get_mut(&fx.task_key).unwrap();
            e.state = LedgerState::Published;
            let mut f = e.finalization.clone().unwrap();
            f.state = "pending".into();
            e.finalization = Some(f);
            Ok(())
        })
        .unwrap();
        let path2 = receipt_file(&fx.task_key, ("DONE", ""));
        let r = run(&fx, &path2);
        assert!(matches!(r, FinalizeResult::Applied { .. }), "{r:?}");
        // The card was applied exactly once — same attempt, not re-completed.
        let item = read_item(&fx);
        assert_eq!(item["status"], "completed");
        assert_eq!(item["auto_result"]["attempt_id"], "att1");
    }

    #[test]
    fn consent_mismatch_is_stale_archived_card_untouched() {
        let (_dir, _g) = setup_runtime();
        // Receipt produced under consent 1; card has moved to consent 2.
        let fx = fixture(2);
        // Ledger entry claims consent 1 (what the receipt was made under).
        crate::todo_ledger::mutate_ledger(|entries| {
            entries.get_mut(&fx.task_key).unwrap().consent_revision = 1;
            Ok(())
        })
        .unwrap();
        let path = receipt_file(&fx.task_key, ("DONE", ""));
        let r = run(&fx, &path);
        assert!(matches!(r, FinalizeResult::StaleArchived { .. }), "{r:?}");
        let item = read_item(&fx);
        assert_eq!(item["status"], "active", "card untouched");
        assert!(item.get("auto_result").is_none());
    }

    #[test]
    fn cannot_auto_keeps_card_active_with_reason() {
        let (_dir, _g) = setup_runtime();
        let fx = fixture(1);
        let path = receipt_file(&fx.task_key, ("CANNOT_AUTO", ""));
        let r = run(&fx, &path);
        assert!(matches!(r, FinalizeResult::Applied { .. }), "{r:?}");
        let item = read_item(&fx);
        assert_eq!(item["status"], "active");
        assert_eq!(item["auto_result"]["outcome"], "cannot_auto");
        assert!(item.get("auto_completed_at").is_none());
    }

    #[test]
    fn timeout_maps_to_timed_out_not_completed() {
        let (_dir, _g) = setup_runtime();
        let fx = fixture(1);
        let path = receipt_file(&fx.task_key, ("DONE", "timeout"));
        let r = run(&fx, &path);
        assert!(matches!(r, FinalizeResult::Applied { .. }), "{r:?}");
        let item = read_item(&fx);
        assert_eq!(item["status"], "active");
        assert_eq!(item["auto_result"]["outcome"], "timed_out");
    }

    #[test]
    fn unknown_task_key_is_unmatched() {
        let (_dir, _g) = setup_runtime();
        let fx = fixture(1);
        let path = receipt_file("k_nonexistent", ("DONE", ""));
        let r = run(&fx, &path);
        assert!(matches!(r, FinalizeResult::Unmatched { .. }), "{r:?}");
        assert!(!path.exists(), "archived out of the inbox");
    }

    impl FinalizeResult {
        fn unwrap_applied(&self) {
            assert!(matches!(self, FinalizeResult::Applied { .. }), "{self:?}");
        }
    }
}
