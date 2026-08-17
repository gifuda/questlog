/**
 * S7 worktree-sweep classifier tests — pure function over targets,
 * executions, ancestry predicate and time. No filesystem, no git.
 */

import { describe, it, expect } from "vitest";
import { classifyTargets, type SweepTarget, type SweepExecutionRow } from "../src/worktree-sweep.js";

const NOW = 1_800_000_000;
const TTL = 7 * 86400;
const OLD = NOW - TTL - 3600; // comfortably past the TTL
const FRESH = NOW - 3600;

function target(overrides?: Partial<SweepTarget>): SweepTarget {
  return {
    branch: "questlog/exec_abc123",
    dir: "/tmp/questlog-worktrees/questlog/exec_abc123",
    dirMtime: OLD,
    tipDate: OLD,
    ...overrides,
  };
}

function execution(overrides?: Partial<SweepExecutionRow>): SweepExecutionRow {
  return {
    id: "exec_abc123",
    package_id: "pkg1",
    status: "passed",
    worktree_path: "/tmp/questlog-worktrees/questlog/exec_abc123",
    branch: "questlog/exec_abc123",
    candidate_sha: null,
    updated_at: OLD,
    ...overrides,
  };
}

describe("classifyTargets (S7)", () => {
  const everythingAncestor = () => true;
  const nothingAncestor = () => false;

  it("unreferenced, terminal, past TTL on BOTH signals → reclaimable", () => {
    const out = classifyTargets({
      targets: [target()],
      executions: [],
      isAncestorOfHead: everythingAncestor,
      now: NOW,
      ttlSeconds: TTL,
    });
    expect(out[0].verdict).toBe("reclaimable");
  });

  it("in-flight reference → keep_active (even when past TTL)", () => {
    const out = classifyTargets({
      targets: [target()],
      executions: [execution({ status: "running", id: "exec_live" })],
      isAncestorOfHead: everythingAncestor,
      now: NOW,
      ttlSeconds: TTL,
    });
    expect(out[0].verdict).toBe("keep_active");
    expect(out[0].referencedBy).toContain("exec_live");
  });

  it("scene-incomplete reference → keep_evidence (the worktree IS the evidence)", () => {
    const out = classifyTargets({
      targets: [target()],
      executions: [execution({ status: "blocked", scene_preserved: false })],
      isAncestorOfHead: everythingAncestor,
      now: NOW,
      ttlSeconds: TTL,
    });
    expect(out[0].verdict).toBe("keep_evidence");
  });

  it("unmerged candidate lineage → keep_unmerged_candidate (BLOCK-1 guard)", () => {
    const out = classifyTargets({
      targets: [target()],
      executions: [execution({ candidate_sha: "abc123" })],
      isAncestorOfHead: nothingAncestor, // NOT an ancestor of HEAD
      now: NOW,
      ttlSeconds: TTL,
    });
    expect(out[0].verdict).toBe("keep_unmerged_candidate");
    expect(out[0].unmergedCandidates).toContain("abc123");
    // …and a MERGED candidate frees the target for reclamation.
    const merged = classifyTargets({
      targets: [target()],
      executions: [execution({ candidate_sha: "abc123" })],
      isAncestorOfHead: everythingAncestor,
      now: NOW,
      ttlSeconds: TTL,
    });
    expect(merged[0].verdict).toBe("reclaimable");
  });

  it("TTL combinator: BOTH mtime and tip date must be old (W1)", () => {
    // Old tip, recent mtime → still fresh.
    const a = classifyTargets({
      targets: [target({ dirMtime: FRESH, tipDate: OLD })],
      executions: [],
      isAncestorOfHead: everythingAncestor,
      now: NOW,
      ttlSeconds: TTL,
    });
    expect(a[0].verdict).toBe("keep_fresh");
    // Recent tip, old mtime → still fresh.
    const b = classifyTargets({
      targets: [target({ dirMtime: OLD, tipDate: FRESH })],
      executions: [],
      isAncestorOfHead: everythingAncestor,
      now: NOW,
      ttlSeconds: TTL,
    });
    expect(b[0].verdict).toBe("keep_fresh");
  });

  it("no activity signal at all → unknown (fail-open)", () => {
    const out = classifyTargets({
      targets: [target({ dirMtime: null, tipDate: null, dir: null })],
      executions: [],
      isAncestorOfHead: everythingAncestor,
      now: NOW,
      ttlSeconds: TTL,
    });
    expect(out[0].verdict).toBe("unknown");
  });

  it("non-execution branch names → unknown (never touch other worktrees)", () => {
    const out = classifyTargets({
      targets: [
        target({ branch: "questlog/controlled-execution", dir: "~/dev/questlog-controlled-execution" }),
      ],
      executions: [],
      isAncestorOfHead: everythingAncestor,
      now: NOW,
      ttlSeconds: TTL,
    });
    expect(out[0].verdict).toBe("unknown");
  });
});
