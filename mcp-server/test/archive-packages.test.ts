/**
 * P2-2 archival tests: partition criteria (each criterion's negative keeps),
 * archive merge idempotency, crash-safe ordering effects, dry-run purity,
 * and end-to-end file mutation on an isolated temp runtime.
 */
import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { promises as fs, mkdtempSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { partitionForArchive, archivePackages } from "../src/archive-packages.js";
import type { TaskPackage } from "../src/task-package.js";

let runtimeDir = "";
let prevRuntime: string | undefined;

function pkg(over: Partial<TaskPackage> & { package_id: string }): TaskPackage {
  return {
    schema_version: "questlog-task-package/v1",
    epic_id: "e1",
    step_id: "s1",
    plan_revision: 1,
    status: "superseded",
    sealed_at: "2026-08-01T00:00:00Z",
    project_path: "/tmp/p",
    repo_root: "/tmp/p",
    base_sha: "abc",
    objective: "test",
    scope: { allowed_paths: ["src/**"], forbidden_paths: [], max_changed_files: 4 },
    executor: { preferred: "zcode", fallbacks: [], sandbox: "workspace-write", timeout_seconds: 60, max_attempts: 1 },
    acceptance: [],
    rollback: { strategy: "preserve_worktree_and_stop" },
    package_hash: "h",
    ...over,
  } as TaskPackage;
}

const NOW = Date.parse("2026-10-15T12:00:00Z");

describe("partitionForArchive criteria", () => {
  const empty = new Set<string>();
  it("archives terminal + unreferenced + non-latest + old", () => {
    // a newer revision sibling exists, so "a" is not the step's max revision
    const { archive, keep } = partitionForArchive(
      [pkg({ package_id: "a", plan_revision: 1 }), pkg({ package_id: "a2", plan_revision: 2, status: "sealed", sealed_at: "2026-10-14T00:00:00Z" })],
      empty,
      NOW,
      30,
    );
    expect(archive.map((p) => p.package_id)).toEqual(["a"]);
    expect(keep.map((p) => p.package_id)).toEqual(["a2"]);
  });
  it("keeps non-terminal (sealed/blocked/draft)", () => {
    for (const status of ["sealed", "blocked", "draft", "ready"] as const) {
      const { archive } = partitionForArchive([pkg({ package_id: "a", status })], empty, NOW, 30);
      expect(archive).toHaveLength(0);
    }
  });
  it("keeps anything referenced by executions", () => {
    const { archive } = partitionForArchive([pkg({ package_id: "a" })], new Set(["a"]), NOW, 30);
    expect(archive).toHaveLength(0);
  });
  it("keeps the latest revision of its step even when terminal+old", () => {
    // single package for the step = max revision → kept
    const { archive } = partitionForArchive([pkg({ package_id: "a", status: "cancelled" })], empty, NOW, 30);
    expect(archive).toHaveLength(0);
    // superseded old + newer revision present → old one archivable
    const r = partitionForArchive(
      [pkg({ package_id: "old", plan_revision: 1 }), pkg({ package_id: "new", plan_revision: 2, status: "passed", sealed_at: "2026-10-14T00:00:00Z" })],
      empty,
      NOW,
      30,
    );
    expect(r.archive.map((p) => p.package_id)).toEqual(["old"]);
  });
  it("age window: fresh terminal LATEST kept, old non-latest still archives", () => {
    const r = partitionForArchive(
      [pkg({ package_id: "a", plan_revision: 1 }), pkg({ package_id: "b", plan_revision: 2, status: "cancelled", sealed_at: "2026-10-10T00:00:00Z" })],
      empty,
      NOW,
      30,
    );
    expect(r.archive.map((p) => p.package_id)).toEqual(["a"]); // old + non-latest
    expect(r.keep.map((p) => p.package_id)).toEqual(["b"]); // fresh (and latest)
  });
  it("unknown sealed_at never archives (conservative)", () => {
    const p = pkg({ package_id: "a", plan_revision: 1, sealed_at: undefined as unknown as string });
    const p2 = pkg({ package_id: "b", plan_revision: 2, status: "passed" });
    const { archive } = partitionForArchive([p, p2], empty, NOW, 30);
    expect(archive.map((x) => x.package_id)).toEqual([]);
  });
  it("keeps rows carrying origin.task_key (todo-producer dedup backstop)", () => {
    const withOrigin = pkg({
      package_id: "a",
      plan_revision: 1,
      origin: { task_key: "k1" },
    } as Partial<TaskPackage> & { package_id: string });
    const sibling = pkg({ package_id: "b", plan_revision: 2, status: "sealed", sealed_at: "2026-10-14T00:00:00Z" });
    const { archive } = partitionForArchive([withOrigin, sibling], empty, NOW, 30);
    expect(archive).toHaveLength(0);
  });
});

describe("archivePackages end-to-end (isolated runtime)", () => {
  beforeEach(() => {
    runtimeDir = mkdtempSync(path.join(os.tmpdir(), "ql-archive-test-"));
    prevRuntime = process.env.QUESTLOG_RUNTIME;
    process.env.QUESTLOG_RUNTIME = runtimeDir;
  });
  afterEach(async () => {
    if (prevRuntime === undefined) delete process.env.QUESTLOG_RUNTIME;
    else process.env.QUESTLOG_RUNTIME = prevRuntime;
    await fs.rm(runtimeDir, { recursive: true, force: true });
  });

  async function seed() {
    const old = pkg({ package_id: "old-1", plan_revision: 1 });
    const cur = pkg({ package_id: "cur-1", plan_revision: 2, status: "sealed", sealed_at: "2026-10-14T00:00:00Z" });
    await fs.writeFile(
      path.join(runtimeDir, "packages.json"),
      JSON.stringify({ version: 1, packages: [old, cur], revision: 7 }),
    );
    await fs.writeFile(
      path.join(runtimeDir, "executions.json"),
      JSON.stringify({ executions: [{ id: "x", package_id: "cur-1" }] }),
    );
    return { old, cur };
  }

  it("dry-run mutates nothing", async () => {
    await seed();
    const res = await archivePackages({ dryRun: true, minAgeDays: 30 });
    expect(res.archived).toBe(1);
    const live = JSON.parse(await fs.readFile(path.join(runtimeDir, "packages.json"), "utf-8"));
    expect(live.packages).toHaveLength(2);
    expect(live.revision).toBe(7);
    await expect(fs.readFile(path.join(runtimeDir, "archive", "packages-archive.json"), "utf-8")).rejects.toThrow();
  });

  it("archives once, bumps revision, merges idempotently", async () => {
    await seed();
    const res = await archivePackages({ minAgeDays: 30 });
    expect(res).toMatchObject({ archived: 1, kept: 1 });
    const live = JSON.parse(await fs.readFile(path.join(runtimeDir, "packages.json"), "utf-8"));
    expect(live.packages.map((p: { package_id: string }) => p.package_id)).toEqual(["cur-1"]);
    expect(live.revision).toBe(8);
    const arc = JSON.parse(await fs.readFile(path.join(runtimeDir, "archive", "packages-archive.json"), "utf-8"));
    expect(arc.packages.map((p: { package_id: string }) => p.package_id)).toEqual(["old-1"]);
    // second run: nothing left to archive, archive unchanged
    const res2 = await archivePackages({ minAgeDays: 30 });
    expect(res2).toMatchObject({ archived: 0, kept: 1 });
  });

  it("refuses to archive when executions.json is unreadable", async () => {
    await seed();
    await fs.writeFile(path.join(runtimeDir, "executions.json"), "{corrupt");
    await expect(archivePackages({ minAgeDays: 30 })).rejects.toThrow(/executions.json unreadable/);
    const live = JSON.parse(await fs.readFile(path.join(runtimeDir, "packages.json"), "utf-8"));
    expect(live.packages).toHaveLength(2);
  });

  it("BLOCK-1: corrupt archive fails loud and preserves both files", async () => {
    await seed();
    const arcDir = path.join(runtimeDir, "archive");
    await fs.mkdir(arcDir, { recursive: true });
    await fs.writeFile(path.join(arcDir, "packages-archive.json"), "{corrupt");
    await expect(archivePackages({ minAgeDays: 30 })).rejects.toThrow(/packages-archive.json corrupt/);
    // neither the archive nor the live file was touched
    expect(await fs.readFile(path.join(arcDir, "packages-archive.json"), "utf-8")).toBe("{corrupt");
    const live = JSON.parse(await fs.readFile(path.join(runtimeDir, "packages.json"), "utf-8"));
    expect(live.packages).toHaveLength(2);
    expect(live.revision).toBe(7);
  });

  it("BLOCK-1: wrong-shape archive (packages not an array) fails loud", async () => {
    await seed();
    const arcDir = path.join(runtimeDir, "archive");
    await fs.mkdir(arcDir, { recursive: true });
    await fs.writeFile(path.join(arcDir, "packages-archive.json"), "[]");
    await expect(archivePackages({ minAgeDays: 30 })).rejects.toThrow(/wrong shape/);
  });

  it("crash window: archive written but main untrimmed → rerun merges idempotently", async () => {
    // simulate the crash-between-steps state: archive already has old-1,
    // main file still contains it too
    await seed();
    const arcDir = path.join(runtimeDir, "archive");
    await fs.mkdir(arcDir, { recursive: true });
    await fs.writeFile(
      path.join(arcDir, "packages-archive.json"),
      JSON.stringify({ version: 1, packages: [pkg({ package_id: "old-1", plan_revision: 1 })], revision: 1 }),
    );
    const res = await archivePackages({ minAgeDays: 30 });
    expect(res).toMatchObject({ archived: 1, kept: 1 });
    const arc = JSON.parse(await fs.readFile(path.join(arcDir, "packages-archive.json"), "utf-8"));
    const ids = arc.packages.map((p: { package_id: string }) => p.package_id).sort();
    expect(ids).toEqual(["old-1"]); // deduped, not duplicated
  });
});
