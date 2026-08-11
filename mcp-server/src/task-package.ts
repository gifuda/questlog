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
  /** Glob patterns for paths the executor MAY modify. */
  allowed_paths: string[];
  /** Glob patterns for paths the executor MUST NOT touch. */
  forbidden_paths: string[];
  /** Maximum number of files the executor may change. */
  max_changed_files: number;
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
  | "sealed" // all validation passed, hash computed
  | "ready" // enqueued, waiting for Runner
  | "superseded" // a newer revision exists
  | "cancelled"
  | "passed" // Runner produced and verified an exact candidate commit
  | "failed" // unexpected Runner failure; evidence is preserved
  | "blocked"; // human decision or package revision is required

export type StepStage = "pending" | "designed" | "coded" | "verified" | "observed";
export type ExecutorKind = "codex" | "zcode" | "manual";

/** The full, sealed TaskPackage — stored only in local Runtime Store. */
export interface TaskPackage {
  schema_version: typeof SCHEMA_VERSION;
  package_id: string;
  epic_id: string;
  step_id: string;
  /** Incremented on every field change; old revisions are superseded. */
  plan_revision: number;
  source_session?: string;
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

  // Scope validation
  if (pkg.scope) {
    if (pkg.scope.allowed_paths.length === 0) {
      issues.push({ field: "scope.allowed_paths", message: "at least one allowed path is required", severity: "error" });
    }
    if (pkg.scope.max_changed_files < 1) {
      issues.push({ field: "scope.max_changed_files", message: "must be ≥ 1", severity: "error" });
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
      if (check.type === "verifier" && !check.verifier_id) {
        issues.push({ field: "acceptance.verifier_id", message: "verifier checks require a verifier_id", severity: "error" });
      }
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
 * Check if the git working tree is clean (no uncommitted changes).
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
