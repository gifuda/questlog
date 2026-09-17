/**
 * G2 acceptance tests (待办租户 plan §4 / §5.2):
 * - producer creates a REAL epic/step/package via the standard core
 *   (manual executor, read-only sandbox, origin carried into the sealed hash)
 * - idempotency: one task_key ⇔ one object triple; replay returns existing
 * - guards: opt-in/type/status/processing; strict read blocks duplicates
 * - lifecycle: parallel task refused; terminal task_key never re-runs
 */

import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { promises as fs } from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { produceTodoPackage } from "../src/todo-producer.js";
import { computeTaskRevision, computeTaskKey } from "../src/task-key.js";
import { computePackageHash, validatePackageContent, TaskPackage } from "../src/task-package.js";
import { listEpics } from "../src/epic-store.js";
import { readLedger, mutateLedger } from "../src/todo-ledger.js";
import { getPackage } from "../src/runtime-store.js";

let dir: string;
let projectDir: string;
let oldRuntime: string | undefined;
let fakeHome = "";

vi.mock("node:os", async (importOriginal) => {
  const real = await importOriginal<typeof import("node:os")>();
  return { ...real, homedir: () => fakeHome };
});

const TODO_ID = "todo-aaaa";

function todoItem(overrides: Record<string, unknown> = {}) {
  return {
    id: TODO_ID,
    type: "todo",
    title: "调研 Redis 持久化策略",
    content: "目标：对比 RDB/AOF/混合模式。验收标准：报告覆盖性能/安全/复杂度三维度并给出建议。",
    suggestion: "输出对比报告并给出选型建议",
    tags: ["redis"],
    status: "active",
    priority: "high",
    source_session: "",
    created_at: "2026-09-17T00:00:00Z",
    updated_at: "2026-09-17T00:00:00Z",
    auto_process: true,
    auto_process_revision: 1,
    ...overrides,
  };
}

async function writeTodos(items: unknown[]) {
  await fs.writeFile(
    path.join(projectDir, ".todos.json"),
    JSON.stringify({ version: 1, project: "test-project", items, revision: 1 }, null, 2),
  );
}

beforeEach(async () => {
  dir = await fs.mkdtemp(path.join(os.tmpdir(), "questlog-producer-"));
  fakeHome = dir;
  oldRuntime = process.env.QUESTLOG_RUNTIME;
  process.env.QUESTLOG_RUNTIME = path.join(dir, "runtime");

  projectDir = path.join(dir, "test-project");
  await fs.mkdir(projectDir);
  execFileSync("git", ["init"], { cwd: projectDir, stdio: "pipe" });
  execFileSync("git", ["config", "user.email", "t@t.com"], { cwd: projectDir, stdio: "pipe" });
  execFileSync("git", ["config", "user.name", "T"], { cwd: projectDir, stdio: "pipe" });
  await fs.writeFile(path.join(projectDir, "README.md"), "# T\n");
  execFileSync("git", ["add", "."], { cwd: projectDir, stdio: "pipe" });
  execFileSync("git", ["commit", "-m", "init"], { cwd: projectDir, stdio: "pipe" });

  await writeTodos([todoItem()]);
});

afterEach(async () => {
  process.env.QUESTLOG_RUNTIME = oldRuntime;
  await fs.rm(dir, { recursive: true, force: true });
});

describe("produceTodoPackage", () => {
  it("creates real epic/step/package with origin, manual executor, sealed", async () => {
    const r = await produceTodoPackage(projectDir, TODO_ID);
    if (!r.ok) throw new Error(r.error);
    expect(r.replay).toBe(false);

    // Real epic with auto_execute bound, step soft-linked to the todo.
    const epics = await listEpics(projectDir);
    if ("error" in epics) throw new Error(epics.error);
    const epic = epics.epics.find((e) => e.id === r.epic_id)!;
    expect(epic.auto_execute).toBe(true);
    const step = epics.steps.find((s) => s.id === r.step_id)!;
    expect(step.links?.todo_ref).toBe(TODO_ID);

    // Sealed standard package: strict read-only policy (G3) — manual
    // executor (never locally executed), zero write rights, read_paths
    // scope, command + native verifier acceptance, origin carried.
    const pkg = await getPackage(r.package_id);
    if ("error" in pkg) throw new Error(pkg.error);
    expect(pkg.pkg.status).toBe("sealed");
    expect(pkg.pkg.executor.preferred).toBe("manual");
    expect(pkg.pkg.executor.sandbox).toBe("read-only");
    expect(pkg.pkg.executor.max_attempts).toBe(1);
    expect(pkg.pkg.scope.allowed_paths).toEqual([]);
    expect(pkg.pkg.scope.read_paths).toEqual(["**"]);
    expect(pkg.pkg.scope.max_changed_files).toBe(0);
    expect(pkg.pkg.authority.auto_edit_worktree).toBe(false);
    expect(pkg.pkg.authority.auto_commit_candidate).toBe(false);
    expect(pkg.pkg.acceptance).toEqual([
      { type: "command", command_id: "verify:readonly" },
      { type: "verifier", verifier_id: "todo:report" },
    ]);
    expect(pkg.pkg.origin).toMatchObject({ type: "todo", tenant_id: "test-project", todo_id: TODO_ID, consent_revision: 1 });
    expect(pkg.pkg.origin!.task_key).toBe(r.task_key);

    // Ledger published with the full triple.
    const ledger = await readLedger();
    expect(ledger[r.task_key]?.state).toBe("published");
    expect(ledger[r.task_key]?.package_id).toBe(r.package_id);
  });

  it("replays an open task_key without creating new objects", async () => {
    const first = await produceTodoPackage(projectDir, TODO_ID);
    if (!first.ok) throw new Error(first.error);

    const second = await produceTodoPackage(projectDir, TODO_ID);
    if (!second.ok) throw new Error(second.error);
    expect(second.replay).toBe(true);
    expect(second.task_key).toBe(first.task_key);
    expect(second.package_id).toBe(first.package_id);

    // No duplicate objects: exactly one epic, one step, one package file.
    const epics = await listEpics(projectDir);
    if ("error" in epics) throw new Error(epics.error);
    expect(epics.epics.length).toBe(1);
    expect(epics.steps.length).toBe(1);
    const ledger = await readLedger();
    expect(Object.keys(ledger).length).toBe(1);
  });

  it("refuses every non-authorized shape", async () => {
    const cases: Array<[Record<string, unknown>, string]> = [
      [{ auto_process: false }, "not_opted_in"],
      [{ type: "knowhow" }, "not_todo_type"],
      [{ status: "completed" }, "not_active"],
      [{ processing: true }, "manual_processing"],
    ];
    for (const [overrides, reason] of cases) {
      await writeTodos([todoItem(overrides)]);
      const r = await produceTodoPackage(projectDir, TODO_ID);
      expect(r.ok, `${reason}: ${r.ok ? "unexpected ok" : ""}`).toBe(false);
      if (!r.ok) expect(r.reason_code).toBe(reason);
    }
  });

  it("blocks on duplicate item ids (strict read, no repair)", async () => {
    await writeTodos([todoItem(), todoItem()]);
    const r = await produceTodoPackage(projectDir, TODO_ID);
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.reason_code).toBe("strict_read_failed");
  });

  it("refuses a parallel task for the same todo (different task_key)", async () => {
    const first = await produceTodoPackage(projectDir, TODO_ID);
    if (!first.ok) throw new Error(first.error);
    // Edit content → new task_revision → different task_key, same todo.
    await writeTodos([todoItem({ content: "改过的目标：覆盖第四个维度。验收标准：报告四维对比。" })]);
    const r = await produceTodoPackage(projectDir, TODO_ID);
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.reason_code).toBe("parallel_task_open");
  });

  it("a terminal task_key never re-runs", async () => {
    const first = await produceTodoPackage(projectDir, TODO_ID);
    if (!first.ok) throw new Error(first.error);
    // Force-close the ledger entry (simulating the G4 finalization).
    await mutateLedger((entries) => {
      entries[first.task_key].state = "closed";
      return { result: true, entries };
    });
    const r = await produceTodoPackage(projectDir, TODO_ID);
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.reason_code).toBe("task_key_terminal");
  });

  it("re-authorization (consent bump) opens a NEW task and reuses the epic", async () => {
    const first = await produceTodoPackage(projectDir, TODO_ID);
    if (!first.ok) throw new Error(first.error);
    await mutateLedger((entries) => {
      entries[first.task_key].state = "closed";
      return { result: true, entries };
    });
    // User toggles off→on: revision 2 → fresh task_key.
    await writeTodos([todoItem({ auto_process_revision: 2 })]);
    const second = await produceTodoPackage(projectDir, TODO_ID);
    if (!second.ok) throw new Error(second.error);
    expect(second.replay).toBe(false);
    expect(second.task_key).not.toBe(first.task_key);
    // Same epic reused (todo_ref lookup), new step+package.
    expect(second.epic_id).toBe(first.epic_id);
    const epics = await listEpics(projectDir);
    if ("error" in epics) throw new Error(epics.error);
    expect(epics.epics.length).toBe(1);
    expect(epics.steps.length).toBe(2);
  });

  it("concurrent produce of the same todo creates exactly one triple", async () => {
    const [a, b] = await Promise.all([
      produceTodoPackage(projectDir, TODO_ID),
      produceTodoPackage(projectDir, TODO_ID),
    ]);
    // Exactly one wins the reserve; the other replays/blocks.
    const oks = [a, b].filter((r) => r.ok);
    expect(oks.length).toBeGreaterThanOrEqual(1);
    const epics = await listEpics(projectDir);
    if ("error" in epics) throw new Error(epics.error);
    expect(epics.epics.length).toBe(1);
    expect(epics.steps.length).toBe(1);
    const ledger = await readLedger();
    expect(Object.keys(ledger).length).toBe(1);
    const runtimePkgs = await fs.readFile(
      path.join(process.env.QUESTLOG_RUNTIME!, "packages.json"),
      "utf-8",
    );
    const parsedPkgs = JSON.parse(runtimePkgs) as { packages: { package_id: string }[] };
    // savePackage keeps superseded history rows — count DISTINCT packages.
    expect(new Set(parsedPkgs.packages.map((p) => p.package_id)).size).toBe(1);
  });

  it("a lost ledger is caught by the package-store cross-check (fail-closed)", async () => {
    const first = await produceTodoPackage(projectDir, TODO_ID);
    if (!first.ok) throw new Error(first.error);
    // Simulate ledger loss/rollback while packages.json survives.
    await fs.rm(path.join(process.env.QUESTLOG_RUNTIME!, "todo-tasks.json"));
    const r = await produceTodoPackage(projectDir, TODO_ID);
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.reason_code).toBe("ledger_package_inconsistent");
  });
});

describe("task identity", () => {
  it("task_revision/task_key are deterministic and content-sensitive", () => {
    const base = { type: "todo", title: "t", content: "c", suggestion: "s" };
    const r1 = computeTaskRevision(base);
    expect(computeTaskRevision(base)).toBe(r1);
    expect(computeTaskRevision({ ...base, content: "c2" })).not.toBe(r1);
    expect(computeTaskRevision({ ...base, acceptance_criteria: "x" })).not.toBe(r1);

    const k1 = computeTaskKey({ tenant_id: "a", todo_id: "b", task_revision: r1, consent_revision: 1 });
    expect(k1).toHaveLength(64);
    expect(
      computeTaskKey({ tenant_id: "a", todo_id: "b", task_revision: r1, consent_revision: 2 }),
    ).not.toBe(k1);
  });

  it("origin participates in the sealed hash", () => {
    const mk = (origin?: TaskPackage["origin"]): Omit<TaskPackage, "package_hash" | "status"> =>
      ({
        schema_version: 1,
        package_id: "pkg_x",
        epic_id: "e",
        step_id: "s",
        plan_revision: 1,
        ...(origin ? { origin } : {}),
        project_path: "/p",
        repo_root: "/p",
        base_sha: "abc",
        objective: "o",
        context_refs: [],
        scope: { allowed_paths: ["a/**"], forbidden_paths: [], max_changed_files: 1 },
        preconditions: [],
        executor: { preferred: "manual", fallbacks: [], sandbox: "read-only", timeout_seconds: 900, max_attempts: 1 },
        acceptance: [{ type: "command", command_id: "verify:readonly" }],
        authority: { auto_edit_worktree: true, auto_commit_candidate: true, auto_merge: false, auto_push: false, auto_deploy: false },
        stop_conditions: [],
        rollback: { strategy: "preserve_worktree_and_stop" },
        sealed_at: "2026",
      }) as Omit<TaskPackage, "package_hash" | "status">;

    const originA = { type: "todo" as const, tenant_id: "t", todo_id: "d", task_revision: "r1", consent_revision: 1, task_key: "k1" };
    const originB = { ...originA, consent_revision: 2 };
    const hNone = computePackageHash(mk());
    const hA = computePackageHash(mk(originA));
    const hB = computePackageHash(mk(originB));
    expect(hNone).not.toBe(hA);
    expect(hA).not.toBe(hB); // consent change → different hash → different package
  });

  it("validatePackageContent rejects partial or unknown-type origins", () => {
    const good = { type: "todo" as const, tenant_id: "t", todo_id: "d", task_revision: "r", consent_revision: 1, task_key: "k" };
    expect(validatePackageContent({ origin: good } as Partial<TaskPackage>).some((i) => i.field.startsWith("origin."))).toBe(false);
    const noKey = validatePackageContent({ origin: { ...good, task_key: "" } } as Partial<TaskPackage>);
    expect(noKey.some((i) => i.field === "origin.task_key" && i.severity === "error")).toBe(true);
    const badType = validatePackageContent({ origin: { ...good, type: "evil" as never } } as Partial<TaskPackage>);
    expect(badType.some((i) => i.field === "origin.type" && i.severity === "error")).toBe(true);
    const zeroConsent = validatePackageContent({ origin: { ...good, consent_revision: 0 } } as Partial<TaskPackage>);
    expect(zeroConsent.some((i) => i.field === "origin.consent_revision")).toBe(true);
  });
});
