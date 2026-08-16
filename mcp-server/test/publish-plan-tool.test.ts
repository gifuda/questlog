/**
 * publish_plan MCP-layer contract tests (S2 acceptance):
 * - one call, multiple steps → published (epic_id / step_ids / package_ids,
 *   next step = manual switch authorization; nothing enqueued)
 * - missing chain / review / authority info → needs_decision with minimal
 *   questions answerable back in the source conversation; nothing created
 * - unknown acceptance command → invalid with deterministic fix items
 * - second-step failure → invalid with full rollback, zero residue
 * - duplicate request → explicit rejection (original publish untouched)
 * - sealed packages still gated by master + project + epic switches, and
 *   authority stays auto_merge/auto_push/auto_deploy=false
 *
 * The tool layer is driven over the real MCP protocol (Client ↔
 * InMemoryTransport ↔ McpServer). pushStepWithPackage is wrapped in a
 * delegating vi.fn for failure injection — everything else runs real.
 */

import { describe, it, expect, beforeEach, afterEach, beforeAll, afterAll, vi } from "vitest";
import { promises as fs } from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import { registerPublishPlanTool } from "../src/publish-plan-tool.js";
import {
  pushStepWithPackage,
  verifyPickupReadiness,
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
  failOnCall: 0,
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
        return { error: `injected failure on call ${mockState.calls}` };
      }
      return mockState.actual!.pushStepWithPackage(cwd, input);
    },
  );
});

// ─── MCP protocol plumbing ───

let client: Client;

beforeAll(async () => {
  const server = new McpServer({ name: "todo-board-test", version: "0.0.0" });
  registerPublishPlanTool(server);
  client = new Client({ name: "contract-test", version: "0.0.0" });
  const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();
  await Promise.all([server.connect(serverTransport), client.connect(clientTransport)]);
});

afterAll(async () => {
  await client.close();
});

// ─── Fixture ───

let dir: string;
let projectDir: string;
let runtimeDir: string;
let oldRuntime: string | undefined;
let oldRegistry: string | undefined;
let titleCounter = 0;

beforeEach(async () => {
  dir = await fs.mkdtemp(path.join(os.tmpdir(), "questlog-pubtool-"));
  projectDir = path.join(dir, "test-project");
  runtimeDir = path.join(dir, "runtime");
  await fs.mkdir(projectDir);

  oldRuntime = process.env.QUESTLOG_RUNTIME;
  oldRegistry = process.env.QUESTLOG_REGISTRY;
  process.env.QUESTLOG_RUNTIME = runtimeDir;
  // Register the temp project so the explicit project_dir passes
  // canonicalizeProjectDir (allowed set = cwd descendants ∪ registered).
  process.env.QUESTLOG_REGISTRY = path.join(dir, ".todo-board.json");
  await fs.writeFile(
    process.env.QUESTLOG_REGISTRY,
    JSON.stringify({ projects: [{ name: "test-project", path: projectDir }], feedback: {} }),
  );

  execFileSync("git", ["init"], { cwd: projectDir, stdio: "pipe" });
  execFileSync("git", ["config", "user.email", "test@test.com"], { cwd: projectDir, stdio: "pipe" });
  execFileSync("git", ["config", "user.name", "Test"], { cwd: projectDir, stdio: "pipe" });
  await fs.writeFile(path.join(projectDir, "README.md"), "# Test\n");
  execFileSync("git", ["add", "."], { cwd: projectDir, stdio: "pipe" });
  execFileSync("git", ["commit", "-m", "initial"], { cwd: projectDir, stdio: "pipe" });

  mockState.failOnCall = 0;
  mockState.calls = 0;
});

afterEach(async () => {
  if (oldRuntime === undefined) delete process.env.QUESTLOG_RUNTIME;
  else process.env.QUESTLOG_RUNTIME = oldRuntime;
  if (oldRegistry === undefined) delete process.env.QUESTLOG_REGISTRY;
  else process.env.QUESTLOG_REGISTRY = oldRegistry;
  await fs.rm(dir, { recursive: true, force: true });
});

/** Call publish_plan over MCP and return its structured three-state result. */
async function callPublish(spec: Record<string, unknown>): Promise<Record<string, unknown>> {
  const res = await client.callTool({
    name: "publish_plan",
    arguments: { ...spec, project_dir: projectDir },
  });
  const structured = (res as { structuredContent?: Record<string, unknown> }).structuredContent;
  if (!structured) {
    throw new Error(`publish_plan returned no structuredContent: ${JSON.stringify(res.content)}`);
  }
  return structured;
}

/** A gap-free spec: confirmed consumers, ordered disjoint scopes, conservative authority, all risks resolved. */
function baseSpec(): Record<string, unknown> {
  titleCounter += 1;
  return {
    title: `MCP publish plan ${titleCounter}`,
    description: "tool-layer contract test plan",
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
  epics: Array<{ id: string; title: string; status: string; auto_execute?: boolean }>;
  steps: Array<{ id: string; epic_id: string; order: number; depends_on?: string[]; package?: { package_id: string; status: string } }>;
}

async function readBoard(): Promise<BoardFile> {
  return JSON.parse(await fs.readFile(path.join(projectDir, ".epics.json"), "utf-8")) as BoardFile;
}

interface RuntimeFile {
  packages: Array<{
    package_id: string; epic_id: string; step_id: string; status: string;
    project_path: string; base_sha: string; objective: string;
    authority: Record<string, boolean>;
  }>;
}

async function readRuntime(): Promise<RuntimeFile> {
  return JSON.parse(await fs.readFile(path.join(runtimeDir, "packages.json"), "utf-8")) as RuntimeFile;
}

/** Only live (non-superseded) entries are meaningful for executability. */
function activePackages(runtime: RuntimeFile): RuntimeFile["packages"] {
  return runtime.packages.filter((p) => p.status !== "superseded");
}

async function writeRunnerConfig(cfg: {
  global_enabled: boolean;
  projectEnabled: boolean;
}): Promise<void> {
  await fs.writeFile(
    path.join(runtimeDir, "runner-config.json"),
    JSON.stringify({
      version: 1,
      global_enabled: cfg.global_enabled,
      // Key by the realpath form — the same form canonicalizeProjectDir
      // resolved for the packages' project_path (/var → /private/var on macOS).
      projects: { [await fs.realpath(projectDir)]: { enabled: cfg.projectEnabled } },
      revision: 1,
    }),
  );
}

async function flipEpicAutoExecute(epicId: string, value: boolean): Promise<void> {
  const epicsPath = path.join(projectDir, ".epics.json");
  const raw = JSON.parse(await fs.readFile(epicsPath, "utf-8")) as BoardFile;
  const epic = raw.epics.find((e) => e.id === epicId);
  if (!epic) throw new Error(`epic ${epicId} not found`);
  epic.auto_execute = value;
  await fs.writeFile(epicsPath, JSON.stringify(raw));
}

// ─── Registration ───

describe("publish_plan registration", () => {
  it("is listed with the mandated contract copy", async () => {
    const listed = await client.listTools();
    const tool = listed.tools.find((t) => t.name === "publish_plan");
    expect(tool).toBeDefined();
    const desc = tool!.description ?? "";
    // Pre-call checklist, auto-register-and-seal, Runner-never-executes-unpublished
    expect(desc).toContain("反例审查");
    expect(desc).toContain("自动登记");
    expect(desc).toContain("永不由 Runner 执行");
    expect(desc).toContain("published");
    expect(desc).toContain("needs_decision");
    expect(desc).toContain("invalid");
  });
});

// ─── published ───

describe("publish_plan: published", () => {
  it("one call creates epic + steps + sealed packages; next step is manual switch authorization", async () => {
    const spec = baseSpec();
    (spec.steps as Array<Record<string, unknown>>).push({
      title: "Verify everything",
      objective: "run the full suite",
      allowed_paths: ["mcp-server/test/**"],
      depends_on: [0, 1],
      acceptance_command_ids: ["test:mcp", "build:mcp"],
      authority: { auto_edit_worktree: true, auto_commit_candidate: true },
      rollback: { strategy: "abort_and_clean" },
      executor: { preferred: "zcode", sandbox: "workspace-write", timeout_seconds: 600, max_attempts: 3 },
    });

    const result = await callPublish(spec);

    expect(result.status).toBe("published");
    expect(String(result.epic_id)).toMatch(/^epic_/);
    const stepIds = result.step_ids as string[];
    const packageIds = result.package_ids as string[];
    expect(stepIds).toHaveLength(3);
    expect(packageIds).toHaveLength(3);
    stepIds.forEach((id) => expect(id).toMatch(/^step_/));
    packageIds.forEach((id) => expect(id).toMatch(/^pkg_/));
    // The contract names manual switch authorization as the next step
    expect(String(result.next_step)).toContain("global_enabled");
    expect(String(result.next_step)).toContain("auto_execute");
    // No residue of the old flow: published copy must not demand enqueue_step
    expect(String(result.next_step)).toContain("no enqueue_step");

    // Board: one active epic, authorization switch untouched, ordered steps
    const board = await readBoard();
    expect(board.epics).toHaveLength(1);
    expect(board.epics[0].id).toBe(result.epic_id);
    expect(board.epics[0].status).toBe("active");
    expect(board.epics[0].auto_execute).toBeUndefined(); // human authorization preserved
    expect(board.steps).toHaveLength(3);
    expect(board.steps[1].depends_on).toEqual([stepIds[0]]);
    expect(board.steps[2].depends_on).toEqual(expect.arrayContaining([stepIds[0], stepIds[1]]));
    for (let i = 0; i < 3; i++) {
      expect(board.steps[i].package?.package_id).toBe(packageIds[i]);
      expect(board.steps[i].package?.status).toBe("sealed");
    }

    // Runtime: sealed only (never enqueued), authority clamped conservative
    const runtime = await readRuntime();
    const active = activePackages(runtime);
    expect(active).toHaveLength(3);
    for (const pkg of active) {
      expect(pkg.status).toBe("sealed");
      expect(pkg.base_sha).toBe(execFileSync("git", ["rev-parse", "HEAD"], { cwd: projectDir }).toString().trim());
      expect(pkg.authority.auto_merge).toBe(false);
      expect(pkg.authority.auto_push).toBe(false);
      expect(pkg.authority.auto_deploy).toBe(false);
    }
  });
});

// ─── needs_decision: missing chain / review / authority info ───

describe("publish_plan: needs_decision", () => {
  const nothingCreated = async () => {
    await expect(fs.access(path.join(projectDir, ".epics.json"))).rejects.toThrow();
    await expect(fs.access(path.join(runtimeDir, "packages.json"))).rejects.toThrow();
  };

  it("missing chain confirmation: consumers listed but not confirmed", async () => {
    const spec = baseSpec();
    spec.chain = { consumers: ["desktop UI", "runner poller"] };
    const result = await callPublish(spec);
    expect(result.status).toBe("needs_decision");
    const questions = result.questions as Array<{ category: string; question: string; affected: string[] }>;
    expect(questions).toHaveLength(1);
    expect(questions[0].category).toBe("consumer_confirmation");
    // Minimal question answerable directly in the source conversation: names
    // the consumers and the exact field to set after confirming
    expect(questions[0].affected).toEqual(["desktop UI", "runner poller"]);
    expect(questions[0].question).toContain("desktop UI");
    expect(questions[0].question).toContain("consumers_confirmed");
    await nothingCreated();
  });

  it("missing review: major risks without resolutions", async () => {
    const spec = baseSpec();
    spec.review = { major_risks: ["data loss", "breaking change"], resolutions: ["backup first"] };
    const result = await callPublish(spec);
    expect(result.status).toBe("needs_decision");
    const questions = result.questions as Array<{ category: string; question: string; affected: string[] }>;
    expect(questions[0].category).toBe("risk_unresolved");
    expect(questions[0].affected).toEqual(["breaking change"]);
    await nothingCreated();
  });

  it("missing authority envelope: requesting auto_merge", async () => {
    const spec = baseSpec();
    (spec.steps as Array<Record<string, unknown>>)[0].authority = {
      auto_edit_worktree: true, auto_commit_candidate: true,
      auto_merge: true, auto_push: false, auto_deploy: false,
    };
    const result = await callPublish(spec);
    expect(result.status).toBe("needs_decision");
    const questions = result.questions as Array<{ category: string; question: string }>;
    expect(questions[0].category).toBe("authority_envelope");
    expect(questions[0].question).toContain("auto_merge");
    await nothingCreated();
  });
});

// ─── invalid ───

describe("publish_plan: invalid", () => {
  it("unknown acceptance command → deterministic fix item, nothing created", async () => {
    const spec = baseSpec();
    (spec.steps as Array<Record<string, unknown>>)[0].acceptance_command_ids = ["definitely:not:registered"];
    const result = await callPublish(spec);
    expect(result.status).toBe("invalid");
    const errors = result.errors as Array<{ field: string; message: string }>;
    expect(errors.some((e) => e.field === "steps[0].acceptance_command_ids")).toBe(true);
    expect(errors.some((e) => e.message.includes("trusted acceptance registry"))).toBe(true);
    const rollback = result.rollback as { performed: boolean; degraded: boolean };
    expect(rollback.performed).toBe(false);
    expect(rollback.degraded).toBe(false);
    await expect(fs.access(path.join(projectDir, ".epics.json"))).rejects.toThrow();
  });

  it("duplicate request → explicit rejection, original publish untouched", async () => {
    const spec = baseSpec();
    const first = await callPublish(spec);
    expect(first.status).toBe("published");
    const second = await callPublish(spec);
    expect(second.status).toBe("invalid");
    const errors = second.errors as Array<{ message: string }>;
    expect(errors.some((e) => e.message.includes("already exists"))).toBe(true);

    const board = await readBoard();
    expect(board.epics).toHaveLength(1);
    expect(board.steps).toHaveLength(2);
    const runtime = await readRuntime();
    expect(activePackages(runtime).every((p) => p.status === "sealed")).toBe(true);
  });
});

// ─── mid-flight failure → zero residue ───

describe("publish_plan: second-step failure", () => {
  it("rolls back fully — packages cancelled, epic deleted, nothing claimable", async () => {
    mockState.failOnCall = 2; // step 1 seals for real; step 2 injected failure

    const result = await callPublish(baseSpec());

    expect(result.status).toBe("invalid");
    const errors = result.errors as Array<{ field: string; message: string }>;
    expect(errors[0].field).toBe("steps[1]");
    expect(errors[0].message).toContain("injected failure");
    const rollback = result.rollback as { performed: boolean; degraded: boolean };
    expect(rollback.performed).toBe(true);
    expect(rollback.degraded).toBe(false);

    const board = await readBoard();
    expect(board.epics).toHaveLength(0);
    expect(board.steps).toHaveLength(0);

    const runtime = await readRuntime();
    const active = activePackages(runtime);
    expect(active).toHaveLength(1);
    expect(active[0].status).toBe("cancelled"); // terminal, never claimable
  });
});

// ─── three-switch gate on sealed packages ───

describe("publish_plan: sealed packages stay gated", () => {
  it("need master + project + epic switches; authority stays conservative", async () => {
    const result = await callPublish(baseSpec());
    expect(result.status).toBe("published");
    const packageIds = result.package_ids as string[];

    // (a) fresh publish: every switch off → not claimable, all three blockers named
    for (const pkgId of packageIds) {
      const r = await verifyPickupReadiness(pkgId);
      if ("error" in r) throw new Error(r.error);
      expect(r.will_be_picked_up).toBe(false);
      expect(r.blockers.map((b) => b.check)).toEqual(
        expect.arrayContaining(["global_enabled", "project_enabled", "epic_auto_execute_allows"]),
      );
    }

    const epicId = result.epic_id as string;

    // (b) epic + project switches on but master off → still blocked (总闸 required)
    await writeRunnerConfig({ global_enabled: false, projectEnabled: true });
    await flipEpicAutoExecute(epicId, true);
    for (const pkgId of packageIds) {
      const r = await verifyPickupReadiness(pkgId);
      if ("error" in r) throw new Error(r.error);
      expect(r.will_be_picked_up).toBe(false);
      expect(r.blockers.some((b) => b.check === "global_enabled")).toBe(true);
    }

    // (c) all three switches on → claimable (proves the chain gates, and that
    // publishing alone never granted any part of it)
    await writeRunnerConfig({ global_enabled: true, projectEnabled: true });
    for (const pkgId of packageIds) {
      const r = await verifyPickupReadiness(pkgId);
      if ("error" in r) throw new Error(r.error);
      expect(r.will_be_picked_up).toBe(true);
      expect(r.blockers).toHaveLength(0);
    }

    // Authority clamp holds regardless of what the flow did
    const runtime = await readRuntime();
    for (const pkg of activePackages(runtime)) {
      expect(pkg.authority.auto_merge).toBe(false);
      expect(pkg.authority.auto_push).toBe(false);
      expect(pkg.authority.auto_deploy).toBe(false);
    }
  });
});
