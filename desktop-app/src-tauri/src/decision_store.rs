//! Shared DecisionRequest store used by the Rust Runner, Tauri UI, and Node MCP.

use crate::{execution_store, file_lock};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const DECISIONS_FILENAME: &str = "decision-requests.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionRequest {
    pub request_id: String,
    pub epic_id: String,
    pub step_id: String,
    pub execution_id: String,
    pub kind: String,
    pub question: String,
    pub options: Vec<String>,
    pub evidence_refs: Vec<String>,
    pub blocking: bool,
    pub package_revision: u32,
    pub resolved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DecisionsFile {
    version: u32,
    requests: Vec<DecisionRequest>,
    #[serde(default)]
    revision: u64,
}

impl Default for DecisionsFile {
    fn default() -> Self {
        Self {
            version: 1,
            requests: vec![],
            revision: 0,
        }
    }
}

fn decisions_path() -> PathBuf {
    execution_store::runtime_dir().join(DECISIONS_FILENAME)
}

fn read_file() -> Result<DecisionsFile, String> {
    let path = decisions_path();
    if !path.exists() {
        return Ok(DecisionsFile::default());
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("read decisions: {e}"))?;
    serde_json::from_str(&raw).map_err(|e| format!("parse decisions: {e}"))
}

fn write_file(data: &mut DecisionsFile) -> Result<(), String> {
    std::fs::create_dir_all(execution_store::runtime_dir())
        .map_err(|e| format!("mkdir runtime: {e}"))?;
    data.revision = data.revision.saturating_add(1);
    let path = decisions_path();
    let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let json =
        serde_json::to_string_pretty(data).map_err(|e| format!("serialize decisions: {e}"))?;
    std::fs::write(&tmp, json).map_err(|e| format!("write decisions tmp: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename decisions: {e}"))?;
    Ok(())
}

pub struct NewDecisionRequest<'a> {
    pub epic_id: &'a str,
    pub step_id: &'a str,
    pub execution_id: &'a str,
    pub kind: &'a str,
    pub question: &'a str,
    pub options: Vec<String>,
    pub evidence_refs: Vec<String>,
    pub package_revision: u32,
}

/// Create one unresolved request per execution/kind/revision. Repeated Runner
/// error handling therefore cannot flood the human inbox.
pub fn create_request(input: NewDecisionRequest<'_>) -> Result<DecisionRequest, String> {
    let path = decisions_path();
    file_lock::with_file_lock(&path, || {
        let mut data = read_file()?;
        if let Some(existing) = data.requests.iter().find(|request| {
            !request.resolved
                && request.execution_id == input.execution_id
                && request.kind == input.kind
                && request.package_revision == input.package_revision
        }) {
            return Ok(existing.clone());
        }

        let request = DecisionRequest {
            request_id: format!("dec_{}", uuid::Uuid::new_v4()),
            epic_id: input.epic_id.to_string(),
            step_id: input.step_id.to_string(),
            execution_id: input.execution_id.to_string(),
            kind: input.kind.to_string(),
            question: input.question.to_string(),
            options: input.options,
            evidence_refs: input.evidence_refs,
            blocking: true,
            package_revision: input.package_revision,
            resolved: false,
            resolved_by: None,
            resolved_at: None,
            resolution: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        data.requests.push(request.clone());
        write_file(&mut data)?;
        Ok(request)
    })
}

pub fn list_requests(include_resolved: bool) -> Result<Vec<DecisionRequest>, String> {
    let mut requests = read_file()?.requests;
    if !include_resolved {
        requests.retain(|request| !request.resolved);
    }
    requests.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(requests)
}

pub fn resolve_request(
    request_id: &str,
    resolution: &str,
    resolved_by: &str,
) -> Result<DecisionRequest, String> {
    if resolution.trim().is_empty() {
        return Err("resolution must not be empty".to_string());
    }
    let path = decisions_path();
    file_lock::with_file_lock(&path, || {
        let mut data = read_file()?;
        let request = data
            .requests
            .iter_mut()
            .find(|request| request.request_id == request_id)
            .ok_or_else(|| format!("decision request '{}' not found", request_id))?;
        if request.resolved {
            return Err(format!(
                "decision request '{}' was already resolved",
                request_id
            ));
        }
        request.resolved = true;
        request.resolution = Some(resolution.trim().to_string());
        request.resolved_by = Some(resolved_by.to_string());
        request.resolved_at = Some(chrono::Utc::now().to_rfc3339());
        let updated = request.clone();
        write_file(&mut data)?;
        Ok(updated)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution_store::TEST_LOCK;

    #[test]
    fn create_deduplicates_and_resolves() {
        let _guard = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("QUESTLOG_RUNTIME", dir.path());
        let input = || NewDecisionRequest {
            epic_id: "e1",
            step_id: "s1",
            execution_id: "x1",
            kind: "verifier_failed",
            question: "Retry?",
            options: vec!["retry".to_string(), "cancel".to_string()],
            evidence_refs: vec!["attempts/x1/verifier-results.json".to_string()],
            package_revision: 2,
        };
        let first = create_request(input()).unwrap();
        let second = create_request(input()).unwrap();
        assert_eq!(first.request_id, second.request_id);
        assert_eq!(list_requests(false).unwrap().len(), 1);
        resolve_request(&first.request_id, "retry", "ui-user").unwrap();
        assert!(list_requests(false).unwrap().is_empty());
        assert_eq!(list_requests(true).unwrap().len(), 1);
    }
}
