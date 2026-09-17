/**
 * TaskPackage v1 — the standard, server-sealed execution contract.
 *
 * A TaskPackage is the ONLY thing the Runner accepts as input for automatic
 * execution. It is created via `prepare`, validated via `validate`, frozen
 * via `seal`, and only a sealed package can be enqueued.
 *
 * Design principles:
 * - The server reads real project paths, Git base SHA, and policy — it never
 *   trusts model-supplied base SHA, authority, or paths.
 * - A sealed package is immutable; any field change creates a new revision
 *   and new hash, superseding the old package.
 * - The full package (with absolute paths, CLI sessions) is stored only in
 *   the local Runtime Store — never in .epics.json. The local Plan Store keeps
 *   only the portable package ID, revision, hash, and summary; it is not part
 *   of the candidate source commit truth.
 */

import crypto from "node:crypto";
import { promises as fs } from "node:fs";
import path from "node:path";

// ─── Types ───

export const SCHEMA_VERSION = 1 as const;

export interface TaskScope {
  /** Glob patterns for paths the executor MAY modify (existing files). */
  allowed_paths: string[];
  /**
   * TaskScope v2 (S3): glob patterns for paths the executor MAY CREATE.
   * Empty/absent on v1 packages — creation then falls back to
   * allowed_paths (unchanged v1 behaviour). Creation rights never apply to a
   * file that already exists in the base tree.
   */
  create_paths?: string[];
  /** Glob patterns for paths the executor MUST NOT touch. */
  forbidden_paths: string[];
  /**
   * 待办租户 G3 (§4.3): the approved READ scope for strict read-only
   * packages. This is a SEPARATE grant — the write-semantics fields above
   * (allowed_paths/create_paths) never double as read authorization, and
   * read_paths never grants any write right.
   */
  read_paths?: string[];
  /** Maximum number of files the executor may change — the LAST-RESORT
   *  breaker against runaway formatters/generators, not a work quota.
   *  Strict read-only packages MUST set 0 (no write rights at all). */
  max_changed_files: number;
  /** S3 three-tier semantics: exceeding the expected estimate is only
   *  recorded; exceeding the warn threshold annotates the bundle and the
   *  candidate but does NOT block; only max trips the breaker. */
  expected_changed_files?: number;
  warn_changed_files?: number;
}

export interface Precondition {
  type: "step_stage" | "git_clean";
  /** For step_stage: the step that must have reached this stage. */
  step_id?: string;
  /** For step_stage: the minimum stage required. */
  at_least?: StepStage;
  /** For git_clean: whether the working tree must be clean. */
  value?: boolean;
}

export interface ExecutorConfig {
  preferred: ExecutorKind;
  fallbacks: ExecutorKind[];
  sandbox: "workspace-write" | "read-only";
  timeout_seconds: number;
  max_attempts: number;
}

export interface AcceptanceCheck {
  type: "command" | "git_diff_scope" | "verifier";
  /** For command: a registered command_id from the trusted acceptance registry. */
  command_id?: string;
  /** For verifier: the verifier identifier. */
  verifier_id?: string;
}

/**
 * 待办租户 G3 (§8): native verifier registry. A verifier is a code-level
 * check the HOST runs against execution evidence — it never executes
 * anything from the report content. Unknown verifier ids refuse to seal.
 *   todo:report — structured research-report verification (todo-report.ts);
 *     canonical implementation runs on the manager during receipt
 *     reconciliation; the LOCAL Runner cannot execute it and fails closed.
 */
export const KNOWN_VERIFIERS: readonly string[] = ["todo:report"];

export interface Authority {
  auto_edit_worktree: boolean;
  auto_commit_candidate: boolean;
  auto_merge: boolean;
  auto_push: boolean;
  auto_deploy: boolean;
}

export type StopCondition =
  | "scope_expanded"
  | "base_sha_drift"
  | "unapproved_external_effect"
  | "repeated_failure";

export interface Rollback {
  strategy: "preserve_worktree_and_stop" | "abort_and_clean";
}

/** Runtime status of a package — separate from the sealed content. */
export type PackageStatus =
  | "draft" // prepare called, not yet sealed
  | "sealed" // frozen; claimable by the Runner once project+epic switches are ON
  | "ready" // explicitly enqueued (optional — switch=authorization model)
  | "superseded" // a newer revision exists
  | "cancelled"
  | "passed" // Runner produced and verified an exact candidate commit
  | "failed" // unexpected Runner failure; evidence is preserved
  | "blocked"; // human decision or package revision is required

export type StepStage = "pending" | "designed" | "coded" | "verified" | "observed";
export type ExecutorKind = "codex" | "zcode" | "manual";

/**
 * Provenance of a package — what authorized its creation (待办租户 plan §4.2).
 * Carried through the sealed hash so a package can never be re-attributed.
 * `type` is a closed set: the Runner refuses automatic execution of packages
 * with an unknown origin type.
 */
export interface PackageOrigin {
  type: "todo";
  /** Stable tenant identity (the registered project's canonical dir name). */
  tenant_id: string;
  todo_id: string;
  /** SHA-256 of the frozen task content (task-key.ts computeTaskRevision). */
  task_revision: string;
  /** The auto_process_revision that authorized this task. */
  consent_revision: number;
  /** SHA-256(tenant_id, todo_id, task_revision, consent_revision) — one
   *  authorization's one-shot work identity; terminal task_keys never re-run. */
  task_key: string;
}

/** The full, sealed TaskPackage — stored only in local Runtime Store. */
export interface TaskPackage {
  schema_version: typeof SCHEMA_VERSION;
  package_id: string;
  /** Human-readable package number, stable across plan revisions
   *  of the same package_id (assigned by prepare, indexed for the UI). */
  display_no?: number;
  epic_id: string;
  step_id: string;
  /** Incremented on every field change; old revisions are superseded. */
  plan_revision: number;
  source_session?: string;
  /** Provenance binding (待办租户): part of the sealed hash. */
  origin?: PackageOrigin;
  /** Canonical absolute project path (resolved by server, not model). */
  project_path: string;
  /** Canonical absolute git repo root (resolved by server). */
  repo_root: string;
  /** Full git commit SHA at seal time — the worktree base. */
  base_sha: string;
  objective: string;
  context_refs: string[];
  scope: TaskScope;
  preconditions: Precondition[];
  executor: ExecutorConfig;
  acceptance: AcceptanceCheck[];
  authority: Authority;
  stop_conditions: StopCondition[];
  rollback: Rollback;
  /** Status (mutable runtime field, not part of the sealed hash). */
  status: PackageStatus;
  sealed_at: string;
  /** SHA-256 of the sealed content (all fields except status/hash themselves). */
  package_hash: string;
}

/** Portable summary stored in .epics.json (no absolute paths or secrets). */
export interface PackageSummary {
  package_id: string;
  plan_revision: number;
  package_hash: string;
  objective: string;
  executor: string;
  status: PackageStatus;
}

// ─── Hash computation ───

/**
 * Compute a deterministic SHA-256 hash of the package content.
 * Excludes `status` and `package_hash` (they are runtime metadata, not content).
 * The hash makes any field change detectable — a modified package must have
 * a different hash, and therefore a different revision.
 */
export function computePackageHash(pkg: Omit<TaskPackage, "package_hash" | "status">): string {
  const canonicalize = (value: unknown): unknown => {
    if (Array.isArray(value)) return value.map(canonicalize);
    if (value && typeof value === "object") {
      const out: Record<string, unknown> = {};
      for (const key of Object.keys(value as Record<string, unknown>).sort()) {
        if (key === "status" || key === "package_hash") continue;
        out[key] = canonicalize((value as Record<string, unknown>)[key]);
      }
      return out;
    }
    return value;
  };
  const canonical = JSON.stringify(canonicalize(pkg));
  return crypto.createHash("sha256").update(canonical).digest("hex");
}

// ─── Validation ───

export interface ValidationIssue {
  field: string;
  message: string;
  severity: "error" | "warning";
}

/**
 * Validate a draft TaskPackage against the schema and policy constraints.
 * Returns a list of issues — errors block sealing, warnings are informational.
 *
 * This does NOT check Git state or project paths — those are resolved by
 * the server during prepare/validate, not by the model.
 */
export function validatePackageContent(pkg: Partial<TaskPackage>): ValidationIssue[] {
  const issues: ValidationIssue[] = [];

  if (!pkg.objective?.trim()) {
    issues.push({ field: "objective", message: "objective is required", severity: "error" });
  }

  if (!pkg.epic_id?.trim()) {
    issues.push({ field: "epic_id", message: "epic_id is required", severity: "error" });
  }
  if (!pkg.step_id?.trim()) {
    issues.push({ field: "step_id", message: "step_id is required", severity: "error" });
  }

  // Scope validation — policy dichotomy (待办租户 G3 §4.3):
  //   workspace-write / legacy read-only: existing rules (allowed_paths
  //     required, max_changed_files ≥ 1)
  //   STRICT read-only (sandbox=read-only AND authority edit/commit false AND
  //     max_changed_files 0): NO write rights at all — allowed/create paths
  //     must be empty, read_paths required
  const isStrictReadOnly =
    pkg.executor?.sandbox === "read-only" &&
    pkg.authority != null &&
    !pkg.authority.auto_edit_worktree &&
    !pkg.authority.auto_commit_candidate &&
    !pkg.authority.auto_merge &&
    !pkg.authority.auto_push &&
    !pkg.authority.auto_deploy &&
    pkg.scope?.max_changed_files === 0;
  if (pkg.scope) {
    if (isStrictReadOnly) {
      if ((pkg.scope.allowed_paths?.length ?? 0) > 0) {
        issues.push({ field: "scope.allowed_paths", message: "read-only policy grants no write rights — allowed_paths must be empty", severity: "error" });
      }
      if ((pkg.scope.create_paths?.length ?? 0) > 0) {
        issues.push({ field: "scope.create_paths", message: "read-only policy grants no creation rights — create_paths must be empty", severity: "error" });
      }
      if ((pkg.scope.read_paths?.length ?? 0) === 0) {
        issues.push({ field: "scope.read_paths", message: "read-only policy requires the approved read scope (read_paths)", severity: "error" });
      }
      if (pkg.scope.expected_changed_files !== undefined || pkg.scope.warn_changed_files !== undefined) {
        issues.push({ field: "scope.expected_changed_files", message: "read-only policy has no change tiers — omit expected/warn counts", severity: "error" });
      }
    } else {
      if (pkg.scope.allowed_paths.length === 0) {
        issues.push({ field: "scope.allowed_paths", message: "at least one allowed path is required", severity: "error" });
      }
      if (pkg.scope.max_changed_files < 1) {
        issues.push({ field: "scope.max_changed_files", message: "must be ≥ 1", severity: "error" });
      }
    }
    // S3 (TaskScope v2): repository-wide creation rights are forbidden — a
    // pattern of "**" or starting with "**/" grants create access everywhere
    // and must be narrowed to an explicit subtree (e.g. "docs/**").
    for (const pattern of pkg.scope.create_paths ?? []) {
      if (pattern === "**" || pattern.startsWith("**/")) {
        issues.push({
          field: "scope.create_paths",
          message: `repository-wide creation pattern '${pattern}' is forbidden — list an explicit subtree instead`,
          severity: "error",
        });
      }
    }
    // Three-tier count semantics must be monotonically ordered.
    const expected = pkg.scope.expected_changed_files;
    const warn = pkg.scope.warn_changed_files;
    if (expected !== undefined && warn !== undefined && expected > warn) {
      issues.push({
        field: "scope.expected_changed_files",
        message: `expected (${expected}) must be ≤ warn (${warn})`,
        severity: "error",
      });
    }
    if (warn !== undefined && warn > pkg.scope.max_changed_files) {
      issues.push({
        field: "scope.warn_changed_files",
        message: `warn (${warn}) must be ≤ max (${pkg.scope.max_changed_files})`,
        severity: "error",
      });
    }
    // S4: the fourth tier ordering — an expected estimate above the breaker
    // means the plan expects to trip its own last resort. Malformed.
    if (expected !== undefined && expected > pkg.scope.max_changed_files) {
      issues.push({
        field: "scope.expected_changed_files",
        message: `expected (${expected}) must be ≤ max (${pkg.scope.max_changed_files}) — the plan cannot expect to trip its own breaker`,
        severity: "error",
      });
    }
  } else {
    issues.push({ field: "scope", message: "scope is required", severity: "error" });
  }

  // Executor validation
  if (pkg.executor) {
    const validExecutors: ExecutorKind[] = ["codex", "zcode", "manual"];
    if (!validExecutors.includes(pkg.executor.preferred)) {
      issues.push({ field: "executor.preferred", message: `must be one of: ${validExecutors.join(", ")}`, severity: "error" });
    }
    if (pkg.executor.timeout_seconds < 60) {
      issues.push({ field: "executor.timeout_seconds", message: "must be ≥ 60s", severity: "warning" });
    }
    if (pkg.executor.max_attempts < 1) {
      issues.push({ field: "executor.max_attempts", message: "must be ≥ 1", severity: "error" });
    }
    if (pkg.executor.preferred === "codex") {
      issues.push({
        field: "executor.preferred",
        message: "codex is design/push-only and is forbidden as a production Runner executor",
        severity: "error",
      });
    }
    if (pkg.executor.fallbacks.length > 0) {
      issues.push({
        field: "executor.fallbacks",
        message: "automatic executor fallback is disabled; use a DecisionRequest instead",
        severity: "error",
      });
    }
  } else {
    issues.push({ field: "executor", message: "executor config is required", severity: "error" });
  }

  // Acceptance validation
  if (!pkg.acceptance || pkg.acceptance.length === 0) {
    issues.push({ field: "acceptance", message: "at least one acceptance check is required", severity: "error" });
  } else {
    for (const check of pkg.acceptance) {
      if (check.type === "command" && !check.command_id) {
        issues.push({ field: "acceptance.command_id", message: "command checks require a command_id", severity: "error" });
      }
      if (check.type === "verifier") {
        if (!check.verifier_id) {
          issues.push({ field: "acceptance.verifier_id", message: "verifier checks require a verifier_id", severity: "error" });
        } else if (!KNOWN_VERIFIERS.includes(check.verifier_id)) {
          issues.push({ field: "acceptance.verifier_id", message: `unknown verifier '${check.verifier_id}' — known: ${KNOWN_VERIFIERS.join(", ")}`, severity: "error" });
        }
      }
    }
  }

  // Origin validation (待办租户 G2): provenance is optional on legacy
  // packages, but when present it must be a COMPLETE, well-formed binding —
  // a partial origin can never authorize anything and must not seal.
  if (pkg.origin !== undefined) {
    if (pkg.origin.type !== "todo") {
      issues.push({
        field: "origin.type",
        message: `unknown origin type '${String(pkg.origin.type)}' — only 'todo' is a known source`,
        severity: "error",
      });
    }
    for (const key of ["tenant_id", "todo_id", "task_revision", "task_key"] as const) {
      if (!String(pkg.origin[key] ?? "").trim()) {
        issues.push({ field: `origin.${key}`, message: "origin requires a non-empty value", severity: "error" });
      }
    }
    if (!(pkg.origin.consent_revision >= 1)) {
      issues.push({
        field: "origin.consent_revision",
        message: "must be ≥ 1 (authorization revisions start at 1)",
        severity: "error",
      });
    }
  }

  // Authority validation — default policy must be conservative
  if (pkg.authority) {
    if (pkg.authority.auto_merge) {
      issues.push({ field: "authority.auto_merge", message: "auto_merge is forbidden in the default policy", severity: "error" });
    }
    if (pkg.authority.auto_push) {
      issues.push({ field: "authority.auto_push", message: "auto_push is forbidden in the default policy", severity: "error" });
    }
    if (pkg.authority.auto_deploy) {
      issues.push({ field: "authority.auto_deploy", message: "auto_deploy is forbidden in the default policy", severity: "error" });
    }
  }

  return issues;
}

// ─── Trusted Acceptance Command Registry ───

/**
 * The acceptance registry maps stable command IDs to real shell commands.
 * TaskPackages reference commands by ID — they never contain raw shell strings.
 * This prevents the model from injecting arbitrary commands as acceptance criteria.
 *
 * New commands must be added here by a human (code review = approval).
 */

export interface TrustedCommand {
  id: string;
  /** The real command to execute. */
  command: string;
  /** Working directory relative to repo root. */
  cwd: string;
  /** Human-readable description. */
  description: string;
}

const ACCEPTANCE_REGISTRY: Record<string, TrustedCommand> = {
  "verify:candidate": {
    id: "verify:candidate",
    command: "git diff --check HEAD^ HEAD",
    cwd: ".",
    description: "Verify the exact candidate commit has a clean patch",
  },
  "verify:readonly": {
    id: "verify:readonly",
    command: "git diff --quiet HEAD",
    cwd: ".",
    description:
      "Read-only artifact contract: worktree unchanged vs base (pair with a non-empty stdout report)",
  },
  // ── TypeScript (MCP server) ──
  "test:mcp": {
    id: "test:mcp",
    command: "npm test",
    cwd: "mcp-server",
    description: "Run MCP server unit tests",
  },
  "build:mcp": {
    id: "build:mcp",
    command: "npm run build",
    cwd: "mcp-server",
    description: "Build MCP server TypeScript",
  },
  "lint:mcp": {
    id: "lint:mcp",
    command: "npx tsc --noEmit",
    cwd: "mcp-server",
    description: "Type-check MCP server",
  },
  // ── Rust (desktop backend) — cargo:* run from src-tauri ──
  "cargo:check": {
    id: "cargo:check",
    command: "cargo check",
    cwd: "desktop-app/src-tauri",
    description: "Fast type/compile check (default first choice for Rust tasks)",
  },
  "cargo:clippy": {
    id: "cargo:clippy",
    command: "cargo clippy -- -D warnings",
    cwd: "desktop-app/src-tauri",
    description: "Strict lint on desktop app Rust",
  },
  "cargo:build": {
    id: "cargo:build",
    command: "cargo build",
    cwd: "desktop-app/src-tauri",
    description: "Compile desktop app Rust (no tests)",
  },
  "test:desktop": {
    id: "test:desktop",
    command: "cargo test",
    cwd: "desktop-app/src-tauri",
    description: "Run desktop app Rust tests",
  },
  // ── Frontend (desktop web) ──
  "build:web": {
    id: "build:web",
    command: "npm run build",
    cwd: "desktop-app",
    description: "Build desktop app frontend",
  },
  "test:web": {
    id: "test:web",
    command: "npm test",
    cwd: "desktop-app",
    description: "Run desktop app frontend vitest",
  },
  "lint:web": {
    id: "lint:web",
    command: "npx tsc --noEmit",
    cwd: "desktop-app",
    description: "Type-check desktop app frontend",
  },
};

export function resolveAcceptanceCommand(commandId: string): TrustedCommand | null {
  return ACCEPTANCE_REGISTRY[commandId] ?? null;
}

export function listAcceptanceCommands(): TrustedCommand[] {
  return Object.values(ACCEPTANCE_REGISTRY);
}

export function isTrustedCommand(commandId: string): boolean {
  return commandId in ACCEPTANCE_REGISTRY;
}

// ─── Git helpers ───

/**
 * Read the current HEAD commit SHA of a git repository.
 * Uses `git rev-parse HEAD` via child_process.
 * Returns null if not in a git repo or git is unavailable.
 */
export async function readGitHeadSha(repoRoot: string): Promise<string | null> {
  try {
    const { execFile } = await import("node:child_process");
    return new Promise((resolve) => {
      execFile("git", ["rev-parse", "HEAD"], { cwd: repoRoot }, (err, stdout) => {
        if (err) {
          resolve(null);
          return;
        }
        resolve(stdout.trim() || null);
      });
    });
  } catch {
    return null;
  }
}

/**
 * Check if the git working tree is clean (no uncommitted TRACKED changes).
 *
 * Untracked files (`??` lines) do NOT count as dirty: the worktree is created
 * from HEAD regardless of untracked files, so they don't affect base_sha
 * semantics. Active projects routinely carry untracked work products
 * (e.g. _Manager plans, .zcode plans).
 */
export async function isGitClean(repoRoot: string): Promise<boolean> {
  try {
    const { execFile } = await import("node:child_process");
    return new Promise((resolve) => {
      execFile("git", ["status", "--porcelain"], { cwd: repoRoot }, (err, stdout) => {
        if (err) {
          resolve(false);
          return;
        }
        const controlFiles = [".todos.json", ".epics.json", ".todo-board.json"];
        const sourceChanges = stdout
          .split("\n")
          .filter(Boolean)
          // Untracked files are a normal state, not dirty source
          .filter((line) => !line.startsWith("??"))
          .map((line) => line.slice(3).replace(/^"|"$/g, ""))
          .filter((file) => !controlFiles.some((name) =>
            file === name || file.endsWith(`/${name}`) || file === `${name}.lock` || file.endsWith(`/${name}.lock`)
          ));
        resolve(sourceChanges.length === 0);
      });
    });
  } catch {
    return false;
  }
}

/**
 * Find the git repo root by walking up from a directory.
 */
export async function findGitRoot(startDir: string): Promise<string | null> {
  let cur = startDir;
  for (;;) {
    try {
      const gitPath = path.join(cur, ".git");
      const st = await fs.stat(gitPath);
      if (st.isDirectory() || st.isFile()) return cur;
    } catch {
      // no .git here
    }
    const parent = path.dirname(cur);
    if (parent === cur) return null;
    cur = parent;
  }
}

// ─── Package factory ───

/**
 * Default authority: conservative. Only worktree edit + candidate commit
 * are allowed by default. merge/push/deploy require explicit human approval.
 */
export function defaultAuthority(): Authority {
  return {
    auto_edit_worktree: true,
    auto_commit_candidate: true,
    auto_merge: false,
    auto_push: false,
    auto_deploy: false,
  };
}

export function defaultExecutor(): ExecutorConfig {
  return {
    preferred: "zcode",
    fallbacks: [],
    sandbox: "workspace-write",
    timeout_seconds: 3600,
    max_attempts: 2,
  };
}

export function defaultStopConditions(): StopCondition[] {
  return ["scope_expanded", "base_sha_drift", "unapproved_external_effect", "repeated_failure"];
}

export function defaultRollback(): Rollback {
  return { strategy: "preserve_worktree_and_stop" };
}
