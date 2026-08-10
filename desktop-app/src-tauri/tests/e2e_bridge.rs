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

fn zcode_available() -> bool {
    let path = "/Applications/ZCode.app/Contents/Resources/glm/zcode.cjs";
    std::path::Path::new(path).exists()
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
fn bridge_ready_package_to_zcode_execution() {
    if !zcode_available() {
        eprintln!("Skipping e2e bridge test — ZCode CLI not available");
        return;
    }

    // Set up isolated runtime
    let runtime_dir = tempfile::tempdir().unwrap();
    std::env::set_var("QUESTLOG_RUNTIME", runtime_dir.path().join("runtime"));
    std::fs::create_dir_all(runtime_dir.path().join("runtime")).unwrap();

    // Create a real git fixture
    let repo_dir = tempfile::tempdir().unwrap();
    let base_sha = init_git_repo(repo_dir.path());

    // Write packages.json (simulating what Node MCP enqueue_step produces)
    // Use the **nested** TaskPackage shape that Node's savePackage actually writes.
    // Use an empty acceptance list — the test verifies the Runner correctly
    // REJECTS packages without acceptance commands (anti-fake-green gate).
    let packages = serde_json::json!({
        "version": 1,
        "packages": [{
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
            "preconditions": [],
            "executor": {
                "preferred": "zcode",
                "fallbacks": [],
                "sandbox": "workspace-write",
                "timeout_seconds": 3600,
                "max_attempts": 2
            },
            "acceptance": [],
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
            "package_hash": "e2e_fake_hash"
        }]
    });
    std::fs::write(
        runtime_dir.path().join("runtime").join("packages.json"),
        serde_json::to_string_pretty(&packages).unwrap(),
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

            let exec = &execs[0];
            eprintln!("Execution status: {:?}", exec.status);

            // Anti-fake-green: no acceptance commands → must NOT be Passed
            assert_ne!(
                exec.status,
                app_lib::execution_store::ExecutionStatus::Passed,
                "execution without acceptance commands must NOT pass"
            );

            if let Some(ref err) = exec.last_error {
                eprintln!("  Error: {}", err);
                // Verify it's the acceptance gate that caught it
                assert!(
                    err.contains("no acceptance commands"),
                    "should fail on acceptance gate, got: {}",
                    err
                );
                eprintln!("✅ Anti-fake-green gate works: candidate commit rejected without acceptance commands");
            }

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

    // Clean up worktrees
    let _ = Command::new("git")
        .args(["worktree", "prune"])
        .current_dir(repo_dir.path())
        .output();
}
