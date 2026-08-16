/**
 * publishPlan transaction tests (S1 acceptance):
 * - published: epic + steps + sealed packages in one call, server-resolved
 *   paths/SHA, no auto-execution authorization granted
 * - second-step failure: full rollback (packages cancelled, epic+steps deleted)
 * - needs_decision: all four gap categories, detected before any mutation
 * - invalid: schema/git/dependency/trusted-command errors, duplicate title,
 *   corrupt .epics.json / packages.json fail-closed
 * - concurrency & idempotency: parallel same/different plans, external
 *   board mutations interleaved without loss
 *
 * pushStepWithPackage is wrapped in a delegating vi.fn so failure injection is
 * surgical — everything else (rollback, stores, locks) runs the real code.
 */

import { describe, it, expect, beforeEach, afterEach, beforeAll, vi } from "vitest";
import { promises as fs } from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";
import {
  publishPlan,
  validatePlanSpec,
  detectDecisionGaps,
  globsMayOverlap,
  type PlanSpec,
} from "../src/plan-publisher.js";
import {
  pushStepWithPackage,
  type PushStepWithPackageInput,
  type PushStepWithPackageResult,
} from "../src/task-package-ops.js";

// ─── Failure-injection mock (delegates to the real implementation) ───

const mockState = vi.hoisted(() => ({
  actual: null as null | {
    pushStepWithPackage: (
      cwd: string,
      input: PushStepWithPackageInput,
    ) => Promise<PushStepWithPackageResult | { error: string }>;
  },
  /** 1-based call number that should return an injected failure (0 = never). */
  failOnCall: 0,
  /** Side effect to run right before the injected failure (e.g. corrupt a store). */
  beforeFail: null as null | (() => Promise<void>),
  calls: 0,
}));

vi.mock("../src/task-package-ops.js", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../src/task-package-ops.js")>();
  mockState.actual = actual;
  return {
    ...actual,
    pushStepWithPackage: vi.fn(actual.pushStepWithPackage),
  };
});

beforeAll(() => {
  vi.mocked(pushStepWithPackage).mockImplementation(
    async (cwd: string, input: PushStepWithPackageInput) => {
      mockState.calls += 1;
      if (mockState.calls === mockState.failOnCall) {
        if (mockState.beforeFail) await mockState.beforeFail();
        return { error: `injected failure on call ${mockState.calls}` };
      }
      return mockState.actual!.pushStepWithPackage(cwd, input);
    },
  );
});

// ─── Fixture ───

let dir: string;
let projectDir: string;
let runtimeDir: string;
let oldRuntime: string | undefined;
let titleCounter = 0;

beforeEach(async () => {
  dir = await fs.mkdtemp(path.join(os.tmpdir(), "questlog-publish-"));
  projectDir = path.join(dir, "test-project");
  runtimeDir = path.join(dir, "runtime");
  await fs.mkdir(projectDir);
  oldRuntime = process.env.QUESTLOG_RUNTIME;
  process.env.QUESTLOG_RUNTIME = runtimeDir;

  execFileSync("git", ["init"], { cwd: projectDir, stdio: "pipe" });
  execFileSync("git", ["config", "user.email", "test@test.com"], { cwd: projectDir, stdio: "pipe" });
  execFileSync("git", ["config", "user.name", "Test"], { cwd: projectDir, stdio: "pipe" });
  await fs.writeFile(path.join(projectDir, "README.md"), "# Test\n");
  execFileSync("git", ["add", "."], { cwd: projectDir, stdio: "pipe" });
  execFileSync("git", ["commit", "-m", "initial"], { cwd: projectDir, stdio: "pipe" });

  mockState.failOnCall = 0;
  mockState.beforeFail = null;
  mockState.calls = 0;
});

afterEach(async () => {
  if (oldRuntime === undefined) {
    delete process.env.QUESTLOG_RUNTIME;
  } else {
    process.env.QUESTLOG_RUNTIME = oldRuntime;
  }
  await fs.rm(dir, { recursive: true, force: true });
});

function gitHead(): string {
  return execFileSync("git", ["rev-parse", "HEAD"], { cwd: projectDir }).toString().trim();
}

/** A fully-resolved, gap-free spec: confirmed consumers, ordered disjoint
 * scopes, conservative authority, every major risk addressed. */
function baseSpec(): PlanSpec {
  titleCounter += 1;
  return {
    title: `Publish plan ${titleCounter}`,
    description: "test plan",
    priority: "P1",
    acceptance: "all steps observed",
    goal: "ship the feature",
    baseline: "feature does not exist",
    chain: {
      upstream: ["core module"],
      change: ["mcp-server/src/one.ts", "mcp-server/src/two.ts"],
      consumers: [],
      risks: [],
    },
    review: { major_risks: [], resolutions: [] },
    steps: [
      {
        title: "Implement one",
        objective: "single verifiable objective for one",
        allowed_paths: ["mcp-server/src/one.ts"],
        forbidden_paths: ["docs/**"],
        depends_on: [],
        acceptance_command_ids: ["lint:mcp"],
      },
      {
        title: "Implement two",
        objective: "single verifiable objective for two",
        allowed_paths: ["mcp-server/src/two.ts"],
        depends_on: [0],
        acceptance_command_ids: ["test:mcp"],
      },
    ],
  };
}

interface BoardFile {
  epics: Array<{ id: string; title: string; status: string; auto_execute?: boolean; priority: string }>;
  steps: Array<{ id: string; epic_id: string; order: number; depends_on?: string[]; package?: { package_id: string; status: string } }>;
}

async function readBoard(): Promise<BoardFile> {
  return JSON.parse(await fs.readFile(path.join(projectDir, ".epics.json"), "utf-8")) as BoardFile;
}

interface RuntimeFile {
  packages: Array<{
    package_id: string; epic_id: string; step_id: string; status: string;
    project_path: string; repo_root: string; base_sha: string; objective: string;
    authority: Record<string, boolean>;
    package_hash: string;
  }>;
}

async function readRuntime(): Promise<RuntimeFile> {
  return JSON.parse(await fs.readFile(path.join(runtimeDir, "packages.json"), "utf-8")) as RuntimeFile;
}

/**
 * The store keeps superseded draft revisions alongside the sealed revision
 * (savePackage upsert semantics) — claims about executability must count
 * only the live, non-superseded entries.
 */
function activePackages(runtime: RuntimeFile): RuntimeFile["packages"] {
  return runtime.packages.filter((p) => p.status !== "superseded");
}

// ─── Pure helpers ───

describe("globsMayOverlap", () => {
  it("detects identical and nested patterns", () => {
    expect(globsMayOverlap("mcp-server/src/**", "mcp-server/src/index.ts")).toBe(true);
    expect(globsMayOverlap("src/*.ts", "src/foo.ts")).toBe(true);
    expect(globsMayOverlap("src/**/*.test.ts", "src/*.ts")).toBe(true);
  });

  it("treats disjoint trees as disjoint", () => {
    expect(globsMayOverlap("mcp-server/src/**", "desktop-app/**")).toBe(false);
    expect(globsMayOverlap("src/a.ts", "src/b.ts")).toBe(false);
  });
});

describe("validatePlanSpec", () => {
  it("accepts the base spec", () => {
    expect(validatePlanSpec(baseSpec())).toEqual([]);
  });

  it("rejects empty steps, bad priority, forward/self dependencies", () => {
    const noSteps = baseSpec();
    (noSteps as { steps: unknown }).steps = [];
    expect(validatePlanSpec(noSteps).some((e) => e.field === "steps")).toBe(true);

    const badPriority = baseSpec();
    badPriority.priority = "P9" as PlanSpec["priority"];
    expect(validatePlanSpec(badPriority).some((e) => e.field === "priority")).toBe(true);

    const forward = baseSpec();
    forward.steps[0].depends_on = [1];
    expect(validatePlanSpec(forward).some((e) => e.field === "steps[0].depends_on")).toBe(true);

    const self = baseSpec();
    self.steps[1].depends_on = [1];
    expect(validatePlanSpec(self).some((e) => e.field === "steps[1].depends_on")).toBe(true);
  });

  it("rejects codex executor, untrusted commands, short timeouts, bad rollback", () => {
    const spec = baseSpec();
    (spec.steps[0] as { executor?: { preferred?: string } }).executor = { preferred: "codex" };
    expect(validatePlanSpec(spec).some((e) => e.field === "steps[0].executor.preferred")).toBe(true);

    const spec2 = baseSpec();
    spec2.steps[0].acceptance_command_ids = ["rm -rf /"];
    expect(validatePlanSpec(spec2).some((e) => e.field === "steps[0].acceptance_command_ids")).toBe(true);

    const spec3 = baseSpec();
    spec3.steps[0].executor = { timeout_seconds: 30 };
    expect(validatePlanSpec(spec3).some((e) => e.field === "steps[0].executor.timeout_seconds")).toBe(true);

    const spec4 = baseSpec();
    spec4.steps[0].rollback = { strategy: "delete_everything" as "abort_and_clean" };
    expect(validatePlanSpec(spec4).some((e) => e.field === "steps[0].rollback.strategy")).toBe(true);
  });
});

describe("detectDecisionGaps", () => {
  it("reports nothing for the base spec", () => {
    expect(detectDecisionGaps(baseSpec())).toEqual([]);
  });

  it("does not report ordered steps with overlapping scopes", () => {
    const spec = baseSpec();
    spec.steps[0].allowed_paths = ["mcp-server/src/**"];
    spec.steps[1].allowed_paths = ["mcp-server/src/**"];
    // steps[1] depends_on [0] → ordered → no conflict
    expect(detectDecisionGaps(spec)).toEqual([]);
  });
});

// ─── published ───

describe("publishPlan: published", () => {
  it("creates epic + steps + sealed packages with server-resolved git state", async () => {
    const spec = baseSpec();
    spec.steps.push({
      title: "Verify everything",
      objective: "run the full suite",
      allowed_paths: ["mcp-server/test/**"],
      depends_on: [0, 1],
      acceptance_command_ids: ["test:mcp", "build:mcp"],
      authority: { auto_edit_worktree: true, auto_commit_candidate: true },
      rollback: { strategy: "abort_and_clean" },
      executor: { preferred: "zcode", sandbox: "workspace-write", timeout_seconds: 600, max_attempts: 3 },
    });

    const result = await publishPlan(projectDir, spec);

    expect(result.status).toBe("published");
    if (result.status !== "published") return;
    expect(result.epic_id).toMatch(/^epic_/);
    expect(result.steps).toHaveLength(3);
    for (const s of result.steps) {
      expect(s.step_id).toMatch(/^step_/);
      expect(s.package_id).toMatch(/^pkg_/);
      expect(s.package_hash).toHaveLength(64);
    }

    // Board: one ACTIVE epic, authorization switch untouched, ordered steps
    const board = await readBoard();
    expect(board.epics).toHaveLength(1);
    const epic = board.epics[0];
    expect(epic.id).toBe(result.epic_id);
    expect(epic.status).toBe("active");
    expect(epic.auto_execute).toBeUndefined(); // human authorization preserved
    expect(epic.priority).toBe("P1");
    expect(board.steps).toHaveLength(3);
    expect(board.steps.map((s) => s.order)).toEqual([1, 2, 3]);

    // depends_on mapped from spec indices to real step ids
    expect(board.steps[1].depends_on).toEqual([result.steps[0].step_id]);
    expect(board.steps[2].depends_on).toEqual(
      expect.arrayContaining([result.steps[0].step_id, result.steps[1].step_id]),
    );

    // Every step carries its sealed package summary
    for (let i = 0; i < 3; i++) {
      expect(board.steps[i].package?.package_id).toBe(result.steps[i].package_id);
      expect(board.steps[i].package?.status).toBe("sealed");
    }

    // Runtime Store: sealed (NOT ready/enqueued), server-resolved values
    const runtime = await readRuntime();
    const active = activePackages(runtime);
    expect(active).toHaveLength(3);
    for (const pkg of active) {
      expect(pkg.status).toBe("sealed");
      expect(pkg.project_path).toBe(projectDir); // resolved by server, not the model
      expect(pkg.repo_root).toBe(projectDir);
      expect(pkg.base_sha).toBe(gitHead());
    }
    const third = active.find((p) => p.step_id === result.steps[2].step_id);
    expect(third?.objective).toBe("run the full suite");
    expect(third?.authority).toMatchObject({
      auto_edit_worktree: true,
      auto_commit_candidate: true,
      auto_merge: false, // conservative clamp holds
      auto_push: false,
      auto_deploy: false,
    });
  });

  it("keeps the epic in draft-limbo-free state: active but never auto-enqueued", async () => {
    const result = await publishPlan(projectDir, baseSpec());
    expect(result.status).toBe("published");
    const runtime = await readRuntime();
    // sealed only — publishPlan never enqueues
    expect(activePackages(runtime).every((p) => p.status === "sealed")).toBe(true);
  });
});

// ─── second-step failure → full rollback ───

describe("publishPlan: mid-flight failure rollback", () => {
  it("cancels sealed packages and deletes epic+steps when step 2 fails", async () => {
    const spec = baseSpec();
    spec.steps.push({
      title: "Never created",
      objective: "wont reach",
      allowed_paths: ["mcp-server/src/three.ts"],
      depends_on: [1],
      acceptance_command_ids: ["lint:mcp"],
    });
    mockState.failOnCall = 2; // step 1 sealed for real; step 2 injected failure

    const result = await publishPlan(projectDir, spec);

    expect(result.status).toBe("invalid");
    if (result.status !== "invalid") return;
    expect(result.errors[0].field).toBe("steps[1]");
    expect(result.errors[0].message).toContain("injected failure");
    expect(result.rollback.performed).toBe(true);
    expect(result.rollback.degraded).toBe(false);

    // Board fully cleaned
    const board = await readBoard();
    expect(board.epics).toHaveLength(0);
    expect(board.steps).toHaveLength(0);

    // Runtime Store: the sealed package from step 1 is cancelled — terminal,
    // never claimable
    const runtime = await readRuntime();
    const active = activePackages(runtime);
    expect(active).toHaveLength(1);
    expect(active[0].status).toBe("cancelled");
  });

  it("keeps the epic as an unclaimable tombstone when the store stays unreadable", async () => {
    const packagesPath = path.join(runtimeDir, "packages.json");
    mockState.failOnCall = 2;
    mockState.beforeFail = async () => {
      // Corrupt AND unreadable: permission error is not the self-healing
      // parse-error path, so the rollback read fails on both attempts.
      await fs.writeFile(packagesPath, "{ sealed junk");
      await fs.chmod(packagesPath, 0o000);
    };

    const result = await publishPlan(projectDir, baseSpec());

    expect(result.status).toBe("invalid");
    if (result.status !== "invalid") return;
    expect(result.rollback.performed).toBe(false);
    expect(result.rollback.degraded).toBe(true);
    expect(result.rollback.detail).toContain("tombstone");

    // Tombstone kept: epic exists, switches off → nothing claimable
    const board = await readBoard();
    expect(board.epics).toHaveLength(1);
    expect(board.epics[0].auto_execute).toBeUndefined();

    await fs.chmod(packagesPath, 0o644).catch(() => {});
  });

  it("recovers fully when the store corrupts but self-heals (parse error rotates to .bak)", async () => {
    mockState.failOnCall = 2;
    mockState.beforeFail = async () => {
      await fs.writeFile(path.join(runtimeDir, "packages.json"), "{ corrupt but parse-error");
      // readable → readPackagesSafe rotates it to .bak and errors; the retry
      // sees a clean (missing) store → rollback completes
    };

    const result = await publishPlan(projectDir, baseSpec());

    expect(result.status).toBe("invalid");
    if (result.status !== "invalid") return;
    expect(result.rollback.performed).toBe(true);
    const board = await readBoard();
    expect(board.epics).toHaveLength(0);
  });
});

// ─── needs_decision ───

describe("publishPlan: needs_decision", () => {
  const untouched = async () => {
    // No board file, no runtime store — gaps are detected before any mutation
    await expect(fs.access(path.join(projectDir, ".epics.json"))).rejects.toThrow();
    await expect(fs.access(path.join(runtimeDir, "packages.json"))).rejects.toThrow();
  };

  it("gap 1: unconfirmed consumers", async () => {
    const spec = baseSpec();
    spec.chain = { consumers: ["desktop UI", "runner poller"] };
    const result = await publishPlan(projectDir, spec);
    expect(result.status).toBe("needs_decision");
    if (result.status !== "needs_decision") return;
    expect(result.questions).toHaveLength(1);
    expect(result.questions[0].category).toBe("consumer_confirmation");
    expect(result.questions[0].affected).toEqual(["desktop UI", "runner poller"]);
    await untouched();
  });

  it("gap 1 escape: consumers_confirmed=true publishes", async () => {
    const spec = baseSpec();
    spec.chain = { consumers: ["desktop UI"], consumers_confirmed: true };
    const result = await publishPlan(projectDir, spec);
    expect(result.status).toBe("published");
  });

  it("gap 2: unordered steps with overlapping write scopes", async () => {
    const spec = baseSpec();
    spec.steps[0].allowed_paths = ["mcp-server/src/**"];
    spec.steps[1].allowed_paths = ["mcp-server/src/index.ts"];
    spec.steps[1].depends_on = []; // unordered now
    const result = await publishPlan(projectDir, spec);
    expect(result.status).toBe("needs_decision");
    if (result.status !== "needs_decision") return;
    expect(result.questions[0].category).toBe("step_scope_conflict");
    await untouched();
  });

  it("gap 2 fix: serializing with depends_on publishes", async () => {
    const spec = baseSpec();
    spec.steps[0].allowed_paths = ["mcp-server/src/**"];
    spec.steps[1].allowed_paths = ["mcp-server/src/index.ts"];
    spec.steps[1].depends_on = [0];
    const result = await publishPlan(projectDir, spec);
    expect(result.status).toBe("published");
  });

  it("gap 3: authority beyond the conservative envelope", async () => {
    const spec = baseSpec();
    spec.steps[0].authority = {
      auto_edit_worktree: true, auto_commit_candidate: true,
      auto_merge: true, auto_push: false, auto_deploy: false,
    };
    const result = await publishPlan(projectDir, spec);
    expect(result.status).toBe("needs_decision");
    if (result.status !== "needs_decision") return;
    expect(result.questions[0].category).toBe("authority_envelope");
    await untouched();
  });

  it("gap 4: major risks without resolutions", async () => {
    const spec = baseSpec();
    spec.review = { major_risks: ["data loss", "breaking change"], resolutions: ["backup first"] };
    const result = await publishPlan(projectDir, spec);
    expect(result.status).toBe("needs_decision");
    if (result.status !== "needs_decision") return;
    expect(result.questions[0].category).toBe("risk_unresolved");
    expect(result.questions[0].affected).toEqual(["breaking change"]);
    await untouched();
  });

  it("all four gaps at once → one minimal question list", async () => {
    const spec = baseSpec();
    spec.chain = { consumers: ["service X"] };
    spec.steps[0].allowed_paths = ["mcp-server/src/**"];
    spec.steps[1].allowed_paths = ["mcp-server/src/**"];
    spec.steps[1].depends_on = [];
    spec.steps[0].authority = {
      auto_edit_worktree: true, auto_commit_candidate: false,
      auto_merge: false, auto_push: true, auto_deploy: false,
    };
    spec.review = { major_risks: ["risk A"], resolutions: [] };
    const result = await publishPlan(projectDir, spec);
    expect(result.status).toBe("needs_decision");
    if (result.status !== "needs_decision") return;
    const categories = result.questions.map((q) => q.category).sort();
    expect(categories).toEqual([
      "authority_envelope",
      "consumer_confirmation",
      "risk_unresolved",
      "step_scope_conflict",
    ]);
    await untouched();
  });
});

// ─── invalid ───

describe("publishPlan: invalid", () => {
  it("rejects schema errors without touching anything", async () => {
    const spec = baseSpec();
    spec.steps[0].objective = "  ";
    const result = await publishPlan(projectDir, spec);
    expect(result.status).toBe("invalid");
    if (result.status !== "invalid") return;
    expect(result.errors.some((e) => e.field === "steps[0].objective")).toBe(true);
    expect(result.rollback.performed).toBe(false);
    expect(result.rollback.degraded).toBe(false);
    await expect(fs.access(path.join(projectDir, ".epics.json"))).rejects.toThrow();
  });

  it("rejects when cwd is not a git repository", async (ctx) => {
    const plainDir = path.join(dir, "not-a-repo");
    await fs.mkdir(plainDir);
    // Premise check: this sandbox's tmpdir lives INSIDE the worktree repo, so
    // every writable dir resolves to a git root. Skip there; CI and the
    // Runner use a system tmpdir with no repo ancestor, where this runs.
    const { findGitRoot } = await import("../src/task-package.js");
    if (await findGitRoot(plainDir)) return ctx.skip();
    const result = await publishPlan(plainDir, baseSpec());
    expect(result.status).toBe("invalid");
    if (result.status !== "invalid") return;
    expect(result.errors[0].message).toContain("not a git repository");
  });

  it("rejects a git repo without commits (no HEAD SHA)", async () => {
    const emptyRepo = path.join(dir, "empty-repo");
    await fs.mkdir(emptyRepo);
    execFileSync("git", ["init"], { cwd: emptyRepo, stdio: "pipe" });
    const result = await publishPlan(emptyRepo, baseSpec());
    expect(result.status).toBe("invalid");
    if (result.status !== "invalid") return;
    expect(result.errors[0].message).toContain("HEAD");
  });

  it("rejects duplicate epic title (idempotent re-publish)", async () => {
    const spec = baseSpec();
    const first = await publishPlan(projectDir, spec);
    expect(first.status).toBe("published");
    const second = await publishPlan(projectDir, spec);
    expect(second.status).toBe("invalid");
    if (second.status !== "invalid") return;
    expect(second.errors[0].message).toContain("already exists");
    // First publish untouched
    const board = await readBoard();
    expect(board.epics).toHaveLength(1);
    expect(board.steps).toHaveLength(2);
  });

  it("fails closed on a corrupt .epics.json", async () => {
    await fs.writeFile(path.join(projectDir, ".epics.json"), "{ definitely not json");
    const result = await publishPlan(projectDir, baseSpec());
    expect(result.status).toBe("invalid");
    if (result.status !== "invalid") return;
    expect(result.errors[0].message).toContain("unreadable");
    // Corrupt content was preserved as a backup, not overwritten
    const files = await fs.readdir(projectDir);
    expect(files.some((f) => f.startsWith(".epics.json.bak"))).toBe(true);
  });

  it("fails closed on a corrupt Runtime Store before creating anything", async () => {
    await fs.mkdir(runtimeDir, { recursive: true });
    await fs.writeFile(path.join(runtimeDir, "packages.json"), "{ corrupt");
    const result = await publishPlan(projectDir, baseSpec());
    expect(result.status).toBe("invalid");
    if (result.status !== "invalid") return;
    expect(result.errors[0].message).toContain("Runtime Store unreadable");
    // Nothing was created — no board file at all
    await expect(fs.access(path.join(projectDir, ".epics.json"))).rejects.toThrow();
  });
});

// ─── concurrency & idempotency ───

describe("publishPlan: concurrency", () => {
  it("same spec published concurrently → exactly one wins, no duplicates", async () => {
    const spec = baseSpec();
    const [a, b] = await Promise.all([
      publishPlan(projectDir, spec),
      publishPlan(projectDir, spec),
    ]);
    const statuses = [a.status, b.status].sort();
    expect(statuses).toEqual(["invalid", "published"]);
    const board = await readBoard();
    expect(board.epics).toHaveLength(1);
    expect(board.steps).toHaveLength(2);
    const runtime = await readRuntime();
    const active = activePackages(runtime);
    expect(active).toHaveLength(2);
    expect(active.every((p) => p.status === "sealed")).toBe(true);
  });

  it("different specs published concurrently → both win", async () => {
    const specA = baseSpec();
    const specB = baseSpec();
    const [a, b] = await Promise.all([
      publishPlan(projectDir, specA),
      publishPlan(projectDir, specB),
    ]);
    expect(a.status).toBe("published");
    expect(b.status).toBe("published");
    const board = await readBoard();
    expect(board.epics).toHaveLength(2);
    expect(board.steps).toHaveLength(4);
    const runtime = await readRuntime();
    expect(activePackages(runtime)).toHaveLength(4);
  });

  it("interleaved external board mutations are never lost", async () => {
    const { pushEpic } = await import("../src/epic-store.js");
    const spec = baseSpec();
    spec.steps.push({
      title: "Third step",
      objective: "third objective",
      allowed_paths: ["mcp-server/src/three.ts"],
      depends_on: [1],
      acceptance_command_ids: ["lint:mcp"],
    });

    const external = Promise.all(
      [1, 2, 3, 4, 5].map((n) =>
        pushEpic(projectDir, { title: `External epic ${n}`, description: "concurrent" }),
      ),
    );
    const published = await publishPlan(projectDir, spec);
    const externalResults = await external;

    expect(published.status).toBe("published");
    expect(externalResults.every((r) => "epic" in r)).toBe(true);

    // Every mutation survived: 1 published + 5 external epics, 3 steps
    const board = await readBoard();
    expect(board.epics).toHaveLength(6);
    expect(board.steps).toHaveLength(3);
    expect(board.epics.find((e) => e.id === (published as { epic_id: string }).epic_id)).toBeTruthy();
  });
});
