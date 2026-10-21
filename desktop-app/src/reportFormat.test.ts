import { describe, expect, it } from "vitest";
import { parseReport, verdictView } from "./reportFormat";

const sample = [
  "调研完成。以下是结论与报告。",
  "",
  "## 结论：无法自动完成",
  "",
  "两个独立硬阻断：**W131 评价窗未满**、keep/revert 属 D2/A3。",
  "",
  "| 口径 | Route C |",
  "|---|---|",
  "| 现行 | 0.724 |",
  "",
  "```json",
  JSON.stringify({
    schema_version: "questlog-todo-report",
    verdict: "CANNOT_AUTO",
    summary: "一句话结论。",
    findings: ["发现一", 42],
    unresolved: ["窗满后重跑", "用户裁决未发生"],
    evidence_refs: ["a.py:1"],
  }),
  "```",
].join("\n");

describe("parseReport", () => {
  it("提取末尾 JSON 围栏为结构化数据，正文去掉围栏", () => {
    const r = parseReport(sample);
    expect(r.structured?.verdict).toBe("CANNOT_AUTO");
    expect(r.structured?.summary).toBe("一句话结论。");
    expect(r.structured?.unresolved).toEqual(["窗满后重跑", "用户裁决未发生"]);
    // 非 string 项被过滤
    expect(r.structured?.findings).toEqual(["发现一"]);
    expect(r.body).toContain("两个独立硬阻断");
    expect(r.body).not.toContain("```json");
    expect(r.body).not.toContain("schema_version");
  });

  it("无围栏时 structured 为 null，正文原样", () => {
    const r = parseReport("# 只有正文\n没有围栏");
    expect(r.structured).toBeNull();
    expect(r.body).toContain("只有正文");
  });

  it("围栏 JSON 损坏时 fail-open：正文保留、structured 置空", () => {
    const r = parseReport("正文\n```json\n{broken\n```\n");
    expect(r.structured).toBeNull();
    expect(r.body).toContain("正文");
    expect(r.body).not.toContain("```");
  });

  it("取最后一个围栏（正文里嵌代码块不干扰）", () => {
    const raw = "```json\n{\"irrelevant\":true}\n```\n中间\n```json\n{\"verdict\":\"OK\"}\n```";
    const r = parseReport(raw);
    expect(r.structured?.verdict).toBe("OK");
    expect(r.body).toContain("中间");
  });
});

describe("verdictView", () => {
  it("已知 verdict 映射文案与色调", () => {
    expect(verdictView("CANNOT_AUTO")).toEqual({ label: "无法自动处理", tone: "amber" });
    expect(verdictView("PASS")?.tone).toBe("success");
  });
  it("未知 verdict 原样透出，不猜", () => {
    expect(verdictView("WEIRD")).toEqual({ label: "WEIRD", tone: "muted" });
    expect(verdictView(undefined)).toBeNull();
  });
});
