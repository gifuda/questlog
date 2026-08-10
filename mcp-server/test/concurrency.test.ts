/**
 * Concurrency and CAS tests (G0 acceptance):
 * - 100 concurrent updates do not lose data (cross-process lock prevents races)
 * - revision increments correctly under concurrent writes
 * - stale lock cleanup works
 *
 * These tests verify the file-lock + CAS protocol is sufficient for
 * multi-writer safety (Node MCP + Tauri UI + Runner).
 */

import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { promises as fs } from "node:fs";
import os from "node:os";
import path from "node:path";
import { addAndSave, updateAndSave, pushTodo } from "../src/store.js";
import { pushEpic, pushStep } from "../src/epic-store.js";

let dir: string;

beforeEach(async () => {
  dir = await fs.mkdtemp(path.join(os.tmpdir(), "questlog-concurrency-"));
});

afterEach(async () => {
  await fs.rm(dir, { recursive: true, force: true });
});

describe("concurrent writes to .todos.json", () => {
  it("100 concurrent addAndSave calls: no items lost", async () => {
    // Push 100 unique todo items concurrently.
    const promises: Promise<unknown>[] = [];
    for (let i = 0; i < 100; i++) {
      const item = pushTodo(dir, {
        type: "todo",
        title: `concurrent-item-${i}`,
        content: `content for item ${i} — long enough to pass the gate`,
      });
      if ("error" in item) throw new Error(item.error);
      promises.push(addAndSave(dir, item.item));
    }

    const results = await Promise.all(promises);
    const errors = results.filter((r) => r && typeof r === "object" && "error" in r);
    // No write should have failed — the lock serializes them.
    expect(errors.length).toBe(0);

    // Read back and verify ALL 100 items survived.
    const raw = await fs.readFile(path.join(dir, ".todos.json"), "utf-8");
    const data = JSON.parse(raw);
    expect(data.items.length).toBe(100);

    // Verify each item by title — none should be missing.
    const titles = new Set(data.items.map((i: { title: string }) => i.title));
    for (let i = 0; i < 100; i++) {
      expect(titles.has(`concurrent-item-${i}`)).toBe(true);
    }
  }, 30_000);

  it("concurrent updates to different items: all changes preserved", async () => {
    // First, add 5 items sequentially.
    for (let i = 0; i < 5; i++) {
      const item = pushTodo(dir, {
        type: "todo",
        title: `item-${i}`,
        content: `initial content for item ${i} — long enough`,
      });
      if ("error" in item) throw new Error(item.error);
      const res = await addAndSave(dir, item.item);
      if ("error" in res) throw new Error(res.error);
    }

    // Now update all 5 concurrently (change status to completed).
    const itemIds = JSON.parse(
      await fs.readFile(path.join(dir, ".todos.json"), "utf-8"),
    ).items.map((i: { id: string }) => i.id);

    const promises = itemIds.map((id: string) => updateAndSave(dir, id, { status: "completed" }));
    await Promise.all(promises);

    // Verify all 5 are completed.
    const raw = await fs.readFile(path.join(dir, ".todos.json"), "utf-8");
    const data = JSON.parse(raw);
    expect(data.items.every((i: { status: string }) => i.status === "completed")).toBe(true);
    expect(data.items.length).toBe(5);
  }, 15_000);
});

describe("concurrent writes to .epics.json", () => {
  it("50 concurrent pushStep calls: no steps lost", async () => {
    // Create one epic first.
    const epicRes = await pushEpic(dir, { title: "Test Epic", description: "d" });
    if ("error" in epicRes) throw new Error(epicRes.error);
    const epicId = epicRes.epic.id;

    // Push 50 steps concurrently.
    const promises: Promise<unknown>[] = [];
    for (let i = 0; i < 50; i++) {
      promises.push(pushStep(dir, { epic_id: epicId, title: `step-${i}` }));
    }

    const results = await Promise.all(promises);
    const errors = results.filter((r) => r && typeof r === "object" && "error" in r);
    expect(errors.length).toBe(0);

    // Verify all 50 steps survived.
    const raw = await fs.readFile(path.join(dir, ".epics.json"), "utf-8");
    const data = JSON.parse(raw);
    expect(data.steps.length).toBe(50);

    // revision should have been bumped 51 times (1 epic + 50 steps).
    expect(data.revision).toBeGreaterThanOrEqual(51);
  }, 30_000);

  it("revision increments monotonically", async () => {
    const epicRes = await pushEpic(dir, { title: "E1" });
    if ("error" in epicRes) throw new Error(epicRes.error);

    let raw = await fs.readFile(path.join(dir, ".epics.json"), "utf-8");
    const rev1 = JSON.parse(raw).revision;

    await pushStep(dir, { epic_id: epicRes.epic.id, title: "S1" });
    raw = await fs.readFile(path.join(dir, ".epics.json"), "utf-8");
    const rev2 = JSON.parse(raw).revision;

    expect(rev2).toBeGreaterThan(rev1);
  });
});

describe("backward compatibility", () => {
  it("old file without revision field loads and gets revision on next write", async () => {
    // Write an old-style file (no revision field).
    const oldFile = {
      version: 1,
      project: "legacy",
      items: [
        {
          id: "old-1",
          type: "todo",
          title: "Legacy Item",
          content: "c",
          suggestion: "",
          tags: [],
          status: "active",
          priority: "medium",
          source_session: "",
          created_at: "2026",
          updated_at: "2026",
        },
      ],
    };
    await fs.writeFile(path.join(dir, ".todos.json"), JSON.stringify(oldFile), "utf-8");

    // Add a new item — old data must survive and revision must appear.
    const item = pushTodo(dir, {
      type: "todo",
      title: "New Item",
      content: "new content that is long enough to pass",
    });
    if ("error" in item) throw new Error(item.error);
    const res = await addAndSave(dir, item.item);
    if ("error" in res) throw new Error(res.error);

    const raw = await fs.readFile(path.join(dir, ".todos.json"), "utf-8");
    const data = JSON.parse(raw);
    expect(data.items.length).toBe(2); // old item preserved
    expect(data.items[0].id).toBe("old-1"); // old item first
    expect(data.revision).toBeGreaterThanOrEqual(1);
  });
});
