/**
 * publishPlan — the atomic plan publishing gate.
 *
 * One call turns a reviewed PlanSpec into: one Epic, all its Steps, and a
 * sealed standard TaskPackage per step — by REUSING the existing
 * pushStepWithPackage → prepare → validate → seal pipeline (no second set of
 * TaskPackage rules). The server resolves project_dir, git root, and HEAD
 * itself; the spec carries no paths and no SHAs to fake.
 *
 * Three-state contract (never throws for expected failures):
 *   published      — everything created; returns epic/step/package IDs only.
 *                    Packages are sealed, NOT enqueued; the epic is NOT given
 *                    auto_execute — Runner authorization stays with a human.
 *   needs_decision — the spec has gaps only a human can close (unconfirmed
 *                    consumers, unordered steps with overlapping write
 *                    scopes, authority beyond the conservative envelope,
 *                    major risks without resolutions). Detected BEFORE any
 *                    mutation: no epic, steps, or packages are created.
 *   invalid        — deterministic errors (schema, git, dependencies,
 *                    untrusted commands) or a mid-flight failure. Any
 *                    half-created state is rolled back: packages of the epic
 *                    are cancelled (never claimable again) and the epic with
 *                    its steps is deleted. If the Runtime Store cannot be
 *                    read, the epic is KEPT as a tombstone (its switches are
 *                    off, so nothing is claimable) and the result reports a
 *                    degraded rollback honestly.
 *
 * Concurrency: every mutation goes through the existing cross-process
 * file-lock + revision (CAS) semantics in epic-store/runtime-store, so
 * concurrent updates by other processes are never overwritten — they
 * interleave between our individually-locked mutations.
 */

import {
  Authority,
  Rollback,
  isTrustedCommand,
  findGitRoot,
  readGitHeadSha,
} from "./task-package.js";
import { pushStepWithPackage } from "./task-package-ops.js";
import {
  pushEpic,
  updateEpic,
  deleteEpic,
  readEpicsSafe,
} from "./epic-store.js";
import {
  getPackagesForEpic,
  updatePackageStatus,
  checkRuntimeStore,
} from "./runtime-store.js";
import { resolveRegisteredProject } from "./store.js";

// ─── PlanSpec — the reviewed, conversation-side plan contract ───

/** Executor budget for a step. Preferred is zcode|manual (codex is a
 * design/push-only executor and is rejected as a deterministic error). */
export interface PlanExecutor {
  preferred?: "zcode" | "manual";
  sandbox?: "workspace-write" | "read-only";
  timeout_seconds?: number;
  max_attempts?: number;
}

export interface PlanStepSpec {
  title: string;
  description?: string;
  /** Single, verifiable objective — becomes the package objective. */
  objective: string;
  /** 0-based indices of earlier steps this step depends on (backward only). */
  depends_on?: number[];
  allowed_paths: string[];
  forbidden_paths?: string[];
  max_changed_files?: number;
  context_refs?: string[];
  acceptance_command_ids: string[];
  /** Requested authority deltas merged over the conservative default —
   * merge/push/deploy=true is a needs_decision gap. */
  authority?: Partial<Authority>;
  rollback?: Rollback;
  executor?: PlanExecutor;
}

/** Change-chain context used for gap detection (consumers must be confirmed). */
export interface PlanChain {
  upstream?: string[];
  change?: string[];
  consumers?: string[];
  /** Human confirmation obtained in the source conversation. */
  consumers_confirmed?: boolean;
  risks?: string[];
}

export interface PlanReview {
  major_risks?: string[];
  /** resolutions[i] addresses major_risks[i] — parallel arrays. */
  resolutions?: string[];
}

export interface PlanSpec {
  title: string;
  description?: string;
  priority?: "P0" | "P1" | "P2" | "P3";
  acceptance?: string;
  goal?: string;
  baseline?: string;
  chain?: PlanChain;
  review?: PlanReview;
  steps: PlanStepSpec[];
}

// ─── Results ───

export interface PublishedStep {
  step_id: string;
  package_id: string;
  plan_revision: number;
  package_hash: string;
}

export interface PublishPublished {
  status: "published";
  epic_id: string;
  steps: PublishedStep[];
  summary: string;
}

export type DecisionCategory =
  | "consumer_confirmation"
  | "step_scope_conflict"
  | "authority_envelope"
  | "risk_unresolved";

export interface DecisionQuestion {
  category: DecisionCategory;
  question: string;
  affected: string[];
}

export interface PublishNeedsDecision {
  status: "needs_decision";
  questions: DecisionQuestion[];
  summary: string;
}

export interface RollbackReport {
  /** true when the board and Runtime Store were fully cleaned. */
  performed: boolean;
  /** true when residue remains but is provably unclaimable. */
  degraded: boolean;
  detail: string;
}

export interface PublishInvalid {
  status: "invalid";
  errors: Array<{ field: string; message: string }>;
  rollback: RollbackReport;
  summary: string;
}

export type PublishPlanResult =
  | PublishPublished
  | PublishNeedsDecision
  | PublishInvalid;

// ─── Static contract validation (pure, no mutation) ───

const PRIORITIES = ["P0", "P1", "P2", "P3"] as const;
const ROLLBACK_STRATEGIES: Rollback["strategy"][] = [
  "preserve_worktree_and_stop",
  "abort_and_clean",
];

function isNonEmptyStringArray(value: unknown): value is string[] {
  return (
    Array.isArray(value) &&
    value.every((v) => typeof v === "string" && v.trim().length > 0)
  );
}

/**
 * Validate the PlanSpec against the publishing contract. Deterministic
 * errors → invalid. Gap-shaped problems (authority escalation, unresolved
 * risks) are intentionally NOT here — those are needs_decision, detected
 * separately before any mutation.
 */
export function validatePlanSpec(spec: PlanSpec): Array<{ field: string; message: string }> {
  const errors: Array<{ field: string; message: string }> = [];
  const push = (field: string, message: string) => errors.push({ field, message });

  if (typeof spec?.title !== "string" || !spec.title.trim()) {
    push("title", "title is required");
  }
  if (spec.priority !== undefined && !PRIORITIES.includes(spec.priority)) {
    push("priority", `priority must be one of: ${PRIORITIES.join(", ")}`);
  }
  if (!Array.isArray(spec?.steps) || spec.steps.length === 0) {
    push("steps", "at least one step is required");
    return errors;
  }

  spec.steps.forEach((step, i) => {
    const at = `steps[${i}]`;
    if (typeof step?.title !== "string" || !step.title.trim()) {
      push(`${at}.title`, "title is required");
    }
    if (typeof step?.objective !== "string" || !step.objective.trim()) {
      push(`${at}.objective`, "objective is required (single verifiable objective)");
    }
    if (!isNonEmptyStringArray(step?.allowed_paths)) {
      push(`${at}.allowed_paths`, "at least one allowed path glob is required");
    }
    if (!isNonEmptyStringArray(step?.acceptance_command_ids)) {
      push(`${at}.acceptance_command_ids`, "at least one acceptance command id is required");
    } else {
      for (const id of step.acceptance_command_ids) {
        if (!isTrustedCommand(id)) {
          push(`${at}.acceptance_command_ids`, `command_id '${id}' is not in the trusted acceptance registry`);
        }
      }
    }
    if (step?.max_changed_files !== undefined && step.max_changed_files < 1) {
      push(`${at}.max_changed_files`, "must be ≥ 1");
    }
    if (step?.forbidden_paths !== undefined && !Array.isArray(step.forbidden_paths)) {
      push(`${at}.forbidden_paths`, "must be an array of path globs");
    }
    if (step?.context_refs !== undefined && !Array.isArray(step.context_refs)) {
      push(`${at}.context_refs`, "must be an array of reference paths");
    }

    // depends_on: integer, in range, backward-only (forward references cannot
    // be materialized — steps are created in listed order).
    if (step?.depends_on !== undefined) {
      if (!Array.isArray(step.depends_on)) {
        push(`${at}.depends_on`, "must be an array of step indices");
      } else {
        for (const j of step.depends_on) {
          if (!Number.isInteger(j) || j < 0 || j >= spec.steps.length) {
            push(`${at}.depends_on`, `index ${String(j)} is out of range (0..${spec.steps.length - 1})`);
          } else if (j === i) {
            push(`${at}.depends_on`, "a step cannot depend on itself");
          } else if (j > i) {
            push(`${at}.depends_on`, `forward dependency on steps[${j}] — reorder the steps or drop the edge (dependencies may only point backwards)`);
          }
        }
      }
    }

    // Executor budget
    const ex = step?.executor;
    if (ex !== undefined) {
      if (ex.preferred !== undefined && !["zcode", "manual"].includes(ex.preferred)) {
        push(`${at}.executor.preferred`, "must be 'zcode' or 'manual' (codex is design/push-only)");
      }
      if (ex.sandbox !== undefined && !["workspace-write", "read-only"].includes(ex.sandbox)) {
        push(`${at}.executor.sandbox`, "must be 'workspace-write' or 'read-only'");
      }
      if (ex.timeout_seconds !== undefined && ex.timeout_seconds < 60) {
        push(`${at}.executor.timeout_seconds`, "must be ≥ 60s");
      }
      if (ex.max_attempts !== undefined && ex.max_attempts < 1) {
        push(`${at}.executor.max_attempts`, "must be ≥ 1");
      }
    }

    // Authority shape (values beyond the conservative envelope are a
    // needs_decision gap, not a schema error — only the shape is checked here).
    const au = step?.authority;
    if (au !== undefined) {
      for (const key of ["auto_edit_worktree", "auto_commit_candidate", "auto_merge", "auto_push", "auto_deploy"] as const) {
        if (au[key] !== undefined && typeof au[key] !== "boolean") {
          push(`${at}.authority.${key}`, "must be a boolean");
        }
      }
    }

    if (step?.rollback !== undefined && !ROLLBACK_STRATEGIES.includes(step.rollback.strategy)) {
      push(`${at}.rollback.strategy`, `must be one of: ${ROLLBACK_STRATEGIES.join(", ")}`);
    }
  });

  // Review / chain shape (coverage is a gap, not an error)
  if (spec.review?.major_risks !== undefined && !isNonEmptyStringArray(spec.review.major_risks)) {
    push("review.major_risks", "must be an array of non-empty risk descriptions");
  }
  if (spec.review?.resolutions !== undefined && !isNonEmptyStringArray(spec.review.resolutions)) {
    push("review.resolutions", "must be an array of non-empty resolution descriptions");
  }
  if (spec.chain?.consumers !== undefined && !isNonEmptyStringArray(spec.chain.consumers)) {
    push("chain.consumers", "must be an array of non-empty consumer descriptions");
  }
  if (spec.chain?.risks !== undefined && !isNonEmptyStringArray(spec.chain.risks)) {
    push("chain.risks", "must be an array of non-empty risk descriptions");
  }

  return errors;
}

// ─── Glob overlap heuristic (for step_scope_conflict detection) ───

/**
 * Convert a glob to a RegExp source: double star → `.*`, single star →
 * `[^/]*`, `?` → `[^/]`, everything else escaped. Approximation: a double
 * star between two path segments requires the middle slash and therefore
 * does not match the zero-segment form — errs toward missing exotic
 * overlaps, which is why callers also apply the fixed-prefix check below.
 */
function globToRegExpSource(glob: string): string {
  let out = "";
  let i = 0;
  while (i < glob.length) {
    const ch = glob[i];
    if (ch === "*") {
      if (glob[i + 1] === "*") {
        while (glob[i] === "*") i++;
        out += ".*";
      } else {
        out += "[^/]*";
        i++;
      }
    } else if (ch === "?") {
      out += "[^/]";
      i++;
    } else {
      out += ch.replace(/[.*+?^${}()|[\]\\]/, "\\$&");
      i++;
    }
  }
  return out;
}

function globMatches(glob: string, candidate: string): boolean {
  return new RegExp(`^${globToRegExpSource(glob)}$`).test(candidate);
}

/** The literal part of a glob before the first wildcard character. */
function fixedPrefix(glob: string): string {
  const m = glob.match(/^[^*?[]*/);
  return m ? m[0] : "";
}

/**
 * Conservative "may these two globs hit the same file?" test.
 * True when either pattern matches the other as a literal, or when their
 * fixed prefixes are equal / segment-boundary prefixes of each other.
 * False positives (asking a human unnecessarily) are acceptable;
 * false negatives are only exotic wildcard combinations — documented.
 */
export function globsMayOverlap(a: string, b: string): boolean {
  if (globMatches(a, b) || globMatches(b, a)) return true;
  const pa = fixedPrefix(a);
  const pb = fixedPrefix(b);
  if (pa === pb) return true;
  if (pa.endsWith("/") && pb.startsWith(pa)) return true;
  if (pb.endsWith("/") && pa.startsWith(pb)) return true;
  return false;
}

function scopesMayOverlap(a: PlanStepSpec, b: PlanStepSpec): string | null {
  for (const ga of a.allowed_paths) {
    for (const gb of b.allowed_paths) {
      if (globsMayOverlap(ga, gb)) return `${ga} ↔ ${gb}`;
    }
  }
  return null;
}

// ─── needs_decision gap detection (pure, runs BEFORE any mutation) ───

/**
 * Transitive dependency closure per step. depends_on is backward-only by
 * validation, so one ascending pass propagates closures fully; the visited
 * set guards against malformed (cyclic) input reaching this anyway.
 */
function dependencyClosures(spec: PlanSpec): Set<number>[] {
  const closures = spec.steps.map((s) => new Set<number>(s.depends_on ?? []));
  for (let i = 0; i < closures.length; i++) {
    const seen = new Set<number>([i]);
    let grew = true;
    while (grew) {
      grew = false;
      for (const j of [...closures[i]]) {
        if (seen.has(j)) continue;
        seen.add(j);
        for (const k of closures[j]) {
          if (!closures[i].has(k)) {
            closures[i].add(k);
            grew = true;
          }
        }
      }
    }
  }
  return closures;
}

/**
 * Detect the four human-decision gap categories. Deterministic and pure —
 * publishPlan runs this BEFORE creating anything, so a needs_decision result
 * guarantees an untouched board and Runtime Store.
 */
export function detectDecisionGaps(spec: PlanSpec): DecisionQuestion[] {
  const questions: DecisionQuestion[] = [];

  // 1. Unconfirmed consumers — downstream impact a human must sign off on.
  const consumers = spec.chain?.consumers ?? [];
  if (consumers.length > 0 && spec.chain?.consumers_confirmed !== true) {
    questions.push({
      category: "consumer_confirmation",
      question:
        `消费者未确认：本方案影响 ${consumers.length} 个下游消费方（${consumers.join("、")}）。` +
        "请确认消费方名单或调整后重发（确认后置 chain.consumers_confirmed=true）。",
      affected: consumers,
    });
  }

  // 2. Unordered steps with overlapping write scopes — parallel in the
  //    dependency graph but writing the same paths: serialize or split.
  const closures = dependencyClosures(spec);
  for (let i = 0; i < spec.steps.length; i++) {
    for (let j = i + 1; j < spec.steps.length; j++) {
      if (closures[j].has(i)) continue; // ordered: j transitively depends on i
      const overlap = scopesMayOverlap(spec.steps[i], spec.steps[j]);
      if (overlap) {
        questions.push({
          category: "step_scope_conflict",
          question:
            `无序步骤写范围冲突：steps[${i}]「${spec.steps[i].title}」与 steps[${j}]「${spec.steps[j].title}」` +
            `无依赖关系却可能写同一批文件（${overlap}）。请二选一：给 steps[${j}] 增加 depends_on=[${i}] 串行化，或收窄两者的 allowed_paths 使其不相交。`,
          affected: [spec.steps[i].title, spec.steps[j].title],
        });
      }
    }
  }

  // 3. Authority beyond the conservative envelope — merge/push/deploy are
  //    human-granted powers, never publish-granted.
  spec.steps.forEach((step, i) => {
    const au = step.authority;
    if (!au) return;
    const escalated = (["auto_merge", "auto_push", "auto_deploy"] as const).filter(
      (k) => au[k] === true,
    );
    if (escalated.length > 0) {
      questions.push({
        category: "authority_envelope",
        question:
          `授权超保守包络：steps[${i}]「${step.title}」请求 ${escalated.join("/")}。` +
          "保守包络仅授予 worktree 编辑与候选提交；请降级为保守授权后重发，或走人工授权流程。",
        affected: [step.title],
      });
    }
  });

  // 4. Major risks without resolutions — resolutions[i] addresses
  //    major_risks[i]; every risk needs a non-empty resolution.
  const risks = spec.review?.major_risks ?? [];
  const resolutions = spec.review?.resolutions ?? [];
  if (risks.length > resolutions.length) {
    const unresolved = risks.slice(resolutions.length);
    questions.push({
      category: "risk_unresolved",
      question:
        `主要风险无对策：${unresolved.length} 项风险缺少对策（${unresolved.join("；")}）。` +
        "请为每项 major_risk 提供对应 resolution（按序配对）后重发。",
      affected: unresolved,
    });
  }

  return questions;
}

// ─── Rollback ───

const NO_ROLLBACK: RollbackReport = {
  performed: false,
  degraded: false,
  detail: "rejected before any mutation — nothing to roll back",
};

/**
 * Full rollback of a half-published epic:
 *   1. cancel every non-terminal package of the epic (cancelled is terminal
 *      and never claimable) — fail-closed store read with one retry (the
 *      reader rotates a corrupt file to .bak, so the retry sees a clean store;
 *      a store that stays unreadable keeps the epic as a tombstone);
 *   2. delete the epic with its steps.
 * If packages cannot be provably cancelled, the epic is KEPT: its
 * auto_execute switch is off, so nothing is claimable — degraded, honest.
 */
async function rollbackEpic(projectPath: string, epicId: string): Promise<RollbackReport> {
  let pkgsResult = await getPackagesForEpic(epicId);
  if ("error" in pkgsResult) {
    pkgsResult = await getPackagesForEpic(epicId);
  }
  if ("error" in pkgsResult) {
    return {
      performed: false,
      degraded: true,
      detail: `Runtime Store unreadable (${pkgsResult.error}) — epic kept as tombstone with all switches off; packages are NOT claimable, but residue remains`,
    };
  }

  const cancellable = pkgsResult.pkgs.filter((p) =>
    ["draft", "sealed", "ready", "blocked"].includes(p.status),
  );
  const cancelErrors: string[] = [];
  for (const pkg of cancellable) {
    const r = await updatePackageStatus(pkg.package_id, "cancelled");
    if ("error" in r) cancelErrors.push(`${pkg.package_id}: ${r.error}`);
  }
  if (cancelErrors.length > 0) {
    return {
      performed: false,
      degraded: true,
      detail: `failed to cancel ${cancelErrors.length}/${cancellable.length} package(s) (${cancelErrors.join("; ")}) — epic kept as tombstone with all switches off`,
    };
  }

  const del = await deleteEpic(projectPath, epicId);
  if ("error" in del) {
    return {
      performed: false,
      degraded: true,
      detail: `${cancellable.length} package(s) cancelled (not claimable), but epic deletion failed: ${del.error}`,
    };
  }
  return {
    performed: true,
    degraded: false,
    detail: `cancelled ${cancellable.length} package(s) and deleted the epic with its ${del.deletedSteps} step(s)`,
  };
}

// ─── publishPlan ───

function invalidResult(
  errors: Array<{ field: string; message: string }>,
  rollback: RollbackReport,
): PublishInvalid {
  const summary =
    rollback.performed || rollback.degraded
      ? `Plan rejected after ${errors.length} error(s); rollback: ${rollback.detail}`
      : `Plan rejected: ${errors.length} deterministic error(s); no mutation was attempted`;
  return { status: "invalid", errors, rollback, summary };
}

/**
 * Publish a reviewed plan atomically-enough: Epic + Steps + sealed packages
 * in one call, with needs_decision gaps detected up front and a full
 * (cancelled-then-deleted) rollback on any mid-flight failure.
 *
 * The epic is set to status "active" because the existing seal-time
 * validation requires an active epic for zcode-executor packages — this is a
 * lifecycle state, NOT authorization: auto_execute / project switch / master
 * switch are all deliberately left off for a human to grant.
 */
export async function publishPlan(cwd: string, spec: PlanSpec): Promise<PublishPlanResult> {
  // Phase 1 — static contract validation (pure).
  const errors = validatePlanSpec(spec);
  if (errors.length > 0) return invalidResult(errors, NO_ROLLBACK);

  // Once the epic exists, every subsequent failure path (including an
  // unexpected throw) must roll it back — hence the mutable epicId. The
  // resolved project path is hoisted for the same reason: rollback must hit
  // the project the mutations actually landed in (registered routing may
  // differ from cwd).
  let projectPath = cwd;
  let createdEpicId: string | null = null;
  try {
    // Phase 2 — server-side resolution. The spec carries no paths/SHAs; the
    // server derives everything from its own cwd and the filesystem.
    projectPath = (await resolveRegisteredProject(cwd)) ?? cwd;
    const repoRoot = await findGitRoot(projectPath);
    if (!repoRoot) {
      return invalidResult(
        [{ field: "project", message: `not a git repository: ${projectPath}` }],
        NO_ROLLBACK,
      );
    }
    const baseSha = await readGitHeadSha(repoRoot);
    if (!baseSha) {
      return invalidResult(
        [{ field: "project", message: "cannot read git HEAD SHA — is the repository initialized with at least one commit?" }],
        NO_ROLLBACK,
      );
    }
    // Fail-closed probes before creating anything we might have to undo.
    const epicsRead = await readEpicsSafe(projectPath);
    if ("error" in epicsRead) {
      return invalidResult(
        [{ field: "project", message: `.epics.json unreadable: ${epicsRead.error}` }],
        NO_ROLLBACK,
      );
    }
    const storeProbe = await checkRuntimeStore();
    if ("error" in storeProbe) {
      return invalidResult(
        [{ field: "project", message: `Runtime Store unreadable: ${storeProbe.error}` }],
        NO_ROLLBACK,
      );
    }

    // Phase 3 — human-decision gaps, BEFORE any mutation.
    const questions = detectDecisionGaps(spec);
    if (questions.length > 0) {
      return {
        status: "needs_decision",
        questions,
        summary: `${questions.length} question(s) require human decisions before publishing — no epic, steps, or packages were created`,
      };
    }

    // Phase 4 — mutate: epic → active → steps with sealed packages.
    const epicResult = await pushEpic(projectPath, {
      title: spec.title,
      description: spec.description,
      priority: spec.priority,
      acceptance: spec.acceptance,
    });
    if ("error" in epicResult) {
      return invalidResult([{ field: "title", message: epicResult.error }], NO_ROLLBACK);
    }
    createdEpicId = epicResult.epic.id;

    const activate = await updateEpic(projectPath, createdEpicId, { status: "active" });
    if ("error" in activate) {
      const rb = await rollbackEpic(projectPath, createdEpicId);
      return invalidResult([{ field: "epic", message: `activation failed: ${activate.error}` }], rb);
    }

    const created: PublishedStep[] = [];
    for (let i = 0; i < spec.steps.length; i++) {
      const step = spec.steps[i];
      // Backward references are validated, so created[j] always exists here.
      const dependsOn = (step.depends_on ?? []).map((j) => created[j].step_id);
      const result = await pushStepWithPackage(projectPath, {
        epic_id: createdEpicId,
        title: step.title,
        description: step.description,
        depends_on: dependsOn,
        objective: step.objective,
        context_refs: step.context_refs,
        allowed_paths: step.allowed_paths,
        forbidden_paths: step.forbidden_paths,
        max_changed_files: step.max_changed_files,
        executor_preferred: step.executor?.preferred,
        executor_sandbox: step.executor?.sandbox,
        executor_timeout_seconds: step.executor?.timeout_seconds,
        executor_max_attempts: step.executor?.max_attempts,
        acceptance_command_ids: step.acceptance_command_ids,
        authority: step.authority,
        rollback: step.rollback,
      });
      if ("error" in result) {
        const rb = await rollbackEpic(projectPath, createdEpicId);
        return invalidResult([{ field: `steps[${i}]`, message: result.error }], rb);
      }
      created.push({
        step_id: result.step_id,
        package_id: result.package_id,
        plan_revision: result.plan_revision,
        package_hash: result.package_hash,
      });
    }

    return {
      status: "published",
      epic_id: createdEpicId,
      steps: created,
      summary:
        `Published epic '${spec.title.trim()}' with ${created.length} step(s) and ${created.length} sealed package(s). ` +
        "Packages are sealed only — not enqueued; auto_execute/project/master switches are left OFF for human authorization.",
    };
  } catch (err) {
    // Unexpected internal failure — fail closed with a best-effort targeted
    // rollback (expected failures return invalid above with their own).
    const rb = createdEpicId
      ? await rollbackEpic(projectPath, createdEpicId)
      : NO_ROLLBACK;
    return invalidResult(
      [{ field: "internal", message: `unexpected failure: ${(err as Error).message}` }],
      rb,
    );
  }
}
