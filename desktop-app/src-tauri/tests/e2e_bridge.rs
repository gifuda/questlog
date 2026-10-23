//! End-to-end bridge test: packages.json → Runner poll → ZCode execution → candidate commit.
//!
//! This test creates a real git fixture, writes a packages.json with a ready
//! package, runs the Runner's poll_once(), and verifies the full chain:
//!   1. Runner reads packages.json
//!   2. Creates an execution
//!   3. Creates a worktree
//!   4. Runs ZCode CLI with the package's objective
//!   5. Verifies the diff is in scope
//!   6. Creates a candidate commit with QuestLog trailers
//!   7. Marks the package as consumed
//!
//! Requires: ZCode CLI configured at ~/.zcode/cli/config.json

use std::process::Command;

/// Both e2e tests mutate the process-global QUESTLOG_RUNTIME env var, so they
/// must not run in parallel within this test binary (cargo runs tests in one
/// process by default). Serialize them with a file-scope mutex.
static RUNTIME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git command failed");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn zcode_available() -> bool {
    let path = "/Applications/ZCode.app/Contents/Resources/glm/zcode.cjs";
    std::path::Path::new(path).exists()
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
        .args(["config", "user.email", "test@test.com"])
        .current_dir(dir)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::fs::write(dir.join("src.ts"), "export const value = 1;\n").unwrap();
    std::fs::write(dir.join(".gitignore"), ".epics.json\n.questlog-output/\n").unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(dir)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(dir)
        .output()
        .unwrap();

    let sha = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .unwrap();
    String::from_utf8_lossy(&sha.stdout).trim().to_string()
}

#[test]
#[ignore = "requires real ZCode, credentials, and explicit operator opt-in"]
fn bridge_ready_package_to_zcode_execution() {
    let _runtime_guard = RUNTIME_LOCK.lock().unwrap();
    require_live_zcode_environment();

    // Set up isolated runtime
    let runtime_dir = tempfile::tempdir().unwrap();
    std::env::set_var("QUESTLOG_RUNTIME", runtime_dir.path().join("runtime"));
    std::fs::create_dir_all(runtime_dir.path().join("runtime")).unwrap();

    // Create a real git fixture
    let repo_dir = tempfile::tempdir().unwrap();
    let base_sha = init_git_repo(repo_dir.path());

    // Build the exact nested TaskPackage shape written by Node MCP, then hash
    // the sealed content with the same canonical algorithm used by the Runner.
    let mut package = serde_json::json!({
            "schema_version": 1,
            "package_id": "pkg_e2e_test",
            "epic_id": "epic_test",
            "step_id": "step_test",
            "plan_revision": 1,
            "project_path": repo_dir.path().to_string_lossy(),
            "repo_root": repo_dir.path().to_string_lossy(),
            "base_sha": base_sha,
            "objective": "Change the value in src.ts from 1 to 42. Only modify src.ts.",
            "context_refs": [],
            "scope": {
                "allowed_paths": ["src.ts"],
                "forbidden_paths": [],
                "max_changed_files": 5
            },
            "preconditions": [{"type": "git_clean", "value": true}],
            "executor": {
                "preferred": "zcode",
                "fallbacks": [],
                "sandbox": "workspace-write",
                "timeout_seconds": 3600,
                "max_attempts": 2
            },
            "acceptance": [{"type": "command", "command_id": "verify:candidate"}],
            "authority": {
                "auto_edit_worktree": true,
                "auto_commit_candidate": true,
                "auto_merge": false,
                "auto_push": false,
                "auto_deploy": false
            },
            "stop_conditions": ["scope_expanded"],
            "rollback": {"strategy": "preserve_worktree_and_stop"},
            "status": "ready",
            "sealed_at": "2026-08-11T00:00:00Z",
            "package_hash": ""
    });
    let package_hash = app_lib::execution_store::compute_package_hash(&package).unwrap();
    package["package_hash"] = serde_json::Value::String(package_hash.clone());

    // The portable summary is local plan state and is ignored by Git in this
    // fixture, matching QuestLog's local-first project data model.
    let epics = serde_json::json!({
        "version": 1,
        "project": "e2e",
        "revision": 1,
        "epics": [{
            "id": "epic_test",
            "title": "E2E",
            "description": "",
            "status": "active",
            "priority": "P0",
            "depends_on": [],
            "blocks": [],
            "links": {},
            "acceptance": "candidate verified",
            "close_reason": "",
            "auto_execute": true,
            "created_at": "2026-08-11T00:00:00Z",
            "updated_at": "2026-08-11T00:00:00Z"
        }],
        "steps": [{
            "id": "step_test",
            "epic_id": "epic_test",
            "title": "Change value",
            "description": "",
            "order": 1,
            "depends_on": [],
            "stage": "designed",
            "links": {},
            "package": {
                "package_id": "pkg_e2e_test",
                "plan_revision": 1,
                "package_hash": package_hash,
                "objective": "Change the value in src.ts from 1 to 42. Only modify src.ts.",
                "executor": "zcode",
                "status": "sealed"
            },
            "created_at": "2026-08-11T00:00:00Z",
            "updated_at": "2026-08-11T00:00:00Z"
        }]
    });
    std::fs::write(
        repo_dir.path().join(".epics.json"),
        serde_json::to_string_pretty(&epics).unwrap(),
    )
    .unwrap();

    let packages = serde_json::json!({
        "version": 1,
        "packages": [package]
    });
    std::fs::write(
        runtime_dir.path().join("runtime").join("packages.json"),
        serde_json::to_string_pretty(&packages).unwrap(),
    )
    .unwrap();

    // Enable the project in runner-config so poll_once's project filter passes.
    // Write the config file directly: set_project_enabled(true) would call
    // ensure_running() and start the background Runner thread, which races
    // with the explicit poll_once() below (both would pick up the same ready
    // package → duplicate executions). File-based enable keeps the test's
    // explicit poll_once() the only poller.
    let runtime_path = runtime_dir.path().join("runtime");
    std::fs::write(
        runtime_path.join("runner-config.json"),
        serde_json::json!({
            "version": 1,
            "global_enabled": false,
            "projects": {
                repo_dir.path().to_string_lossy().to_string(): { "enabled": true }
            },
            "revision": 1
        })
        .to_string(),
    )
    .unwrap();

    // Run the Runner's poll_once — this should:
    // 1. Find the ready package
    // 2. Create execution + worktree
    // 3. Run ZCode with the objective
    // 4. Create candidate commit
    let mut runner = app_lib::runner::Runner::new(app_lib::runner::RunnerConfig {
        enabled: true,
        poll_interval: std::time::Duration::from_secs(1),
        runner_id: "e2e-test".to_string(),
    });

    let result = runner.poll_once();

    match &result {
        Ok(count) => {
            eprintln!("Runner processed {} package(s)", count);

            // Verify the execution was created
            let execs = app_lib::execution_store::list_executions().unwrap();
            assert!(!execs.is_empty(), "should have at least one execution");

            // Debug: dump ALL executions (not just [0]) to diagnose flaky Running status
            for (i, ex) in execs.iter().enumerate() {
                eprintln!(
                    "  [{}] {} pkg={} status={:?} err={:?}",
                    i, ex.id, ex.package_id, ex.status, ex.last_error
                );
            }

            let exec = &execs[0];
            eprintln!("Execution status: {:?}", exec.status);

            assert_eq!(
                exec.status,
                app_lib::execution_store::ExecutionStatus::Passed,
                "real ZCode candidate should pass exact-candidate verification: {:?}",
                exec.last_error
            );
            let candidate_sha = exec.candidate_sha.as_ref().expect("candidate SHA");
            let detail = app_lib::attempt_bundle::read_bundle(&exec.id).unwrap();
            let verifier_path = detail.1.join("verifier-results.json");
            let verifier: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(verifier_path).unwrap()).unwrap();
            assert_eq!(
                verifier[0]["candidate_sha"].as_str(),
                Some(candidate_sha.as_str()),
                "verifier must bind to the exact candidate SHA"
            );
            assert!(verifier[0]["passed"].as_bool().unwrap_or(false));

            // ─── AttemptBundle 完整性验证 ───
            let (manifest, bundle_path) = app_lib::attempt_bundle::read_bundle(&exec.id).unwrap();
            eprintln!("AttemptBundle at: {}", bundle_path.display());

            // 关键文件必须存在
            for file in [
                "manifest.json",
                "task-package.json",
                "environment-snapshot.json",
                "events.jsonl",
                "outcome.json",
            ] {
                assert!(
                    bundle_path.join(file).exists(),
                    "bundle missing required file: {}",
                    file
                );
            }

            // stdout/stderr 应该有（CLI 产出后被复制进来）
            if let Some(ref stdout_ref) = manifest.stdout_ref {
                assert!(bundle_path.join(stdout_ref).exists(), "stdout file missing");
            }
            if let Some(ref stderr_ref) = manifest.stderr_ref {
                assert!(bundle_path.join(stderr_ref).exists(), "stderr file missing");
            }

            // integrity_hash 必须存在且可复算
            let hash = manifest
                .integrity_hash
                .expect("bundle must be sealed with integrity hash");
            eprintln!("Integrity hash: {}", &hash[..16]);
            assert!(
                app_lib::attempt_bundle::verify_integrity(&bundle_path).unwrap(),
                "integrity hash verification failed — bundle tampered or incomplete"
            );

            // outcome 必须是 passed
            let outcome_str = std::fs::read_to_string(bundle_path.join("outcome.json")).unwrap();
            let outcome: serde_json::Value = serde_json::from_str(&outcome_str).unwrap();
            assert_eq!(outcome["outcome"].as_str(), Some("passed"));

            // environment-snapshot 必须有 executor 和 base_sha
            let env_str =
                std::fs::read_to_string(bundle_path.join("environment-snapshot.json")).unwrap();
            let env: serde_json::Value = serde_json::from_str(&env_str).unwrap();
            assert_eq!(env["executor"].as_str(), Some("zcode"));
            assert_eq!(env["base_sha"].as_str(), Some(base_sha.as_str()));

            eprintln!(
                "✓ AttemptBundle fully verified: {} files, sealed, integrity OK",
                std::fs::read_dir(&bundle_path).unwrap().count()
            );

            // Main repo working tree must be untouched regardless
            let main_content = std::fs::read_to_string(repo_dir.path().join("src.ts")).unwrap();
            assert!(
                main_content.contains("1"),
                "main workspace should be untouched"
            );
            assert!(
                !main_content.contains("42"),
                "main workspace should NOT have the change"
            );
        }
        Err(e) => {
            panic!("Runner crashed: {}", e);
        }
    }

    // Verify the package was consumed (no longer "ready")
    let remaining = app_lib::execution_store::read_ready_packages().unwrap();
    assert!(
        remaining.is_empty(),
        "package should have been consumed by the runner"
    );

    // Execution worktree must be cleaned up (removed + registration pruned)
    let wt_list = git(repo_dir.path(), &["worktree", "list"]);
    assert!(
        !wt_list.contains("questlog-worktrees"),
        "execution worktree should be cleaned up:\n{wt_list}"
    );
    let _ = Command::new("git")
        .args(["worktree", "prune"])
        .current_dir(repo_dir.path())
        .output();
}

/// Read-only NoDiff e2e: a read-only package legitimately produces no diff.
/// Under the read-only artifact contract, NoDiff counts as SUCCESS only when
/// the declared acceptance commands pass against the unchanged worktree AND
/// the run produced a non-empty stdout report.
#[test]
#[ignore = "requires real ZCode, credentials, and explicit operator opt-in"]
fn bridge_readonly_package_no_diff_counts_as_passed() {
    let _runtime_guard = RUNTIME_LOCK.lock().unwrap();
    require_live_zcode_environment();

    let runtime_dir = tempfile::tempdir().unwrap();
    std::env::set_var("QUESTLOG_RUNTIME", runtime_dir.path().join("runtime"));
    std::fs::create_dir_all(runtime_dir.path().join("runtime")).unwrap();

    let repo_dir = tempfile::tempdir().unwrap();
    let base_sha = init_git_repo(repo_dir.path());

    let mut package = serde_json::json!({
            "schema_version": 1,
            "package_id": "pkg_e2e_readonly",
            "epic_id": "epic_readonly",
            "step_id": "step_readonly_test",
            "plan_revision": 1,
            "project_path": repo_dir.path().to_string_lossy(),
            "repo_root": repo_dir.path().to_string_lossy(),
            "base_sha": base_sha,
            "objective": "Read src.ts and print its content to stdout. Do NOT create, modify, or delete any files.",
            "context_refs": [],
            "scope": {
                "allowed_paths": ["src.ts"],
                "forbidden_paths": [],
                "max_changed_files": 5
            },
            "preconditions": [],
            "executor": {
                "preferred": "zcode",
                "fallbacks": [],
                "sandbox": "read-only",
                "timeout_seconds": 3600,
                "max_attempts": 2
            },
            "acceptance": [{"type": "command", "command_id": "verify:readonly"}],
            "authority": {
                "auto_edit_worktree": true,
                "auto_commit_candidate": true,
                "auto_merge": false,
                "auto_push": false,
                "auto_deploy": false
            },
            "stop_conditions": ["scope_expanded"],
            "rollback": {"strategy": "preserve_worktree_and_stop"},
            "status": "ready",
            "sealed_at": "2026-08-11T00:00:00Z",
            "package_hash": ""
    });
    let package_hash = app_lib::execution_store::compute_package_hash(&package).unwrap();
    package["package_hash"] = serde_json::Value::String(package_hash.clone());

    let epics = serde_json::json!({
        "version": 1,
        "project": "e2e",
        "revision": 1,
        "epics": [{
            "id": "epic_readonly",
            "title": "Read-only",
            "description": "",
            "status": "active",
            "priority": "P1",
            "depends_on": [],
            "blocks": [],
            "links": {},
            "auto_execute": true,
            "created_at": "2026-08-11T00:00:00Z",
            "updated_at": "2026-08-11T00:00:00Z"
        }],
        "steps": [{
            "id": "step_readonly_test",
            "epic_id": "epic_readonly",
            "title": "Read only",
            "description": "",
            "order": 1,
            "depends_on": [],
            "stage": "designed",
            "links": {},
            "package": {
                "package_id": "pkg_e2e_readonly",
                "plan_revision": 1,
                "package_hash": package_hash,
                "objective": "Read src.ts and print its content to stdout.",
                "executor": "zcode",
                "status": "sealed"
            },
            "created_at": "2026-08-11T00:00:00Z",
            "updated_at": "2026-08-11T00:00:00Z"
        }]
    });
    std::fs::write(
        repo_dir.path().join(".epics.json"),
        serde_json::to_string_pretty(&epics).unwrap(),
    )
    .unwrap();

    let packages = serde_json::json!({
        "version": 1,
        "packages": [package]
    });
    std::fs::write(
        runtime_dir.path().join("runtime").join("packages.json"),
        serde_json::to_string_pretty(&packages).unwrap(),
    )
    .unwrap();

    // Enable via config file directly (see first test: set_project_enabled
    // would start a racing background Runner thread).
    let runtime_path = runtime_dir.path().join("runtime");
    std::fs::write(
        runtime_path.join("runner-config.json"),
        serde_json::json!({
            "version": 1,
            "global_enabled": false,
            "projects": {
                repo_dir.path().to_string_lossy().to_string(): { "enabled": true }
            },
            "revision": 1
        })
        .to_string(),
    )
    .unwrap();

    let mut runner = app_lib::runner::Runner::new(app_lib::runner::RunnerConfig {
        enabled: true,
        poll_interval: std::time::Duration::from_secs(1),
        runner_id: "e2e-readonly-test".to_string(),
    });

    let result = runner.poll_once();

    match &result {
        Ok(count) => {
            eprintln!("Runner processed {} package(s)", count);

            let execs = app_lib::execution_store::list_executions().unwrap();
            assert!(!execs.is_empty(), "should have at least one execution");
            let exec = &execs[0];
            eprintln!("Execution status: {:?}", exec.status);
            assert_eq!(
                exec.status,
                app_lib::execution_store::ExecutionStatus::Passed,
                "read-only NoDiff must count as Passed, not blocked: {:?}",
                exec.last_error
            );
            assert!(
                exec.candidate_sha.is_none(),
                "read-only task must NOT create a candidate commit"
            );

            // Bundle outcome must be no_diff (not passed — no candidate exists)
            let (_, bundle_path) = app_lib::attempt_bundle::read_bundle(&exec.id).unwrap();
            let outcome_str = std::fs::read_to_string(bundle_path.join("outcome.json")).unwrap();
            let outcome: serde_json::Value = serde_json::from_str(&outcome_str).unwrap();
            assert_eq!(outcome["outcome"].as_str(), Some("no_diff"));

            // Package must be marked passed
            let pkg_json =
                std::fs::read_to_string(runtime_dir.path().join("runtime").join("packages.json"))
                    .unwrap();
            let pkgs: serde_json::Value = serde_json::from_str(&pkg_json).unwrap();
            let status = pkgs["packages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["package_id"] == "pkg_e2e_readonly")
                .unwrap()["status"]
                .as_str()
                .unwrap();
            assert_eq!(status, "passed", "read-only package must be passed");

            // Step advances to verified — machine acceptance == verified.
            // The old "B mode" auto-granted observed, which displayed
            // un-elapsed observation windows as complete (2026-08-18 S8
            // incident); observed is now the human/observation-window gate.
            let epics_json = std::fs::read_to_string(repo_dir.path().join(".epics.json")).unwrap();
            let epics_data: serde_json::Value = serde_json::from_str(&epics_json).unwrap();
            let stage = epics_data["steps"]
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s["id"] == "step_readonly_test")
                .unwrap()["stage"]
                .as_str()
                .unwrap();
            assert_eq!(stage, "verified", "step must advance to verified (observed is human-gated)");
        }
        Err(e) => {
            panic!("Runner crashed: {}", e);
        }
    }

    // Execution worktree must be cleaned up (removed + registration pruned)
    let wt_list = git(repo_dir.path(), &["worktree", "list"]);
    assert!(
        !wt_list.contains("questlog-worktrees"),
        "execution worktree should be cleaned up:\n{wt_list}"
    );
    let _ = Command::new("git")
        .args(["worktree", "prune"])
        .current_dir(repo_dir.path())
        .output();
}
