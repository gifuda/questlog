/**
 * publish_plan — the single external plan-publishing entry (S2).
 *
 * A thin MCP wrapper around the S1 core (publishPlan): the zod schema is a
 * 1:1 structural mirror of PlanSpec, and ALL plan semantics (enum values,
 * registry membership, dependency shape, gap detection, packaging, rollback)
 * stay in plan-publisher.ts — nothing is validated or packaged here twice.
 * On success the core registers the Epic + Steps and seals one standard
 * TaskPackage per step; nothing is ever enqueued and no switch is opened.
 */

import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { z } from "zod";
import {
  publishPlan,
  type PlanSpec,
  type PublishPlanResult,
} from "./plan-publisher.js";
import {
  canonicalizeProjectDir,
  resolveRegisteredProject,
} from "./store.js";

/**
 * Resolve the effective project directory for publish_plan. Mirrors index.ts
 * epicCwd for epic-registration tools (explicit validated project_dir, else
 * registered-project routing with cwd fallback); restated here so this module
 * does not import index.ts (which registers it — a cycle, and its stdio
 * side effect would leak into tests). Routing semantics live in store.ts.
 */
async function resolveToolCwd(
  input: { project_dir?: string },
): Promise<{ cwd: string } | { error: string }> {
  if (input.project_dir) {
    const canonical = await canonicalizeProjectDir(input.project_dir, process.cwd());
    if (!canonical) {
      return { error: `project_dir 非法或被拒绝（需为当前工作区子目录或已注册项目）: ${input.project_dir}` };
    }
    return { cwd: canonical };
  }
  const routed = await resolveRegisteredProject(process.cwd());
  return { cwd: routed ?? process.cwd() };
}

// ─── PlanSpec structural schema ───
//
// Types mirror the TS PlanSpec only. Enum-ish fields (priority, executor
// kinds, rollback strategy) are plain strings HERE on purpose: their value
// rules belong to validatePlanSpec in the core, so a bad value surfaces as
// the contract's `invalid` (field + fix item) instead of a protocol-layer
// zod error. The cast below is safe because the core re-validates everything.

const authoritySchema = z.object({
  auto_edit_worktree: z.boolean().optional(),
  auto_commit_candidate: z.boolean().optional(),
  auto_merge: z.boolean().optional(),
  auto_push: z.boolean().optional(),
  auto_deploy: z.boolean().optional(),
});

const executorSchema = z.object({
  preferred: z.string().optional().describe("'zcode' | 'manual' (codex is design/push-only — rejected as invalid)"),
  sandbox: z.string().optional().describe("'workspace-write' | 'read-only'"),
  timeout_seconds: z.number().optional().describe("≥ 60"),
  max_attempts: z.number().optional().describe("≥ 1"),
});

const stepSchema = z.object({
  title: z.string().describe("Step title, concise and action-oriented"),
  description: z.string().optional().describe("What this step covers"),
  objective: z.string().describe("Single, verifiable objective — what success looks like"),
  depends_on: z.array(z.number()).optional().describe("0-based indices of EARLIER steps this step depends on (backward only)"),
  allowed_paths: z.array(z.string()).describe("Glob patterns for paths the executor MAY modify"),
  forbidden_paths: z.array(z.string()).optional().describe("Glob patterns the executor MUST NOT touch"),
  max_changed_files: z.number().optional().describe("Maximum files the executor may change (default 10)"),
  context_refs: z.array(z.string()).optional().describe("Reference docs/paths for context"),
  acceptance_command_ids: z.array(z.string()).describe("Trusted command IDs from the acceptance registry (e.g. lint:mcp, test:mcp, build:mcp, cargo:check) — see docs/acceptance-commands.md"),
  authority: authoritySchema.optional().describe("Requested authority deltas over the conservative default; merge/push/deploy=true is a needs_decision gap"),
  rollback: z.object({ strategy: z.string() }).optional().describe("'preserve_worktree_and_stop' | 'abort_and_clean'"),
  executor: executorSchema.optional(),
});

// Top level must be a RAW SHAPE (field → zod type), not z.object — the
// McpServer.tool legacy overload only accepts raw shapes (see index.ts).
const planShape = {
  title: z.string().describe("Plan title — same rules as push_epic (pure title, no priority prefix)"),
  description: z.string().optional().describe("One line: what problem does this plan solve?"),
  priority: z.string().optional().describe("'P0' | 'P1' | 'P2' | 'P3'"),
  acceptance: z.string().optional().describe("验收条件，自由文本（观察期约定写这里）"),
  goal: z.string().optional().describe("The goal this plan achieves — completed in-conversation before calling"),
  baseline: z.string().optional().describe("Current state the goal is measured against"),
  chain: z.object({
    upstream: z.array(z.string()).optional().describe("Upstream modules/data this plan builds on"),
    change: z.array(z.string()).optional().describe("The files/surfaces this plan changes"),
    consumers: z.array(z.string()).optional().describe("Downstream consumers of the changed surfaces"),
    consumers_confirmed: z.boolean().optional().describe("true only after a human confirmed the consumer list in the source conversation"),
    risks: z.array(z.string()).optional().describe("Chain-level risks"),
  }).optional(),
  review: z.object({
    major_risks: z.array(z.string()).optional().describe("Counter-example review: main risks found"),
    resolutions: z.array(z.string()).optional().describe("resolutions[i] addresses major_risks[i] — parallel arrays, every risk needs one"),
  }).optional(),
  steps: z.array(stepSchema).describe("Ordered steps; created in listed order, depends_on may only point backwards"),
  project_dir: z.string().optional().describe("Override auto-detection: absolute path of the target project directory (server validates)"),
};

const DESCRIPTION = [
  "Publish a fully-reviewed plan (PlanSpec) atomically: ONE call creates the Epic, all its Steps, and one sealed standard TaskPackage per step. This is the ONLY external publishing entry — prepare/validate/seal_task_package remain as internal reuse tools; do not hand-assemble packages for new plans.",
  "",
  "## Before calling (CRITICAL — finish ALL of this in the source conversation FIRST)",
  "目标（goal）、完整链路（chain：upstream / change / consumers）、依赖（steps[].depends_on，只准向后）、权限（authority：保守包络内才可自动）、验收（acceptance_command_ids：受信注册表内的命令）、回滚（rollback 策略）、主要反例审查（review：major_risks ↔ resolutions 逐条配对）。",
  "Consumers listed without consumers_confirmed、风险无对策、权限超出保守包络（auto_merge/auto_push/auto_deploy）、无依赖却写范围重叠的步骤 → needs_decision：什么都不创建，输出可直接回到对话回答的最小问题。",
  "",
  "## On success",
  "系统自动登记 Epic + Steps 并为每步封存标准包（sealed）。包只封存、绝不入队；本工具不打开任何开关。无包或未经本工具发布的方案永不由 Runner 执行。",
  "",
  "## Return contract — strictly three states",
  "- published: epic_id + step_ids + package_ids（index 对齐）。下一步是人工开关授权：总闸(global_enabled) + 项目开关 + Epic auto_execute 全部打开后 Runner 才会在下次轮询自动领取——不需要 enqueue_step。",
  "- needs_decision: minimal questions answerable directly back in the source conversation; 回答后调整 spec 重发。Nothing was created.",
  "- invalid: deterministic fix items (field + message) plus a rollback report — 零可执行半成品（半途失败会取消已封包并删除 Epic）。修复后重发。",
  "",
  "## Field notes",
  "- Server resolves project path, git root, and base SHA itself — never send paths/SHAs to fake them.",
  "- Duplicate title of an existing epic → invalid (explicit rejection; the original stays untouched).",
].join("\n");

// ─── Result rendering (three-state contract) ───

const PUBLISHED_NEXT_STEP =
  "Manual switch authorization: turn ON the master switch (runner-config global_enabled), the project switch, AND the epic auto_execute switch — publish_plan never opens any switch. Once all three are on, the Runner claims the sealed packages on its next poll automatically (no enqueue_step needed).";

function renderResult(result: PublishPlanResult): {
  content: Array<{ type: "text"; text: string }>;
  structuredContent: Record<string, unknown>;
} {
  if (result.status === "published") {
    const stepIds = result.steps.map((s) => s.step_id);
    const packageIds = result.steps.map((s) => s.package_id);
    const lines = [
      `✅ published — epic ${result.epic_id}`,
      `   step_ids:    ${stepIds.join(", ")}`,
      `   package_ids: ${packageIds.join(", ")} (sealed — NOT enqueued; index-aligned with step_ids)`,
      `   下一步（人工）：开关授权——总闸(global_enabled) + 项目开关 + Epic auto_execute 全开后 Runner 自动领取；本工具不开任何开关。`,
    ];
    return {
      content: [{ type: "text", text: lines.join("\n") }],
      structuredContent: {
        status: "published",
        epic_id: result.epic_id,
        step_ids: stepIds,
        package_ids: packageIds,
        next_step: PUBLISHED_NEXT_STEP,
        summary: result.summary,
      },
    };
  }

  if (result.status === "needs_decision") {
    const lines = [
      `needs_decision — ${result.questions.length} 个待决问题（先在来源对话里回答，再调整 spec 重发 publish_plan；未创建任何对象）:`,
      ...result.questions.map((q, i) => `  ${i + 1}. [${q.category}] ${q.question}`),
    ];
    return {
      content: [{ type: "text", text: lines.join("\n") }],
      structuredContent: {
        status: "needs_decision",
        questions: result.questions,
        next_step: "Answer the questions in the source conversation, adjust the spec, and re-call publish_plan.",
        summary: result.summary,
      },
    };
  }

  const lines = [
    `invalid — ${result.errors.length} 个确定性错误（零可执行半成品；修复后重发）:`,
    ...result.errors.map((e) => `  • ${e.field}: ${e.message}`),
    `rollback: ${result.rollback.performed ? "performed" : result.rollback.degraded ? "degraded (residue kept but unclaimable)" : "not needed (nothing was created)"} — ${result.rollback.detail}`,
  ];
  return {
    content: [{ type: "text", text: lines.join("\n") }],
    structuredContent: {
      status: "invalid",
      errors: result.errors,
      rollback: result.rollback,
      next_step: "Fix every listed error, then re-call publish_plan with the corrected spec.",
      summary: result.summary,
    },
  };
}

// ─── Registration ───

export function registerPublishPlanTool(server: McpServer): void {
  server.tool("publish_plan", DESCRIPTION, planShape, async (input) => {
    const resolved = await resolveToolCwd(input);
    if ("error" in resolved) {
      return { content: [{ type: "text", text: `Error: ${resolved.error}` }], isError: true };
    }
    const { project_dir: _projectDir, ...spec } = input;
    // Structural zod output → PlanSpec: safe cast because every value rule
    // (enums, registry membership, ranges, dependencies) is enforced by
    // validatePlanSpec inside publishPlan — the single validation authority.
    const result = await publishPlan(resolved.cwd, spec as unknown as PlanSpec);
    return renderResult(result);
  });
}
