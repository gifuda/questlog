import { describe, it, expect } from "vitest";
import {
  scanForUnregisteredProgress,
  parseTranscript,
} from "../hooks/lib/progress-scan.mjs";

/**
 * Stop-hook scan logic tests. The scan is a pure function (transcript lines +
 * registry data + a readEpicsFile injection), so no fs/stdio mocking needed —
 * we pass in-memory data.
 */

const REGISTRY = {
  projects: [{ name: "proj-a", path: "/tmp/proj-a" }],
};

const EPICS = {
  version: 1,
  project: "proj-a",
  epics: [
    { id: "epic-1", title: "HC-01 权威日历批准", status: "active" },
    { id: "epic-2", title: "HC-04 消费者迁移", status: "draft" },
  ],
  steps: [
    { id: "step-1", epic_id: "epic-1", title: "S1 设计接口", stage: "designed" },
    { id: "step-2", epic_id: "epic-1", title: "S2 实现迁移", stage: "coded" },
    { id: "step-3", epic_id: "epic-1", title: "S3 验证闭环", stage: "observed" },
  ],
};

function readEpicsFile() {
  return Promise.resolve(EPICS);
}

function transcriptLine(text) {
  return JSON.stringify({ message: { content: [{ type: "text", text }] } });
}

function toolCallLine(name, input) {
  return JSON.stringify({ message: { content: [{ type: "tool_use", name, input }] } });
}

describe("parseTranscript", () => {
  it("collects update_step ids, push_step epics, push_epic titles, and text", () => {
    const lines = [
      toolCallLine("update_step", { id: "step-1", stage: "coded" }),
      toolCallLine("push_step", { epic_id: "epic-2", title: "X" }),
      toolCallLine("push_epic", { title: "HC-05 新方案" }),
      transcriptLine("代码写完了"),
    ];
    const r = parseTranscript(lines);
    expect(r.updatedIds.has("step-1")).toBe(true);
    expect(r.createdStepIds.has("epic-2")).toBe(true);
    expect(r.createdEpicTitles).toContain("HC-05 新方案");
    expect(r.texts.join("")).toContain("代码写完了");
  });
});

describe("scanForUnregisteredProgress", () => {
  it("flags a step mentioned as done but not registered (advance)", async () => {
    // step-2 is at stage=coded; "验证通过" is the coded→verified signal.
    const lines = [
      transcriptLine("S2 实现迁移 验证通过了"),
    ];
    const out = await scanForUnregisteredProgress({ transcriptLines: lines, registry: REGISTRY, readEpicsFile });
    expect(out).toHaveLength(1);
    expect(out[0]).toMatchObject({ kind: "advance", stepId: "step-2", toStage: "verified" });
  });

  it("does not flag a step already updated this session", async () => {
    const lines = [
      toolCallLine("update_step", { id: "step-1", stage: "coded" }),
      transcriptLine("S1 设计接口 设计定稿了"),
    ];
    const out = await scanForUnregisteredProgress({ transcriptLines: lines, registry: REGISTRY, readEpicsFile });
    expect(out).toHaveLength(0);
  });

  it("skips observed steps (nothing left to advance)", async () => {
    const lines = [
      transcriptLine("S3 验证闭环 稳定运行了"),
    ];
    const out = await scanForUnregisteredProgress({ transcriptLines: lines, registry: REGISTRY, readEpicsFile });
    expect(out).toHaveLength(0);
  });

  it("flags an epic with zero steps when work mentions completion (no-steps mode)", async () => {
    const emptyEpics = {
      version: 1,
      project: "proj-a",
      epics: [{ id: "epic-2", title: "HC-04 消费者迁移", status: "draft" }],
      steps: [],
    };
    const lines = [
      transcriptLine("HC-04 消费者迁移 Batch A 到 D 代码全部写完了，验证通过"),
    ];
    const out = await scanForUnregisteredProgress({
      transcriptLines: lines,
      registry: REGISTRY,
      readEpicsFile: () => Promise.resolve(emptyEpics),
    });
    expect(out).toHaveLength(1);
    expect(out[0]).toMatchObject({ kind: "no-steps", epicTitle: "HC-04 消费者迁移" });
  });

  it("does not flag no-steps epic if steps were created this session", async () => {
    const emptyEpics = {
      version: 1,
      project: "proj-a",
      epics: [{ id: "epic-2", title: "HC-04 消费者迁移", status: "draft" }],
      steps: [],
    };
    const lines = [
      toolCallLine("push_step", { epic_id: "epic-2", title: "S1" }),
      transcriptLine("HC-04 消费者迁移 代码写完了"),
    ];
    const out = await scanForUnregisteredProgress({
      transcriptLines: lines,
      registry: REGISTRY,
      readEpicsFile: () => Promise.resolve(emptyEpics),
    });
    expect(out).toHaveLength(0);
  });

  it("honors skip signals (user said don't register)", async () => {
    const lines = [
      transcriptLine("S1 设计接口 设计定稿了，但不用登记"),
    ];
    const out = await scanForUnregisteredProgress({ transcriptLines: lines, registry: REGISTRY, readEpicsFile });
    expect(out).toHaveLength(0);
  });

  it("ignores projects whose epics file is unreadable", async () => {
    const lines = [transcriptLine("S1 设计接口 设计定稿了")];
    const out = await scanForUnregisteredProgress({
      transcriptLines: lines,
      registry: REGISTRY,
      readEpicsFile: () => Promise.resolve(null),
    });
    expect(out).toHaveLength(0);
  });
});
