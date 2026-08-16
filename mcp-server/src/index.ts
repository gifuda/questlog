import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { z } from "zod";
import path from "node:path";
import { pushTodo, addAndSave, listTodos, updateAndSave, resolveProjectDir, findSubProjects, searchTodos, suggestSimilarProject, readFeedback, canonicalizeProjectDir, resolveRegisteredProject } from "./store.js";
import { pushEpic, updateEpic, pushStep, updateStep, listEpics, epicProgress, actionableSteps } from "./epic-store.js";
import {
  prepareTaskPackage, validateTaskPackage, sealTaskPackage, enqueueStep,
  verifyPickupReadiness,
} from "./task-package-ops.js";
import {
  listDecisionRequests, resolveDecision,
} from "./decision-request.js";
import { registerPublishPlanTool } from "./publish-plan-tool.js";

const server = new McpServer({
  name: "todo-board",
  version: "0.1.0",
});

/**
 * Resolve the effective project directory for a tool call.
 * - Explicit `project_dir` → validated against the allowed set
 *   (cwd/descendants ∪ registered projects). Disallowed → { error }.
 * - No `project_dir` → the tool's normal auto-detection/fallback.
 * Returns { cwd } or { error } so tools can short-circuit consistently.
 */
async function resolveCwd(
  input: { project_dir?: string },
  fallback: () => Promise<string> | string
): Promise<{ cwd: string } | { error: string }> {
  if (!input.project_dir) {
    return { cwd: await fallback() };
  }
  const canonical = await canonicalizeProjectDir(input.project_dir, process.cwd());
  if (!canonical) {
    return { error: `project_dir 非法或被拒绝（需为当前工作区子目录或已注册项目）: ${input.project_dir}` };
  }
  return { cwd: canonical };
}

/**
 * Fallback for epic-registration tools: route to the registered project that
 * matches the current working directory, so registration lands in the
 * .epics.json the desktop board reads. Falls back to the tool's original
 * default when there is no registered match.
 *
 * If the tool call passes an explicit `project_dir`, it takes priority
 * (validated against the allowed set) — without this, cross-project
 * registration silently lands in the wrong project's .epics.json.
 */
export async function epicCwd(
  fallback: () => Promise<string> | string,
  input?: { project_dir?: string } | Record<string, unknown>,
): Promise<{ cwd: string } | { error: string }> {
  const projectDir =
    input && typeof input === "object" && "project_dir" in input
      ? (input as { project_dir?: string }).project_dir
      : undefined;
  if (projectDir) {
    const resolved = await resolveCwd({ project_dir: projectDir }, () => process.cwd());
    if ("error" in resolved) return resolved;
    return { cwd: resolved.cwd };
  }
  const routed = await resolveRegisteredProject(process.cwd());
  const cwd = routed ?? (await fallback());
  return { cwd };
}

// ─── push_todo ───
server.tool(
  "push_todo",
  [
    "Push a todo/knowhow/idea item to the 桌面清单贴 board. Auto-detects the correct project directory by scanning subdirectories.",
    "",
    "## Trigger (CRITICAL)",
    "Only call this when the USER explicitly asked to push/record an item. Do NOT push on your own initiative. Do NOT push repeatedly to \"fill a quota\".",
    "",
    "## One item per call",
    "Push exactly ONE item per call.",
    "",
    "## Per-type standard templates (CRITICAL — apply the template for the chosen type)",
    "",
    "### knowhow — 简明扼要的通识性知识，具备长期参考价值",
    "Required structure: 背景一句话（在什么场景下发现）+ 核心要点 1-3 条（怎么做）+ 适用边界（什么时候不适用）。",
    "If a required field is missing, ASK the user to fill it before pushing.",
    "",
    "### todo — 有明确验收标准的行动项",
    "Required structure: 目标（做什么）+ 验收标准（什么算完成，可检验）+ 优先级 + 截止时间（可选）。",
    "If acceptance criteria are missing, ASK the user: \"验收标准是什么？\" before pushing.",
    "",
    "### idea — 尚未验证的改进假设，不是结论",
    "Required structure: 现状痛点（现在哪里不好）+ 改进假设（改成什么样会更好）+ 预期收益（为什么值得试）+ 验证方式（怎么判断有效）。",
    "If it is already decided and planned, it is a todo, not an idea.",
    "",
    "## Fill-before-push rule",
    "If the user only said \"记一下\" without providing all fields, first ask short questions to fill the missing fields, then push.",
    "If the user explicitly said \"别问了直接记\" (don't ask, just record), push with whatever info is available — do NOT ask.",
    "",
    "## Common field rules",
    "| Field | What to put | What NOT to put |",
    "|-------|------------|-----------------|",
    "| type | knowhow / todo / idea | Do not repeat this word anywhere else |",
    "| title | Pure content summary, 1 line, ≤40 chars | No type prefix ([todo]), no date, no emoji, no brackets, no session ID |",
    "| content | Per the type template above | Do not repeat title. Do not pad with filler. |",
    "| suggestion | One concrete next action with verb + object + expected result | No vague phrases like \"考虑/建议/后续/进一步/关注\" |",
    "| tags | 2-4 lowercase nouns for grouping | No type name, no date, no adjectives |",
    "| priority | high / medium / low (only meaningful for todo type) | — |",
    "",
    "## Drafting self-check (does NOT block pushing)",
    "When organizing content, ask: will this still be useful in 3 months? Does only this conversation have it? What is lost if not recorded? Use answers to make fields concrete — but never use these questions to refuse a push the user requested.",
    "",
    "## Examples of GOOD title: \"Redis 缓存热点查询\" / \"API 网关限流方案\" / \"日志分级策略\"",
    "## Examples of BAD title: \"[todo] Redis 缓存\" / \"2026-06-11 Redis 缓存\" / \"todo: 需要做缓存\" / \"💡 Redis 缓存\"",
  ].join("\n"),
  {
    type: z.enum(["knowhow", "todo", "idea"]).describe("Item type"),
    title: z.string().describe("Pure content summary. No prefixes, no dates, no emoji. ≤40 chars."),
    content: z.string().optional().describe("Full context and details for future reference"),
    suggestion: z.string().optional().describe("One concrete next action step"),
    tags: z.array(z.string()).optional().describe("2-4 lowercase noun tags for grouping"),
    priority: z.enum(["high", "medium", "low"]).optional().describe("Priority — only meaningful for todo type"),
    project_dir: z.string().optional().describe("Override auto-detection: absolute path of the target project directory"),
  },
  async (input) => {
    const resolved = await resolveCwd(input, () => resolveProjectDir(process.cwd(), input.title, input.tags ?? []));
    if ("error" in resolved) return { content: [{ type: "text", text: `Error: ${resolved.error}` }], isError: true };
    const cwd = resolved.cwd;
    const result = pushTodo(cwd, input);

    if ("error" in result) return { content: [{ type: "text", text: `Error: ${result.error}` }], isError: true };

    const saveResult = await addAndSave(cwd, result.item);
    if ("error" in saveResult) return { content: [{ type: "text", text: `Error: ${saveResult.error}` }], isError: true };

    // Check if the project name might be a typo of an existing project
    const projectName = path.basename(cwd);
    const similar = await suggestSimilarProject(projectName);
    const typoHint = similar
      ? `\n⚠️ 项目名「${projectName}」和已有项目「${similar}」很相似，是否需要合并？`
      : "";

    return {
      content: [{
        type: "text",
        text: `Pushed [${result.item.type}] "${result.item.title}" to project "${projectName}" (${cwd}, id: ${result.item.id})${typoHint}`,
      }],
    };
  }
);

// ─── search_todo ───
server.tool(
  "search_todo",
  [
    "Search active items across ALL projects by keyword. Use when the user references a todo/idea/knowhow by name in any conversation, even if it belongs to a different project. Returns matching items with project info so the user can act on them from any session.",
    "",
    "## After working on a found item",
    "If the user asked you to process a found item: mark it processing=true when you start (via update_todo), and set status='completed' when the work is actually done. Never leave an item active after claiming it is done.",
  ].join("\n"),
  {
    keyword: z.string().describe("Search term — matched against title, content, suggestion, and tags (case-insensitive)"),
  },
  async ({ keyword }) => {
    const results = await searchTodos(keyword);

    if (results.length === 0) {
      return { content: [{ type: "text", text: `No active items matching "${keyword}" found across any project.` }] };
    }

    const lines: string[] = [
      `Found ${results.length} item(s) matching "${keyword}":`,
      "---",
    ];

    for (const item of results) {
      const processingMark = item.processing ? " 🔄处理中" : "";
      lines.push(`[${item.type}] ${item.title}${processingMark}`);
      lines.push(`  project: ${item.project} (${item.project_path})`);
      if (item.content) lines.push(`  content: ${item.content}`);
      if (item.suggestion) lines.push(`  suggestion: ${item.suggestion}`);
      if (item.tags.length) lines.push(`  tags: ${item.tags.join(", ")}`);
      lines.push(`  priority: ${item.priority}`);
      lines.push(`  id: ${item.id}`);
      lines.push("");
    }

    return { content: [{ type: "text", text: lines.join("\n") }] };
  }
);

// ─── list_todos ───
server.tool(
  "list_todos",
  "List all todo items for the current project. Auto-detects project from subdirectories. Call at conversation start.",
  {
    project_dir: z.string().optional().describe("Override auto-detection: absolute path of the target project directory"),
  },
  async (input) => {
    const resolved = await resolveCwd(input, () => process.cwd());
    if ("error" in resolved) return { content: [{ type: "text", text: `Error: ${resolved.error}` }], isError: true };
    const cwd = resolved.cwd;
    const result = await listTodos(cwd);

    if ("error" in result) return { content: [{ type: "text", text: `Error: ${result.error}` }], isError: true };

    const active = result.data.items.filter((i) => i.status === "active");
    const completed = result.data.items.filter((i) => i.status === "completed");

    // If cwd has no items and is not a project root, show available sub-projects
    if (active.length === 0 && completed.length === 0) {
      const subs = await findSubProjects(cwd);
      if (subs.length > 0) {
        const lines: string[] = [
          `Workspace: ${path.basename(cwd)} (not a project root)`,
          `Found ${subs.length} sub-project(s):`,
          "",
        ];
        for (const sub of subs) {
          const name = path.basename(sub);
          const subResult = await listTodos(sub);
          const subActive = "data" in subResult
            ? subResult.data.items.filter((i: { status: string }) => i.status === "active").length
            : 0;
          lines.push(`  ${name} (${subActive} active) → ${sub}`);
        }
        lines.push("");
        lines.push("Items will be auto-routed to the matching sub-project by title/tags.");
        return { content: [{ type: "text", text: lines.join("\n") }] };
      }
    }

    const lines: string[] = [
      `Project: ${result.data.project}`,
      `Active: ${active.length} | Completed: ${completed.length}`,
      "---",
    ];

    for (const item of active) {
      const processingMark = item.processing ? " 🔄处理中" : "";
      lines.push(`[${item.type}] ${item.title}${processingMark}`);
      if (item.tags.length) lines.push(`  tags: ${item.tags.join(", ")}`);
      if (item.suggestion) lines.push(`  suggestion: ${item.suggestion}`);
      lines.push(`  id: ${item.id}`);
      lines.push("");
    }

    // Quality feedback: tell the AI when recent pushes got archived quickly
    const feedback = await readFeedback(cwd);
    if (feedback.archived_soon >= 2) {
      const ratio = feedback.pushes > 0 ? (feedback.archived_soon / feedback.pushes).toFixed(0) : "0";
      lines.push(`⚠️ 质量反馈：最近 ${feedback.pushes} 条推送中有 ${feedback.archived_soon} 条被用户快速归档（${ratio}% 被弃）。`);
      lines.push(`   推送请大幅收敛：只推有长期价值的 knowhow、有明确验收标准的 todo、有验证方式的 idea。宁可少推。`);
    }

    return { content: [{ type: "text", text: lines.join("\n") }] };
  }
);

// ─── update_todo ───
server.tool(
  "update_todo",
  [
    "Update a todo item's status, content, tags, priority, or processing flag.",
    "",
    "## Completion writeback rule (CRITICAL)",
    "When the user references a todo item (via the 📋 copied reference \"处理待办: xxx\" / \"Handle todo: xxx\", search_todo, or by name) and it is DONE in this conversation, you MUST call update_todo with status='completed' before ending the related work — do NOT just tell the user it is done.",
    "Mark processing=true when you START working on a referenced item (if the user pasted a reference and you're going to act on it), and set status='completed' when finished.",
    "",
    "## Allowed transitions",
    "knowhow: active → archived | todo: active → completed → archived | idea: active → converted → archived",
  ].join("\n"),
  {
    id: z.string().describe("Item ID"),
    status: z.enum(["active", "completed", "archived", "converted"]).optional().describe("New status"),
    content: z.string().optional().describe("Updated content"),
    tags: z.array(z.string()).optional().describe("Updated tags"),
    priority: z.enum(["high", "medium", "low"]).optional().describe("Updated priority"),
    title: z.string().optional().describe("Updated title"),
    processing: z.boolean().optional().describe("Mark the item as being processed (true when you start working on it)"),
    project_dir: z.string().optional().describe("Absolute path of the target project directory. Defaults to process.cwd()"),
  },
  async (input) => {
    const resolved = await resolveCwd(input, () => process.cwd());
    if ("error" in resolved) return { content: [{ type: "text", text: `Error: ${resolved.error}` }], isError: true };
    const cwd = resolved.cwd;
    const result = await updateAndSave(cwd, input.id, input);

    if ("error" in result) return { content: [{ type: "text", text: `Error: ${result.error}` }], isError: true };

    return { content: [{ type: "text", text: `Updated item ${input.id}` }] };
  }
);

// ════════════════════════════════════════════════════════════
// 方案追踪模块 — 4 个独立工具，与待办完全隔离
// ════════════════════════════════════════════════════════════

// ─── push_epic ───
server.tool(
  "push_epic",
  [
    "Create a new Epic (方案) in the 方案追踪 module of 桌面清单贴. An Epic is a multi-week improvement track with steps and dependencies — NOT a single task (use push_todo for tasks).",
    "",
    "## Trigger",
    "Call this when the USER explicitly asks to create a plan / epic / 方案 / roadmap item — OR when you are about to start multi-step work that has no registered epic yet: registration is part of the workflow, not an extra ask. Before starting tracked work, call list_epics to check; if the epic is missing, create it first (then push_step its steps), otherwise there is no object to register progress against and the board falls behind reality.",
    "Do NOT auto-create epics from casual conversation.",
    "",
    "## When to use Epic vs Todo",
    "- Epic: a multi-step improvement that spans days/weeks, has dependencies, needs progress tracking (e.g. '采集机路径修复').",
    "- Todo: a single atomic action with clear acceptance criteria (e.g. '修复 symlink').",
    "- If unsure, ask the user: '这是一个独立待办，还是一个跨步骤的方案？'",
    "",
    "## One plan per conversation (CRITICAL)",
    "在一个对话/会话中只创建 **一个顶级方案**（本轮对话的主题）。后续拆解一律作为它的**子方案**（parent_id=父方案id）或**步骤**，不要另起顶级方案。",
    "这样避免平铺过多方案导致丢失重点。层级最多三层：顶级方案 → 子方案 → 步骤（子方案不能再有子方案）。",
    "",
    "## Field rules",
    "| Field | What to put |",
    "|-------|------------|",
    "| title | 纯方案标题，不带优先级前缀（优先级放 priority 字段）|",
    "| description | 一句话说清这个方案要解决什么问题 |",
    "| priority | P0 (最高) / P1 / P2 / P3 |",
    "| depends_on | 依赖的其他 epic id 列表（必须先完成才能启动本方案）|",
    "| parent_id | 可选。作为子方案挂到父方案下（需要父方案已存在）|",
    "| acceptance | 可选但强烈推荐。验收条件，自由文本（如\"所有步骤 observed 且观察 3 天无复发\"）。由本对话设定，done 前需确认满足 |",
  ].join("\n"),
  {
    title: z.string().describe("Epic title — pure title, NO priority prefix (priority goes in the priority field)"),
    description: z.string().optional().describe("One-line: what problem does this epic solve?"),
    priority: z.enum(["P0", "P1", "P2", "P3"]).optional().describe("Priority — P0 highest"),
    depends_on: z.array(z.string()).optional().describe("Epic IDs this epic depends on (must complete first)"),
    parent_id: z.string().optional().describe("Parent epic ID to attach as a child plan (max 3 levels: top → child → step)"),
    acceptance: z.string().optional().describe("验收条件，自由文本。由创建方案的对话设定，done 前需确认满足。例：\"所有步骤 observed 且线上观察 3 天无复发\""),
    project_dir: z.string().optional().describe("Override auto-detection: absolute path of the target project directory"),
  },
  async (input) => {
    const resolved = await epicCwd(() => resolveProjectDir(process.cwd(), input.title, []), input);
    if ("error" in resolved) return { content: [{ type: "text", text: `Error: ${resolved.error}` }], isError: true };
    const cwd = resolved.cwd;
    const result = await pushEpic(cwd, input);
    if ("error" in result) return { content: [{ type: "text", text: `Error: ${result.error}` }], isError: true };

    const e = result.epic;
    const depHint = e.depends_on.length > 0
      ? `\n依赖: ${e.depends_on.join(", ")} — 依赖未完成时本方案自动标为 blocked`
      : "";
    return {
      content: [{
        type: "text",
        text: `Created epic "${e.title}" (id: ${e.id}, priority: ${e.priority}, status: draft) in ${path.basename(cwd)}.${depHint}\n\n下一步：用 push_step 添加步骤。`,
      }],
    };
  }
);

// ─── update_epic ───
server.tool(
  "update_epic",
  [
    "Update an Epic's status, title, description, priority, dependencies, or links. Used to advance an epic through its lifecycle (draft → active → verifying → observing → done).",
    "",
    "## Status machine",
    "draft → active → verifying → observing → done",
    "Side states: blocked (auto-derived from unmet depends_on), paused (user pause).",
    "",
    "## Auto-derived: blocked status",
    "If an epic's depends_on includes any epic NOT in 'done' status, it is automatically set to 'blocked'. When all dependencies reach 'done', a blocked epic auto-releases to 'draft'. You don't need to manage blocked manually.",
    "",
    "## Done gate (CRITICAL — prevents \"75% but shown as done\")",
    "设 status='done' 时系统校验当前进度：",
    "- 进度 = 100%（所有步骤 observed）→ 直接放行",
    "- 进度 < 100% → 必须同时传 close_reason（收尾原因），否则报错拒绝。",
    "这是唯一阻止\"进度未满却显示完成\"的硬门禁。提前收尾是合法的（方向变更、合并），但必须有显式原因。",
    "",
    "## Typical workflow",
    "1. User starts working on an epic → update_epic status='active'",
    "2. Code done, entering verification → status='verifying'",
    "3. Verified, entering observation window → status='observing'",
    "4. Observation passed → status='done' (this unblocks dependents!)",
  ].join("\n"),
  {
    id: z.string().describe("Epic ID"),
    title: z.string().optional(),
    description: z.string().optional(),
    status: z.enum(["draft", "active", "verifying", "observing", "done", "paused"]).optional().describe("New status (blocked is auto-derived, do not set manually)"),
    priority: z.enum(["P0", "P1", "P2", "P3"]).optional(),
    depends_on: z.array(z.string()).optional().describe("Epic IDs this depends on (replaces list)"),
    links_doc: z.string().optional().describe("Document URL/path"),
    links_branch: z.string().optional().describe("Code branch name"),
    links_note: z.string().optional().describe("Memo / memory notes"),
    acceptance: z.string().optional().describe("验收条件，自由文本。done 前需确认满足"),
    close_reason: z.string().optional().describe("收尾原因。进度<100% 提前标 done 时必填，否则被系统拒绝"),
    project_dir: z.string().optional().describe("Absolute path of the target project directory"),
  },
  async (input) => {
    const resolved = await epicCwd(() => process.cwd(), input);
    if ("error" in resolved) return { content: [{ type: "text", text: `Error: ${resolved.error}` }], isError: true };
    const cwd = resolved.cwd;
    const { id, ...patch } = input;
    const result = await updateEpic(cwd, id, patch);
    if ("error" in result) return { content: [{ type: "text", text: `Error: ${result.error}` }], isError: true };
    return { content: [{ type: "text", text: `Updated epic ${id}.` }] };
  }
);

// ─── push_step ───
server.tool(
  "push_step",
  [
    "Add a Step (步骤) to an existing Epic. A step has a 5-stage progress: pending → designed → coded → verified → observed (0/25/50/75/100%). The epic's overall progress is the mean of its steps.",
    "",
    "## Trigger",
    "Call this when the USER asks to break down an epic into steps, when continuing a planning conversation — OR when you are about to start work on an epic that has no steps yet: register the steps first (registration is part of the workflow, not an extra ask). Ask the user '这个方案要拆成哪几个步骤？' if not clear.",
    "",
    "## Field rules",
    "- title: step name, concise and action-oriented (e.g. 'S1 路径修复' or 'git HEAD对齐')",
    "- description: what this step covers (optional but recommended)",
    "- epic_id: must be an existing epic's id (from list_epics or push_epic return)",
  ].join("\n"),
  {
    epic_id: z.string().describe("Parent epic ID"),
    title: z.string().describe("Step title, e.g. 'S1 路径修复'"),
    description: z.string().optional().describe("What this step covers"),
    depends_on: z.array(z.string()).optional().describe("Step IDs this step depends on (must be observed first). Empty = serial by order. Multiple parallel steps: leave depends_on empty and they become actionable together once their order-predecessors finish"),
    project_dir: z.string().optional().describe("Absolute path of the target project directory"),
  },
  async (input) => {
    const resolved = await epicCwd(() => process.cwd(), input);
    if ("error" in resolved) return { content: [{ type: "text", text: `Error: ${resolved.error}` }], isError: true };
    const cwd = resolved.cwd;
    const result = await pushStep(cwd, input);
    if ("error" in result) return { content: [{ type: "text", text: `Error: ${result.error}` }], isError: true };
    return {
      content: [{
        type: "text",
        text: `Added step "${result.step.title}" (id: ${result.step.id}, stage: pending) to epic ${result.step.epic_id}.`,
      }],
    };
  }
);

// ─── push_step_with_package ───
server.tool(
  "push_step_with_package",
  [
    "Create a step AND seal its standard package in one atomic operation. This is the 'create step = create package' flow: pass both step metadata and package parameters, and the tool internally runs push_step → prepare → validate → seal.",
    "If prepare/seal fails, the step is rolled back (deleted) — no half-created step without a package is left behind.",
    "",
    "## When to call",
    "- When creating a step that should be auto-executed by the Runner (the standard flow for ymos and other active projects)",
    "- When the user says '创建步骤' or '添加步骤' and the step is meant for automated execution",
    "- This replaces the manual push_step → prepare → validate → seal 4-step sequence",
    "",
    "## Field rules",
    "- title/description/depends_on: step metadata (same as push_step)",
    "- objective/allowed_paths/acceptance_command_ids: package parameters (same as prepare_task_package)",
    "- The step is created as pending with a sealed package attached (step.package summary written to .epics.json)",
    "- The package is sealed. If the epic's auto_execute switch is ON (▶) and the project switch is ON, the Runner picks it up on its next poll — no enqueue_step needed. Otherwise it waits as 'sealed'.",
  ].join("\n"),
  {
    epic_id: z.string().describe("Parent epic ID"),
    title: z.string().describe("Step title, e.g. 'S1 路径修复'"),
    description: z.string().optional().describe("What this step covers"),
    depends_on: z.array(z.string()).optional().describe("Step IDs this step depends on"),
    objective: z.string().describe("Single, verifiable objective — what success looks like"),
    allowed_paths: z.array(z.string()).describe("Glob patterns for paths the executor MAY modify"),
    forbidden_paths: z.array(z.string()).optional().describe("Glob patterns for paths the executor MUST NOT touch"),
    max_changed_files: z.number().optional().describe("Maximum files the executor may change (default 10)"),
    executor_preferred: z.enum(["zcode", "manual"]).optional().describe("zcode executes automatically; manual is tracking-only"),
    executor_sandbox: z.enum(["workspace-write", "read-only"]).optional().describe("workspace-write (default) allows scoped edits; read-only forbids any file change (report tasks — output goes to stdout)"),
    executor_timeout_seconds: z.number().optional(),
    executor_max_attempts: z.number().optional(),
    acceptance_command_ids: z.array(z.string()).optional().describe("Trusted command IDs from the acceptance registry"),
    context_refs: z.array(z.string()).optional().describe("Reference docs/paths for context"),
    project_dir: z.string().optional().describe("Override project directory (server validates)"),
  },
  async (input) => {
    const resolved = await epicCwd(() => process.cwd(), input);
    if ("error" in resolved) return { content: [{ type: "text", text: `Error: ${resolved.error}` }], isError: true };
    const { pushStepWithPackage } = await import("./task-package-ops.js");
    const result = await pushStepWithPackage(resolved.cwd, input);
    if ("error" in result) return { content: [{ type: "text", text: `Error: ${result.error}` }], isError: true };
    return {
      content: [{
        type: "text",
        text: [
          `✅ Step + sealed package created atomically:`,
          `   Step: ${result.step_id} (pending)`,
          `   Package: ${result.package_id} (sealed, rev ${result.plan_revision}, hash ${result.package_hash.slice(0, 8)})`,
          `   Objective: ${result.summary.objective}`,
          ``,
          `Runner pickup: automatic once the project + epic switches are ON (switch = authorization); enqueue_step is optional.`,
        ].join("\n"),
      }],
    };
  }
);

// ─── update_step ───
server.tool(
  "update_step",
  [
    "Advance a Step's stage (the progress slider) or update its fields. This is how progress flows into the system — steps advance, epic progress auto-recomputes.",
    "",
    "## 5-stage progress",
    "| Stage | % | Meaning |",
    "|-------|---|---------|",
    "| pending | 0 | Not started |",
    "| designed | 25 | Design/plan done |",
    "| coded | 50 | Implementation done |",
    "| verified | 75 | Verified working |",
    "| observed | 100 | Observation window passed — truly closed |",
    "",
    "## When to advance",
    "- Finished designing → stage='designed'",
    "- Finished coding → stage='coded'",
    "- Ran verification → stage='verified' (AI 验证通过即可推进到这)",
    "- Observation window passed → stage='observed' (step fully done)",
    "",
    "## verified vs observed（重要语义）",
    "- verified（75%）：AI 验证通过（测试跑过、实跑确认）。AI 可以自主推进到这。",
    "- observed（100%）：跨会话确认稳定。观察期约定写在所属 epic 的 acceptance 字段里（如\"观察 3 天无复发\"），由用户确认稳定后再推进。",
    "",
    "## Completion writeback rule (CRITICAL)",
    "When the user says a step is done — code written, verification passed, observation window closed — you MUST call update_step with the new stage BEFORE ending the related work. Do NOT just tell the user it is done; the board must not fall behind reality (a Stop hook blocks session end when completed steps were mentioned but not logged).",
    "",
    "When ALL steps of an epic reach 'observed', you MUST also call update_epic status='done' (this unblocks dependents) before ending the related work.",
  ].join("\n"),
  {
    id: z.string().describe("Step ID"),
    title: z.string().optional(),
    description: z.string().optional(),
    stage: z.enum(["pending", "designed", "coded", "verified", "observed"]).optional().describe("New stage (progress slider)"),
    order: z.number().optional().describe("Reorder position"),
    depends_on: z.array(z.string()).optional().describe("Step IDs this step depends on (replaces list)"),
    todo_ref: z.string().optional().describe("Soft-link a todo item ID (reference, not move)"),
    project_dir: z.string().optional().describe("Absolute path of the target project directory"),
  },
  async (input) => {
    const resolved = await epicCwd(() => process.cwd(), input);
    if ("error" in resolved) return { content: [{ type: "text", text: `Error: ${resolved.error}` }], isError: true };
    const cwd = resolved.cwd;
    const { id, ...patch } = input;
    const result = await updateStep(cwd, id, patch);
    if ("error" in result) return { content: [{ type: "text", text: `Error: ${result.error}` }], isError: true };

    // Show recomputed epic progress after step update
    const data = await listEpics(cwd);
    if ("error" in data) return { content: [{ type: "text", text: `Error reading epics: ${data.error}` }], isError: true };
    const step = data.steps.find((s) => s.id === id);
    if (step) {
      const progress = epicProgress(step.epic_id, data);
      const epic = data.epics.find((e) => e.id === step.epic_id);
      // 推进到 observed 时提醒验收条件（防止跳过观察期）
      const acceptanceHint = step.stage === "observed" && epic?.acceptance
        ? `\n📌 验收条件提醒：「${epic.acceptance}」请确认已满足（observed = 跨会话确认稳定）。`
        : "";
      return {
        content: [{
          type: "text",
          text: `Updated step ${id} → ${step.stage}.\nEpic "${epic?.title ?? step.epic_id}" progress now: ${progress}%.${acceptanceHint}`,
        }],
      };
    }
    return { content: [{ type: "text", text: `Updated step ${id}.` }] };
  }
);

// ─── list_epics ───
server.tool(
  "list_epics",
  [
    "List all plans (Epics) with their steps and progress in the current project. Call this when the user references a plan by name (e.g. '继续方案: xxx' / 'Continue plan: xxx', or a title) — find the matching epic and continue from where it left off.",
    "",
    "## When to call",
    "- Call at conversation start when resuming tracked work — know what is registered before starting (mirrors list_todos).",
    "- User pastes a copied reference like '继续方案: 方案标题' / 'Continue plan: Plan title'",
    "- User asks about plan progress / next step",
    "- User says '推进方案' / '继续做这个方案' / 'continue this plan'",
    "- Before starting multi-step work: check whether the epic already exists; if not, push_epic + push_step to register it first",
    "",
    "## Output includes",
    "- Hierarchical: top-level epics first (newest first), child epics nested under their parent",
    "- Epic: title, status, priority, progress%, dependencies (depends_on), child count",
    "- Steps: order, title, stage, stage%, links",
    "- Actionable steps (what to work on next, possibly parallel)",
  ].join("\n"),
  {
    project_dir: z.string().optional().describe("Absolute path of the target project directory"),
  },
  async (input) => {
    const resolved = await epicCwd(() => process.cwd(), input);
    if ("error" in resolved) return { content: [{ type: "text", text: `Error: ${resolved.error}` }], isError: true };
    const cwd = resolved.cwd;
    const data = await listEpics(cwd);
    if ("error" in data) return { content: [{ type: "text", text: `Error: ${data.error}` }], isError: true };

    if (data.epics.length === 0) {
      return { content: [{ type: "text", text: "当前项目还没有方案。创建：push_epic（多步骤改进）或 push_todo（单条行动）。" }] };
    }

    const lines: string[] = [
      `Project: ${data.project}`,
      `Plans: ${data.epics.length} | Steps: ${data.steps.length}`,
      "---",
    ];

    const formatEpic = (epic: import("./epic-store.js").Epic, indent: string) => {
      const progress = epicProgress(epic.id, data);
      const epicSteps = data.steps
        .filter((s) => s.epic_id === epic.id)
        .sort((a, b) => a.order - b.order);
      const actionable = actionableSteps(epic.id, data.steps);
      const children = data.epics
        .filter((e) => e.parent_id === epic.id)
        .sort((a, b) => b.created_at.localeCompare(a.created_at));

      lines.push(`${indent}📊 ${epic.title} — ${progress}% (${epic.status})`);
      if (epic.priority) lines.push(`${indent}  priority: ${epic.priority}`);
      if (epic.description) lines.push(`${indent}  desc: ${epic.description}`);
      if (epic.depends_on.length) lines.push(`${indent}  depends_on: ${epic.depends_on.join(", ")}`);
      if (children.length) lines.push(`${indent}  子方案 ${children.length} 个`);
      lines.push(`${indent}  id: ${epic.id}`);

      for (const step of epicSteps) {
        const isActionable = actionable.some((a) => a.id === step.id);
        const depHint = step.depends_on?.length
          ? ` (等 ${step.depends_on.length} 个前置)`
          : "";
        lines.push(`${indent}    ${step.order}. ${step.title} — ${step.stage}${isActionable ? " ← 可做" : ""}${depHint}`);
      }
      if (actionable.length > 1) {
        lines.push(`${indent}  → 当前可并行推进: ${actionable.map((a) => a.title).join("、")}`);
      }

      for (const child of children) {
        lines.push("");
        formatEpic(child, `${indent}  `);
      }
    };

    // 顶层方案按创建时间倒序
    const topLevel = data.epics
      .filter((e) => !e.parent_id)
      .sort((a, b) => b.created_at.localeCompare(a.created_at));

    for (const epic of topLevel) {
      formatEpic(epic, "");
      lines.push("");
    }

    return { content: [{ type: "text", text: lines.join("\n") }] };
  }
);

// ════════════════════════════════════════════════════════════════
// TaskPackage tools — controlled execution hard gates
// Feature flag: QUESTLOG_EXECUTION_PACKAGES=0 disables these tools.
// When disabled, all existing Todo/Epic/Step tools continue to work —
// the feature is purely additive.
// ════════════════════════════════════════════════════════════════

const EXECUTION_PACKAGES_ENABLED = process.env.QUESTLOG_EXECUTION_PACKAGES !== "0";

if (EXECUTION_PACKAGES_ENABLED) {

// ─── publish_plan (single external publishing entry; reuses the S1 core) ───
registerPublishPlanTool(server);

// ─── prepare_task_package ───
server.tool(
  "prepare_task_package",
  [
    "Prepare a draft TaskPackage for a step. The server resolves the real project path, git root, and base SHA — it does NOT trust model-supplied values.",
    "Returns a draft package with validation issues to fix before sealing. The package is NOT executable until sealed.",
    "",
    "## When to call",
    "- User asks to create an executable task / standard task package",
    "- After defining a Step that should be auto-executed by the Runner",
    "- Before enqueue_step — you must prepare → validate → seal first",
  ].join("\n"),
  {
    epic_id: z.string().describe("The epic this package belongs to"),
    step_id: z.string().describe("The step this package executes"),
    objective: z.string().describe("Single, verifiable objective — what success looks like"),
    allowed_paths: z.array(z.string()).describe("Glob patterns for paths the executor MAY modify"),
    forbidden_paths: z.array(z.string()).optional().describe("Glob patterns for paths the executor MUST NOT touch"),
    max_changed_files: z.number().optional().describe("Maximum files the executor may change (default 10)"),
    executor_preferred: z.enum(["zcode", "manual"]).optional().describe("zcode executes automatically; manual is tracking-only. Codex may push packages but is never a Runner executor."),
    executor_sandbox: z.enum(["workspace-write", "read-only"]).optional().describe("workspace-write (default) allows scoped edits; read-only forbids any file change (report tasks — output goes to stdout)"),
    executor_timeout_seconds: z.number().optional(),
    executor_max_attempts: z.number().optional(),
    acceptance_command_ids: z.array(z.string()).optional().describe("Trusted command IDs from the acceptance registry (e.g. test:mcp, build:mcp)"),
    context_refs: z.array(z.string()).optional().describe("Reference docs/paths for context"),
    project_dir: z.string().optional().describe("Override project directory (server validates)"),
  },
  async (input) => {
    const resolved = await epicCwd(() => process.cwd(), input);
    if ("error" in resolved) return { content: [{ type: "text", text: `Error: ${resolved.error}` }], isError: true };
    const result = await prepareTaskPackage(resolved.cwd, input);
    if ("error" in result) return { content: [{ type: "text", text: `Error: ${result.error}` }], isError: true };
    const lines = [
      `📦 Draft TaskPackage prepared: ${result.package_id}`,
      `   Project: ${result.resolved_project_path}`,
      `   Base SHA: ${result.resolved_base_sha}`,
      `   Issues: ${result.issues.length} (${result.issues.filter((i) => i.severity === "error").length} errors)`,
    ];
    if (result.issues.length > 0) {
      lines.push("", "Fix these before sealing:");
      for (const issue of result.issues) {
        lines.push(`  ${issue.severity === "error" ? "❌" : "⚠️"}  ${issue.field}: ${issue.message}`);
      }
    }
    lines.push("", `Next: validate_task_package → seal_task_package → enqueue_step`);
    return { content: [{ type: "text", text: lines.join("\n") }] };
  }
);

// ─── validate_task_package ───
server.tool(
  "validate_task_package",
  [
    "Validate a TaskPackage against schema, Git state, dependencies, and the trusted command registry.",
    "Returns all issues (errors block sealing). Checks: content schema, git clean, base SHA drift, step dependencies, command registry membership.",
  ].join("\n"),
  {
    package_id: z.string().describe("The package ID from prepare_task_package"),
  },
  async (input) => {
    const resolved = await epicCwd(() => process.cwd(), input);
    if ("error" in resolved) return { content: [{ type: "text", text: `Error: ${resolved.error}` }], isError: true };
    const result = await validateTaskPackage(resolved.cwd, input.package_id);
    if ("error" in result) return { content: [{ type: "text", text: `Error: ${result.error}` }], isError: true };
    const lines = [
      `🔍 Validation: ${result.valid ? "✅ VALID" : "❌ INVALID"}`,
      `   Package: ${result.package_id}`,
      `   Git clean: ${result.server_checks.git_clean}`,
      `   Base SHA current: ${result.server_checks.base_sha_current}`,
      `   Dependencies met: ${result.server_checks.dependencies_met}`,
      `   Commands trusted: ${result.server_checks.commands_trusted}`,
    ];
    if (result.issues.length > 0) {
      lines.push("", "Issues:");
      for (const issue of result.issues) {
        lines.push(`  ${issue.severity === "error" ? "❌" : "⚠️"}  ${issue.field}: ${issue.message}`);
      }
    }
    if (result.valid) {
      lines.push("", "✅ Ready to seal. Call seal_task_package next.");
    }
    return { content: [{ type: "text", text: lines.join("\n") }] };
  }
);

// ─── seal_task_package ───
server.tool(
  "seal_task_package",
  [
    "Seal a TaskPackage — freeze its content, compute the final hash, mark as sealed (immutable).",
    "Only valid packages can be sealed. After sealing, any field change requires a new revision.",
    "A sealed package is claimable by the Runner on its next poll once the project + epic switches are ON (switch = authorization) — enqueue_step is optional, and manual-executor packages stay sealed for tracking only.",
  ].join("\n"),
  {
    package_id: z.string().describe("The package ID to seal"),
  },
  async (input) => {
    const resolved = await epicCwd(() => process.cwd(), input);
    if ("error" in resolved) return { content: [{ type: "text", text: `Error: ${resolved.error}` }], isError: true };
    const result = await sealTaskPackage(resolved.cwd, input.package_id);
    if ("error" in result) return { content: [{ type: "text", text: `Error: ${result.error}` }], isError: true };
    const lines = [
      `🔒 TaskPackage sealed: ${result.package_id}`,
      `   Revision: ${result.plan_revision}`,
      `   Hash: ${result.package_hash.slice(0, 16)}...`,
      `   Sealed at: ${result.sealed_at}`,
      "",
      "The package is now immutable. It runs automatically once the project + epic switches are ON; enqueue_step is optional.",
    ];
    return { content: [{ type: "text", text: lines.join("\n") }] };
  }
);

// ─── enqueue_step ───
server.tool(
  "enqueue_step",
  [
    "Enqueue a sealed TaskPackage for Runner execution. Only sealed, valid, dependency-met packages can be enqueued.",
    "The Runner picks up packages with status 'ready'. This does NOT start execution immediately — it makes the step available.",
    "Double-checks validity at enqueue time (base SHA may have drifted since seal).",
  ].join("\n"),
  {
    step_id: z.string().describe("The step whose sealed package should be enqueued"),
  },
  async (input) => {
    const resolved = await epicCwd(() => process.cwd(), input);
    if ("error" in resolved) return { content: [{ type: "text", text: `Error: ${resolved.error}` }], isError: true };
    const result = await enqueueStep(resolved.cwd, input.step_id);
    if ("error" in result) return { content: [{ type: "text", text: `Error: ${result.error}` }], isError: true };
    const tag = result.runner_eligible ? "✅" : "⚠️";
    const lines = [`${tag} ${result.message}`];
    if (!result.runner_eligible && result.blockers.length > 0) {
      lines.push("", "Blockers:");
      for (const b of result.blockers) {
        lines.push(`  • [${b.check}] ${b.reason}`);
      }
      lines.push("", "Fix the blockers above, then re-check with verify_pickup_readiness.");
    }
    return { content: [{ type: "text", text: lines.join("\n") }] };
  }
);

// ─── verify_pickup_readiness ───
server.tool(
  "verify_pickup_readiness",
  [
    "Verify whether a sealed/ready TaskPackage will actually be picked up by the Runner's next poll.",
    "Mirrors the filters in poll_once: project enabled, epic auto_execute allows, not already in-flight, acceptance commands registered.",
    "Callable before or after enqueue_step. Does NOT mutate any state.",
    "Use this when a package is enqueued but not getting picked up, or to pre-check before enqueueing.",
  ].join("\n"),
  {
    package_id: z.string().describe("The package ID to check"),
  },
  async (input) => {
    const result = await verifyPickupReadiness(input.package_id);
    if ("error" in result) {
      return { content: [{ type: "text", text: `Error: ${result.error}` }], isError: true };
    }
    const lines = [
      `🔍 Readiness check for package ${result.package_id}`,
      `  Status: ${result.status}`,
      `  Will be picked up: ${result.will_be_picked_up ? "✅ YES" : "❌ NO"}`,
      "",
      "Runner-side checks:",
      `  • project_enabled:          ${result.runner_side_checks.project_enabled}`,
      `  • epic_auto_execute_allows: ${result.runner_side_checks.epic_auto_execute_allows}`,
      `  • runner_thread_alive:      ${result.runner_side_checks.runner_thread_alive}`,
      `  • already_in_flight:        ${result.runner_side_checks.already_in_flight}`,
      `  • acceptance_commands_ok:   ${result.acceptance_commands_available}`,
    ];
    if (result.blockers.length > 0) {
      lines.push("", "Blockers:");
      for (const b of result.blockers) {
        lines.push(`  • [${b.check}] ${b.reason}`);
      }
    } else if (result.will_be_picked_up) {
      lines.push("", "No blockers — this package will be picked up on the next Runner poll.");
    }
    return { content: [{ type: "text", text: lines.join("\n") }] };
  }
);

// ─── list_decision_requests ───
server.tool(
  "list_decision_requests",
  [
    "List unresolved DecisionRequests that need human attention. These are generated when the Runner encounters scope expansion, base SHA drift, repeated failures, or authorization needs.",
    "Call this when the user asks about blocked steps, pending decisions, or what needs attention. The user can resolve these from any MCP conversation — no context copy-paste needed.",
  ].join("\n"),
  {
    include_resolved: z.boolean().optional().describe("Include already-resolved requests (default: false)"),
    epic_id: z.string().optional().describe("Filter by epic"),
    step_id: z.string().optional().describe("Filter by step"),
  },
  async (input) => {
    const reqs = await listDecisionRequests({
      include_resolved: input.include_resolved,
      epic_id: input.epic_id,
      step_id: input.step_id,
    });
    if (reqs.length === 0) {
      return { content: [{ type: "text", text: "No pending decision requests." }] };
    }
    const lines = [`🔔 ${reqs.length} decision request(s) pending:`];
    for (const req of reqs) {
      lines.push("", `━━━ ${req.request_id} ━━━`);
      lines.push(`  Kind: ${req.kind}`);
      lines.push(`  Epic: ${req.epic_id}`);
      lines.push(`  Step: ${req.step_id}`);
      lines.push(`  Question: ${req.question}`);
      if (req.options.length > 0) {
        lines.push(`  Options: ${req.options.join(" | ")}`);
      }
      if (req.evidence_refs.length > 0) {
        lines.push(`  Evidence: ${req.evidence_refs.join(", ")}`);
      }
      lines.push(`  Blocking: ${req.blocking}`);
      lines.push(`  Created: ${req.created_at}`);
      if (req.resolved) {
        lines.push(`  ✅ Resolved by ${req.resolved_by}: ${req.resolution}`);
      }
    }
    lines.push("", "Resolve with: resolve_decision(request_id, resolution)");
    return { content: [{ type: "text", text: lines.join("\n") }] };
  }
);

// ─── resolve_decision ───
server.tool(
  "resolve_decision",
  [
    "Resolve a DecisionRequest by recording the human's answer.",
    "This does not restart an execution or mutate the sealed package. Retry explicitly, or revise and reseal the package, after recording the decision.",
  ].join("\n"),
  {
    request_id: z.string().describe("The decision request ID from list_decision_requests"),
    resolution: z.string().describe("The answer/approval/direction to give the Runner"),
  },
  async (input) => {
    const result = await resolveDecision(input.request_id, input.resolution);
    if ("error" in result) {
      return { content: [{ type: "text", text: `Error: ${result.error}` }], isError: true };
    }
    return {
      content: [{
        type: "text",
        text: `✅ Decision ${input.request_id} recorded: "${input.resolution}"\nNo execution was started. Retry explicitly, or revise and reseal the package if the decision changes scope, authority, base SHA, or direction.`,
      }],
    };
  }
);

// ─── start ───

} // end EXECUTION_PACKAGES_ENABLED

async function main() {
  const transport = new StdioServerTransport();
  await server.connect(transport);
}

main().catch((err) => {
  console.error("MCP server error:", err);
  process.exit(1);
});
