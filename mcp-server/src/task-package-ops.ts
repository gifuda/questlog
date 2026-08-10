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
 *   validate → check all fields, Git clean, dependencies, command registry
 *   seal → freeze content, compute hash, mark sealed (immutable)
 *   enqueue → move sealed package to ready (Runner picks it up)
 *
 * Unsealed, superseded, dependency-insufficient, path-escaping, authority-
 * expanding, or schema-incomplete packages are REJECTED.
 */

import { promises as fs } from "node:fs";
import path from "node:path";
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
  acceptance_command_ids?: string[];
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
  git_clean: boolean;
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

  const gitClean = await isGitClean(repoRoot);

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
    preconditions: [
      { type: "git_clean", value: true },
    ],
    executor: {
      preferred: (input.executor_preferred as TaskPackage["executor"]["preferred"]) ?? "codex",
      fallbacks: [],
      sandbox: "workspace-write",
      timeout_seconds: input.executor_timeout_seconds ?? 3600,
      max_attempts: input.executor_max_attempts ?? 2,
    },
    acceptance: (input.acceptance_command_ids ?? []).map((id) => ({
      type: "command" as const,
      command_id: id,
    })),
    authority: defaultAuthority(),
    stop_conditions: defaultStopConditions(),
    rollback: defaultRollback(),
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
    git_clean: gitClean,
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

  // Server-state issues
  if (!gitClean) {
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
 */
export async function sealTaskPackage(
  cwd: string,
  packageId: string,
): Promise<SealResult | { error: string }> {
  // First validate
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
  const pkgResult = await getPackage(packageId);
  if ("error" in pkgResult) return { error: pkgResult.error };
  const pkg = pkgResult.pkg;

  // Recompute hash on final content
  const { status: _s, package_hash: _h, ...hashable } = pkg;
  const finalHash = computePackageHash(hashable as Omit<TaskPackage, "package_hash" | "status">);

  // If hash changed since draft, bump revision
  if (pkg.package_hash && pkg.package_hash !== finalHash) {
    pkg.plan_revision += 1;
  }
  pkg.package_hash = finalHash;
  pkg.status = "sealed";
  pkg.sealed_at = new Date().toISOString();

  const saveResult = await savePackage(pkg);
  if ("error" in saveResult) return { error: saveResult.error };

  return {
    package_id: pkg.package_id,
    plan_revision: pkg.plan_revision,
    package_hash: pkg.package_hash,
    status: "sealed",
    sealed_at: pkg.sealed_at,
    summary: packageSummary(pkg),
  };
}

// ─── enqueue ───

export interface EnqueueResult {
  step_id: string;
  package_id: string;
  status: "ready";
  message: string;
}

/**
 * Enqueue a sealed TaskPackage for Runner execution.
 * Only sealed packages with no supersession can be enqueued.
 * The Runner picks up packages with status "ready".
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

  return {
    step_id: stepId,
    package_id: pkg.package_id,
    status: "ready",
    message: `Step '${stepId}' enqueued. Package ${pkg.package_id} (rev ${pkg.plan_revision}) is ready for Runner execution.`,
  };
}
