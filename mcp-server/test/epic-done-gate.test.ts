import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { promises as fs } from "node:fs";
import os from "node:os";
import path from "node:path";
import { pushEpic, updateEpic, pushStep, updateStep } from "../src/epic-store.js";

/**
 * done-gate tests: an epic with progress < 100% cannot be set to "done"
 * unless close_reason is provided. This prevents the "75% but shown as done"
 * bug. Progress = 100% passes freely.
 *
 * These tests hit the disk (updateEpic writes .epics.json), so each gets a
 * fresh temp dir — mirroring store-integrity.test.ts's pattern.
 */

let dir: string;

beforeEach(async () => {
  dir = await fs.mkdtemp(path.join(os.tmpdir(), "questlog-done-gate-"));
});

afterEach(async () => {
  await fs.rm(dir, { recursive: true, force: true });
});

describe("updateEpic done-gate", () => {
  it("rejects done when progress < 100% and close_reason is empty", async () => {
    const epic = await pushEpic(dir, { title: "E1" });
    if ("error" in epic) throw new Error(epic.error);
    await pushStep(dir, { epic_id: epic.epic.id, title: "S1" }); // pending → progress 0%

    const res = await updateEpic(dir, epic.epic.id, { status: "done" });
    expect("error" in res).toBe(true);
    if ("error" in res) {
      expect(res.error).toMatch(/100%/);
      expect(res.error).toMatch(/close_reason/);
    }
  });

  it("allows done when progress < 100% AND close_reason is provided", async () => {
    const epic = await pushEpic(dir, { title: "E1" });
    if ("error" in epic) throw new Error(epic.error);
    await pushStep(dir, { epic_id: epic.epic.id, title: "S1" }); // progress 0%

    const res = await updateEpic(dir, epic.epic.id, {
      status: "done",
      close_reason: "方向调整，合并到方案 B",
    });
    expect("ok" in res).toBe(true);
  });

  it("allows done freely when progress = 100% (all steps observed)", async () => {
    const epic = await pushEpic(dir, { title: "E1" });
    if ("error" in epic) throw new Error(epic.error);
    const step = await pushStep(dir, { epic_id: epic.epic.id, title: "S1" });
    if ("error" in step) throw new Error(step.error);
    await updateStep(dir, step.step.id, { stage: "observed" }); // progress → 100%

    const res = await updateEpic(dir, epic.epic.id, { status: "done" });
    expect("ok" in res).toBe(true);
  });

  it("does not gate non-done status changes", async () => {
    const epic = await pushEpic(dir, { title: "E1" });
    if ("error" in epic) throw new Error(epic.error);
    await pushStep(dir, { epic_id: epic.epic.id, title: "S1" }); // progress 0%

    // active / verifying / observing should all pass without close_reason
    for (const status of ["active", "verifying", "observing"] as const) {
      const res = await updateEpic(dir, epic.epic.id, { status });
      expect("ok" in res).toBe(true);
    }
  });

  it("trims close_reason before checking emptiness", async () => {
    const epic = await pushEpic(dir, { title: "E1" });
    if ("error" in epic) throw new Error(epic.error);
    await pushStep(dir, { epic_id: epic.epic.id, title: "S1" });

    // whitespace-only reason should be treated as empty
    const res = await updateEpic(dir, epic.epic.id, {
      status: "done",
      close_reason: "   ",
    });
    expect("error" in res).toBe(true);
  });
});
