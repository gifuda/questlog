/**
 * G1 acceptance tests (待办租户 plan, 数据契约与安全写回底座):
 * - auto_process opt-in contract: push/update round-trip, revision bump on
 *   off→on, never on knowhow/idea
 * - unknown fields survive MCP writes (forward compatibility with the Rust
 *   side's raw-JSON patching)
 * - owner-safe lock: live owner blocks (no age-based steal), dead owner
 *   reclaimed (v2 JSON + legacy format), empty stale lock reclaimed,
 *   unparseable lock never stolen
 */

import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { promises as fs } from "node:fs";
import { spawn } from "node:child_process";
import os from "node:os";
import path from "node:path";
import { addAndSave, updateAndSave, pushTodo } from "../src/store.js";
import { withFileLock } from "../src/file-lock.js";

let fakeHome = "";

vi.mock("node:os", async (importOriginal) => {
  const real = await importOriginal<typeof import("node:os")>();
  return {
    ...real,
    homedir: () => fakeHome,
  };
});

let dir: string;

beforeEach(async () => {
  dir = await fs.mkdtemp(path.join(os.tmpdir(), "questlog-g1-"));
  fakeHome = dir;
});

afterEach(async () => {
  await fs.rm(dir, { recursive: true, force: true });
});

function makeItem(overrides: Partial<Parameters<typeof pushTodo>[1]> = {}) {
  const r = pushTodo(dir, {
    type: "todo",
    title: "研究 Redis 缓存策略",
    content: "目标：对比主流缓存策略。验收标准：输出对比报告，覆盖延迟/内存/淘汰策略三个维度。",
    ...overrides,
  });
  if ("error" in r) throw new Error(r.error);
  return r.item;
}

describe("auto_process opt-in contract", () => {
  it("push with auto_process=true carries revision 1", async () => {
    const item = makeItem({ auto_process: true });
    expect(item.auto_process).toBe(true);
    expect(item.auto_process_revision).toBe(1);
    await addAndSave(dir, item);
    const raw = JSON.parse(await fs.readFile(path.join(dir, ".todos.json"), "utf-8"));
    expect(raw.items[0].auto_process).toBe(true);
    expect(raw.items[0].auto_process_revision).toBe(1);
  });

  it("push without auto_process omits the field entirely", async () => {
    const item = makeItem();
    expect(item.auto_process).toBeUndefined();
    await addAndSave(dir, item);
    const raw = JSON.parse(await fs.readFile(path.join(dir, ".todos.json"), "utf-8"));
    expect(raw.items[0].auto_process).toBeUndefined();
    expect(raw.items[0].auto_process_revision).toBeUndefined();
  });

  it("opt-in rejected on knowhow and idea items", () => {
    const kh = pushTodo(dir, {
      type: "knowhow",
      title: "Redis 缓存知识",
      content: "背景：缓存场景。要点：三条。边界：不适用于强一致。",
      auto_process: true,
    });
    expect("error" in kh).toBe(true);
    const idea = pushTodo(dir, {
      type: "idea",
      title: "缓存优化假设",
      content: "痛点：延迟高。假设：换策略。收益：更快。验证：基准测试。",
      auto_process: true,
    });
    expect("error" in idea).toBe(true);
  });

  it("off→on bumps revision; on→on and on→off do not", async () => {
    const item = makeItem({ auto_process: true });
    await addAndSave(dir, item);
    const id = item.id;

    // on → on: no bump (still 1)
    await updateAndSave(dir, id, { auto_process: true });
    let raw = JSON.parse(await fs.readFile(path.join(dir, ".todos.json"), "utf-8"));
    expect(raw.items[0].auto_process_revision).toBe(1);

    // on → off: no bump
    await updateAndSave(dir, id, { auto_process: false });
    raw = JSON.parse(await fs.readFile(path.join(dir, ".todos.json"), "utf-8"));
    expect(raw.items[0].auto_process).toBe(false);
    expect(raw.items[0].auto_process_revision).toBe(1);

    // off → on: bump (fresh authorization — old results unusable)
    await updateAndSave(dir, id, { auto_process: true });
    raw = JSON.parse(await fs.readFile(path.join(dir, ".todos.json"), "utf-8"));
    expect(raw.items[0].auto_process_revision).toBe(2);
  });

  it("explicit auto_process_revision only moves forward", async () => {
    const item = makeItem({ auto_process: true });
    await addAndSave(dir, item);
    // Attempt to roll back to 0 — must stay at 1.
    await updateAndSave(dir, item.id, { auto_process_revision: 0 });
    const raw = JSON.parse(await fs.readFile(path.join(dir, ".todos.json"), "utf-8"));
    expect(raw.items[0].auto_process_revision).toBe(1);
  });

  it("off→on with higher explicit revision keeps the higher value", async () => {
    const item = makeItem();
    await addAndSave(dir, item);
    // Bump path sets revision 1, then the explicit 5 applies via max().
    await updateAndSave(dir, item.id, { auto_process: true, auto_process_revision: 5 });
    let raw = JSON.parse(await fs.readFile(path.join(dir, ".todos.json"), "utf-8"));
    expect(raw.items[0].auto_process).toBe(true);
    expect(raw.items[0].auto_process_revision).toBe(5);
    // Rolling back explicitly is refused.
    await updateAndSave(dir, item.id, { auto_process_revision: 1 });
    raw = JSON.parse(await fs.readFile(path.join(dir, ".todos.json"), "utf-8"));
    expect(raw.items[0].auto_process_revision).toBe(5);
  });

  it("off→on with lower explicit revision still bumps to 1", async () => {
    const item = makeItem();
    await addAndSave(dir, item);
    await updateAndSave(dir, item.id, { auto_process: true, auto_process_revision: 0 });
    const raw = JSON.parse(await fs.readFile(path.join(dir, ".todos.json"), "utf-8"));
    expect(raw.items[0].auto_process_revision).toBe(1);
  });
});

describe("unknown field preservation", () => {
  it("top-level and item-level unknown keys survive MCP writes", async () => {
    const item = makeItem();
    await addAndSave(dir, item);

    // Simulate a future/Rust-side writer having added unknown keys.
    const file = path.join(dir, ".todos.json");
    const withUnknown = JSON.parse(await fs.readFile(file, "utf-8"));
    withUnknown.future_top_level = { nested: true };
    withUnknown.items[0].future_item_key = "keep-me";
    await fs.writeFile(file, JSON.stringify(withUnknown, null, 2));

    const r = await updateAndSave(dir, item.id, { status: "completed" });
    expect("error" in r).toBe(false);

    const after = JSON.parse(await fs.readFile(file, "utf-8"));
    expect(after.future_top_level.nested).toBe(true);
    expect(after.items[0].future_item_key).toBe("keep-me");
    expect(after.items[0].status).toBe("completed");
  });
});

/** Resolve once a child exits — yields a pid that is (recently) dead. */
async function spawnAndWaitExit(cmd: string): Promise<number> {
  return await new Promise((resolve) => {
    const child = spawn(cmd, []);
    child.on("exit", () => resolve(child.pid!));
  });
}

describe("owner-safe lock", () => {
  it("live owner blocks acquisition — no age-based steal", async () => {
    const file = path.join(dir, "live.json");
    const lockPath = file + ".lock";
    const child = spawn("sleep", ["5"]);
    await fs.writeFile(
      lockPath,
      JSON.stringify({ v: 2, pid: child.pid, token: "t", created_ms: 0 }),
    );
    await expect(withFileLock(file, async () => 1, 400)).rejects.toThrow(/live owner|timeout/);
    expect((await fs.stat(lockPath)).isFile()).toBe(true);
    child.kill();
    await new Promise((r) => child.on("exit", r));
  }, 10_000);

  it("v2 lock with dead owner is reclaimed", async () => {
    const file = path.join(dir, "dead-v2.json");
    const deadPid = await spawnAndWaitExit("true");
    await fs.writeFile(
      file + ".lock",
      JSON.stringify({ v: 2, pid: deadPid, token: "t", created_ms: 0 }),
    );
    const out = await withFileLock(file, async () => 42);
    expect(out).toBe(42);
  });

  it("legacy pid-uuid lock with dead owner is reclaimed", async () => {
    const file = path.join(dir, "dead-legacy.json");
    const deadPid = await spawnAndWaitExit("true");
    await fs.writeFile(file + ".lock", `${deadPid}-some-uuid`);
    const out = await withFileLock(file, async () => 42);
    expect(out).toBe(42);
  });

  it("empty stale lock is reclaimed", async () => {
    const file = path.join(dir, "empty.json");
    const lockPath = file + ".lock";
    await fs.writeFile(lockPath, "");
    const old = new Date(Date.now() - 30_000);
    await fs.utimes(lockPath, old, old);
    const out = await withFileLock(file, async () => 42);
    expect(out).toBe(42);
  });

  it("unparseable lock is never stolen, even when old", async () => {
    const file = path.join(dir, "garbage.json");
    const lockPath = file + ".lock";
    await fs.writeFile(lockPath, "garbage-not-a-pid");
    const old = new Date(Date.now() - 30_000);
    await fs.utimes(lockPath, old, old);
    await expect(withFileLock(file, async () => 1, 400)).rejects.toThrow(/blocked/);
    expect((await fs.stat(lockPath)).isFile()).toBe(true);
  });
});
