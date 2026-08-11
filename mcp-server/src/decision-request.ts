/**
 * DecisionRequest — structured human-feedback object.
 *
 * When the Runner encounters something it can't handle (scope expansion,
 * base SHA drift, repeated failures, need for authorization), it creates a
 * DecisionRequest. Humans answer via any MCP client (list_decision_requests
 * + resolve_decision) or the QuestLog UI — no context copy-paste needed.
 *
 * The request is stored in the Runtime Store (not .epics.json) because it
 * references execution-specific data (logs, diffs, session IDs).
 */

import { promises as fs } from "node:fs";
import path from "node:path";
import os from "node:os";
import crypto from "node:crypto";
import { withFileLock } from "./file-lock.js";

export type DecisionKind =
  | "approval" // needs authorization to proceed
  | "blocked" // scope/path/dependency issue
  | "retry_exhausted" // max attempts reached
  | "git_drift" // base SHA changed
  | "verifier_failed" // acceptance checks failed
  | "adjudication"; // two valid paths, human must decide

export interface DecisionRequest {
  request_id: string;
  epic_id: string;
  step_id: string;
  execution_id: string;
  kind: DecisionKind;
  /** The single question the human must answer. */
  question: string;
  /** Pre-defined options (if applicable). */
  options: string[];
  /** References to evidence (log paths, diff SHAs, commit SHAs). */
  evidence_refs: string[];
  /** Whether this blocks the step from proceeding. */
  blocking: boolean;
  /** Package revision this request applies to. */
  package_revision: number;
  /** Resolution fields (filled when answered). */
  resolved: boolean;
  resolved_by?: string;
  resolved_at?: string;
  resolution?: string;
  created_at: string;
}

interface DecisionsFile {
  version: number;
  requests: DecisionRequest[];
  revision?: number;
}

function runtimeDir(): string {
  if (process.env.QUESTLOG_RUNTIME) return process.env.QUESTLOG_RUNTIME;
  return path.join(os.homedir(), "questlog-runtime");
}

function decisionsPath(): string {
  return path.join(runtimeDir(), "decision-requests.json");
}

async function ensureDir(): Promise<void> {
  await fs.mkdir(runtimeDir(), { recursive: true });
}

async function readDecisionsSafe(): Promise<
  { ok: true; data: DecisionsFile } | { notFound: true } | { error: string }
> {
  const filePath = decisionsPath();
  let raw: string;
  try {
    raw = await fs.readFile(filePath, "utf-8");
  } catch (err) {
    const code = (err as NodeJS.ErrnoException).code;
    if (code === "ENOENT") return { notFound: true };
    return { error: `read error: ${(err as Error).message}` };
  }
  try {
    return { ok: true, data: JSON.parse(raw) as DecisionsFile };
  } catch (err) {
    return { error: `corrupt: ${(err as Error).message}` };
  }
}

async function writeDecisions(data: DecisionsFile): Promise<void> {
  await ensureDir();
  const filePath = decisionsPath();
  data.revision = (data.revision ?? 0) + 1;
  const tmp = `${filePath}.tmp-${crypto.randomUUID()}`;
  await fs.writeFile(tmp, JSON.stringify(data, null, 2), "utf-8");
  await fs.rename(tmp, filePath);
}

/**
 * Create a new DecisionRequest. Called by the Runner when it encounters
 * a situation requiring human judgment.
 */
export async function createDecisionRequest(input: {
  epic_id: string;
  step_id: string;
  execution_id: string;
  kind: DecisionKind;
  question: string;
  options?: string[];
  evidence_refs?: string[];
  blocking?: boolean;
  package_revision?: number;
}): Promise<DecisionRequest> {
  const req: DecisionRequest = {
    request_id: `dec_${crypto.randomUUID()}`,
    epic_id: input.epic_id,
    step_id: input.step_id,
    execution_id: input.execution_id,
    kind: input.kind,
    question: input.question,
    options: input.options ?? [],
    evidence_refs: input.evidence_refs ?? [],
    blocking: input.blocking ?? true,
    package_revision: input.package_revision ?? 1,
    resolved: false,
    created_at: new Date().toISOString(),
  };

  await ensureDir();
  await withFileLock(decisionsPath(), async () => {
    const result = await readDecisionsSafe();
    let file: DecisionsFile;
    if ("ok" in result) {
      file = result.data;
    } else if ("notFound" in result) {
      file = { version: 1, requests: [], revision: 0 };
    } else {
      throw new Error(result.error);
    }
    file.requests.push(req);
    await writeDecisions(file);
  });

  return req;
}

/**
 * List unresolved DecisionRequests (or all, if includeResolved=true).
 * Can filter by epic_id or step_id.
 */
export async function listDecisionRequests(options?: {
  include_resolved?: boolean;
  epic_id?: string;
  step_id?: string;
}): Promise<DecisionRequest[]> {
  const result = await readDecisionsSafe();
  if (!("ok" in result)) return [];
  let reqs = result.data.requests;
  if (!options?.include_resolved) {
    reqs = reqs.filter((r) => !r.resolved);
  }
  if (options?.epic_id) {
    reqs = reqs.filter((r) => r.epic_id === options.epic_id);
  }
  if (options?.step_id) {
    reqs = reqs.filter((r) => r.step_id === options.step_id);
  }
  // Newest first
  return reqs.sort((a, b) => b.created_at.localeCompare(a.created_at));
}

/**
 * Resolve a DecisionRequest by recording the human's answer.
 *
 * Resolution is deliberately non-executing: it never mutates an old attempt,
 * releases a package, or starts the Runner. The user must explicitly retry or
 * revise/reseal the package after the decision has been recorded.
 */
export async function resolveDecision(
  requestId: string,
  resolution: string,
  resolvedBy?: string,
): Promise<{ ok: true } | { error: string }> {
  await ensureDir();
  return withFileLock(
    decisionsPath(),
    async () => {
      const result = await readDecisionsSafe();
      let file: DecisionsFile;
      if ("ok" in result) {
        file = result.data;
      } else if ("notFound" in result) {
        return { error: "no decision requests stored" };
      } else {
        return { error: result.error };
      }
      const req = file.requests.find((r) => r.request_id === requestId);
      if (!req) return { error: `decision request '${requestId}' not found` };
      if (req.resolved) return { error: `already resolved at ${req.resolved_at}` };
      req.resolved = true;
      req.resolution = resolution;
      req.resolved_by = resolvedBy ?? "mcp-user";
      req.resolved_at = new Date().toISOString();
      await writeDecisions(file);
      return { ok: true as const };
    },
    10_000,
  ).catch((err: Error) => ({ error: err.message }));
}
