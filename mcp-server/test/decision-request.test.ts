/**
 * DecisionRequest lifecycle tests.
 */

import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { promises as fs } from "node:fs";
import os from "node:os";
import path from "node:path";
import {
  createDecisionRequest,
  listDecisionRequests,
  resolveDecision,
} from "../src/decision-request.js";

let dir: string;
let oldRuntime: string | undefined;

beforeEach(async () => {
  dir = await fs.mkdtemp(path.join(os.tmpdir(), "questlog-dec-"));
  oldRuntime = process.env.QUESTLOG_RUNTIME;
  process.env.QUESTLOG_RUNTIME = path.join(dir, "runtime");
});

afterEach(async () => {
  if (oldRuntime === undefined) delete process.env.QUESTLOG_RUNTIME;
  else process.env.QUESTLOG_RUNTIME = oldRuntime;
  await fs.rm(dir, { recursive: true, force: true });
});

describe("DecisionRequest lifecycle", () => {
  it("creates and lists a request", async () => {
    const req = await createDecisionRequest({
      epic_id: "e1",
      step_id: "s1",
      execution_id: "exec1",
      kind: "approval",
      question: "Allow worktree write to src/?",
      options: ["approve", "deny"],
      evidence_refs: ["log:exec1.log"],
    });

    expect(req.request_id).toMatch(/^dec_/);
    expect(req.resolved).toBe(false);
    expect(req.blocking).toBe(true);

    const list = await listDecisionRequests();
    expect(list.length).toBe(1);
    expect(list[0].request_id).toBe(req.request_id);
  });

  it("resolves a request and hides it from default list", async () => {
    const req = await createDecisionRequest({
      epic_id: "e1",
      step_id: "s1",
      execution_id: "exec1",
      kind: "blocked",
      question: "Scope expanded — continue?",
      options: ["yes", "no"],
    });

    const result = await resolveDecision(req.request_id, "yes", "user1");
    expect("ok" in result).toBe(true);

    // Default list excludes resolved
    const unresolved = await listDecisionRequests();
    expect(unresolved.length).toBe(0);

    // Include resolved
    const all = await listDecisionRequests({ include_resolved: true });
    expect(all.length).toBe(1);
    expect(all[0].resolved).toBe(true);
    expect(all[0].resolution).toBe("yes");
    expect(all[0].resolved_by).toBe("user1");
  });

  it("rolls the resolution back when the mapped action fails (S0-B fail-closed)", async () => {
    // DR references a nonexistent execution → the cancel action genuinely
    // fails → the decision must NOT stand resolved.
    const req = await createDecisionRequest({
      epic_id: "e1",
      step_id: "s1",
      execution_id: "exec_ghost",
      kind: "blocked",
      question: "cancel?",
      options: ["cancel"],
    });

    const result = await resolveDecision(req.request_id, "cancel");
    expect("error" in result).toBe(true);
    if ("error" in result) {
      expect(result.error).toContain("rolled back");
    }

    const stored = await listDecisionRequests({ include_resolved: true });
    const rolled = stored.find((r) => r.request_id === req.request_id);
    expect(rolled).toBeTruthy();
    expect(rolled!.resolved).toBe(false);
    expect(rolled!.resolution).toBeUndefined();
    expect(rolled!.last_failed_resolution).toBeTruthy();
    expect(rolled!.last_failed_resolution).toContain("cancel");

    // After the rollback the request is answerable again.
    const again = await resolveDecision(req.request_id, "cancel");
    expect("error" in again).toBe(true);
  });

  it("prevents double resolution", async () => {
    const req = await createDecisionRequest({
      epic_id: "e1",
      step_id: "s1",
      execution_id: "exec1",
      kind: "git_drift",
      question: "Base SHA drifted — re-seal?",
    });

    await resolveDecision(req.request_id, "re-seal");
    const second = await resolveDecision(req.request_id, "different answer");
    expect("error" in second).toBe(true);
  });

  it("filters by epic_id and step_id", async () => {
    await createDecisionRequest({
      epic_id: "e1", step_id: "s1", execution_id: "x",
      kind: "blocked", question: "Q1",
    });
    await createDecisionRequest({
      epic_id: "e1", step_id: "s2", execution_id: "x",
      kind: "blocked", question: "Q2",
    });
    await createDecisionRequest({
      epic_id: "e2", step_id: "s3", execution_id: "x",
      kind: "blocked", question: "Q3",
    });

    const e1Reqs = await listDecisionRequests({ epic_id: "e1" });
    expect(e1Reqs.length).toBe(2);

    const s2Reqs = await listDecisionRequests({ step_id: "s2" });
    expect(s2Reqs.length).toBe(1);
    expect(s2Reqs[0].step_id).toBe("s2");
  });

  it("supports all decision kinds", async () => {
    const kinds = ["approval", "blocked", "retry_exhausted", "git_drift", "verifier_failed", "adjudication"];
    for (const kind of kinds) {
      const req = await createDecisionRequest({
        epic_id: "e1",
        step_id: "s1",
        execution_id: "x",
        kind: kind as "approval",
        question: `Q-${kind}`,
      });
      expect(req.kind).toBe(kind);
    }
    const all = await listDecisionRequests();
    expect(all.length).toBe(kinds.length);
  });
});
