/**
 * Todo research-report contract + native verifier (待办租户 G3, plan §8).
 *
 * The executing model produces a structured report object at the END of its
 * stdout, inside a fenced ```json block marked schema_version
 * "questlog-todo-report". The report's own claim of DONE is just a claim —
 * `verifyTodoReport` is the host-side check that decides whether it may
 * auto-close the todo. It NEVER executes anything from the report content
 * (no commands, no URL fetches); evidence references are checked against a
 * caller-supplied snapshot file list only.
 *
 * Verdict mapping (plan §5.3):
 *   accepted_report — DONE + structurally valid + refs exist + no unresolved
 *   cannot_auto    — model-declared CANNOT_AUTO with a reason (business outcome)
 *   needs_review   — NEEDS_REVIEW, or DONE with defects a human must judge
 *   rejected       — structural failure; the report cannot be trusted at all
 */

export const REPORT_SCHEMA_VERSION = "questlog-todo-report" as const;

export type ReportVerdict = "DONE" | "CANNOT_AUTO" | "NEEDS_REVIEW";

export interface TodoReport {
  schema_version: typeof REPORT_SCHEMA_VERSION;
  /** Must equal the producing task's task_key — binds report to task. */
  task_key: string;
  verdict: ReportVerdict;
  summary: string;
  findings: string[];
  /** References like "src/lib.rs:42" or "src/lib.rs" — must exist in the snapshot. */
  evidence_refs: string[];
  /** Open issues that block the declared goal. Non-empty on DONE → needs_review. */
  unresolved: string[];
  limitations: string[];
  /** Required when verdict === "CANNOT_AUTO". */
  cannot_auto_reason?: string;
}

export type VerifiedOutcome =
  | "accepted_report"
  | "cannot_auto"
  | "needs_review"
  | "rejected";

export interface VerifyContext {
  /** Raw executor stdout (progress noise + the fenced report block). */
  stdout: string;
  /** The task_key this report must be bound to. */
  task_key: string;
  /** File paths present in the approved input snapshot (repo @ base SHA). */
  snapshot_files: ReadonlySet<string> | string[];
  exit_ok: boolean;
  timed_out: boolean;
  /** Hard budget (wall clock / output size) exceeded during execution. */
  budget_exceeded: boolean;
  /** The worktree changed during a read-only execution. */
  changes_detected: boolean;
}

export interface VerifyResult {
  outcome: VerifiedOutcome;
  reasons: string[];
  report?: TodoReport;
}

const FENCE_RE = /```json\s*\n([\s\S]*?)\n```/g;

/**
 * Extract the report object from stdout. Counter-example hardened:
 * - prose mentioning DONE / echo of the prompt does NOT count (only a fenced
 *   json block with the marker schema_version counts)
 * - zero candidate blocks → no report
 * - multiple candidate blocks → rejected unless they are IDENTICAL objects
 *   (a duplicated trailing block is common CLI behaviour; a conflicting one
 *   means the model emitted two answers)
 * - truncated JSON (fence never closed / invalid JSON) → rejected
 */
export function extractTodoReport(stdout: string): { report?: TodoReport; reasons: string[] } {
  const candidates: TodoReport[] = [];
  const reasons: string[] = [];
  let m: RegExpExecArray | null;
  FENCE_RE.lastIndex = 0;
  while ((m = FENCE_RE.exec(stdout)) !== null) {
    const body = m[1].trim();
    let parsed: unknown;
    try {
      parsed = JSON.parse(body);
    } catch {
      reasons.push("a fenced json block failed to parse (truncated or invalid)");
      continue;
    }
    if (
      parsed &&
      typeof parsed === "object" &&
      (parsed as Record<string, unknown>).schema_version === REPORT_SCHEMA_VERSION
    ) {
      candidates.push(parsed as TodoReport);
    }
  }
  if (candidates.length === 0) {
    return { reasons: ["no structured report block found in stdout", ...reasons] };
  }
  const first = candidates[0];
  for (const c of candidates.slice(1)) {
    if (JSON.stringify(canonicalKeyOrder(c)) !== JSON.stringify(canonicalKeyOrder(first))) {
      return { reasons: ["multiple conflicting report blocks in stdout"] };
    }
  }
  return { report: first, reasons };
}

function canonicalKeyOrder(v: unknown): unknown {
  if (Array.isArray(v)) return v.map(canonicalKeyOrder);
  if (v && typeof v === "object") {
    const out: Record<string, unknown> = {};
    for (const k of Object.keys(v as Record<string, unknown>).sort()) {
      out[k] = canonicalKeyOrder((v as Record<string, unknown>)[k]);
    }
    return out;
  }
  return v;
}

/**
 * The native report verifier. Pure function over caller-supplied evidence —
 * no file reads, no command execution, no network.
 */
export function verifyTodoReport(ctx: VerifyContext): VerifyResult {
  const reasons: string[] = [];

  // 1. Execution preconditions — a report cannot outrank a failed execution.
  if (ctx.timed_out) return { outcome: "rejected", reasons: ["execution timed out"] };
  if (ctx.budget_exceeded) return { outcome: "rejected", reasons: ["execution budget exceeded"] };
  if (!ctx.exit_ok) return { outcome: "rejected", reasons: ["execution exited non-zero"] };
  if (ctx.changes_detected) {
    return { outcome: "rejected", reasons: ["read-only policy violated: worktree changed"] };
  }

  // 2. Structural extraction.
  const { report, reasons: extractReasons } = extractTodoReport(ctx.stdout);
  if (!report) {
    return { outcome: "rejected", reasons: [...extractReasons] };
  }

  // 3. Task binding — the report must belong to THIS task, not a replay.
  if (report.task_key !== ctx.task_key) {
    return {
      outcome: "rejected",
      reasons: [`task_key mismatch: report=${report.task_key.slice(0, 12)}… expected=${ctx.task_key.slice(0, 12)}…`],
      report,
    };
  }

  // 4. Field-level structure.
  if (!VALID_VERDICTS.includes(report.verdict)) {
    reasons.push(`invalid verdict '${String(report.verdict)}'`);
  }
  if (!report.summary || !report.summary.trim()) reasons.push("summary is empty");
  if (!Array.isArray(report.findings) || report.findings.length === 0) {
    reasons.push("findings is empty — the report covers nothing");
  }
  if (report.verdict === "CANNOT_AUTO" && !report.cannot_auto_reason?.trim()) {
    reasons.push("CANNOT_AUTO requires cannot_auto_reason");
  }
  if (reasons.length > 0) return { outcome: "rejected", reasons, report };

  // 5. Verdict semantics.
  if (report.verdict === "CANNOT_AUTO") {
    return { outcome: "cannot_auto", reasons: [], report };
  }
  if (report.verdict === "NEEDS_REVIEW") {
    return { outcome: "needs_review", reasons: ["model declared NEEDS_REVIEW"], report };
  }

  // verdict === DONE — earn the auto-close.
  if (report.unresolved && report.unresolved.length > 0) {
    return {
      outcome: "needs_review",
      reasons: [`DONE declared with ${report.unresolved.length} unresolved item(s) — human review required`],
      report,
    };
  }

  // 6. Evidence references must point into the approved snapshot.
  const snapshot = ctx.snapshot_files;
  const has = (p: string) =>
    Array.isArray(snapshot) ? snapshot.includes(p) : snapshot.has(p);
  const badRefs: string[] = [];
  for (const ref of report.evidence_refs ?? []) {
    // "path:line" — split on the LAST colon so Windows-style prefixes or
    // colons inside the path cannot smuggle a different file.
    const idx = ref.lastIndexOf(":");
    const file = idx > 0 ? ref.slice(0, idx) : ref;
    if (!file || !has(file)) badRefs.push(ref);
  }
  if (badRefs.length > 0) {
    return {
      outcome: "needs_review",
      reasons: [`evidence reference(s) not present in the approved snapshot: ${badRefs.join(", ")}`],
      report,
    };
  }

  return { outcome: "accepted_report", reasons: [], report };
}

const VALID_VERDICTS: readonly string[] = ["DONE", "CANNOT_AUTO", "NEEDS_REVIEW"];
