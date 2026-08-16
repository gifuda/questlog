/**
 * TaskPackage MCP operations: prepare, validate, seal, enqueue.
 *
 * These are the server-side hard gates. The model calls these tools to
 * create a standard execution contract; the server resolves real paths,
 * Git state, and policy — it never trusts model-supplied base SHA, authority,
 * or project paths.
 *
 * Flow:
 *   prepare → draft package with server-resolved project_path/repo_root/base_sha
 *   validate → check all fields, declared preconditions, dependencies, command registry
 *   seal → freeze content, compute hash, mark sealed (immutable)
 *   enqueue → move sealed package to ready (Runner picks it up)
 *
 * git_clean is a *declared* precondition only: packages must opt in
 * explicitly. Execution happens in an isolated worktree created from
 * base_sha, so a dirty main workspace is not an obstacle by default.
 *
 * Unsealed, superseded, dependency-insufficient, path-escaping, authority-
 * expanding, or schema-incomplete packages are REJECTED.
 */

import { promises as fs } from "node:fs";
import path from "node:path";
import os from "node:os";
import crypto from "node:crypto";
import {
  TaskPackage,
  PackageSummary,
  ValidationIssue,
  validatePackageContent,
  computePackageHash,
  readGitHeadSha,
  findGitRoot,
  isGitClean,
  resolveAcceptanceCommand,
  defaultAuthority,
  defaultExecutor,
  defaultStopConditions,
  defaultRollback,
  Authority,
  Rollback,
  SCHEMA_VERSION,
} from "./task-package.js";
import {
  savePackage,
  getPackage,
  updatePackageStatus,
  packageSummary,
} from "./runtime-store.js";
import { readEpicsSafe } from "./epic-store.js";
import { resolveRegisteredProject } from "./store.js";

// ─── prepare ───

export interface PrepareInput {
  epic_id: string;
  step_id: string;
  objective: string;
  context_refs?: string[];
  allowed_paths: string[];
  forbidden_paths?: string[];
  max_changed_files?: number;
  executor_preferred?: string;
  executor_timeout_seconds?: number;
  executor_max_attempts?: number;
  /** Executor sandbox: workspace-write (default) allows edits inside the
   * worktree scope; read-only forbids any file change (report tasks). */
  executor_sandbox?: "workspace-write" | "read-only";
  acceptance_command_ids?: string[];
  /**
   * Conservative authority override, merged over the default. The server
   * clamps auto_merge/auto_push/auto_deploy to false REGARDLESS of input —
   * those powers are never grantable through prepare (defense in depth on
   * top of validatePackageContent, which rejects them at seal time).
   */
  authority?: Partial<Authority>;
  /** Rollback strategy override — must be a known strategy or prepare fails. */
  rollback?: Rollback;
  /** Working directory override — resolved by server if omitted. */
  project_dir?: string;
}

export interface PrepareResult {
  package_id: string;
  plan_revision: number;
  status: "draft";
  /** Server-resolved values (model cannot fake these). */
  resolved_project_path: string;
  resolved_repo_root: string;
  resolved_base_sha: string | null;
  /** Issues to fix before sealing (from content validation). */
  issues: ValidationIssue[];
  /** Summary of what was prepared. */
  summary: string;
}

/**
 * Prepare a draft TaskPackage.
 * The server resolves canonical project path, git root, and base SHA.
 * Returns a draft package with validation issues to fix before sealing.
 */
export async function prepareTaskPackage(
  cwd: string,
  input: PrepareInput,
): Promise<PrepareResult | { error: string }> {
  // Resolve canonical project path (route to registered project if needed)
  let projectPath = cwd;
  if (input.project_dir) {
    projectPath = input.project_dir;
  } else {
    const registered = await resolveRegisteredProject(cwd);
    if (registered) projectPath = registered;
  }

  // Resolve git root
  const repoRoot = await findGitRoot(projectPath);
  if (!repoRoot) {
    return { error: `not a git repository: ${projectPath}` };
  }

  // Read real base SHA (never trust model-supplied value)
  const baseSha = await readGitHeadSha(repoRoot);
  if (!baseSha) {
    return { error: "cannot read git HEAD SHA — is the repository initialized with at least one commit?" };
  }

  // The step must exist in the target project's .epics.json — a package for
  // a phantom step can never write back its summary at seal time and would
  // be cancelled late (after validation work). Fail early with a clear fix.
  const epicsResult = await readEpicsSafe(projectPath);
  if (!("ok" in epicsResult)) {
    return {
      error: `cannot verify step '${input.step_id}': .epics.json ${"notFound" in epicsResult ? "missing" : "unreadable"} in ${projectPath} — create the epic/step first (push_epic/push_step)`,
    };
  }
  const targetStep = epicsResult.data.steps.find(
    (s) => s.id === input.step_id && s.epic_id === input.epic_id,
  );
  if (!targetStep) {
    return {
      error: `step '${input.step_id}' not found in epic '${input.epic_id}' (${projectPath}) — create the step first (push_step)`,
    };
  }

  // Rollback strategy: an unknown strategy is an explicit error, not a silent
  // fallback — a plan that asked for the wrong strategy must be told, not
  // quietly downgraded to the default.
  const VALID_ROLLBACK_STRATEGIES: Rollback["strategy"][] = [
    "preserve_worktree_and_stop",
    "abort_and_clean",
  ];
  if (input.rollback && !VALID_ROLLBACK_STRATEGIES.includes(input.rollback.strategy)) {
    return { error: `invalid rollback strategy '${input.rollback.strategy}' — must be one of: ${VALID_ROLLBACK_STRATEGIES.join(", ")}` };
  }
  const rollback: Rollback = input.rollback ?? defaultRollback();

  // Authority: merge the requested fields over the conservative default, then
  // clamp the escalation powers to false. The clamp is unconditional — merge/
  // push/deploy are human-granted, never prepare-granted.
  const authority: Authority = input.authority
    ? { ...defaultAuthority(), ...input.authority, auto_merge: false, auto_push: false, auto_deploy: false }
    : defaultAuthority();

  // Build draft package
  const packageId = `pkg_${crypto.randomUUID()}`;
  const draftPkg: TaskPackage = {
    schema_version: SCHEMA_VERSION,
    package_id: packageId,
    epic_id: input.epic_id,
    step_id: input.step_id,
    plan_revision: 1,
    project_path: projectPath,
    repo_root: repoRoot,
    base_sha: baseSha,
    objective: input.objective,
    context_refs: input.context_refs ?? [],
    scope: {
      allowed_paths: input.allowed_paths,
      forbidden_paths: input.forbidden_paths ?? [],
      max_changed_files: input.max_changed_files ?? 10,
    },
    // No implicit preconditions: git_clean is opt-in (the standard package is
    // an independent carrier; execution happens in an isolated worktree).
    preconditions: [],
    executor: {
      preferred: (input.executor_preferred as TaskPackage["executor"]["preferred"]) ?? "zcode",
      fallbacks: [],
      sandbox: input.executor_sandbox ?? "workspace-write",
      timeout_seconds: input.executor_timeout_seconds ?? 3600,
      max_attempts: input.executor_max_attempts ?? 2,
    },
    acceptance: (input.acceptance_command_ids ?? []).map((id) => ({
      type: "command" as const,
      command_id: id,
    })),
    authority,
    stop_conditions: defaultStopConditions(),
    rollback,
    status: "draft",
    sealed_at: "",
    package_hash: "",
  };

  // Validate content
  const issues = validatePackageContent(draftPkg);

  // Compute preliminary hash (without status/hash fields)
  const hashInput = { ...draftPkg };
  const { status: _s, package_hash: _h, ...hashable } = hashInput;
  draftPkg.package_hash = computePackageHash(hashable as Omit<TaskPackage, "package_hash" | "status">);

  // Save draft to Runtime Store
  const saveResult = await savePackage(draftPkg);
  if ("error" in saveResult) return { error: saveResult.error };

  const errorCount = issues.filter((i) => i.severity === "error").length;
  const summary = `Draft package ${packageId} prepared. Project: ${path.basename(projectPath)}, base SHA: ${baseSha.slice(0, 8)}. ${errorCount} error(s), ${issues.length - errorCount} warning(s) to fix before sealing.`;

  return {
    package_id: packageId,
    plan_revision: 1,
    status: "draft",
    resolved_project_path: projectPath,
    resolved_repo_root: repoRoot,
    resolved_base_sha: baseSha,
    issues,
    summary,
  };
}

// ─── validate ───

export interface ValidateResult {
  package_id: string;
  valid: boolean;
  issues: ValidationIssue[];
  /** Server-side checks: git, dependencies, command registry. */
  server_checks: {
    git_clean: boolean;
    base_sha_current: boolean;
    dependencies_met: boolean;
    commands_trusted: boolean;
  };
  summary: string;
}

/**
 * Validate a TaskPackage against schema, Git state, dependencies, and the
 * trusted command registry. Returns all issues — valid=true only when
 * zero errors.
 */
export async function validateTaskPackage(
  cwd: string,
  packageId: string,
): Promise<ValidateResult | { error: string }> {
  const pkgResult = await getPackage(packageId);
  if ("error" in pkgResult) return { error: pkgResult.error };
  const pkg = pkgResult.pkg;

  // Content validation
  const issues = validatePackageContent(pkg);

  // Server-side checks
  const gitClean = await isGitClean(pkg.repo_root);
  const currentSha = await readGitHeadSha(pkg.repo_root);
  const baseShaCurrent = currentSha === pkg.base_sha;

  // Check dependencies (step preconditions)
  const epicsResult = await readEpicsSafe(pkg.project_path);
  let dependenciesMet = true;
  if ("ok" in epicsResult) {
    const data = epicsResult.data;
    const epic = data.epics.find((candidate) => candidate.id === pkg.epic_id);
    const step = data.steps.find(
      (candidate) => candidate.id === pkg.step_id && candidate.epic_id === pkg.epic_id,
    );
    if (!epic) {
      issues.push({
        field: "epic_id",
        message: `epic '${pkg.epic_id}' not found in the Plan Store`,
        severity: "error",
      });
      dependenciesMet = false;
    } else if (pkg.executor.preferred === "zcode" && epic.status !== "active") {
      issues.push({
        field: "epic_id",
        message: `automatic execution requires epic '${pkg.epic_id}' to be active`,
        severity: "error",
      });
      dependenciesMet = false;
    }
    if (!step) {
      issues.push({
        field: "step_id",
        message: `step '${pkg.step_id}' not found in epic '${pkg.epic_id}'`,
        severity: "error",
      });
      dependenciesMet = false;
    }
    for (const pre of pkg.preconditions) {
      if (pre.type === "step_stage" && pre.step_id && pre.at_least) {
        const depStep = data.steps.find((s) => s.id === pre.step_id);
        if (!depStep) {
          issues.push({
            field: `preconditions.step_id=${pre.step_id}`,
            message: "dependency step not found",
            severity: "error",
          });
          dependenciesMet = false;
        } else {
          const stageOrder = ["pending", "designed", "coded", "verified", "observed"];
          if (stageOrder.indexOf(depStep.stage) < stageOrder.indexOf(pre.at_least)) {
            issues.push({
              field: `preconditions.step_id=${pre.step_id}`,
              message: `dependency step is at '${depStep.stage}', needs '${pre.at_least}'`,
              severity: "error",
            });
            dependenciesMet = false;
          }
        }
      }
    }
  } else if ("notFound" in epicsResult) {
    issues.push({
      field: "project_path",
      message: "Plan Store .epics.json is missing; create the Epic and Step before sealing",
      severity: "error",
    });
    dependenciesMet = false;
  } else if ("error" in epicsResult) {
    issues.push({
      field: "project_path",
      message: `cannot read epics file: ${epicsResult.error}`,
      severity: "error",
    });
    dependenciesMet = false;
  }

  // Check acceptance commands are in the trusted registry
  let commandsTrusted = true;
  for (const check of pkg.acceptance) {
    if (check.type === "command" && check.command_id) {
      const cmd = resolveAcceptanceCommand(check.command_id);
      if (!cmd) {
        issues.push({
          field: `acceptance.command_id=${check.command_id}`,
          message: "command_id is not in the trusted acceptance registry",
          severity: "error",
        });
        commandsTrusted = false;
      }
    }
  }

  // Server-state issues. git_clean is a *declared* precondition: packages must
  // opt in explicitly. The standard package is an independent carrier and the
  // Runner executes in an isolated worktree, so a dirty main workspace is not
  // an obstacle by default.
  const demandsGitClean = pkg.preconditions.some((p) => p.type === "git_clean" && p.value === true);
  if (demandsGitClean && !gitClean) {
    issues.push({ field: "git", message: "working tree is not clean (precondition: git_clean)", severity: "error" });
  }
  if (!baseShaCurrent) {
    issues.push({
      field: "base_sha",
      message: `base SHA has drifted (sealed: ${pkg.base_sha.slice(0, 8)}, current: ${(currentSha ?? "").slice(0, 8)})`,
      severity: "error",
    });
  }

  const errorCount = issues.filter((i) => i.severity === "error").length;
  const valid = errorCount === 0;

  return {
    package_id: packageId,
    valid,
    issues,
    server_checks: {
      git_clean: gitClean,
      base_sha_current: baseShaCurrent,
      dependencies_met: dependenciesMet,
      commands_trusted: commandsTrusted,
    },
    summary: valid
      ? `Package ${packageId} is valid and ready to seal.`
      : `Package ${packageId} has ${errorCount} error(s) to fix before sealing.`,
  };
}

// ─── seal ───

export interface SealResult {
  package_id: string;
  plan_revision: number;
  package_hash: string;
  status: "sealed";
  sealed_at: string;
  summary: PackageSummary;
}

/**
 * Seal a TaskPackage — freeze it, compute final hash, mark as sealed.
 * Only valid packages can be sealed. Sealed packages are immutable;
 * any field change requires a new revision.
 *
 * With the "switch = authorization" model a sealed package is claimable by
 * the Runner on its next poll once the project AND epic switches are ON —
 * no explicit enqueue_step required. Use executor "manual" to keep a package
 * sealed for tracking only.
 */
export async function sealTaskPackage(
  cwd: string,
  packageId: string,
): Promise<SealResult | { error: string }> {
  // Immutability gate FIRST: only a draft (or a blocked/failed package being
  // deliberately revised) may be sealed. Re-sealing a sealed/ready package
  // would silently overwrite sealed_at/hash/revision — breaking the
  // "sealed = frozen" invariant the Runner's hash verification relies on.
  // To change content, prepare a new revision instead.
  const existing = await getPackage(packageId);
  if ("error" in existing) return { error: existing.error };
  if (!["draft", "blocked", "failed"].includes(existing.pkg.status)) {
    return {
      error: `package ${packageId} is '${existing.pkg.status}' — sealed packages are immutable; prepare a new revision (prepare_task_package) instead of re-sealing`,
    };
  }

  // Then validate
  const valResult = await validateTaskPackage(cwd, packageId);
  if ("error" in valResult) return { error: valResult.error };
  if (!valResult.valid) {
    const errors = valResult.issues
      .filter((i) => i.severity === "error")
      .map((i) => `  • ${i.field}: ${i.message}`)
      .join("\n");
    return { error: `cannot seal — ${valResult.issues.filter((i) => i.severity === "error").length} validation error(s):\n${errors}` };
  }

  // Get package and seal it
  const pkg = existing.pkg;

  // sealed_at is part of the immutable content, so set it before hashing.
  pkg.sealed_at = new Date().toISOString();

  // If content changed since draft, bump revision BEFORE hashing so the
  // hash reflects the final revision number (plan_revision is part of the
  // hashable content). Computing the hash first, then bumping revision,
  // would freeze a stale hash that doesn't match the stored revision.
  const { status: _s0, package_hash: draftHash, ...hashableDraft } = pkg;
  const draftContentHash = computePackageHash(
    hashableDraft as Omit<TaskPackage, "package_hash" | "status">,
  );
  if (pkg.package_hash && pkg.package_hash !== draftContentHash) {
    pkg.plan_revision += 1;
  }

  // Now compute the final hash over the fully-finalized content (including
  // any revision bump). This is the immutable hash that Runner will verify.
  const { status: _s, package_hash: _h, ...hashable } = pkg;
  const finalHash = computePackageHash(hashable as Omit<TaskPackage, "package_hash" | "status">);

  pkg.package_hash = finalHash;
  pkg.status = "sealed";

  const saveResult = await savePackage(pkg);
  if ("error" in saveResult) return { error: saveResult.error };

  // Write package summary back to .epics.json on the step — in the PACKAGE's
  // project, not the caller's cwd. The seal tool only receives package_id
  // (no project_dir), so cwd is the MCP server's own project; a cross-project
  // package would fail writeback with "step not found" and get cancelled.
  const summary = packageSummary(pkg);
  const { updateStep } = await import("./epic-store.js");
  const summaryResult = await updateStep(pkg.project_path, pkg.step_id, {
    package: {
      package_id: summary.package_id,
      plan_revision: summary.plan_revision,
      package_hash: summary.package_hash,
      objective: summary.objective,
      executor: summary.executor,
      status: summary.status,
    },
  });
  if ("error" in summaryResult) {
    await updatePackageStatus(pkg.package_id, "cancelled");
    return { error: `package summary writeback failed; package was cancelled: ${summaryResult.error}` };
  }

  return {
    package_id: pkg.package_id,
    plan_revision: pkg.plan_revision,
    package_hash: pkg.package_hash,
    status: "sealed",
    sealed_at: pkg.sealed_at,
    summary,
  };
}

// ─── push_step_with_package (create step + seal package atomically) ───

export interface PushStepWithPackageInput extends Omit<PrepareInput, "step_id"> {
  /** Step metadata */
  title: string;
  description?: string;
  depends_on?: string[];
}

export interface PushStepWithPackageResult {
  step_id: string;
  package_id: string;
  plan_revision: number;
  package_hash: string;
  status: "sealed";
  summary: PackageSummary;
}

/**
 * Create a step AND seal its standard package in one atomic operation.
 * If prepare/seal fails, the step is rolled back (deleted) so no half-created
 * step without a package is left behind.
 *
 * This is the "create step = create package" flow: the conversation-side AI
 * passes both step metadata and package parameters, and the tool internally
 * runs pushStep → prepareTaskPackage → validateTaskPackage → sealTaskPackage.
 */
export async function pushStepWithPackage(
  cwd: string,
  input: PushStepWithPackageInput,
): Promise<PushStepWithPackageResult | { error: string }> {
  const { pushStep, deleteStep } = await import("./epic-store.js");

  // 1. Create the step
  const stepResult = await pushStep(cwd, {
    epic_id: input.epic_id,
    title: input.title,
    description: input.description,
    depends_on: input.depends_on,
  });
  if ("error" in stepResult) return { error: stepResult.error };
  const stepId = stepResult.step.id;

  // 2. Prepare the package (server resolves project_path/repo_root/base_sha)
  const prepResult = await prepareTaskPackage(cwd, {
    ...input,
    step_id: stepId,
  });
  if ("error" in prepResult) {
    await deleteStep(cwd, stepId);
    return { error: `prepare failed (step rolled back): ${prepResult.error}` };
  }

  // 3. Validate
  const valResult = await validateTaskPackage(cwd, prepResult.package_id);
  if ("error" in valResult) {
    await deleteStep(cwd, stepId);
    return { error: `validate failed (step rolled back): ${valResult.error}` };
  }
  if (!valResult.valid) {
    await deleteStep(cwd, stepId);
    const errors = valResult.issues
      .filter((i) => i.severity === "error")
      .map((i) => `${i.field}: ${i.message}`)
      .join("; ");
    return { error: `validation failed (step rolled back): ${errors}` };
  }

  // 4. Seal (also writes step.package summary back to .epics.json)
  const sealResult = await sealTaskPackage(cwd, prepResult.package_id);
  if ("error" in sealResult) {
    await deleteStep(cwd, stepId);
    return { error: `seal failed (step rolled back): ${sealResult.error}` };
  }

  return {
    step_id: stepId,
    package_id: sealResult.package_id,
    plan_revision: sealResult.plan_revision,
    package_hash: sealResult.package_hash,
    status: "sealed",
    summary: sealResult.summary,
  };
}

// ─── enqueue ───

export interface EnqueueResult {
  step_id: string;
  package_id: string;
  status: "ready";
  message: string;
  /** Whether the Runner will actually pick up this package on its next poll. */
  runner_eligible: boolean;
  /** Blocking reasons if runner_eligible is false (empty if true). */
  blockers: ReadinessBlocker[];
}

/**
 * Enqueue a sealed TaskPackage for Runner execution (explicit, optional).
 * Only sealed packages with no supersession can be enqueued.
 * Since the "switch = authorization" change the Runner picks up both
 * "sealed" and "ready" packages whose switches are ON — enqueue is a
 * conversation-side convenience, not a precondition.
 */
export async function enqueueStep(
  cwd: string,
  stepId: string,
): Promise<EnqueueResult | { error: string }> {
  // Get the step's latest package
  const pkgResult = await getPackage(`pkg_${stepId}`); // convention: package references by step
  // Actually packages have their own IDs — we need to search by step
  // For now, require the caller to know the package_id. But the spec says
  // enqueue takes a step_id. Let's search packages for this step.
  // We'll use a helper.
  const { getPackagesForStep } = await import("./runtime-store.js");
  const pkgs = await getPackagesForStep(stepId);
  if (pkgs.length === 0) {
    return { error: `no task package found for step '${stepId}'. prepare and seal one first.` };
  }
  const pkg = pkgs[0];

  if (pkg.status !== "sealed") {
    return {
      error: `package ${pkg.package_id} is '${pkg.status}', not 'sealed'. Seal it first.`,
    };
  }

  if (pkg.executor.preferred === "manual") {
    return {
      error: `package ${pkg.package_id} uses the manual policy. It remains sealed for tracking and is never placed on the automatic Runner queue.`,
    };
  }
  if (pkg.executor.preferred !== "zcode") {
    return {
      error: `executor '${pkg.executor.preferred}' is forbidden by the production Runner policy; use zcode or manual`,
    };
  }

  // Double-check validity (base SHA may have drifted since seal)
  const valResult = await validateTaskPackage(cwd, pkg.package_id);
  if ("error" in valResult) return { error: valResult.error };
  if (!valResult.valid) {
    const errors = valResult.issues
      .filter((i) => i.severity === "error")
      .map((i) => `  • ${i.field}: ${i.message}`)
      .join("\n");
    return { error: `cannot enqueue — validation failed:\n${errors}` };
  }

  // Move to ready
  const statusResult = await updatePackageStatus(pkg.package_id, "ready");
  if ("error" in statusResult) return { error: statusResult.error };

  // Readiness gate: check Runner-side filters so the caller knows whether
  // the package will actually be picked up (project enabled, epic auto_execute,
  // not already in-flight, acceptance commands registered). Non-blocking —
  // we already flipped status to ready; this just reports eligibility.
  const readiness = await verifyPickupReadiness(pkg.package_id);
  if ("error" in readiness) {
    // Readiness check failed (e.g. package suddenly gone) — package is enqueued
    // but we can't verify eligibility. Report as not eligible with the error.
    return {
      step_id: stepId,
      package_id: pkg.package_id,
      status: "ready" as const,
      runner_eligible: false,
      blockers: [{ check: "readiness_check", reason: readiness.error }],
      message: `Step '${stepId}' enqueued but readiness check failed — ${readiness.error}`,
    };
  }

  return {
    step_id: stepId,
    package_id: pkg.package_id,
    status: "ready",
    runner_eligible: readiness.will_be_picked_up,
    blockers: readiness.blockers,
    message: readiness.will_be_picked_up
      ? `Step '${stepId}' enqueued. Package ${pkg.package_id} (rev ${pkg.plan_revision}) is ready for Runner execution.`
      : `Step '${stepId}' enqueued but BLOCKED — ${readiness.blockers.map((b) => b.reason).join("; ")}`,
  };
}

// ─── readiness gate ───

export interface ReadinessBlocker {
  check: string;
  reason: string;
}

export interface ReadinessResult {
  package_id: string;
  status: string;
  will_be_picked_up: boolean;
  blockers: ReadinessBlocker[];
  runner_side_checks: {
    global_enabled: boolean;
    project_enabled: boolean;
    epic_auto_execute_allows: boolean;
    runner_thread_alive: boolean;
    already_in_flight: boolean;
  };
  acceptance_commands_available: boolean;
}

/** Runtime config shape written by Rust runner_config.rs. */
interface RunnerConfigFile {
  version: number;
  global_enabled?: boolean;
  projects: Record<string, { enabled: boolean }>;
  revision?: number;
}

/** Execution status values considered "in-flight" by the Runner.
 * Rust ExecutionStatus serializes snake_case (`#[serde(rename_all =
 * "snake_case")]`) — an earlier PascalCase version of this set never matched
 * any real execution. Keep these exact. */
export const IN_FLIGHT_STATUSES = new Set([
  "queued",
  "running",
  "candidate",
  "verifying",
  "cancel_requested",
  "pause_requested",
]);

/** Execution record shape (subset of Rust Execution struct). */
interface ExecutionRecord {
  id: string;
  package_id: string;
  status: string;
}

interface ExecutionsFile {
  version: number;
  executions: ExecutionRecord[];
  revision?: number;
}

/**
 * Resolve the runtime data directory (mirrors runtime-store.ts runtimeDir).
 */
function resolveRuntimeDir(): string {
  if (process.env.QUESTLOG_RUNTIME) return process.env.QUESTLOG_RUNTIME;
  return path.join(os.homedir(), "questlog-runtime");
}

/**
 * Read runner-config.json. Returns null if missing/corrupt (fail-safe:
 * treat as "nothing enabled" — matches Rust's fail-closed semantics).
 */
async function readRunnerConfig(): Promise<RunnerConfigFile | null> {
  const filePath = path.join(resolveRuntimeDir(), "runner-config.json");
  try {
    const raw = await fs.readFile(filePath, "utf-8");
    return JSON.parse(raw) as RunnerConfigFile;
  } catch (err) {
    const code = (err as NodeJS.ErrnoException).code;
    if (code === "ENOENT") return null;
    // Corrupt — fail-closed (Rust backs up + returns default all-off)
    return null;
  }
}

/**
 * Read executions.json. Returns [] if missing/corrupt.
 */
async function readExecutions(): Promise<ExecutionRecord[]> {
  const filePath = path.join(resolveRuntimeDir(), "executions.json");
  try {
    const raw = await fs.readFile(filePath, "utf-8");
    const data = JSON.parse(raw) as ExecutionsFile;
    return data.executions ?? [];
  } catch {
    return [];
  }
}

/**
 * Verify whether a sealed/ready package will actually be picked up by the
 * Runner's next poll. Mirrors the filters in runner.rs::poll_once:
 *   1. Project must be enabled in runner-config.json
 *   2. Epic auto_execute must not be false
 *   3. No other execution for this package is already in-flight
 * Plus checks acceptance commands are registered (would fail at verify time).
 *
 * Does NOT mutate any state. Callable before or after enqueue_step.
 */
export async function verifyPickupReadiness(
  packageId: string,
): Promise<ReadinessResult | { error: string }> {
  const pkgResult = await getPackage(packageId);
  if ("error" in pkgResult) return { error: pkgResult.error };
  const pkg = pkgResult.pkg;

  const blockers: ReadinessBlocker[] = [];

  // Check 0: the master kill switch. Mirrors poll_once's first gate —
  // without it, MCP would report "eligible" while the Runner never polls.
  const config = await readRunnerConfig();
  const globalEnabled = config?.global_enabled === true;
  if (!globalEnabled) {
    blockers.push({
      check: "global_enabled",
      reason:
        "runner-config.json global_enabled is not true — the master kill switch is OFF, no package will be claimed regardless of project/epic switches",
    });
  }

  // Check 1: project enabled
  const projectEnabled = config?.projects?.[pkg.project_path]?.enabled === true;
  if (!projectEnabled) {
    blockers.push({
      check: "project_enabled",
      reason: `project '${pkg.project_path}' is not enabled in runner-config.json (or file missing/corrupt)`,
    });
  }

  // Check 2: epic auto_execute (read .epics.json). Fail-closed on BOTH
  // ends: only an explicit `auto_execute: true` authorizes — null is
  // fail-closed (no implicit inheritance from the project switch), and a
  // missing/unreadable epic file or unknown epic is a BLOCKER (a package
  // whose epic can't be verified must not report "will be picked up" and
  // then burn an attempt into PreflightBlocked).
  let epicAutoExecuteAllows = true;
  const epicsResult = await readEpicsSafe(pkg.project_path);
  if ("ok" in epicsResult) {
    const epic = epicsResult.data.epics.find((e) => e.id === pkg.epic_id);
    if (!epic) {
      epicAutoExecuteAllows = false;
      blockers.push({
        check: "epic_auto_execute_allows",
        reason: `epic '${pkg.epic_id}' not found in ${pkg.project_path}/.epics.json`,
      });
    } else if (epic.auto_execute !== true) {
      epicAutoExecuteAllows = false;
      blockers.push({
        check: "epic_auto_execute_allows",
        reason:
          epic.auto_execute === false
            ? `epic '${pkg.epic_id}' has auto_execute=false (never auto-execute)`
            : `epic '${pkg.epic_id}' has no explicit auto_execute=true (switch not opened)`,
      });
    }
  } else {
    epicAutoExecuteAllows = false;
    blockers.push({
      check: "epic_auto_execute_allows",
      reason: `.epics.json ${"notFound" in epicsResult ? "missing" : "unreadable"} in ${pkg.project_path} — cannot verify epic authorization`,
    });
  }

  // Check 3: already in-flight
  const executions = await readExecutions();
  const alreadyInFlight = executions.some(
    (e) =>
      e.package_id === pkg.package_id && IN_FLIGHT_STATUSES.has(e.status),
  );
  if (alreadyInFlight) {
    blockers.push({
      check: "already_in_flight",
      reason: `an execution for package ${pkg.package_id} is already in-flight (Queued/Running/Candidate/Verifying)`,
    });
  }

  // Check 4: acceptance commands all registered
  let commandsAvailable = true;
  for (const check of pkg.acceptance) {
    if (check.type === "command" && check.command_id) {
      if (!resolveAcceptanceCommand(check.command_id)) {
        commandsAvailable = false;
        blockers.push({
          check: "acceptance_commands_available",
          reason: `acceptance command_id '${check.command_id}' is not in the trusted registry`,
        });
      }
    }
  }

  // Runner thread is alive iff the master kill switch is ON and at least
  // one project is enabled (the Rust Runner's exact thread-management rule).
  const anyProjectEnabled = config
    ? Object.values(config.projects).some((p) => p.enabled)
    : false;
  const runnerThreadAlive = globalEnabled && anyProjectEnabled;

  const willBePickedUp =
    blockers.length === 0 &&
    (pkg.status === "ready" || pkg.status === "sealed");

  return {
    package_id: pkg.package_id,
    status: pkg.status,
    will_be_picked_up: willBePickedUp,
    blockers,
    runner_side_checks: {
      global_enabled: globalEnabled,
      project_enabled: projectEnabled,
      epic_auto_execute_allows: epicAutoExecuteAllows,
      runner_thread_alive: runnerThreadAlive,
      already_in_flight: alreadyInFlight,
    },
    acceptance_commands_available: commandsAvailable,
  };
}
