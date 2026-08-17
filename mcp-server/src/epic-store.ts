/**
 * 方案追踪模块的存储层（MCP 侧）。
 * 与 Rust epic_store.rs 对齐：读写 .epics.json，派生进度与依赖联动。
 *
 * 数据存储在每个项目目录的 .epics.json，与 .todos.json 平行、完全隔离。
 *
 * Fail-closed policy: only ENOENT (file missing) returns an empty default —
 * that is safe because there is no data to lose. Permission errors, IO errors,
 * and corrupt JSON all propagate as errors, never falling back to empty data
 * that would be silently written over the real file on the next mutation.
 */
import { promises as fs } from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { backupCorruptFile } from "./store.js";
import { withFileLock } from "./file-lock.js";

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
  /** 验收条件（自由文本，由创建方案的对话显式填写）。人类可读，代码不解析。 */
  acceptance?: string;
  /**
   * Runner 按方案粒度的自动执行开关（镜像 Rust 端 Epic.auto_execute）。
   * - undefined/缺失 → 继承项目级开关
   * - true → 项目开关开启时自动执行
   * - false → 永不自动执行（即使项目开关开了）
   */
  auto_execute?: boolean;
  /** 收尾原因。progress<100% 提前标 done 时强制必填。 */
  close_reason?: string;
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
  /** TaskPackage 摘要（seal 后回写）。只存可移植信息，不存绝对路径或 CLI session。 */
  package?: {
    package_id: string;
    plan_revision: number;
    package_hash: string;
    objective: string;
    executor: string;
    status: string;
  };
  created_at: string;
  updated_at: string;
}

export interface EpicsFile {
  version: number;
  project: string;
  epics: Epic[];
  steps: Step[];
  /**
   * Monotonic revision counter for CAS (compare-and-swap) concurrency control.
   * Incremented on every write. Old files without this field default to 0
   * on read, then get the field written on the next mutation (backward compat).
   */
  revision?: number;
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

/**
 * Read .epics.json with fail-closed semantics.
 * Returns `{ ok, data }` on success, `{ notFound }` on ENOENT (safe to create),
 * or `{ error }` on permission/IO/corrupt — the caller must NOT write over
 * the file in the error case, or it would destroy user data.
 */
export async function readEpicsSafe(cwd: string): Promise<
  { ok: true; data: EpicsFile } | { notFound: true } | { error: string }
> {
  const filePath = resolveEpicsPath(cwd);
  let raw: string;
  try {
    raw = await fs.readFile(filePath, "utf-8");
  } catch (err) {
    const code = (err as NodeJS.ErrnoException).code;
    if (code === "ENOENT") return { notFound: true };
    // Permission, IO, or any other non-ENOENT error → fail-closed.
    return { error: `read error (${code ?? "unknown"}): ${(err as Error).message}` };
  }
  try {
    return { ok: true, data: JSON.parse(raw) as EpicsFile };
  } catch (err) {
    // Corrupt JSON — preserve as backup, then signal error (fail-closed).
    await backupCorruptFile(filePath, "epic");
    return { error: `corrupt JSON (backed up): ${(err as Error).message}` };
  }
}

/**
 * Convenience wrapper: returns parsed data or empty default on ENOENT.
 * Throws on permission/IO/corrupt errors — callers that need graceful
 * degradation should use readEpicsSafe instead.
 */
export async function readEpics(cwd: string): Promise<EpicsFile> {
  const result = await readEpicsSafe(cwd);
  if ("ok" in result) return result.data;
  if ("notFound" in result) {
    return { version: 1, project: path.basename(cwd), epics: [], steps: [] };
  }
  throw new Error(result.error);
}

/**
 * Write .epics.json atomically. Bumps the revision counter for CAS.
 * This does NOT acquire a lock — callers that need cross-process safety
 * must use mutateEpics (which wraps read+write in withFileLock).
 */
async function writeEpics(cwd: string, data: EpicsFile): Promise<void> {
  const filePath = resolveEpicsPath(cwd);
  // Bump revision on every write. The first write to an old (revision-less)
  // file sets revision to 1.
  data.revision = (data.revision ?? 0) + 1;
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
    .sort((a, b) => a.order - b.order || a.created_at.localeCompare(b.created_at));
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

/**
 * 读取 → 修改 → 派生 → 写入，全程持有跨进程文件锁。
 * Fail-closed: if the file exists but is corrupt or unreadable, the mutation
 * is rejected with an error result rather than writing over the real data.
 * The cross-process lock ensures Node MCP, Tauri UI, and the Runner never
 * race on the same file.
 */
async function mutateEpics<T>(
  cwd: string,
  fn: (data: EpicsFile) => T,
): Promise<{ data: EpicsFile; result: T } | { error: string }> {
  const filePath = resolveEpicsPath(cwd);
  return withFileLock(
    filePath,
    async () => {
      const readResult = await readEpicsSafe(cwd);
      let data: EpicsFile;
      if ("ok" in readResult) {
        data = readResult.data;
      } else if ("notFound" in readResult) {
        data = { version: 1, project: path.basename(cwd), epics: [], steps: [], revision: 0 };
      } else {
        return { error: readResult.error };
      }
      const result = fn(data);
      deriveDependencies(data);
      await writeEpics(cwd, data);
      return { data, result };
    },
    // Longer timeout for .epics.json mutations — these are less frequent but
    // may coincide with Tauri UI doing a full re-derive.
    10_000,
  );
}

// ─── Epic operations ───

export async function pushEpic(cwd: string, input: {
  title: string;
  description?: string;
  priority?: string;
  depends_on?: string[];
  parent_id?: string;
  acceptance?: string;
}): Promise<{ epic: Epic } | { error: string }> {
  const title = input.title.trim();
  if (!title) return { error: "title is required" };

  // 层级校验：parent_id 必须指向一个存在的顶级方案（最多三层）
  if (input.parent_id) {
    // Fail-closed: parent check against a corrupt file must reject, not
    // silently treat the file as empty and bypass hierarchy validation.
    const readResult = await readEpicsSafe(cwd);
    if ("error" in readResult) return { error: readResult.error };
    if ("notFound" in readResult) return { error: `parent epic '${input.parent_id}' not found` };
    const parent = readResult.data.epics.find((e) => e.id === input.parent_id);
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
    acceptance: input.acceptance?.trim() ?? "",
    close_reason: "",
    created_at: now,
    updated_at: now,
  };

  const res = await mutateEpics(cwd, (data) => {
    // 重名检查
    if (data.epics.some((e) => e.title === epic.title)) {
      return { error: `epic with title '${epic.title}' already exists` };
    }
    data.epics.push(epic);
    return { epic };
  });
  if ("error" in res) return { error: res.error };
  return res.result;
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
  acceptance?: string;
  close_reason?: string;
}): Promise<{ ok: true } | { error: string }> {
  if (patch.status && !VALID_STATUSES.includes(patch.status)) {
    return { error: `invalid status: ${patch.status}` };
  }
  const res = await mutateEpics(cwd, (data) => {
    // done 门禁：进度<100% 且 close_reason 空 → 拒绝（防止"75% 却显示完成"）。
    // progress 仅由 step stage 派生，不受 status 影响，改动顺序无妨。
    if (patch.status === "done") {
      const progress = epicProgress(id, data);
      if (progress < 100) {
        const reason = (patch.close_reason ?? "").trim();
        if (!reason) {
          return {
            error: `进度 ${progress}% 未达 100%，提前收尾需填写 close_reason（收尾原因，如"方向调整"）`,
          };
        }
      }
    }
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
    if (patch.acceptance !== undefined) epic.acceptance = patch.acceptance;
    if (patch.close_reason !== undefined) epic.close_reason = patch.close_reason;
    epic.updated_at = new Date().toISOString();
    return { ok: true as const };
  });
  if ("error" in res) return { error: res.error };
  return res.result;
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

  const res = await mutateEpics(cwd, (data) => {
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
  });
  if ("error" in res) return { error: res.error };
  return res.result;
}

export async function updateStep(cwd: string, id: string, patch: {
  title?: string;
  description?: string;
  stage?: StepStage;
  order?: number;
  depends_on?: string[];
  todo_ref?: string;
  package?: Step["package"];
}): Promise<{ ok: true } | { error: string }> {
  if (patch.stage && !VALID_STAGES.includes(patch.stage)) {
    return { error: `invalid stage: ${patch.stage}` };
  }
  const res = await mutateEpics(cwd, (data) => {
    const step = data.steps.find((s) => s.id === id);
    if (!step) return { error: `step '${id}' not found` };
    if (patch.title !== undefined) step.title = patch.title;
    if (patch.description !== undefined) step.description = patch.description;
    if (patch.stage !== undefined) step.stage = patch.stage;
    if (patch.order !== undefined) step.order = patch.order;
    if (patch.depends_on !== undefined) step.depends_on = patch.depends_on;
    if (patch.todo_ref !== undefined) step.links.todo_ref = patch.todo_ref || undefined;
    if (patch.package !== undefined) step.package = patch.package;
    step.updated_at = new Date().toISOString();
    // 同步父 epic 的 updated_at
    const epic = data.epics.find((e) => e.id === step.epic_id);
    if (epic) epic.updated_at = step.updated_at;
    return { ok: true as const };
  });
  if ("error" in res) return { error: res.error };
  return res.result;
}

// ─── Read for listing ───

export async function listEpics(cwd: string): Promise<EpicsFile | { error: string }> {
  const result = await readEpicsSafe(cwd);
  if ("ok" in result) {
    // S5: single-exit stable (order, created_at) ordering — never expose
    // JSON insertion order (a later-inserted step with a smaller order
    // must list first, matching the Rust backend and the Runner's order
    // semantics).
    result.data.steps.sort(
      (a, b) => a.order - b.order || a.created_at.localeCompare(b.created_at),
    );
    return result.data;
  }
  if ("notFound" in result) {
    return { version: 1, project: path.basename(cwd), epics: [], steps: [] };
  }
  return { error: result.error };
}

/**
 * Delete a step by ID. Used for rollback when push_step_with_package fails
 * mid-flow (step created but prepare/seal failed). Returns ok or error.
 */
export async function deleteStep(cwd: string, stepId: string): Promise<{ ok: true } | { error: string }> {
  const res = await mutateEpics(cwd, (data) => {
    const idx = data.steps.findIndex((s) => s.id === stepId);
    if (idx === -1) return { error: `step '${stepId}' not found` };
    data.steps.splice(idx, 1);
    return { ok: true as const };
  });
  if ("error" in res) return { error: res.error };
  return res.result;
}

/**
 * Delete an epic AND cascade-delete all of its steps in one locked mutation.
 * Rollback-oriented: publishPlan uses this to remove a half-published epic
 * whose packages were already cancelled. It deliberately has no status guard
 * (the caller is responsible for making packages unclaimable FIRST), so it is
 * not exposed as a general-purpose user action.
 */
export async function deleteEpic(
  cwd: string,
  epicId: string,
): Promise<{ ok: true; deletedSteps: number } | { error: string }> {
  const res = await mutateEpics(cwd, (data) => {
    const idx = data.epics.findIndex((e) => e.id === epicId);
    if (idx === -1) return { error: `epic '${epicId}' not found` };
    data.epics.splice(idx, 1);
    const before = data.steps.length;
    data.steps = data.steps.filter((s) => s.epic_id !== epicId);
    return { ok: true as const, deletedSteps: before - data.steps.length };
  });
  if ("error" in res) return { error: res.error };
  return res.result;
}

export { VALID_STATUSES, VALID_STAGES };
