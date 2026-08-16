/**
 * Audit-closure tests (S3): seal immutability, prepare step validation,
 * DecisionRequest options validation + action mirror, readiness fail-closed.
 */

import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { promises as fs } from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";
import {
  prepareTaskPackage,
  sealTaskPackage,
  verifyPickupReadiness,
} from "../src/task-package-ops.js";
import { createDecisionRequest, resolveDecision, listDecisionRequests } from "../src/decision-request.js";
import { savePackage, getPackage } from "../src/runtime-store.js";
import {
  TaskPackage,
  defaultExecutor,
  defaultAuthority,
  defaultStopConditions,
  defaultRollback,
  SCHEMA_VERSION,
  computePackageHash,
} from "../src/task-package.js";

let dir: string;
let oldRuntime: string | undefined;
let projectDir: string;

beforeEach(async () => {
  dir = await fs.mkdtemp(path.join(os.tmpdir(), "questlog-audit-"));
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

  // Project enabled + global on
  await fs.writeFile(
    path.join(runtimeDir, "runner-config.json"),
    JSON.stringify({
      version: 2,
      global_enabled: true,
      projects: { [projectDir]: { enabled: true } },
      revision: 1,
    }),
  );
});

afterEach(async () => {
  if (oldRuntime === undefined) delete process.env.QUESTLOG_RUNTIME;
  else process.env.QUESTLOG_RUNTIME = oldRuntime;
  await fs.rm(dir, { recursive: true, force: true });
});

async function buildPackage(overrides?: Partial<TaskPackage>): Promise<TaskPackage> {
  const pkg: TaskPackage = {
    schema_version: SCHEMA_VERSION,
    package_id: `pkg_${Math.random().toString(36).slice(2, 10)}`,
    epic_id: "e1",
    step_id: "s1",
    plan_revision: 1,
    project_path: projectDir,
    repo_root: projectDir,
    base_sha: execFileSync("git", ["rev-parse", "HEAD"], { cwd: projectDir }).toString().trim(),
    objective: "test objective",
    context_refs: [],
    scope: { allowed_paths: ["src/**"], forbidden_paths: [], max_changed_files: 10 },
    preconditions: [],
    executor: defaultExecutor(),
    acceptance: [{ type: "command", command_id: "cargo:check" }],
    authority: defaultAuthority(),
    stop_conditions: defaultStopConditions(),
    rollback: defaultRollback(),
    status: "sealed",
    sealed_at: new Date().toISOString(),
    plan_revision_delta: 0,
    package_hash: "",
    ...overrides,
  };
  pkg.package_hash = computePackageHash(pkg);
  await savePackage(pkg);
  return pkg;
}

async function writeExecutions(rows: unknown[]): Promise<void> {
  await fs.writeFile(
    path.join(process.env.QUESTLOG_RUNTIME!, "executions.json"),
    JSON.stringify({ version: 1, executions: rows, revision: 1 }),
  );
}

describe("seal immutability", () => {
  it("re-sealing a sealed package is rejected", async () => {
    const prep = await prepareTaskPackage(projectDir, {
      epic_id: "e1",
      step_id: "s1",
      objective: "add a test file",
      allowed_paths: ["test/**"],
      acceptance_command_ids: ["test:mcp"],
    });
    if ("error" in prep) throw new Error(prep.error);
    const sealed = await sealTaskPackage(projectDir, prep.package_id);
    if ("error" in sealed) throw new Error(sealed.error);

    const again = await sealTaskPackage(projectDir, prep.package_id);
    if (!("error" in again)) throw new Error("re-seal must be rejected");
    expect(again.error).toContain("immutable");

    // The stored package is untouched: same hash, same revision.
    const stored = await getPackage(prep.package_id);
    if ("error" in stored) throw new Error(stored.error);
    expect(stored.pkg.package_hash).toBe(sealed.package_hash);
    expect(stored.pkg.plan_revision).toBe(sealed.plan_revision);
  });
});

describe("prepare step validation", () => {
  it("rejects a phantom step id", async () => {
    const result = await prepareTaskPackage(projectDir, {
      epic_id: "e1",
      step_id: "ghost_step",
      objective: "nothing",
      allowed_paths: ["x/**"],
      acceptance_command_ids: ["test:mcp"],
    });
    if (!("error" in result)) throw new Error("phantom step must be rejected");
    expect(result.error).toContain("ghost_step");
    expect(result.error).toContain("not found");
  });
});

describe("DecisionRequest options validation + action mirror", () => {
  it("rejects a resolution outside the declared options", async () => {
    const req = await createDecisionRequest({
      epic_id: "e1",
      step_id: "s1",
      execution_id: "exec_x",
      kind: "blocked",
      question: "continue?",
      options: ["retry", "cancel"],
    });
    const result = await resolveDecision(req.request_id, "bogus");
    if (!("error" in result)) throw new Error("undeclared option must be rejected");
    expect(result.error).toContain("not one of the declared options");
    // Still unresolved.
    const list = await listDecisionRequests();
    expect(list.length).toBe(1);
  });

  it("retry resolution creates a child attempt and readies the package", async () => {
    const pkg = await buildPackage({ status: "blocked" });
    const now = Math.floor(Date.now() / 1000);
    await writeExecutions([{
      id: "exec_parent",
      package_id: pkg.package_id,
      epic_id: "e1",
      step_id: "s1",
      project_path: projectDir,
      status: "timed_out",
      lease: null,
      cli_session: null,
      worktree_path: null,
      branch: null,
      base_sha: pkg.base_sha,
      candidate_sha: null,
      attempts: 1,
      last_error: "timeout",
      created_at: now,
      updated_at: now,
      attempt_no: 1,
    }]);
    const req = await createDecisionRequest({
      epic_id: "e1",
      step_id: "s1",
      execution_id: "exec_parent",
      kind: "blocked",
      question: "retry?",
      options: ["retry", "revise_package", "cancel"],
    });

    const result = await resolveDecision(req.request_id, "retry");
    if ("error" in result) throw new Error(result.error);
    expect(result.action).toContain("retry scheduled");

    const execs = JSON.parse(
      await fs.readFile(path.join(process.env.QUESTLOG_RUNTIME!, "executions.json"), "utf-8"),
    ).executions;
    const child = execs.find((e: { parent_attempt_id?: string }) => e.parent_attempt_id === "exec_parent");
    expect(child).toBeTruthy();
    expect(child.status).toBe("queued");
    expect(child.attempt_no).toBe(2);

    const stored = await getPackage(pkg.package_id);
    if ("error" in stored) throw new Error(stored.error);
    expect(stored.pkg.status).toBe("ready");
  });

  it("resume resolution maps to a controlled retry child", async () => {
    const pkg = await buildPackage({ status: "blocked" });
    const now = Math.floor(Date.now() / 1000);
    await writeExecutions([{
      id: "exec_paused",
      package_id: pkg.package_id,
      epic_id: "e1",
      step_id: "s1",
      project_path: projectDir,
      status: "blocked",
      lease: null,
      cli_session: "sess_paused1",
      worktree_path: "/tmp/wt-paused",
      branch: "questlog/exec_paused",
      base_sha: pkg.base_sha,
      candidate_sha: null,
      attempts: 1,
      last_error: "paused by user",
      created_at: now,
      updated_at: now,
      attempt_no: 1,
    }]);
    const req = await createDecisionRequest({
      epic_id: "e1",
      step_id: "s1",
      execution_id: "exec_paused",
      kind: "approval",
      question: "resume?",
      options: ["resume", "revise_package", "cancel"],
    });

    const result = await resolveDecision(req.request_id, "resume");
    if ("error" in result) throw new Error(result.error);
    expect(result.action).toContain("retry scheduled");
    const execs = JSON.parse(
      await fs.readFile(path.join(process.env.QUESTLOG_RUNTIME!, "executions.json"), "utf-8"),
    ).executions;
    const child = execs.find((e: { parent_attempt_id?: string }) => e.parent_attempt_id === "exec_paused");
    expect(child).toBeTruthy();
    expect(child.status).toBe("queued");
    expect(child.attempt_no).toBe(2);
  });

  it("cancel resolution marks the execution cancelled", async () => {
    const pkg = await buildPackage({ status: "blocked" });
    const now = Math.floor(Date.now() / 1000);
    await writeExecutions([{
      id: "exec_block",
      package_id: pkg.package_id,
      epic_id: "e1",
      step_id: "s1",
      project_path: projectDir,
      status: "blocked",
      lease: null,
      cli_session: null,
      worktree_path: null,
      branch: null,
      base_sha: pkg.base_sha,
      candidate_sha: null,
      attempts: 1,
      last_error: "blocked",
      created_at: now,
      updated_at: now,
      attempt_no: 1,
    }]);
    const req = await createDecisionRequest({
      epic_id: "e1",
      step_id: "s1",
      execution_id: "exec_block",
      kind: "blocked",
      question: "cancel?",
      options: ["retry", "cancel"],
    });

    const result = await resolveDecision(req.request_id, "cancel");
    if ("error" in result) throw new Error(result.error);
    const execs = JSON.parse(
      await fs.readFile(path.join(process.env.QUESTLOG_RUNTIME!, "executions.json"), "utf-8"),
    ).executions;
    expect(execs[0].status).toBe("cancelled");
  });

  it("cancel on a terminal execution is refused (evidence preserved)", async () => {
    const pkg = await buildPackage({ status: "blocked" });
    const now = Math.floor(Date.now() / 1000);
    await writeExecutions([{
      id: "exec_terminal",
      package_id: pkg.package_id,
      epic_id: "e1",
      step_id: "s1",
      project_path: projectDir,
      status: "timed_out",
      lease: null,
      cli_session: null,
      worktree_path: null,
      branch: null,
      base_sha: pkg.base_sha,
      candidate_sha: null,
      attempts: 1,
      last_error: "timeout",
      created_at: now,
      updated_at: now,
      attempt_no: 1,
    }]);
    const req = await createDecisionRequest({
      epic_id: "e1",
      step_id: "s1",
      execution_id: "exec_terminal",
      kind: "retry_exhausted",
      question: "budget exhausted",
      options: ["revise_package", "cancel"],
    });

    const result = await resolveDecision(req.request_id, "cancel");
    // Decision recorded, but the action must refuse and the terminal state stays.
    if ("error" in result) throw new Error(result.error);
    expect(result.action).toContain("refusing to overwrite");
    const execs = JSON.parse(
      await fs.readFile(path.join(process.env.QUESTLOG_RUNTIME!, "executions.json"), "utf-8"),
    ).executions;
    expect(execs[0].status).toBe("timed_out");
    expect(execs[0].last_error).toBe("timeout");
  });

  it("readiness reports the global kill switch as a blocker", async () => {
    // Flip the fixture config to global OFF.
    await fs.writeFile(
      path.join(process.env.QUESTLOG_RUNTIME!, "runner-config.json"),
      JSON.stringify({
        version: 2,
        global_enabled: false,
        projects: { [projectDir]: { enabled: true } },
        revision: 2,
      }),
    );
    const pkg = await buildPackage();
    const result = await verifyPickupReadiness(pkg.package_id);
    if ("error" in result) throw new Error(result.error);
    expect(result.will_be_picked_up).toBe(false);
    expect(result.blockers.some((b) => b.check === "global_enabled")).toBe(true);
    expect(result.runner_side_checks.runner_thread_alive).toBe(false);
  });
});

describe("readiness fail-closed", () => {
  it("unknown epic is a blocker, not permissive", async () => {
    const pkg = await buildPackage({ epic_id: "ghost_epic" });
    const result = await verifyPickupReadiness(pkg.package_id);
    if ("error" in result) throw new Error(result.error);
    expect(result.will_be_picked_up).toBe(false);
    expect(result.blockers.some((b) => b.check === "epic_auto_execute_allows")).toBe(true);
  });

  it("missing .epics.json is a blocker, not permissive", async () => {
    const pkg = await buildPackage();
    await fs.rm(path.join(projectDir, ".epics.json"));
    const result = await verifyPickupReadiness(pkg.package_id);
    if ("error" in result) throw new Error(result.error);
    expect(result.will_be_picked_up).toBe(false);
    expect(result.blockers.some((b) => b.check === "epic_auto_execute_allows")).toBe(true);
  });
});

describe("Node↔Rust status contract", () => {
  it("status sets match the Rust ExecutionStatus serialization", async () => {
    // Parse the Rust enum and derive its serde snake_case serialization —
    // the same trick as the acceptance-registry parity test. A PascalCase
    // IN_FLIGHT set once silently matched nothing; this keeps both ends on
    // the same vocabulary.
    const rustPath = path.resolve(
      __dirname,
      "../../desktop-app/src-tauri/src/execution_store.rs",
    );
    const source = await fs.readFile(rustPath, "utf-8");
    const match = source.match(/pub enum ExecutionStatus\s*\{(.*?)\n\}/s);
    expect(match, "Rust ExecutionStatus enum not found").not.toBeNull();
    const toSnake = (name: string) =>
      name.replace(/([a-z0-9])([A-Z])/g, "$1_$2").toLowerCase();
    const rustStatuses = new Set<string>();
    for (const line of match![1].split("\n")) {
      const variant = line.trim().match(/^([A-Z][A-Za-z0-9]*),?/);
      if (variant) rustStatuses.add(toSnake(variant[1]));
    }
    expect(rustStatuses.size).toBeGreaterThan(5);
    expect(rustStatuses.has("cancel_requested")).toBe(true); // sanity

    const { IN_FLIGHT_STATUSES } = await import("../src/task-package-ops.js");
    const { RETRYABLE_STATUSES } = await import("../src/decision-actions.js");

    // Every member must be a real serialized status (catches PascalCase drift).
    for (const s of IN_FLIGHT_STATUSES) {
      expect(rustStatuses.has(s), `IN_FLIGHT '${s}' not a Rust status`).toBe(true);
    }
    for (const s of RETRYABLE_STATUSES) {
      expect(rustStatuses.has(s), `RETRYABLE '${s}' not a Rust status`).toBe(true);
    }
    // Exact membership: if Rust adds/renames an in-flight status, this fails
    // and forces the MCP mirror to be updated in the same commit.
    expect([...IN_FLIGHT_STATUSES].sort()).toEqual(
      ["cancel_requested", "candidate", "pause_requested", "queued", "running", "verifying"].sort(),
    );
    expect([...RETRYABLE_STATUSES].sort()).toEqual(
      ["blocked", "cancelled", "failed", "stale", "timed_out"].sort(),
    );
  });
});
