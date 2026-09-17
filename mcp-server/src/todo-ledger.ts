/**
 * Todo-task ledger (待办租户 plan §4.4) — the idempotency anchor for the
 * standard-package producer.
 *
 * Maps task_key → the objects created for that one-shot authorization, with a
 * preparing/published/closed lifecycle. `preparing` entries after a crash are
 * recovered by the G4 sweep (it re-anchors by task_key, never creating a
 * second Epic/Step/package). Lives in the local Runtime Store
 * (`todo-tasks.json`), never in the project's .epics.json.
 *
 * Guarantees:
 * - one task_key ⇔ at most one (epic_id, step_id, package_id) triple
 * - a terminal (closed/superseded) task_key is never reopened
 * - a (tenant_id, todo_id) with an open task cannot start a parallel one
 */

import { promises as fs } from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { withFileLock } from "./file-lock.js";

export type LedgerState = "preparing" | "published" | "closed" | "superseded";

export interface LedgerEntry {
  task_key: string;
  tenant_id: string;
  todo_id: string;
  task_revision: string;
  consent_revision: number;
  /** Distinguishes retries of the same production operation. */
  operation_id: string;
  state: LedgerState;
  epic_id: string;
  step_id: string;
  package_id: string;
  created_at: string;
  updated_at: string;
}

interface LedgerFile {
  version: 1;
  entries: Record<string, LedgerEntry>;
}

function runtimeDir(): string {
  return process.env.QUESTLOG_RUNTIME ?? `${process.env.HOME}/questlog-runtime`;
}

function ledgerPath(): string {
  return path.join(runtimeDir(), "todo-tasks.json");
}

async function readRaw(): Promise<LedgerFile> {
  const raw = await fs.readFile(ledgerPath(), "utf-8").catch(() => null);
  if (!raw) return { version: 1, entries: {} };
  const parsed = JSON.parse(raw) as LedgerFile;
  if (!parsed || typeof parsed !== "object" || typeof parsed.entries !== "object") {
    // A corrupt ledger BLOCKS (fail-closed) rather than silently resetting —
    // an empty ledger would let the producer duplicate every task.
    throw new Error(`todo-task ledger unreadable: ${ledgerPath()} — manual repair required`);
  }
  return parsed;
}

async function writeRaw(data: LedgerFile): Promise<void> {
  await fs.mkdir(runtimeDir(), { recursive: true });
  const tmp = ledgerPath() + `.tmp-${crypto.randomUUID()}`;
  await fs.writeFile(tmp, JSON.stringify(data, null, 2), "utf-8");
  await fs.rename(tmp, ledgerPath());
}

export async function readLedger(): Promise<Record<string, LedgerEntry>> {
  return (await readRaw()).entries;
}

export function isOpen(entry: LedgerEntry): boolean {
  return entry.state === "preparing" || entry.state === "published";
}

/** Any open entry for the same (tenant_id, todo_id) — parallel-start blocker. */
export function findOpenByTodo(
  entries: Record<string, LedgerEntry>,
  tenantId: string,
  todoId: string,
): LedgerEntry | undefined {
  return Object.values(entries).find(
    (e) => e.tenant_id === tenantId && e.todo_id === todoId && isOpen(e),
  );
}

/**
 * Atomically upsert an entry under the ledger lock. The mutator receives the
 * current entries and returns either the new entry map or an error — an
 * error aborts with no write.
 */
export async function mutateLedger<T>(
  fn: (entries: Record<string, LedgerEntry>) => { result: T; entries: Record<string, LedgerEntry> } | { error: string },
): Promise<{ ok: true; value: T } | { ok: false; error: string }> {
  // The lock file lives beside the ledger — make sure the runtime dir exists
  // before trying to create the lock (first-ever call).
  await fs.mkdir(runtimeDir(), { recursive: true });
  return withFileLock(ledgerPath(), async () => {
    const current = await readRaw();
    const mutated = fn(current.entries);
    if ("error" in mutated) return { ok: false, error: mutated.error };
    await writeRaw({ version: 1, entries: mutated.entries });
    return { ok: true, value: mutated.result };
  }, 10_000);
}

export function newOperationId(): string {
  return `op_${crypto.randomUUID()}`;
}

export function nowIso(): string {
  return new Date().toISOString();
}

/**
 * Hold the ledger lock for an arbitrary async section (the producer's
 * object-creation phase). Nothing else may take the ledger lock inside.
 */
export async function withLedgerLock<T>(fn: () => Promise<T>): Promise<T> {
  await fs.mkdir(runtimeDir(), { recursive: true });
  return withFileLock(ledgerPath(), fn, 10_000);
}

/**
 * Lock-free ledger entry update — ONLY legal while holding the ledger lock
 * via withLedgerLock (re-entrant O_EXCL acquisition would self-deadlock).
 * Used to record created object ids into a `preparing` entry step by step,
 * so a crash mid-creation leaves the recovery sweep full anchors.
 */
export async function mutateLedgerHeld(
  fn: (entries: Record<string, LedgerEntry>) => { error: string } | { result: undefined; entries: Record<string, LedgerEntry> },
): Promise<{ ok: true } | { ok: false; error: string }> {
  const current = await readRaw();
  const mutated = fn(current.entries);
  if ("error" in mutated) return { ok: false, error: mutated.error };
  await writeRaw({ version: 1, entries: mutated.entries });
  return { ok: true };
}
