/**
 * Decision action executor — the side-effect half of resolve_decision.
 *
 * Mirrors the Rust mapping (commands.rs resolve_decision_request +
 * execution_store.rs create_retry_attempt/release_lease) 1:1, so a recorded
 * resolution has the same effect no matter which end (MCP conversation or
 * QuestLog UI) records it:
 *
 *   retry                               → create a child attempt (idempotent,
 *                                         budget-capped, package back to ready)
 *   cancel                              → mark the execution cancelled
 *   revise_package / transfer_manual / reject → cancel the lingering execution
 *   anything else (validated upstream)  → record only, no automatic action
 *
 * CONTRACT: if the Rust mapping changes, change this file in the same commit.
 */

import { promises as fs } from "node:fs";
import path from "node:path";
import os from "node:os";
import crypto from "node:crypto";
import { withFileLock } from "./file-lock.js";
import { getPackage, updatePackageStatus } from "./runtime-store.js";

/** Rust ExecutionStatus serializes snake_case — keep these exact. */
export const RETRYABLE_STATUSES = new Set([
  "failed",
  "blocked",
  "timed_out",
  "cancelled",
  "stale",
]);

interface ExecutionRow {
  id: string;
  package_id: string;
  epic_id: string;
  step_id: string;
  project_path: string;
  status: string;
  lease: unknown;
  cli_session: string | null;
  worktree_path: string | null;
  branch: string | null;
  base_sha: string;
  candidate_sha: string | null;
  attempts: number;
  last_error: string | null;
  created_at: number;
  updated_at: number;
  attempt_no?: number;
  parent_attempt_id?: string;
  bundle_path?: string;
  executor?: string;
}

interface ExecutionsFile {
  version: number;
  executions: ExecutionRow[];
  revision?: number;
}

function executionsPath(): string {
  const dir = process.env.QUESTLOG_RUNTIME ?? path.join(os.homedir(), "questlog-runtime");
  return path.join(dir, "executions.json");
}

async function readExecutionsLocked(): Promise<ExecutionsFile> {
  const raw = await fs.readFile(executionsPath(), "utf-8");
  return JSON.parse(raw) as ExecutionsFile;
}

async function writeExecutionsLocked(data: ExecutionsFile): Promise<void> {
  data.revision = (data.revision ?? 0) + 1;
  const tmp = executionsPath() + ".tmp-" + crypto.randomUUID();
  await fs.writeFile(tmp, JSON.stringify(data, null, 2), "utf-8");
  await fs.rename(tmp, executionsPath());
}

export type DecisionActionResult = { ok: true; detail: string } | { error: string };

/**
 * Execute the action mapped to a resolution. Called AFTER the resolution
 * has been recorded (both ends record-then-act).
 */
export async function executeDecisionAction(
  executionId: string,
  resolution: string,
): Promise<DecisionActionResult> {
  const action = resolution.trim().toLowerCase();
  switch (action) {
    case "retry":
      return retryAttempt(executionId);
    case "cancel":
      return cancelExecution(executionId, "cancelled via decision request");
    case "revise_package":
    case "transfer_manual":
    case "reject":
      // These need human work outside the Runner — cancel the lingering
      // execution so it doesn't stay in the queue.
      return cancelExecution(executionId, `resolved as '${action}'`);
    default:
      return { ok: true, detail: "recorded (no automatic action)" };
  }
}

/** Mirror of Rust execution_store::create_retry_attempt. */
async function retryAttempt(parentId: string): Promise<DecisionActionResult> {
  return withFileLock(executionsPath(), async (): Promise<DecisionActionResult> => {
    const data = await readExecutionsLocked();
    const parent = data.executions.find((e) => e.id === parentId);
    if (!parent) return { error: `execution '${parentId}' not found` };
    if (!RETRYABLE_STATUSES.has(parent.status)) {
      return { error: `execution '${parentId}' is not in a retryable terminal state (${parent.status})` };
    }
    // Idempotency under the same lock: an already-queued/running child wins.
    const existing = data.executions.find(
      (e) =>
        e.parent_attempt_id === parentId &&
        (e.status === "queued" || e.status === "running"),
    );
    if (existing) {
      return { ok: true as const, detail: `retry already scheduled (${existing.id})` };
    }
    // Budget cap INSIDE the lock (re-read the package now) — no snapshot
    // TOCTOU between the cap check and the child creation.
    const pkgResult = await getPackage(parent.package_id);
    if ("error" in pkgResult) return { error: pkgResult.error };
    const maxAttempts = Math.max(1, pkgResult.pkg.executor?.max_attempts ?? 2);
    if ((parent.attempt_no ?? 1) >= maxAttempts) {
      return {
        error: `maximum attempts reached (${maxAttempts}) — revise the package before retrying`,
      };
    }
    const now = Math.floor(Date.now() / 1000);
    const childId = `exec_${crypto.randomUUID()}`;
    data.executions.push({
      id: childId,
      package_id: parent.package_id,
      epic_id: parent.epic_id,
      step_id: parent.step_id,
      project_path: parent.project_path,
      status: "queued",
      lease: null,
      cli_session: null,
      worktree_path: null,
      branch: null,
      base_sha: parent.base_sha,
      candidate_sha: null,
      attempts: 0,
      last_error: null,
      created_at: now,
      updated_at: now,
      attempt_no: (parent.attempt_no ?? 1) + 1,
      parent_attempt_id: parent.id,
    });
    await writeExecutionsLocked(data);
    // Rust flips the package back to ready so the next poll re-claims it.
    await updatePackageStatus(parent.package_id, "ready");
    return { ok: true as const, detail: `retry scheduled (${childId})` };
  }).catch((err: Error) => ({ error: err.message }) as DecisionActionResult);
}

/** Mirror of Rust execution_store::release_lease(_, Cancelled, msg) —
 *  including the terminal-evidence guard: never overwrite a truly terminal
 *  state (passed/failed/cancelled/timed_out). */
async function cancelExecution(
  executionId: string,
  message: string,
): Promise<DecisionActionResult> {
  return withFileLock(executionsPath(), async () => {
    const data = await readExecutionsLocked();
    const exec = data.executions.find((e) => e.id === executionId);
    if (!exec) return { error: `execution '${executionId}' not found` };
    if (["passed", "failed", "cancelled", "timed_out"].includes(exec.status)) {
      return {
        error: `execution '${executionId}' is already terminal (${exec.status}) — refusing to overwrite`,
      };
    }
    exec.status = "cancelled";
    exec.lease = null;
    exec.last_error = message;
    exec.updated_at = Math.floor(Date.now() / 1000);
    await writeExecutionsLocked(data);
    return { ok: true as const, detail: `execution ${executionId} cancelled` };
  }).catch((err: Error) => ({ error: err.message }) as DecisionActionResult);
}
