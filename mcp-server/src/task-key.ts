/**
 * Task identity for the 待办租户 pipeline (plan §5.2).
 *
 * - `task_revision`  = SHA-256 of the frozen task CONTENT — what the
 *   authorized work is. Any edit to the content (or to the explicit
 *   acceptance criteria / input scope) produces a new revision, and a new
 *   task_key.
 * - `task_key`       = SHA-256(tenant_id, todo_id, task_revision,
 *   consent_revision) — the one-shot work identity of ONE authorization.
 *   A terminal task_key never re-runs; re-authorization bumps
 *   consent_revision, yielding a fresh task_key.
 *
 * Canonical form: JSON with recursively sorted object keys, so the hash is
 * independent of field insertion order (mirrors the package-hash canonical
 * form; a Rust mirror, if ever needed, must byte-match this shape).
 */

import crypto from "node:crypto";

function sortedCanonical(value: unknown): string {
  const canonicalize = (v: unknown): unknown => {
    if (Array.isArray(v)) return v.map(canonicalize);
    if (v && typeof v === "object") {
      const out: Record<string, unknown> = {};
      for (const key of Object.keys(v as Record<string, unknown>).sort()) {
        out[key] = canonicalize((v as Record<string, unknown>)[key]);
      }
      return out;
    }
    return v;
  };
  return JSON.stringify(canonicalize(value));
}

export interface TaskContent {
  type: string;
  title: string;
  content: string;
  suggestion: string;
  /**
   * The report-completion criteria the acceptance will check against.
   * Empty string means "as declared inside content" (v1 default).
   */
  acceptance_criteria?: string;
  /** The frozen input scope. Empty string means "the repo snapshot at seal". */
  input_scope?: string;
}

export function computeTaskRevision(c: TaskContent): string {
  const canonical = sortedCanonical({
    type: c.type,
    title: c.title,
    content: c.content,
    suggestion: c.suggestion,
    acceptance_criteria: c.acceptance_criteria ?? "",
    input_scope: c.input_scope ?? "",
  });
  return crypto.createHash("sha256").update(canonical).digest("hex");
}

export function computeTaskKey(input: {
  tenant_id: string;
  todo_id: string;
  task_revision: string;
  consent_revision: number;
}): string {
  const canonical = sortedCanonical(input);
  return crypto.createHash("sha256").update(canonical).digest("hex");
}
