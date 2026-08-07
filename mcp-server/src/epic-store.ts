/**
 * 方案追踪模块的存储层（MCP 侧）。
 * 与 Rust epic_store.rs 对齐：读写 .epics.json，派生进度与依赖联动。
 *
 * 数据存储在每个项目目录的 .epics.json，与 .todos.json 平行、完全隔离。
 */
import { promises as fs } from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { backupCorruptFile } from "./store.js";

const FILENAME = ".epics.json";

export type EpicStatus =
  | "draft" | "active" | "verifying" | "observing" | "done" | "blocked" | "paused";

export type StepStage = "pending" | "designed" | "coded" | "verified" | "observed";

export interface EpicLinks {
  doc?: string;
  branch?: string;
  note?: string;
}

export interface StepLinks {
  todo_ref?: string;
  doc?: string;
}

export interface Epic {
  id: string;
  title: string;
  description: string;
  status: EpicStatus;
  priority: string;
  /** 父方案 id。空 = 顶级方案。最多三层：顶级 → 子 → 步骤。 */
  parent_id?: string;
  depends_on: string[];
  blocks: string[];
  links: EpicLinks;
  max_active?: number;
  created_at: string;
  updated_at: string;
}

export interface Step {
  id: string;
  epic_id: string;
  title: string;
  description: string;
  order: number;
  /** 显式依赖的其他步骤 id（可跨 order）。空 = 按 order 串行推进。 */
  depends_on?: string[];
  stage: StepStage;
  links: StepLinks;
  created_at: string;
  updated_at: string;
}

export interface EpicsFile {
  version: number;
  project: string;
  epics: Epic[];
  steps: Step[];
}

const STAGE_PCT: Record<StepStage, number> = {
  pending: 0, designed: 25, coded: 50, verified: 75, observed: 100,
};

const VALID_STATUSES: EpicStatus[] = ["draft", "active", "verifying", "observing", "done", "blocked", "paused"];
const VALID_STAGES: StepStage[] = ["pending", "designed", "coded", "verified", "observed"];

function resolveEpicsPath(cwd: string): string {
  return path.join(cwd, FILENAME);
}

async function fileExists(p: string): Promise<boolean> {
  try { await fs.access(p); return true; } catch { return false; }
}

async function readEpics(cwd: string): Promise<EpicsFile> {
  const filePath = resolveEpicsPath(cwd);
  let raw: string;
  try {
    raw = await fs.readFile(filePath, "utf-8");
  } catch (err) {
    if ((err as NodeJS.ErrnoException).code === "ENOENT") {
      return { version: 1, project: path.basename(cwd), epics: [], steps: [] };
    }
    console.warn(`[epic] failed to read ${filePath}: ${(err as Error).message}`);
    return { version: 1, project: path.basename(cwd), epics: [], steps: [] };
  }
  try {
    return JSON.parse(raw) as EpicsFile;
  } catch {
    // Corrupt file — back it up instead of silently overwriting on next write.
    await backupCorruptFile(filePath, "epic");
    return { version: 1, project: path.basename(cwd), epics: [], steps: [] };
  }
}

async function writeEpics(cwd: string, data: EpicsFile): Promise<void> {
  const filePath = resolveEpicsPath(cwd);
  const tmp = filePath + ".tmp-" + crypto.randomUUID();
  await fs.writeFile(tmp, JSON.stringify(data, null, 2), "utf-8");
  await fs.rename(tmp, filePath);
}

/** 计算方案进度（层级）：有子方案 = 子方案进度均值；否则 = 步骤阶段均值。派生，不存储。 */
export function epicProgress(epicId: string, data: EpicsFile): number {
  return epicProgressVisited(epicId, data, new Set());
}

/**
 * epicProgress 的内部递归实现，携带 visited 集合防御 parent_id 环。
 * 遇到环（某个子方案经 parent_id 链指回自身或祖先）时，把已访问的节点
 * 当作"无子方案"处理（回退到步骤均值分支，返回该节点自身的进度），
 * 从而切断递归，避免栈溢出。.epics.json 是用户可手改的本地文件，
 * 环形 parent_id 必须被容忍而不能让进程崩溃。
 */
function epicProgressVisited(epicId: string, data: EpicsFile, visited: Set<string>): number {
  if (visited.has(epicId)) return 0; // 环检测：当作无贡献，切断递归
  visited.add(epicId);
  const children = data.epics.filter((e) => e.parent_id === epicId);
  if (children.length > 0) {
    const total = children.reduce((sum, c) => sum + epicProgressVisited(c.id, data, visited), 0);
    return Math.floor(total / children.length);
  }
  const epicSteps = data.steps.filter((s) => s.epic_id === epicId);
  if (epicSteps.length === 0) return 0;
  const total = epicSteps.reduce((sum, s) => sum + STAGE_PCT[s.stage], 0);
  return Math.floor(total / epicSteps.length);
}

/**
 * 计算「当前可做步骤」：未完成 + 所有前置（order 更小 ∪ depends_on）都已 observed。
 * 多个可做步骤表示可并行推进。
 */
export function actionableSteps(epicId: string, steps: Step[]): Step[] {
  const epicSteps = steps
    .filter((s) => s.epic_id === epicId)
    .sort((a, b) => a.order - b.order);
  const observedIds = new Set(
    epicSteps.filter((s) => s.stage === "observed").map((s) => s.id)
  );

  return epicSteps.filter((s) => {
    if (s.stage === "observed") return false;
    const orderPrereqs = epicSteps.filter((p) => p.order < s.order).map((p) => p.id);
    const explicit = s.depends_on ?? [];
    return [...orderPrereqs, ...explicit].every((id) => observedIds.has(id));
  });
}

/**
 * 派生依赖联动：
 * 1. 重建 blocks（depends_on 的反向图）
 * 2. 自动 blocked：依赖未完成的 epic → blocked（done/paused 除外）
 * 3. 依赖全部满足且当前是 blocked → 释放回 draft
 */
export function deriveDependencies(data: EpicsFile): void {
  // 1. 清空并重建 blocks
  for (const e of data.epics) e.blocks = [];
  for (const e of data.epics) {
    for (const depId of e.depends_on) {
      const dep = data.epics.find((x) => x.id === depId);
      if (dep && !dep.blocks.includes(e.id)) dep.blocks.push(e.id);
    }
  }

  // 2. 自动 blocked 状态
  const doneIds = new Set(data.epics.filter((e) => e.status === "done").map((e) => e.id));
  for (const e of data.epics) {
    if (e.status === "done" || e.status === "paused") continue;
    const unmet = e.depends_on.filter((d) => !doneIds.has(d));
    if (unmet.length > 0) {
      e.status = "blocked";
    } else if (e.status === "blocked") {
      // 依赖全部满足，释放回 draft（用户决定下一步）
      e.status = "draft";
    }
  }
}

/** 读取 → 修改 → 派生 → 写入。 */
async function mutateEpics<T>(cwd: string, fn: (data: EpicsFile) => T): Promise<{ data: EpicsFile; result: T }> {
  const data = await readEpics(cwd);
  const result = fn(data);
  deriveDependencies(data);
  await writeEpics(cwd, data);
  return { data, result };
}

// ─── Epic operations ───

export async function pushEpic(cwd: string, input: {
  title: string;
  description?: string;
  priority?: string;
  depends_on?: string[];
  parent_id?: string;
}): Promise<{ epic: Epic } | { error: string }> {
  const title = input.title.trim();
  if (!title) return { error: "title is required" };

  // 层级校验：parent_id 必须指向一个存在的顶级方案（最多三层）
  if (input.parent_id) {
    const existing = await readEpics(cwd);
    const parent = existing.epics.find((e) => e.id === input.parent_id);
    if (!parent) return { error: `parent epic '${input.parent_id}' not found` };
    if (parent.parent_id) {
      return { error: "最多三层：子方案不能再有子方案。把步骤直接加到该子方案下。" };
    }
  }

  const now = new Date().toISOString();
  const epic: Epic = {
    id: `epic_${crypto.randomUUID()}`,
    title,
    description: input.description?.trim() ?? "",
    status: "draft",
    priority: input.priority ?? "P1",
    parent_id: input.parent_id,
    depends_on: input.depends_on ?? [],
    blocks: [],
    links: {},
    created_at: now,
    updated_at: now,
  };

  return mutateEpics(cwd, (data) => {
    // 重名检查
    if (data.epics.some((e) => e.title === epic.title)) {
      return { error: `epic with title '${epic.title}' already exists` };
    }
    data.epics.push(epic);
    return { epic };
  }).then(({ result }) => result);
}

export async function updateEpic(cwd: string, id: string, patch: {
  title?: string;
  description?: string;
  status?: EpicStatus;
  priority?: string;
  depends_on?: string[];
  links_doc?: string;
  links_branch?: string;
  links_note?: string;
}): Promise<{ ok: true } | { error: string }> {
  if (patch.status && !VALID_STATUSES.includes(patch.status)) {
    return { error: `invalid status: ${patch.status}` };
  }
  return mutateEpics(cwd, (data) => {
    const epic = data.epics.find((e) => e.id === id);
    if (!epic) return { error: `epic '${id}' not found` };
    if (patch.title !== undefined) epic.title = patch.title;
    if (patch.description !== undefined) epic.description = patch.description;
    if (patch.status !== undefined) epic.status = patch.status;
    if (patch.priority !== undefined) epic.priority = patch.priority;
    if (patch.depends_on !== undefined) epic.depends_on = patch.depends_on;
    if (patch.links_doc !== undefined) epic.links.doc = patch.links_doc || undefined;
    if (patch.links_branch !== undefined) epic.links.branch = patch.links_branch || undefined;
    if (patch.links_note !== undefined) epic.links.note = patch.links_note || undefined;
    epic.updated_at = new Date().toISOString();
    return { ok: true as const };
  }).then(({ result }) => result);
}

// ─── Step operations ───

export async function pushStep(cwd: string, input: {
  epic_id: string;
  title: string;
  description?: string;
  depends_on?: string[];
}): Promise<{ step: Step } | { error: string }> {
  const title = input.title.trim();
  if (!title) return { error: "title is required" };

  return mutateEpics(cwd, (data) => {
    const epic = data.epics.find((e) => e.id === input.epic_id);
    if (!epic) return { error: `epic '${input.epic_id}' not found` };

    const nextOrder = data.steps
      .filter((s) => s.epic_id === input.epic_id)
      .reduce((max, s) => Math.max(max, s.order), 0) + 1;

    const now = new Date().toISOString();
    const step: Step = {
      id: `step_${crypto.randomUUID()}`,
      epic_id: input.epic_id,
      title,
      description: input.description?.trim() ?? "",
      order: nextOrder,
      depends_on: input.depends_on ?? [],
      stage: "pending",
      links: {},
      created_at: now,
      updated_at: now,
    };
    data.steps.push(step);
    epic.updated_at = now;
    return { step };
  }).then(({ result }) => result);
}

export async function updateStep(cwd: string, id: string, patch: {
  title?: string;
  description?: string;
  stage?: StepStage;
  order?: number;
  depends_on?: string[];
  todo_ref?: string;
}): Promise<{ ok: true } | { error: string }> {
  if (patch.stage && !VALID_STAGES.includes(patch.stage)) {
    return { error: `invalid stage: ${patch.stage}` };
  }
  return mutateEpics(cwd, (data) => {
    const step = data.steps.find((s) => s.id === id);
    if (!step) return { error: `step '${id}' not found` };
    if (patch.title !== undefined) step.title = patch.title;
    if (patch.description !== undefined) step.description = patch.description;
    if (patch.stage !== undefined) step.stage = patch.stage;
    if (patch.order !== undefined) step.order = patch.order;
    if (patch.depends_on !== undefined) step.depends_on = patch.depends_on;
    if (patch.todo_ref !== undefined) step.links.todo_ref = patch.todo_ref || undefined;
    step.updated_at = new Date().toISOString();
    // 同步父 epic 的 updated_at
    const epic = data.epics.find((e) => e.id === step.epic_id);
    if (epic) epic.updated_at = step.updated_at;
    return { ok: true as const };
  }).then(({ result }) => result);
}

// ─── Read for listing ───

export async function listEpics(cwd: string): Promise<EpicsFile> {
  return readEpics(cwd);
}

export { VALID_STATUSES, VALID_STAGES };
