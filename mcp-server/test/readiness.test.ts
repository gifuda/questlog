/**
 * verifyPickupReadiness tests: the readiness gate mirrors Runner's poll_once
 * filters. These tests verify that the gate correctly reports whether a
 * sealed/ready package will be picked up, and surfaces the right blockers.
 *
 * Tested branches:
 *   1. All-clear: project enabled, epic auto_execute=true, no in-flight → eligible
 *   2. Project disabled → blocker (project_enabled)
 *   3. Epic auto_execute=false → blocker (epic_auto_execute_allows)
 *   3b. Epic auto_execute absent/null → blocker (fail-closed, mirrors Rust)
 *   4. Already in-flight → blocker (already_in_flight)
 *   5. Unknown acceptance command → blocker (acceptance_commands_available)
 *   6. enqueue_step returns runner_eligible + blockers
 */

import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { promises as fs } from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";
import {
  prepareTaskPackage,
  sealTaskPackage,
  enqueueStep,
  verifyPickupReadiness,
} from "../src/task-package-ops.js";
import { savePackage, getPackage } from "../src/runtime-store.js";
import { TaskPackage, defaultExecutor, defaultAuthority, defaultStopConditions, defaultRollback, SCHEMA_VERSION } from "../src/task-package.js";

let dir: string;
let oldRuntime: string | undefined;
let projectDir: string;

beforeEach(async () => {
  dir = await fs.mkdtemp(path.join(os.tmpdir(), "questlog-ready-"));
  oldRuntime = process.env.QUESTLOG_RUNTIME;
  const runtimeDir = path.join(dir, "runtime");
  process.env.QUESTLOG_RUNTIME = runtimeDir;
  await fs.mkdir(runtimeDir, { recursive: true });

  projectDir = path.join(dir, "test-project");
  await fs.mkdir(projectDir);
  execFileSync("git", ["init"], { cwd: projectDir, stdio: "pipe" });
  execFileSync("git", ["config", "user.email", "test@test.com"], { cwd: projectDir, stdio: "pipe" });
  execFileSync("git", ["config", "user.name", "Test"], { cwd: projectDir, stdio: "pipe" });
  await fs.writeFile(path.join(projectDir, "README.md"), "# Test\n");
  execFileSync("git", ["add", "."], { cwd: projectDir, stdio: "pipe" });
  execFileSync("git", ["commit", "-m", "initial"], { cwd: projectDir, stdio: "pipe" });

  // Seed .epics.json with an active epic + step
  const now = new Date().toISOString();
  await fs.writeFile(
    path.join(projectDir, ".epics.json"),
    JSON.stringify({
      version: 1,
      project: "test-project",
      revision: 0,
      epics: [{
        id: "e1",
        title: "Test Epic",
        description: "desc",
        status: "active",
        priority: "P0",
        depends_on: [],
        blocks: [],
        links: {},
        auto_execute: true,
        created_at: now,
        updated_at: now,
      }],
      steps: [{
        id: "s1",
        epic_id: "e1",
        title: "Test Step",
        description: "desc",
        order: 1,
        stage: "designed",
        links: {},
        created_at: now,
        updated_at: now,
      }],
    }),
  );
});

afterEach(async () => {
  if (oldRuntime === undefined) {
    delete process.env.QUESTLOG_RUNTIME;
  } else {
    process.env.QUESTLOG_RUNTIME = oldRuntime;
  }
});

/** Helper: write runner-config.json */
async function writeRunnerConfig(projects: Record<string, { enabled: boolean }>): Promise<void> {
  const config = {
    version: 1,
    global_enabled: true,
    projects,
    revision: 1,
  };
  await fs.writeFile(
    path.join(process.env.QUESTLOG_RUNTIME!, "runner-config.json"),
    JSON.stringify(config),
  );
}

/** Helper: write executions.json */
async function writeExecutions(executions: Array<{ id: string; package_id: string; status: string }>): Promise<void> {
  const data = {
    version: 1,
    executions,
    revision: 1,
  };
  await fs.writeFile(
    path.join(process.env.QUESTLOG_RUNTIME!, "executions.json"),
    JSON.stringify(data),
  );
}

/** Helper: build a minimal sealed package directly (skip the full prepare chain). */
async function buildSealedPackage(overrides?: {
  acceptance?: TaskPackage["acceptance"];
}): Promise<string> {
  const pkgId = `pkg_${Math.random().toString(36).slice(2, 10)}`;
  const pkg: TaskPackage = {
    schema_version: SCHEMA_VERSION,
    package_id: pkgId,
    epic_id: "e1",
    step_id: "s1",
    plan_revision: 1,
    project_path: projectDir,
    repo_root: projectDir,
    base_sha: execFileSync("git", ["rev-parse", "HEAD"], { cwd: projectDir }).toString().trim(),
    objective: "test objective",
    context_refs: [],
    scope: {
      allowed_paths: ["src/**"],
      forbidden_paths: [],
      max_changed_files: 10,
    },
    preconditions: [{ type: "git_clean", value: true }],
    executor: defaultExecutor(),
    acceptance: overrides?.acceptance ?? [{ type: "command", command_id: "cargo:check" }],
    authority: defaultAuthority(),
    stop_conditions: defaultStopConditions(),
    rollback: defaultRollback(),
    status: "sealed",
    sealed_at: new Date().toISOString(),
    plan_revision_delta: 0,
    package_hash: "placeholder-will-be-recomputed",
  };
  // Compute real hash
  pkg.package_hash = await import("../src/task-package.js").then((m) => m.computePackageHash(pkg));
  await savePackage(pkg);
  return pkgId;
}

describe("verifyPickupReadiness", () => {
  it("reports eligible when project enabled, epic allows, no in-flight, commands registered", async () => {
    await writeRunnerConfig({ [projectDir]: { enabled: true } });
    await writeExecutions([]);
    const pkgId = await buildSealedPackage();

    const result = await verifyPickupReadiness(pkgId);
    expect("error" in result).toBe(false);
    if ("error" in result) return;
    expect(result.will_be_picked_up).toBe(true);
    expect(result.blockers).toHaveLength(0);
    expect(result.runner_side_checks.project_enabled).toBe(true);
    expect(result.runner_side_checks.epic_auto_execute_allows).toBe(true);
    expect(result.runner_side_checks.runner_thread_alive).toBe(true);
    expect(result.acceptance_commands_available).toBe(true);
  });

  it("reports blocker when project not enabled", async () => {
    await writeRunnerConfig({ [projectDir]: { enabled: false } });
    await writeExecutions([]);
    const pkgId = await buildSealedPackage();

    const result = await verifyPickupReadiness(pkgId);
    if ("error" in result) throw new Error(result.error);
    expect(result.will_be_picked_up).toBe(false);
    expect(result.blockers.some((b) => b.check === "project_enabled")).toBe(true);
    expect(result.runner_side_checks.project_enabled).toBe(false);
  });

  it("reports blocker when epic auto_execute=false", async () => {
    await writeRunnerConfig({ [projectDir]: { enabled: true } });
    await writeExecutions([]);

    // Flip epic auto_execute to false
    const epicsPath = path.join(projectDir, ".epics.json");
    const raw = JSON.parse(await fs.readFile(epicsPath, "utf-8"));
    raw.epics[0].auto_execute = false;
    await fs.writeFile(epicsPath, JSON.stringify(raw));

    const pkgId = await buildSealedPackage();
    const result = await verifyPickupReadiness(pkgId);
    if ("error" in result) throw new Error(result.error);
    expect(result.will_be_picked_up).toBe(false);
    expect(result.blockers.some((b) => b.check === "epic_auto_execute_allows")).toBe(true);
    expect(result.runner_side_checks.epic_auto_execute_allows).toBe(false);
  });

  it("reports blocker when epic auto_execute is absent (fail-closed)", async () => {
    await writeRunnerConfig({ [projectDir]: { enabled: true } });
    await writeExecutions([]);

    // Delete auto_execute → null: with "switch = authorization" the project
    // switch alone must not authorize execution (mirrors Rust is_epic_auto_execute).
    const epicsPath = path.join(projectDir, ".epics.json");
    const raw = JSON.parse(await fs.readFile(epicsPath, "utf-8"));
    delete raw.epics[0].auto_execute;
    await fs.writeFile(epicsPath, JSON.stringify(raw));

    const pkgId = await buildSealedPackage();
    const result = await verifyPickupReadiness(pkgId);
    if ("error" in result) throw new Error(result.error);
    expect(result.will_be_picked_up).toBe(false);
    expect(result.blockers.some((b) => b.check === "epic_auto_execute_allows")).toBe(true);
    expect(result.runner_side_checks.epic_auto_execute_allows).toBe(false);
  });

  it("reports blocker when execution already in-flight", async () => {
    await writeRunnerConfig({ [projectDir]: { enabled: true } });
    const pkgId = await buildSealedPackage();
    await writeExecutions([
      { id: "exec_1", package_id: pkgId, status: "Running" },
    ]);

    const result = await verifyPickupReadiness(pkgId);
    if ("error" in result) throw new Error(result.error);
    expect(result.will_be_picked_up).toBe(false);
    expect(result.blockers.some((b) => b.check === "already_in_flight")).toBe(true);
    expect(result.runner_side_checks.already_in_flight).toBe(true);
  });

  it("reports blocker for unknown acceptance command_id", async () => {
    await writeRunnerConfig({ [projectDir]: { enabled: true } });
    await writeExecutions([]);
    const pkgId = await buildSealedPackage({
      acceptance: [{ type: "command", command_id: "nonexistent:cmd" }],
    });

    const result = await verifyPickupReadiness(pkgId);
    if ("error" in result) throw new Error(result.error);
    expect(result.will_be_picked_up).toBe(false);
    expect(result.blockers.some((b) => b.check === "acceptance_commands_available")).toBe(true);
    expect(result.acceptance_commands_available).toBe(false);
  });

  it("runner_thread_alive is false when no projects enabled", async () => {
    await writeRunnerConfig({ "/some/other/project": { enabled: false } });
    await writeExecutions([]);
    const pkgId = await buildSealedPackage();

    const result = await verifyPickupReadiness(pkgId);
    if ("error" in result) throw new Error(result.error);
    expect(result.runner_side_checks.runner_thread_alive).toBe(false);
  });
});

describe("enqueueStep returns runner_eligible", () => {
  it("returns runner_eligible=false with blockers when project disabled", async () => {
    await writeRunnerConfig({ [projectDir]: { enabled: false } });
    await writeExecutions([]);

    // Prepare + seal + enqueue the full chain
    const prepareResult = await prepareTaskPackage(projectDir, {
      epic_id: "e1",
      step_id: "s1",
      objective: "test obj",
      allowed_paths: ["src/**"],
      acceptance_command_ids: ["cargo:check"],
    });
    if ("error" in prepareResult) throw new Error(prepareResult.error);

    const sealResult = await sealTaskPackage(projectDir, prepareResult.package_id);
    if ("error" in sealResult) throw new Error(sealResult.error);

    const enqueueResult = await enqueueStep(projectDir, "s1");
    if ("error" in enqueueResult) throw new Error(enqueueResult.error);

    expect(enqueueResult.runner_eligible).toBe(false);
    expect(enqueueResult.blockers.length).toBeGreaterThan(0);
    expect(enqueueResult.message).toContain("BLOCKED");
  });
});
