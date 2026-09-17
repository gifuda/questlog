/**
 * Todo-package producer (待办租户 G2, plan §4).
 *
 * Turns ONE opted-in todo item into ONE standard TaskPackage via the existing
 * prepare/validate/seal core — no pseudo-epic, no bypassed validation:
 *
 *   strict-read todos → opt-in guards → task_revision/task_key
 *     → ledger reserve (idempotency: one task_key, one object triple)
 *     → real Epic (one per todo, todo_ref-bound Step inside)
 *     → prepare → validate → seal  (executor=manual: execution happens on the
 *       collector platform, NEVER on the local Runner)
 *     → ledger publish
 *
 * A crash between reserve and publish leaves a `preparing` entry — the G4
 * sweep re-anchors by task_key and completes or supersedes it; it never
 * creates a second triple for the same task_key.
 */

import { promises as fs } from "node:fs";
import path from "node:path";
import { TodoItem } from "./schema.js";
import { PackageOrigin } from "./task-package.js";
import { computeTaskRevision, computeTaskKey } from "./task-key.js";
import {
  readLedger,
  findOpenByTodo,
  mutateLedger,
  withLedgerLock,
  mutateLedgerHeld,
  newOperationId,
  nowIso,
  LedgerEntry,
} from "./todo-ledger.js";
import { pushEpic, pushStep, updateStep, updateEpic, listEpics } from "./epic-store.js";
import { prepareTaskPackage, validateTaskPackage, sealTaskPackage } from "./task-package-ops.js";

export interface ProduceOptions {
  /** Report-completion criteria; frozen into task_revision when provided. */
  acceptance_criteria?: string;
  /** Frozen input scope; frozen into task_revision when provided. */
  input_scope?: string;
  /** Per-task execution wall-clock budget (clamped 60–3600s; default 900).
   *  Large research tasks need more than the default — the schedule config's
   *  per_task_timeout_secs flows through here. */
  timeout_seconds?: number;
}

export type ProduceResult =
  | {
      ok: true;
      replay: boolean;
      task_key: string;
      epic_id: string;
      step_id: string;
      package_id: string;
      package_hash: string;
    }
  | { ok: false; error: string; reason_code: string };

/** Strict item-level read: duplicate ids and malformed items block the
 *  channel — the producer never authorizes from a repaired view (§9.1). */
async function strictReadItem(projectDir: string, todoId: string): Promise<TodoItem | { error: string }> {
  const raw = await fs.readFile(path.join(projectDir, ".todos.json"), "utf-8").catch(() => null);
  if (raw === null) return { error: `.todos.json not found in ${projectDir}` };
  let parsed: { items?: unknown };
  try {
    parsed = JSON.parse(raw);
  } catch (e) {
    return { error: `.todos.json is not valid JSON: ${(e as Error).message}` };
  }
  if (!Array.isArray(parsed.items)) return { error: ".todos.json items is not an array" };
  const seen = new Set<string>();
  let match: TodoItem | undefined;
  for (const it of parsed.items as TodoItem[]) {
    if (typeof it?.id !== "string") return { error: "item without string id — strict read blocked" };
    if (seen.has(it.id)) return { error: `duplicate item id '${it.id}' — strict read blocked` };
    seen.add(it.id);
    if (it.id === todoId) match = it;
  }
  if (!match) return { error: `todo '${todoId}' not found` };
  return match;
}

function guard(item: TodoItem): { error: string; reason_code: string } | null {
  if (item.type !== "todo") {
    return { error: `item is type='${item.type}' — auto-processing is todo-only`, reason_code: "not_todo_type" };
  }
  if (item.status !== "active") {
    return { error: `item status='${item.status}' — only active items can start`, reason_code: "not_active" };
  }
  if (!item.auto_process) {
    return { error: "auto_process opt-in is off", reason_code: "not_opted_in" };
  }
  if (item.processing) {
    return { error: "item is being processed manually (processing=true)", reason_code: "manual_processing" };
  }
  return null;
}

/**
 * Cross-check the package store (audit P0): the ledger is the idempotency
 * anchor, but if it is ever lost/rolled back while packages.json still holds
 * todo-origin rows, a produce must NOT silently create a second triple.
 *
 * Called ONLY when the ledger has no entry for this task_key (when it does,
 * the reserve transaction is authoritative). Rows are collapsed by
 * package_id to the highest plan_revision — savePackage keeps superseded
 * history rows that are NOT state. The LEDGER remains the authority for
 * open/terminal decisions: an open package row whose task_key has a TERMINAL
 * ledger entry is stale metadata (G4 marks it), not an open task.
 */
async function crossCheckPackageStore(
  entries: Record<string, import("./todo-ledger.js").LedgerEntry>,
  tenantId: string,
  todoId: string,
  taskKey: string,
): Promise<{ error: string; reason_code: string } | null> {
  const runtime = process.env.QUESTLOG_RUNTIME ?? `${process.env.HOME}/questlog-runtime`;
  const raw = await fs.readFile(path.join(runtime, "packages.json"), "utf-8").catch(() => null);
  if (raw === null) return null;
  type PkgRow = {
    package_id?: string;
    plan_revision?: number;
    status?: string;
    origin?: { tenant_id?: string; todo_id?: string; task_key?: string };
  };
  let parsed: { packages?: PkgRow[] };
  try {
    parsed = JSON.parse(raw);
  } catch {
    return { error: "packages.json unreadable — manual repair required", reason_code: "package_store_unreadable" };
  }
  // Collapse to each package's CURRENT row (highest revision wins).
  const current = new Map<string, PkgRow>();
  for (const p of parsed.packages ?? []) {
    const id = p.package_id ?? "?";
    const prev = current.get(id);
    if (!prev || (p.plan_revision ?? 0) > (prev.plan_revision ?? 0)) current.set(id, p);
  }
  const OPEN = new Set(["draft", "sealed", "ready"]);
  for (const p of current.values()) {
    const o = p.origin;
    if (!o) continue;
    if (o.task_key === taskKey) {
      return {
        error: `package ${p.package_id ?? "?"} already carries this task_key (status ${p.status}) but the ledger has no entry — ledger/package inconsistency, manual repair required`,
        reason_code: "ledger_package_inconsistent",
      };
    }
    if (o.tenant_id === tenantId && o.todo_id === todoId) {
      const rowLedger = o.task_key ? entries[o.task_key] : undefined;
      if (!rowLedger) {
        if (OPEN.has(p.status ?? "")) {
          return {
            error: `open package ${p.package_id ?? "?"} for this todo has no ledger entry (${(o.task_key ?? "").slice(0, 12)}…) — ledger/package inconsistency, manual repair required`,
            reason_code: "ledger_package_inconsistent",
          };
        }
        continue; // terminal row without ledger: history, not state
      }
      if ((rowLedger.state === "preparing" || rowLedger.state === "published") && OPEN.has(p.status ?? "")) {
        return {
          error: `open package ${p.package_id ?? "?"} for this todo under a different task_key (${(o.task_key ?? "").slice(0, 12)}…) — recover or close it first`,
          reason_code: "parallel_task_open",
        };
      }
    }
  }
  return null;
}

export async function produceTodoPackage(
  projectDir: string,
  todoId: string,
  opts: ProduceOptions = {},
): Promise<ProduceResult> {
  // 1. Strict read + authorization guards.
  const item = await strictReadItem(projectDir, todoId);
  if ("error" in item) return { ok: false, error: item.error, reason_code: "strict_read_failed" };
  const blocked = guard(item);
  if (blocked) return { ok: false, ...blocked };

  const tenantId = path.basename(projectDir);
  const consentRevision = item.auto_process_revision ?? 1;
  const taskRevision = computeTaskRevision({
    type: item.type,
    title: item.title,
    content: item.content,
    suggestion: item.suggestion,
    acceptance_criteria: opts.acceptance_criteria,
    input_scope: opts.input_scope,
  });
  const taskKey = computeTaskKey({
    tenant_id: tenantId,
    todo_id: todoId,
    task_revision: taskRevision,
    consent_revision: consentRevision,
  });

  // 2.5 Cross-check the package store BEFORE reserving, but only when the
  //     ledger has no entry for this task_key (when it does, the reserve
  //     transaction below is authoritative). A lost/rolled-back ledger must
  //     not enable a second object triple (audit P0).
  const entriesNow = await readLedger();
  if (!entriesNow[taskKey]) {
    const cross = await crossCheckPackageStore(entriesNow, tenantId, todoId, taskKey);
    if (cross) return { ok: false, ...cross };
  }

  // 2. Ledger reserve (idempotency anchor). One lock-protected transaction:
  //    replay open → return existing; terminal → refuse; parallel → refuse.
  type ReserveOutcome = { kind: "replay" | "terminal" | "parallel" | "reserved"; entry: LedgerEntry };
  const reserve = await mutateLedger<ReserveOutcome>((entries) => {
    const existing = entries[taskKey];
    if (existing) {
      if (existing.state === "preparing" || existing.state === "published") {
        return { result: { kind: "replay" as const, entry: existing }, entries };
      }
      return {
        result: { kind: "terminal" as const, entry: existing },
        entries,
      };
    }
    const parallel = findOpenByTodo(entries, tenantId, todoId);
    if (parallel) {
      return {
        result: { kind: "parallel" as const, entry: parallel },
        entries,
      };
    }
    const now = nowIso();
    const entry: LedgerEntry = {
      task_key: taskKey,
      tenant_id: tenantId,
      todo_id: todoId,
      task_revision: taskRevision,
      consent_revision: consentRevision,
      operation_id: newOperationId(),
      state: "preparing",
      epic_id: "",
      step_id: "",
      package_id: "",
      created_at: now,
      updated_at: now,
    };
    return {
      result: { kind: "reserved" as const, entry },
      entries: { ...entries, [taskKey]: entry },
    };
  });
  if (!reserve.ok) return { ok: false, error: reserve.error, reason_code: "ledger_error" };
  switch (reserve.value.kind) {
    case "replay": {
      const e = reserve.value.entry;
      if (e.state === "preparing") {
        return {
          ok: false,
          error: `task_key has an unfinished 'preparing' entry (operation ${e.operation_id}) — G4 sweep must recover it before a new produce`,
          reason_code: "preparing_unrecovered",
        };
      }
      return {
        ok: true,
        replay: true,
        task_key: e.task_key,
        epic_id: e.epic_id,
        step_id: e.step_id,
        package_id: e.package_id,
        package_hash: "",
      };
    }
    case "terminal":
      return {
        ok: false,
        error: `task_key is ${reserve.value.entry.state} — a finished task never re-runs; re-authorize (toggle auto_process off→on) for a fresh task`,
        reason_code: "task_key_terminal",
      };
    case "parallel":
      return {
        ok: false,
        error: `another open task for this todo exists (task_key ${reserve.value.entry.task_key.slice(0, 12)}…, state ${reserve.value.entry.state}) — recover or close it first`,
        reason_code: "parallel_task_open",
      };
  }

  // 3–6. Creation phase under the LEDGER lock (audit P1): producers are
  //      serialized end-to-end, and every created object id is anchored into
  //      the `preparing` entry the moment it exists — a crash mid-way leaves
  //      the G4 recovery sweep full anchors instead of orphan objects.
  return withLedgerLock(async (): Promise<ProduceResult> => {
    const anchor = async (patch: Partial<LedgerEntry>, reason: string): Promise<string | null> => {
      const r = await mutateLedgerHeld((entries) => {
        const e = entries[taskKey];
        if (!e || e.state !== "preparing") {
          return { error: `ledger entry for ${taskKey.slice(0, 12)}… disappeared or changed — manual repair` };
        }
        Object.assign(e, patch, { updated_at: nowIso() });
        return { result: undefined, entries };
      });
      return r.ok ? null : `${reason}: ${r.error}`;
    };

    // Find or create the todo's real auto-process Epic (one open epic per
    // todo, located by a step's todo_ref — never by title guessing).
    let epicId: string | undefined;
    const epicsResult = await listEpics(projectDir);
    if ("error" in epicsResult) {
      return { ok: false, error: `cannot read .epics.json: ${epicsResult.error}`, reason_code: "epics_read_failed" };
    }
    for (const epic of epicsResult.epics) {
      if (epic.status === "done") continue;
      const bound = epicsResult.steps.some((s) => s.epic_id === epic.id && s.links?.todo_ref === todoId);
      if (bound) {
        epicId = epic.id;
        break;
      }
    }
    if (!epicId) {
      const created = await pushEpic(projectDir, {
        title: `自动处理·${item.title.slice(0, 20)}·${todoId.slice(0, 6)}`,
        description: `QuestLog 待办自动处理（todo_id=${todoId}，task_key=${taskKey.slice(0, 16)}…）。执行发生在采集机平台；本 Epic 仅承载标准包与回执对账。`,
        priority: "P2",
        acceptance: "机器验收通过的研究报告交付（verify_todo_report），Step 最高推进到 verified，不自动 observed。",
      });
      if ("error" in created) {
        return { ok: false, error: `create epic failed: ${created.error}`, reason_code: "epic_create_failed" };
      }
      epicId = created.epic.id;
    }
    // The opt-in authorization binds to this epic's auto-execution switch;
    // the epic is operational (active), not a draft.
    const setSwitch = await updateEpic(projectDir, epicId, { auto_execute: true, status: "active" });
    if ("error" in setSwitch) {
      return { ok: false, error: `set epic auto_execute failed: ${setSwitch.error}`, reason_code: "epic_switch_failed" };
    }
    const anchorErr = await anchor({ epic_id: epicId }, "anchor epic failed");
    if (anchorErr) return { ok: false, error: anchorErr, reason_code: "ledger_anchor_failed" };

    // Create this task's execution step, soft-linked to the todo.
    const stepResult = await pushStep(projectDir, {
      epic_id: epicId,
      title: `夜间执行·${item.title.slice(0, 24)}`,
      description: `task_key=${taskKey}，task_revision=${taskRevision.slice(0, 16)}…，consent_revision=${consentRevision}。`,
    });
    if ("error" in stepResult) {
      return { ok: false, error: `create step failed: ${stepResult.error}`, reason_code: "step_create_failed" };
    }
    const stepId = stepResult.step.id;
    const linkResult = await updateStep(projectDir, stepId, { todo_ref: todoId });
    if ("error" in linkResult) {
      return { ok: false, error: `link todo_ref failed: ${linkResult.error}`, reason_code: "step_link_failed" };
    }
    const stepAnchorErr = await anchor({ step_id: stepId }, "anchor step failed");
    if (stepAnchorErr) return { ok: false, error: stepAnchorErr, reason_code: "ledger_anchor_failed" };

    // Standard package via the existing core. executor=manual: the local
    // Runner never claims it (execution happens on the collector platform).
    // STRICT read-only policy (G3 §4.3): zero write rights (authority all
    // false, max_changed_files 0, no allowed/create paths); the read scope is
    // granted solely by read_paths. Acceptance = trusted read-only command +
    // the native todo:report verifier (§8).
    const origin: PackageOrigin = {
      type: "todo",
      tenant_id: tenantId,
      todo_id: todoId,
      task_revision: taskRevision,
      consent_revision: consentRevision,
      task_key: taskKey,
    };
    const objective = [
      `【自动处理任务】${item.title}`,
      "",
      item.content,
      item.suggestion ? `\n建议产出：${item.suggestion}` : "",
      "",
      "本任务为只读研究类：执行器不得修改任何项目文件；产出为结构化研究报告。",
      opts.acceptance_criteria ? `报告完成条件：${opts.acceptance_criteria}` : "",
      opts.input_scope ? `输入范围：${opts.input_scope}` : "",
    ]
      .filter(Boolean)
      .join("\n");

    const prepared = await prepareTaskPackage(projectDir, {
      epic_id: epicId,
      step_id: stepId,
      objective,
      context_refs: [".todos.json"],
      allowed_paths: [],
      forbidden_paths: [],
      read_paths: ["**"],
      max_changed_files: 0,
      executor_preferred: "manual",
      executor_sandbox: "read-only",
      executor_timeout_seconds: opts.timeout_seconds ?? 900,
      executor_max_attempts: 1,
      acceptance_command_ids: ["verify:readonly"],
      acceptance_verifier_ids: ["todo:report"],
      authority: { auto_edit_worktree: false, auto_commit_candidate: false },
      origin,
    });
    if ("error" in prepared) {
      return { ok: false, error: `prepare failed: ${prepared.error}`, reason_code: "prepare_failed" };
    }
    const hasErrors = prepared.issues.some((i) => i.severity === "error");
    if (hasErrors) {
      return {
        ok: false,
        error: `prepare produced validation errors: ${prepared.issues.filter((i) => i.severity === "error").map((i) => `${i.field}: ${i.message}`).join("; ")}`,
        reason_code: "prepare_invalid",
      };
    }

    const validated = await validateTaskPackage(projectDir, prepared.package_id);
    if ("error" in validated) {
      return { ok: false, error: `validate failed: ${validated.error}`, reason_code: "validate_failed" };
    }
    if (!validated.valid) {
      return {
        ok: false,
        error: `package invalid: ${validated.issues.filter((i) => i.severity === "error").map((i) => `${i.field}: ${i.message}`).join("; ")}`,
        reason_code: "validate_invalid",
      };
    }

    const sealed = await sealTaskPackage(projectDir, prepared.package_id);
    if ("error" in sealed) {
      return { ok: false, error: `seal failed: ${sealed.error}`, reason_code: "seal_failed" };
    }
    const pkgAnchorErr = await anchor({ package_id: prepared.package_id }, "anchor package failed");
    if (pkgAnchorErr) return { ok: false, error: pkgAnchorErr, reason_code: "ledger_anchor_failed" };

    // Publish the ledger entry (lock already held — mutateLedgerHeld).
    const publish = await mutateLedgerHeld((entries) => {
      const e = entries[taskKey];
      if (!e || e.state !== "preparing") {
        return { error: `ledger entry for ${taskKey.slice(0, 12)}… disappeared or changed — manual repair` };
      }
      e.state = "published";
      e.epic_id = epicId!;
      e.step_id = stepId;
      e.package_id = prepared.package_id;
      e.updated_at = nowIso();
      return { result: undefined, entries };
    });
    if (!publish.ok) {
      return { ok: false, error: publish.error, reason_code: "ledger_publish_failed" };
    }

    return {
      ok: true,
      replay: false,
      task_key: taskKey,
      epic_id: epicId,
      step_id: stepId,
      package_id: prepared.package_id,
      package_hash: sealed.package_hash,
    };
  });
}
