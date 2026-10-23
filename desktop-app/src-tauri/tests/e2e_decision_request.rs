//! E2E: acceptance failure → DecisionRequest creation.
//! Runs once, verifies the decision-requests.json is created, then cleans up.

use std::process::Command;

fn zcode_available() -> bool {
    std::path::Path::new("/Applications/ZCode.app/Contents/Resources/glm/zcode.cjs").exists()
}

fn require_live_zcode_environment() {
    assert_eq!(
        std::env::var("QUESTLOG_LIVE_ZCODE_E2E").as_deref(),
        Ok("1"),
        "live ZCode E2E requires explicit opt-in: QUESTLOG_LIVE_ZCODE_E2E=1"
    );
    assert!(
        zcode_available(),
        "live ZCode E2E was explicitly requested, but ZCode CLI is missing at /Applications/ZCode.app/Contents/Resources/glm/zcode.cjs"
    );
    let home = std::env::var_os("HOME").expect("live ZCode E2E requires HOME to be set");
    let config = std::path::PathBuf::from(home).join(".zcode/cli/config.json");
    assert!(
        config.is_file(),
        "live ZCode E2E was explicitly requested, but CLI config is missing: {}",
        config.display()
    );
}

fn init_git_repo(dir: &std::path::Path) -> String {
    Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "t@t.com"])
        .current_dir(dir)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "T"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::fs::write(dir.join("src.ts"), "export const value = 1;\n").unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(dir)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(dir)
        .output()
        .unwrap();
    String::from_utf8_lossy(
        &Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir)
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string()
}

#[test]
#[ignore = "requires real ZCode, credentials, and explicit operator opt-in"]
fn acceptance_failure_creates_decision_request() {
    require_live_zcode_environment();

    let runtime_dir = tempfile::tempdir().unwrap();
    std::env::set_var("QUESTLOG_RUNTIME", runtime_dir.path().join("runtime"));
    std::fs::create_dir_all(runtime_dir.path().join("runtime")).unwrap();

    let repo_dir = tempfile::tempdir().unwrap();
    let base_sha = init_git_repo(repo_dir.path());

    // Package with an acceptance command that DOESN'T EXIST in the registry.
    // Runner will produce a candidate (ZCode changes src.ts), but acceptance
    // resolution will fail → AcceptanceFailed → DecisionRequest created.
    let mut package = serde_json::json!({
        "schema_version": 1,
        "package_id": "pkg_decision_test",
        "epic_id": "epic_test",
        "step_id": "step_test",
        "plan_revision": 1,
        "project_path": repo_dir.path().to_string_lossy(),
        "repo_root": repo_dir.path().to_string_lossy(),
        "base_sha": base_sha,
        "objective": "Change the value in src.ts from 1 to 42. Only modify src.ts.",
        "context_refs": [],
        "scope": { "allowed_paths": ["src.ts"], "forbidden_paths": [], "max_changed_files": 5 },
        "preconditions": [],
        "executor": { "preferred": "zcode", "fallbacks": [], "sandbox": "workspace-write", "timeout_seconds": 120, "max_attempts": 2 },
        "acceptance": [{ "type": "command", "command_id": "test:nonexistent_command" }],
        "authority": { "auto_edit_worktree": true, "auto_commit_candidate": true, "auto_merge": false, "auto_push": false, "auto_deploy": false },
        "stop_conditions": ["scope_expanded"],
        "rollback": { "strategy": "preserve_worktree_and_stop" },
        "status": "ready",
        "sealed_at": "2026-08-11T00:00:00Z",
        "package_hash": ""
    });
    // Compute real hash — Runner's preflight checks stored == computed
    let package_hash = app_lib::execution_store::compute_package_hash(&package).unwrap();
    package["package_hash"] = serde_json::Value::String(package_hash.clone());
    std::fs::write(
        runtime_dir.path().join("runtime").join("packages.json"),
        serde_json::to_string_pretty(&serde_json::json!({ "version": 1, "packages": [package] }))
            .unwrap(),
    )
    .unwrap();

    // Also need .epics.json for the project (Runner reads it for step stage + package summary)
    let epics = serde_json::json!({
        "version": 1, "project": "e2e", "revision": 1,
        "epics": [{"id":"epic_test","title":"E2E","description":"","status":"active","priority":"P0","depends_on":[],"blocks":[],"links":{},"acceptance":"test","close_reason":"","auto_execute":true,"created_at":"2026-08-11T00:00:00Z","updated_at":"2026-08-11T00:00:00Z"}],
        "steps": [{"id":"step_test","epic_id":"epic_test","title":"Test","description":"","order":1,"depends_on":[],"stage":"pending","links":{},"package":{
            "package_id": "pkg_decision_test",
            "plan_revision": 1,
            "package_hash": package_hash,
            "objective": "Change the value in src.ts from 1 to 42. Only modify src.ts.",
            "executor": "zcode",
            "status": "ready"
        },"created_at":"2026-08-11T00:00:00Z","updated_at":"2026-08-11T00:00:00Z"}]
    });
    std::fs::write(
        repo_dir.path().join(".epics.json"),
        serde_json::to_string_pretty(&epics).unwrap(),
    )
    .unwrap();

    eprintln!("Running Runner poll_once with bad acceptance...");
    // Enable the project in runner-config so poll_once's project filter passes.
    // NOTE: set_project_enabled(true) starts the background Runner thread which
    // would race with the explicit poll_once() below (duplicate executions).
    // Stop it immediately — the test's explicit poll_once() must be the only poller.
    app_lib::runner_config::set_project_enabled(repo_dir.path().to_string_lossy().as_ref(), true)
        .expect("enable project for e2e");
    app_lib::runner::stop_runner();
    let mut runner = app_lib::runner::Runner::new(app_lib::runner::RunnerConfig {
        enabled: true,
        poll_interval: std::time::Duration::from_secs(1),
        runner_id: "decision-test".to_string(),
    });
    runner.poll_once().unwrap();

    // Verify execution was created
    let execs = app_lib::execution_store::list_executions().unwrap();
    assert!(!execs.is_empty(), "should have an execution");
    let exec = &execs[0];
    eprintln!("Execution status: {:?}", exec.status);
    eprintln!("Execution last_error: {:?}", exec.last_error);

    // Verify DecisionRequest was created
    let dr_path = runtime_dir
        .path()
        .join("runtime")
        .join("decision-requests.json");
    assert!(
        dr_path.exists(),
        "decision-requests.json must be created on acceptance failure"
    );
    let dr_raw = std::fs::read_to_string(&dr_path).unwrap();
    let dr: serde_json::Value = serde_json::from_str(&dr_raw).unwrap();
    let requests = dr["requests"].as_array().unwrap();
    assert!(
        !requests.is_empty(),
        "should have at least one decision request"
    );
    let req = &requests[0];
    eprintln!(
        "DecisionRequest kind: {}",
        req["kind"].as_str().unwrap_or("?")
    );
    eprintln!(
        "DecisionRequest question: {}",
        req["question"].as_str().unwrap_or("?")
    );
    assert_eq!(req["kind"].as_str(), Some("verifier_failed"));
    assert_eq!(req["step_id"].as_str(), Some("step_test"));

    // Verify AttemptBundle was also sealed
    let (manifest, bundle_path) = app_lib::attempt_bundle::read_bundle(&exec.id).unwrap();
    assert_eq!(manifest.outcome, "acceptance_failed");
    eprintln!("✓ AttemptBundle outcome: {}", manifest.outcome);

    // Evidence integrity: the DecisionRequest mirror must be part of the
    // SEALED bundle — decisions_ref persisted, and the integrity hash valid
    // AFTER the decision write (regression: decisions.json used to be
    // written after sealing, silently invalidating the hash).
    assert_eq!(
        manifest.decisions_ref.as_deref(),
        Some("decisions.json"),
        "decisions_ref must persist in the sealed manifest"
    );
    assert!(
        bundle_path.join("decisions.json").exists(),
        "decisions.json must exist inside the sealed bundle"
    );
    assert!(
        app_lib::attempt_bundle::verify_integrity(&bundle_path).unwrap(),
        "sealed bundle integrity must hold with decisions.json included"
    );
    eprintln!("✓ Bundle integrity verified (decisions.json covered by hash)");
    eprintln!(
        "✓ DecisionRequest kind: {}",
        req["kind"].as_str().unwrap_or("?")
    );
    eprintln!("✓ Full chain verified: acceptance fail → DecisionRequest + sealed bundle");

    // Cleanup worktrees
    let _ = Command::new("git")
        .args(["worktree", "prune"])
        .current_dir(repo_dir.path())
        .output();
}
