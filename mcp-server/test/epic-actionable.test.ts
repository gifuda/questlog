import { describe, it, expect, beforeEach } from "vitest";
import { actionableSteps } from "../src/epic-store.js";
import type { Step } from "../src/epic-store.js";
import { makeStep, resetIds, type StepSeed } from "./fixtures.js";

// actionableSteps(epicId, steps):
//   - within the epic, steps sorted by `order`
//   - a step is "actionable" if it's NOT observed AND every prerequisite
//     (all steps with smaller order ∪ explicit depends_on) IS observed.
//   - multiple actionable steps ⇒ parallelizable.

function stepsFrom(seeds: StepSeed[]): Step[] {
  return seeds.map(makeStep);
}

describe("actionableSteps", () => {
  beforeEach(resetIds);

  it("returns the first step when nothing is observed yet", () => {
    const steps = stepsFrom([
      { id: "S1", epicId: "E1", order: 0 },
      { id: "S2", epicId: "E1", order: 1 },
    ]);
    const result = actionableSteps("E1", steps);
    expect(result.map((s) => s.id)).toEqual(["S1"]);
  });

  it("unblocks the next step after the prior one is observed", () => {
    const steps = stepsFrom([
      { id: "S1", epicId: "E1", order: 0, stage: "observed" },
      { id: "S2", epicId: "E1", order: 1, stage: "pending" },
    ]);
    const result = actionableSteps("E1", steps);
    expect(result.map((s) => s.id)).toEqual(["S2"]);
  });

  it("keeps S2 blocked while S1 is only coded (not observed)", () => {
    const steps = stepsFrom([
      { id: "S1", epicId: "E1", order: 0, stage: "coded" },
      { id: "S2", epicId: "E1", order: 1, stage: "pending" },
    ]);
    const result = actionableSteps("E1", steps);
    // S1 is not observed yet, so it's the only actionable; S2 stays blocked.
    expect(result.map((s) => s.id)).toEqual(["S1"]);
  });

  it("blocks a step whose explicit depends_on is not yet observed, even when its order-prereqs are met", () => {
    // S1 (order 0, observed) → unblocks S2 by order.
    // S2 (order 1, pending) but explicitly depends on S3 (order 2, not observed).
    // S2 should NOT be actionable because its explicit dep S3 isn't observed,
    // even though S1 (its order-prereq) is.
    const steps = stepsFrom([
      { id: "S1", epicId: "E1", order: 0, stage: "observed" },
      { id: "S2", epicId: "E1", order: 1, stage: "pending", dependsOn: ["S3"] },
      { id: "S3", epicId: "E1", order: 2, stage: "pending" },
    ]);
    const result = actionableSteps("E1", steps);
    // S2 is blocked by its unmet explicit dep S3.
    // S3 has order-prereqs S1 (observed) + S2 (not observed) → blocked.
    // So nothing is actionable.
    expect(result.map((s) => s.id)).toEqual([]);
  });

  it("exposes parallelizable steps when two steps have no ordering between them", () => {
    // Two steps both at order 0, neither observed → both actionable.
    const steps = stepsFrom([
      { id: "S1", epicId: "E1", order: 0 },
      { id: "S2", epicId: "E1", order: 0 },
    ]);
    const result = actionableSteps("E1", steps);
    expect(result.map((s) => s.id).sort()).toEqual(["S1", "S2"]);
  });

  it("returns empty when all steps are observed", () => {
    const steps = stepsFrom([
      { id: "S1", epicId: "E1", order: 0, stage: "observed" },
      { id: "S2", epicId: "E1", order: 1, stage: "observed" },
    ]);
    const result = actionableSteps("E1", steps);
    expect(result).toEqual([]);
  });

  it("ignores steps belonging to other epics", () => {
    const steps = stepsFrom([
      { id: "S1", epicId: "E1", order: 0 },
      { id: "S2", epicId: "E2", order: 0 }, // different epic
    ]);
    const result = actionableSteps("E1", steps);
    expect(result.map((s) => s.id)).toEqual(["S1"]);
  });

  it("handles a step whose explicit dep is observed but order-based prereq is not", () => {
    // S3 depends on [S2] explicitly. order: S1=0, S2=1, S3=2.
    // If S1 observed, S2 observed, S3 pending → S3 actionable because both
    // its order-prereqs (S1, S2) and explicit dep (S2) are observed.
    const steps = stepsFrom([
      { id: "S1", epicId: "E1", order: 0, stage: "observed" },
      { id: "S2", epicId: "E1", order: 1, stage: "observed" },
      { id: "S3", epicId: "E1", order: 2, dependsOn: ["S2"] },
    ]);
    const result = actionableSteps("E1", steps);
    expect(result.map((s) => s.id)).toEqual(["S3"]);
  });
});
