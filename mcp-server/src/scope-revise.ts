/**
 * scope-revise — revise_scope 决议的自动后半段（2026-10-14 用户裁决：
 * "要么自动执行要么提示"；点击即人工授权，机器补机械半段）。
 *
 * 流程：读执行 → 读旧包 → 读 attempts/<exec>/scene/changed-paths.json 的
 * 现场分类 → 取 unauthorized_* 路径与原范围做并集（已存在文件→修改权，
 * 新文件→新建权）→ 走标准 prepare→validate→seal 出修订包。Runner 领取
 * 修订包后首次尝试自动回放现场（S2 语义：same base only），工作不重干。
 *
 * 护栏：越界路径 >10 条拒绝自动（人工修订）；越界为 0 视为无需修订；
 * 自动失败一律 fail-open 回退到旧行为（包保持 blocked 等人工），绝不
 * 半途放开范围。范围只做并集——绝不收窄、绝不新增执行权限字段。
 */
import { promises as fs } from "node:fs";
import path from "node:path";
import os from "node:os";
import { getPackage } from "./runtime-store.js";
import {
  prepareTaskPackage,
  validateTaskPackage,
  sealTaskPackage,
} from "./task-package-ops.js";
import type { PrepareInput } from "./task-package-ops.js";

/** 越界路径超过此数拒绝自动修订（执行器大面积越界=目标本身有问题）。 */
export const MAX_AUTO_VIOLATED = 10;

export interface ChangedPathEntry {
  path: string;
  category: string;
  existed_in_base: boolean;
  untracked: boolean;
}

export interface OldScope {
  allowed_paths?: string[];
  create_paths?: string[];
  forbidden_paths?: string[];
  max_changed_files?: number;
}

/** 并集范围纯函数（可测）：已存在文件并入修改权，新文件并入新建权，去重保序。 */
export function unionScope(
  old: OldScope,
  violated: ChangedPathEntry[],
): { allowed_paths: string[]; create_paths: string[] } {
  const merge = (base: string[] | undefined, add: string[]): string[] =>
    Array.from(new Set([...(base ?? []), ...add]));
  return {
    allowed_paths: merge(
      old.allowed_paths,
      violated.filter((v) => v.existed_in_base).map((v) => v.path),
    ),
    create_paths: merge(
      old.create_paths,
      violated.filter((v) => !v.existed_in_base).map((v) => v.path),
    ),
  };
}

export interface AutoResealResult {
  ok: boolean;
  note: string;
  package_id?: string;
}

function runtimeDir(): string {
  return process.env.QUESTLOG_RUNTIME ?? `${os.homedir()}/questlog-runtime`;
}

export async function autoResealRevision(
  executionId: string,
): Promise<AutoResealResult> {
  // 1) 执行记录 → package_id
  let exec: { id: string; package_id: string } | undefined;
  try {
    const raw = await fs.readFile(
      path.join(runtimeDir(), "executions.json"),
      "utf-8",
    );
    const data = JSON.parse(raw) as { executions?: { id: string; package_id: string }[] };
    exec = (data.executions ?? []).find((e) => e.id === executionId);
  } catch {
    /* fallthrough */
  }
  if (!exec) return { ok: false, note: `execution ${executionId} not found` };

  // 2) 旧包
  const got = await getPackage(exec.package_id);
  if ("error" in got) return { ok: false, note: `old package: ${got.error}` };
  const old = got.pkg as {
    epic_id: string;
    step_id: string;
    objective: string;
    project_path: string;
    scope?: OldScope;
    executor?: {
      preferred?: string;
      sandbox?: "workspace-write" | "read-only";
      timeout_seconds?: number;
      max_attempts?: number;
    };
    acceptance?: { type: string; command_id?: string; verifier_id?: string }[];
    acceptance_command_ids?: string[] | null;
  };

  // 3) 现场分类
  let violated: ChangedPathEntry[] = [];
  try {
    const raw = await fs.readFile(
      path.join(runtimeDir(), "attempts", executionId, "scene", "changed-paths.json"),
      "utf-8",
    );
    const scene = JSON.parse(raw) as { paths?: ChangedPathEntry[] };
    violated = (scene.paths ?? []).filter((p) => (p.category ?? "").startsWith("unauthorized"));
  } catch {
    return { ok: false, note: "scene/changed-paths.json unreadable — manual revision required" };
  }
  if (violated.length === 0)
    return { ok: false, note: "no unauthorized paths — nothing to revise" };
  if (violated.length > MAX_AUTO_VIOLATED)
    return { ok: false, note: `violated paths ${violated.length} > cap ${MAX_AUTO_VIOLATED} — manual revision required` };

  // 4) 并集范围 + 5) 标准封包管线（服务器解析 base/路径，无法伪造）
  const scope = unionScope(old.scope ?? {}, violated);
  const input: PrepareInput = {
    epic_id: old.epic_id,
    step_id: old.step_id,
    objective: old.objective,
    allowed_paths: scope.allowed_paths,
    create_paths: scope.create_paths,
    forbidden_paths: old.scope?.forbidden_paths ?? [],
    max_changed_files: old.scope?.max_changed_files,
    executor_preferred: old.executor?.preferred,
    executor_timeout_seconds: old.executor?.timeout_seconds,
    executor_max_attempts: old.executor?.max_attempts,
    executor_sandbox: old.executor?.sandbox,
    acceptance_command_ids: old.acceptance_command_ids
      ?? (old.acceptance ?? [])
          .filter((a: { type: string; command_id?: string }) => a.type === "command" && a.command_id)
          .map((a: { command_id?: string }) => a.command_id!)
      ?? undefined,
    project_dir: old.project_path,
  };
  try {
    const prep = await prepareTaskPackage(old.project_path, input);
    if ("error" in prep) return { ok: false, note: `prepare: ${prep.error}` };
    const val = await validateTaskPackage(old.project_path, prep.package_id);
    if ("error" in val) return { ok: false, note: `validate: ${val.error}` };
    const errors = val.issues.filter((i: { severity: string; message: string }) => i.severity === "error");
    if (errors.length > 0)
      return { ok: false, note: `validate: ${errors.map((e: { message: string }) => e.message).join("; ")}` };
    const sealed = await sealTaskPackage(old.project_path, prep.package_id);
    if ("error" in sealed) return { ok: false, note: `seal: ${sealed.error}` };
    return {
      ok: true,
      package_id: sealed.package_id,
      note: `auto-revision sealed (+${violated.map((v) => v.path).join(", ")}); scene restores on first attempt`,
    };
  } catch (e) {
    return { ok: false, note: `pipeline: ${e instanceof Error ? e.message : String(e)}` };
  }
}
