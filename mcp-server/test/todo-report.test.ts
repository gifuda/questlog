/**
 * G3 acceptance tests (待办租户 plan §8 / §12 报告行):
 * The model's self-declared DONE is just a claim — verifyTodoReport is the
 * host-side gate. Counter-examples (echo DONE, bare marker, conflicting
 * blocks, truncation, fake references, missing coverage) must NEVER yield
 * accepted_report.
 */

import { describe, it, expect } from "vitest";
import { extractTodoReport, verifyTodoReport, REPORT_SCHEMA_VERSION, TodoReport } from "../src/todo-report.js";
import { validatePackageContent, TaskPackage } from "../src/task-package.js";

const KEY = "k".repeat(64);
const FILES = new Set(["src/lib.rs", "src/main.rs", "README.md"]);

function report(overrides: Partial<TodoReport> = {}): TodoReport {
  return {
    schema_version: REPORT_SCHEMA_VERSION,
    task_key: KEY,
    verdict: "DONE",
    summary: "对比完成：RDB 快但丢数据，AOF 安全但慢，混合模式均衡。",
    findings: ["RDB 快照粒度为 save 配置间隔", "AOF everysec 最多丢 1s"],
    evidence_refs: ["src/lib.rs:42", "README.md"],
    unresolved: [],
    limitations: ["未覆盖分布式场景"],
    ...overrides,
  };
}

function stdoutOf(r: TodoReport | string): string {
  const body = typeof r === "string" ? r : JSON.stringify(r, null, 2);
  return `正在分析…\n进度: 读取文件\n\`\`\`json\n${body}\n\`\`\`\n`;
}

const BASE = {
  stdout: stdoutOf(report()),
  task_key: KEY,
  snapshot_files: FILES,
  exit_ok: true,
  timed_out: false,
  budget_exceeded: false,
  changes_detected: false,
};

describe("extractTodoReport", () => {
  it("extracts the marked fenced block and ignores prose/echo", () => {
    const { report: r } = extractTodoReport(`TODO_VERDICT: DONE\n${BASE.stdout}别的输出`);
    expect(r?.verdict).toBe("DONE");
  });

  it("a bare DONE marker or echoed prompt is NOT a report", () => {
    expect(extractTodoReport("TODO_VERDICT: DONE\nall done.").report).toBeUndefined();
    expect(extractTodoReport("```json\n{\"verdict\":\"DONE\"}\n```").report).toBeUndefined();
  });

  it("identical duplicate blocks are tolerated; conflicting ones rejected", () => {
    const dup = BASE.stdout + BASE.stdout;
    expect(extractTodoReport(dup).report?.verdict).toBe("DONE");
    const other = stdoutOf(report({ verdict: "CANNOT_AUTO", cannot_auto_reason: "x" }));
    expect(extractTodoReport(BASE.stdout + other).report).toBeUndefined();
  });

  it("truncated JSON is rejected, not salvaged", () => {
    const truncated = "```json\n" + JSON.stringify(report()).slice(0, 40) + "\n```";
    expect(extractTodoReport(truncated).report).toBeUndefined();
  });
});

describe("verifyTodoReport", () => {
  it("a complete DONE report with real refs → accepted_report", () => {
    expect(verifyTodoReport(BASE).outcome).toBe("accepted_report");
  });

  it("execution preconditions outrank the report", () => {
    expect(verifyTodoReport({ ...BASE, timed_out: true }).outcome).toBe("rejected");
    expect(verifyTodoReport({ ...BASE, exit_ok: false }).outcome).toBe("rejected");
    expect(verifyTodoReport({ ...BASE, budget_exceeded: true }).outcome).toBe("rejected");
    expect(verifyTodoReport({ ...BASE, changes_detected: true }).outcome).toBe("rejected");
  });

  it("task binding: a report from another task is rejected", () => {
    const other = stdoutOf(report({ task_key: "x".repeat(64) }));
    expect(verifyTodoReport({ ...BASE, stdout: other }).outcome).toBe("rejected");
  });

  it("fake evidence references downgrade to needs_review, never accepted", () => {
    const fake = stdoutOf(report({ evidence_refs: ["src/ghost.rs:1"] }));
    const r = verifyTodoReport({ ...BASE, stdout: fake });
    expect(r.outcome).toBe("needs_review");
    expect(r.reasons[0]).toContain("not present in the approved snapshot");
  });

  it("DONE with unresolved items → needs_review", () => {
    const u = stdoutOf(report({ unresolved: ["缺少基准数据"] }));
    expect(verifyTodoReport({ ...BASE, stdout: u }).outcome).toBe("needs_review");
  });

  it("empty findings / empty summary → rejected (covers nothing)", () => {
    expect(verifyTodoReport({ ...BASE, stdout: stdoutOf(report({ findings: [] })) }).outcome).toBe("rejected");
    expect(verifyTodoReport({ ...BASE, stdout: stdoutOf(report({ summary: " " })) }).outcome).toBe("rejected");
  });

  it("CANNOT_AUTO is a business outcome — with reason ok, without reason rejected", () => {
    const ok = stdoutOf(report({ verdict: "CANNOT_AUTO", cannot_auto_reason: "需修改代码才能完成" }));
    const r = verifyTodoReport({ ...BASE, stdout: ok });
    expect(r.outcome).toBe("cannot_auto");
    const noReason = stdoutOf(report({ verdict: "CANNOT_AUTO" }));
    expect(verifyTodoReport({ ...BASE, stdout: noReason }).outcome).toBe("rejected");
  });

  it("NEEDS_REVIEW maps to needs_review", () => {
    const n = stdoutOf(report({ verdict: "NEEDS_REVIEW" }));
    expect(verifyTodoReport({ ...BASE, stdout: n }).outcome).toBe("needs_review");
  });
});

describe("strict read-only package policy (validatePackageContent)", () => {
  const base = {
    objective: "o",
    epic_id: "e",
    step_id: "s",
    executor: { preferred: "manual" as const, fallbacks: [], sandbox: "read-only" as const, timeout_seconds: 900, max_attempts: 1 },
    acceptance: [
      { type: "command" as const, command_id: "verify:readonly" },
      { type: "verifier" as const, verifier_id: "todo:report" },
    ],
  };
  const strictScope = { allowed_paths: [], create_paths: [], forbidden_paths: [], read_paths: ["**"], max_changed_files: 0 };
  const noAuthority = { auto_edit_worktree: false, auto_commit_candidate: false, auto_merge: false, auto_push: false, auto_deploy: false };

  it("strict shape passes with zero write rights", () => {
    const issues = validatePackageContent({ ...base, scope: strictScope, authority: noAuthority } as Partial<TaskPackage>);
    expect(issues.filter((i) => i.severity === "error")).toEqual([]);
  });

  it("write-semantics fields on a strict package are errors", () => {
    const bad = validatePackageContent({
      ...base,
      scope: { ...strictScope, allowed_paths: ["src/**"] },
      authority: noAuthority,
    } as Partial<TaskPackage>);
    expect(bad.some((i) => i.field === "scope.allowed_paths" && i.severity === "error")).toBe(true);
  });

  it("missing read_paths on a strict package is an error", () => {
    const bad = validatePackageContent({
      ...base,
      scope: { ...strictScope, read_paths: [] },
      authority: noAuthority,
    } as Partial<TaskPackage>);
    expect(bad.some((i) => i.field === "scope.read_paths" && i.severity === "error")).toBe(true);
  });

  it("unknown verifier ids refuse to seal", () => {
    const bad = validatePackageContent({
      ...base,
      scope: strictScope,
      authority: noAuthority,
      acceptance: [{ type: "verifier", verifier_id: "evil:verifier" }],
    } as Partial<TaskPackage>);
    expect(bad.some((i) => i.field === "acceptance.verifier_id" && i.severity === "error")).toBe(true);
  });

  it("LEGACY read-only shape (default authority, max ≥ 1) keeps passing — regression", () => {
    const legacy = validatePackageContent({
      ...base,
      scope: { allowed_paths: ["docs/**"], create_paths: [], forbidden_paths: [], max_changed_files: 1 },
      authority: { auto_edit_worktree: true, auto_commit_candidate: true, auto_merge: false, auto_push: false, auto_deploy: false },
    } as Partial<TaskPackage>);
    expect(legacy.filter((i) => i.severity === "error")).toEqual([]);
  });
});
