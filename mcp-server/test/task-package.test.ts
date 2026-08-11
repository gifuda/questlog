/**
 * TaskPackage hard-gate tests (G0 acceptance):
 * - Unsealed packages cannot be enqueued
 * - Forged base SHA / authority / paths are rejected
 * - Sealed packages produce deterministic hashes
 * - Validation catches schema errors, untrusted commands, git drift
 * - The trusted acceptance registry prevents arbitrary shell injection
 */

import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { promises as fs } from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";
import {
  validatePackageContent,
  computePackageHash,
  resolveAcceptanceCommand,
  isTrustedCommand,
  listAcceptanceCommands,
  defaultAuthority,
  defaultExecutor,
  isGitClean,
  TaskPackage,
} from "../src/task-package.js";
import {
  prepareTaskPackage,
  validateTaskPackage,
  sealTaskPackage,
  enqueueStep,
} from "../src/task-package-ops.js";
import { getPackage, getPackagesForStep, getReadyPackages } from "../src/runtime-store.js";
import { pushEpic, pushStep } from "../src/epic-store.js";

let dir: string;
let oldRuntime: string | undefined;

beforeEach(async () => {
  dir = await fs.mkdtemp(path.join(os.tmpdir(), "questlog-pkg-"));
  // Isolate runtime store
  oldRuntime = process.env.QUESTLOG_RUNTIME;
  process.env.QUESTLOG_RUNTIME = path.join(dir, "runtime");

  // Initialize a git repo for the test project
  const projectDir = path.join(dir, "test-project");
  await fs.mkdir(projectDir);
  execFileSync("git", ["init"], { cwd: projectDir, stdio: "pipe" });
  execFileSync("git", ["config", "user.email", "test@test.com"], { cwd: projectDir, stdio: "pipe" });
  execFileSync("git", ["config", "user.name", "Test"], { cwd: projectDir, stdio: "pipe" });
  // Need at least one commit for HEAD SHA
  await fs.writeFile(path.join(projectDir, "README.md"), "# Test\n");
  execFileSync("git", ["add", "."], { cwd: projectDir, stdio: "pipe" });
  execFileSync("git", ["commit", "-m", "initial"], { cwd: projectDir, stdio: "pipe" });
  const now = new Date().toISOString();
  await fs.writeFile(path.join(projectDir, ".epics.json"), JSON.stringify({
    version: 1,
    project: "test-project",
    revision: 0,
    epics: [{
      id: "e1",
      title: "Test epic",
      description: "",
      status: "active",
      priority: "p1",
      depends_on: [],
      blocks: [],
      links: {},
      created_at: now,
      updated_at: now,
    }],
    steps: [{
      id: "s1",
      epic_id: "e1",
      title: "Test step",
      description: "",
      order: 1,
      depends_on: [],
      stage: "designed",
      links: {},
      created_at: now,
      updated_at: now,
    }],
  }, null, 2));
});

afterEach(async () => {
  if (oldRuntime === undefined) {
    delete process.env.QUESTLOG_RUNTIME;
  } else {
    process.env.QUESTLOG_RUNTIME = oldRuntime;
  }
  await fs.rm(dir, { recursive: true, force: true });
});

// ─── Pure validation tests ───

describe("validatePackageContent", () => {
  it("rejects missing objective", () => {
    const issues = validatePackageContent({} as Partial<TaskPackage>);
    expect(issues.some((i) => i.field === "objective" && i.severity === "error")).toBe(true);
  });

  it("rejects empty allowed_paths", () => {
    const issues = validatePackageContent({
      objective: "do X",
      epic_id: "e1",
      step_id: "s1",
      scope: { allowed_paths: [], forbidden_paths: [], max_changed_files: 5 },
    });
    expect(issues.some((i) => i.field === "scope.allowed_paths")).toBe(true);
  });

  it("rejects auto_merge, auto_push, auto_deploy in authority", () => {
    const issues = validatePackageContent({
      objective: "do X",
      epic_id: "e1",
      step_id: "s1",
      scope: { allowed_paths: ["src/**"], forbidden_paths: [], max_changed_files: 5 },
      executor: { preferred: "codex", fallbacks: [], sandbox: "workspace-write", timeout_seconds: 3600, max_attempts: 2 },
      acceptance: [{ type: "command", command_id: "test:mcp" }],
      authority: {
        auto_edit_worktree: true,
        auto_commit_candidate: true,
        auto_merge: true, // FORBIDDEN
        auto_push: false,
        auto_deploy: false,
      },
    });
    expect(issues.some((i) => i.field === "authority.auto_merge")).toBe(true);
  });

  it("accepts conservative authority (default)", () => {
    const auth = defaultAuthority();
    expect(auth.auto_merge).toBe(false);
    expect(auth.auto_push).toBe(false);
    expect(auth.auto_deploy).toBe(false);
    expect(auth.auto_edit_worktree).toBe(true);
    expect(auth.auto_commit_candidate).toBe(true);
  });

  it("rejects command acceptance without command_id", () => {
    const issues = validatePackageContent({
      objective: "do X",
      epic_id: "e1",
      step_id: "s1",
      scope: { allowed_paths: ["src/**"], forbidden_paths: [], max_changed_files: 5 },
      executor: { preferred: "codex", fallbacks: [], sandbox: "workspace-write", timeout_seconds: 3600, max_attempts: 2 },
      acceptance: [{ type: "command" }], // missing command_id
    });
    expect(issues.some((i) => i.field === "acceptance.command_id")).toBe(true);
  });

  it("defaults production execution to zcode and rejects codex or fallbacks", () => {
    expect(defaultExecutor().preferred).toBe("zcode");
    const base: Partial<TaskPackage> = {
      objective: "do X",
      epic_id: "e1",
      step_id: "s1",
      scope: { allowed_paths: ["src/**"], forbidden_paths: [], max_changed_files: 5 },
      acceptance: [{ type: "command", command_id: "verify:candidate" }],
      authority: defaultAuthority(),
    };
    const codexIssues = validatePackageContent({
      ...base,
      executor: { ...defaultExecutor(), preferred: "codex" },
    });
    expect(codexIssues.some((issue) => issue.field === "executor.preferred" && issue.severity === "error")).toBe(true);

    const fallbackIssues = validatePackageContent({
      ...base,
      executor: { ...defaultExecutor(), fallbacks: ["manual"] },
    });
    expect(fallbackIssues.some((issue) => issue.field === "executor.fallbacks" && issue.severity === "error")).toBe(true);
  });
});

// ─── Hash determinism ───

describe("computePackageHash", () => {
  it("produces deterministic SHA-256 for same content", () => {
    const pkg = {
      schema_version: 1 as const,
      package_id: "pkg_1",
      epic_id: "e1",
      step_id: "s1",
      plan_revision: 1,
      project_path: "/test",
      repo_root: "/test",
      base_sha: "abc123",
      objective: "test",
      context_refs: [],
      scope: { allowed_paths: ["src/**"], forbidden_paths: [], max_changed_files: 5 },
      preconditions: [],
      executor: { preferred: "codex" as const, fallbacks: [], sandbox: "workspace-write" as const, timeout_seconds: 3600, max_attempts: 2 },
      acceptance: [],
      authority: defaultAuthority(),
      stop_conditions: ["scope_expanded"],
      rollback: { strategy: "preserve_worktree_and_stop" as const },
      sealed_at: "",
    };
    const hash1 = computePackageHash(pkg);
    const hash2 = computePackageHash({ ...pkg });
    expect(hash1).toBe(hash2);
    expect(hash1).toHaveLength(64); // SHA-256 hex
  });

  it("changes when any field changes", () => {
    const pkg = {
      schema_version: 1 as const,
      package_id: "pkg_1",
      epic_id: "e1",
      step_id: "s1",
      plan_revision: 1,
      project_path: "/test",
      repo_root: "/test",
      base_sha: "abc123",
      objective: "test",
      context_refs: [],
      scope: { allowed_paths: ["src/**"], forbidden_paths: [], max_changed_files: 5 },
      preconditions: [],
      executor: { preferred: "codex" as const, fallbacks: [], sandbox: "workspace-write" as const, timeout_seconds: 3600, max_attempts: 2 },
      acceptance: [],
      authority: defaultAuthority(),
      stop_conditions: ["scope_expanded"],
      rollback: { strategy: "preserve_worktree_and_stop" as const },
      sealed_at: "",
    };
    const hash1 = computePackageHash(pkg);
    const hash2 = computePackageHash({ ...pkg, objective: "changed" });
    expect(hash1).not.toBe(hash2);
  });

  it("excludes status and package_hash from hashing", () => {
    const pkg = {
      schema_version: 1 as const,
      package_id: "pkg_1",
      epic_id: "e1",
      step_id: "s1",
      plan_revision: 1,
      project_path: "/test",
      repo_root: "/test",
      base_sha: "abc123",
      objective: "test",
      context_refs: [],
      scope: { allowed_paths: ["src/**"], forbidden_paths: [], max_changed_files: 5 },
      preconditions: [],
      executor: { preferred: "codex" as const, fallbacks: [], sandbox: "workspace-write" as const, timeout_seconds: 3600, max_attempts: 2 },
      acceptance: [],
      authority: defaultAuthority(),
      stop_conditions: ["scope_expanded"],
      rollback: { strategy: "preserve_worktree_and_stop" as const },
      sealed_at: "",
    };
    // @ts-expect-error: status and package_hash are not in the input type
    const hash1 = computePackageHash({ ...pkg, status: "draft", package_hash: "xxx" });
    // @ts-expect-error: different status should produce same hash
    const hash2 = computePackageHash({ ...pkg, status: "sealed", package_hash: "yyy" });
    expect(hash1).toBe(hash2);
  });

  it("canonicalizes nested object keys and matches the Rust vector", () => {
    const first = {
      z: 1,
      a: { y: 2, x: [{ b: 2, a: 1 }] },
      status: "ready",
      package_hash: "ignored",
    };
    const reordered = {
      package_hash: "different",
      status: "blocked",
      a: { x: [{ a: 1, b: 2 }], y: 2 },
      z: 1,
    };
    // Deliberately use a narrow runtime fixture instead of pretending this is
    // a complete TaskPackage; the canonicalizer accepts arbitrary JSON data.
    const hash1 = computePackageHash(first as unknown as Omit<TaskPackage, "package_hash" | "status">);
    const hash2 = computePackageHash(reordered as unknown as Omit<TaskPackage, "package_hash" | "status">);
    expect(hash1).toBe(hash2);
    expect(hash1).toBe("5a0838288edf4a4f2df2106be08daed20d6b4e86ff9e17b00eee9df629cf22c9");
  });
});

// ─── Trusted acceptance registry ───

describe("acceptance registry", () => {
  it("resolves known command IDs", () => {
    const cmd = resolveAcceptanceCommand("test:mcp");
    expect(cmd).not.toBeNull();
    expect(cmd!.command).toBe("npm test");
    expect(cmd!.cwd).toBe("mcp-server");
  });

  it("rejects unknown command IDs", () => {
    expect(resolveAcceptanceCommand("rm -rf /")).toBeNull();
    expect(isTrustedCommand("evil-command")).toBe(false);
  });

  it("lists all registered commands", () => {
    const cmds = listAcceptanceCommands();
    expect(cmds.length).toBeGreaterThanOrEqual(4);
    expect(cmds.some((c) => c.id === "test:mcp")).toBe(true);
  });

  it("registers cargo:check, cargo:clippy, cargo:build for Rust tasks", () => {
    for (const id of ["cargo:check", "cargo:clippy", "cargo:build"]) {
      const cmd = resolveAcceptanceCommand(id);
      expect(cmd, `${id} should be registered`).not.toBeNull();
      expect(cmd!.cwd).toBe("desktop-app/src-tauri");
    }
  });

  it("registers test:web and lint:web for frontend tasks", () => {
    for (const id of ["test:web", "lint:web"]) {
      const cmd = resolveAcceptanceCommand(id);
      expect(cmd, `${id} should be registered`).not.toBeNull();
      expect(cmd!.cwd).toBe("desktop-app");
    }
  });

  // Registry consistency: the Node-side and Rust-side registries must have
  // exactly the same set of command IDs. If they drift, MCP will accept a
  // command_id that Runner rejects (or vice versa). This test parses the Rust
  // source to extract IDs — a brittle but effective guard.
  it("Node registry matches Rust registry (same command ID set)", async () => {
    const rustPath = path.resolve(__dirname, "../../desktop-app/src-tauri/src/runner.rs");
    const rustSource = await fs.readFile(rustPath, "utf-8");
    // Extract the ACCEPTANCE_REGISTRY block
    const match = rustSource.match(/ACCEPTANCE_REGISTRY[^=]*=\s*&\[(.*?)\];/s);
    expect(match, "Rust ACCEPTANCE_REGISTRY not found").not.toBeNull();
    const rustIds = new Set<string>();
    for (const line of match![1].split("\n")) {
      const idMatch = line.match(/"([a-z]+:[a-z]+)"/);
      if (idMatch) rustIds.add(idMatch[1]);
    }
    const nodeIds = new Set(listAcceptanceCommands().map((c) => c.id));
    expect(rustIds.size).toBeGreaterThan(0);
    expect(nodeIds).toEqual(rustIds);
  });
});

// ─── prepare / validate / seal / enqueue integration ───

describe("prepare → validate → seal → enqueue flow", () => {
  it("treats QuestLog metadata as control-plane state, not dirty source", async () => {
    const projectDir = path.join(dir, "test-project");
    expect(await isGitClean(projectDir)).toBe(true);
    await fs.writeFile(path.join(projectDir, "new-source.ts"), "export const x = 1;\n");
    expect(await isGitClean(projectDir)).toBe(false);
  });

  it("prepares a draft package with server-resolved base SHA", async () => {
    const projectDir = path.join(dir, "test-project");
    const result = await prepareTaskPackage(projectDir, {
      epic_id: "e1",
      step_id: "s1",
      objective: "add a test file",
      allowed_paths: ["test/**"],
      acceptance_command_ids: ["test:mcp"],
    });
    if ("error" in result) throw new Error(result.error);
    expect(result.status).toBe("draft");
    expect(result.resolved_base_sha).toMatch(/^[0-9a-f]{40}$/); // full SHA
    expect(result.resolved_repo_root).toBe(projectDir);
  });

  it("seals a valid package and makes it immutable", async () => {
    const projectDir = path.join(dir, "test-project");
    const prep = await prepareTaskPackage(projectDir, {
      epic_id: "e1",
      step_id: "s1",
      objective: "add a test file",
      allowed_paths: ["test/**"],
      acceptance_command_ids: ["test:mcp"],
    });
    if ("error" in prep) throw new Error(prep.error);

    // Fix any issues (test:mcp should be trusted)
    const sealed = await sealTaskPackage(projectDir, prep.package_id);
    if ("error" in sealed) throw new Error(sealed.error);
    expect(sealed.status).toBe("sealed");
    expect(sealed.package_hash).toHaveLength(64);

    // Verify package is stored
    const stored = await getPackage(sealed.package_id);
    if ("error" in stored) throw new Error(stored.error);
    expect(stored.pkg.status).toBe("sealed");
  });

  // Regression: seal used to compute the hash, then bump plan_revision (which
  // is part of the hashable content), leaving a stale stored hash. Runner's
  // hash check would then reject the package. This test ensures the stored
  // hash matches a fresh recompute over the stored content.
  it("stored hash matches a fresh recompute after seal (revision-bump race)", async () => {
    const projectDir = path.join(dir, "test-project");
    const prep = await prepareTaskPackage(projectDir, {
      epic_id: "e1",
      step_id: "s1",
      objective: "add a test file",
      allowed_paths: ["test/**"],
      acceptance_command_ids: ["test:mcp"],
    });
    if ("error" in prep) throw new Error(prep.error);

    const sealed = await sealTaskPackage(projectDir, prep.package_id);
    if ("error" in sealed) throw new Error(sealed.error);

    // Read the stored package and recompute hash from scratch
    const stored = await getPackage(sealed.package_id);
    if ("error" in stored) throw new Error(stored.error);
    const { status: _s, package_hash: _h, ...hashable } = stored.pkg;
    const freshHash = computePackageHash(
      hashable as Omit<TaskPackage, "package_hash" | "status">,
    );
    expect(stored.pkg.package_hash).toBe(freshHash);
  });

  it("rejects sealing when git working tree is dirty", async () => {
    const projectDir = path.join(dir, "test-project");
    // Make the tree dirty
    await fs.writeFile(path.join(projectDir, "uncommitted.txt"), "dirty");

    const prep = await prepareTaskPackage(projectDir, {
      epic_id: "e1",
      step_id: "s1",
      objective: "add a test file",
      allowed_paths: ["test/**"],
      acceptance_command_ids: ["test:mcp"],
    });
    if ("error" in prep) throw new Error(prep.error);

    const sealed = await sealTaskPackage(projectDir, prep.package_id);
    expect("error" in sealed).toBe(true);
    if ("error" in sealed) {
      expect(sealed.error).toContain("git");
    }
  });

  it("rejects enqueue for unsealed package", async () => {
    const projectDir = path.join(dir, "test-project");
    const prep = await prepareTaskPackage(projectDir, {
      epic_id: "e1",
      step_id: "s1",
      objective: "add a test file",
      allowed_paths: ["test/**"],
      acceptance_command_ids: ["test:mcp"],
    });
    if ("error" in prep) throw new Error(prep.error);
    // Don't seal — try to enqueue directly

    const result = await enqueueStep(projectDir, "s1");
    expect("error" in result).toBe(true);
  });

  it("rejects acceptance with untrusted command ID", async () => {
    const projectDir = path.join(dir, "test-project");
    const prep = await prepareTaskPackage(projectDir, {
      epic_id: "e1",
      step_id: "s1",
      objective: "evil task",
      allowed_paths: ["src/**"],
      acceptance_command_ids: ["rm -rf /"], // NOT in registry
    });
    if ("error" in prep) throw new Error(prep.error);

    const sealed = await sealTaskPackage(projectDir, prep.package_id);
    expect("error" in sealed).toBe(true);
    if ("error" in sealed) {
      expect(sealed.error).toContain("not in the trusted acceptance registry");
    }
  });

  it("rejects prepare when not a git repo", async () => {
    const nonGit = await fs.mkdtemp(path.join(os.tmpdir(), "questlog-nogit-"));
    try {
      const result = await prepareTaskPackage(nonGit, {
        epic_id: "e1",
        step_id: "s1",
        objective: "test",
        allowed_paths: ["src/**"],
        acceptance_command_ids: ["test:mcp"],
      });
      expect("error" in result).toBe(true);
    } finally {
      await fs.rm(nonGit, { recursive: true, force: true });
    }
  });

  it("keeps manual packages sealed and out of the automatic queue", async () => {
    const projectDir = path.join(dir, "test-project");
    const prep = await prepareTaskPackage(projectDir, {
      epic_id: "e1",
      step_id: "s1",
      objective: "track this task without automatic execution",
      allowed_paths: ["README.md"],
      executor_preferred: "manual",
      acceptance_command_ids: ["verify:candidate"],
    });
    if ("error" in prep) throw new Error(prep.error);
    const sealed = await sealTaskPackage(projectDir, prep.package_id);
    if ("error" in sealed) throw new Error(sealed.error);

    const enqueue = await enqueueStep(projectDir, "s1");
    expect("error" in enqueue).toBe(true);
    if ("error" in enqueue) expect(enqueue.error).toContain("manual policy");
    const stored = await getPackage(prep.package_id);
    if ("error" in stored) throw new Error(stored.error);
    expect(stored.pkg.status).toBe("sealed");
  });
});
