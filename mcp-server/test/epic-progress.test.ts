import { describe, it, expect, beforeEach } from "vitest";
import { epicProgress } from "../src/epic-store.js";
import { makeEpicsFile, resetIds, type EpicSeed, type StepSeed } from "./fixtures.js";

// epicProgress(epicId, data):
//   - has child epics  → mean of children's progress
//   - else has steps   → mean of step stage percentages
//   - else             → 0
// Percentages: pending 0, designed 25, coded 50, verified 75, observed 100.

describe("epicProgress", () => {
  beforeEach(resetIds);

  it("returns 0 for an epic with no steps and no children", () => {
    const data = makeEpicsFile([{ id: "E1" }], []);
    expect(epicProgress("E1", data)).toBe(0);
  });

  it("returns 0 when the only step is pending", () => {
    const data = makeEpicsFile(
      [{ id: "E1" }],
      [{ id: "S1", epicId: "E1", stage: "pending" }],
    );
    expect(epicProgress("E1", data)).toBe(0);
  });

  it("returns 100 when the only step is observed", () => {
    const data = makeEpicsFile(
      [{ id: "E1" }],
      [{ id: "S1", epicId: "E1", stage: "observed" }],
    );
    expect(epicProgress("E1", data)).toBe(100);
  });

  it("averages step stages and floors the result (50 + 75 = 125 / 2 = 62)", () => {
    const data = makeEpicsFile(
      [{ id: "E1" }],
      [
        { id: "S1", epicId: "E1", stage: "coded" }, // 50
        { id: "S2", epicId: "E1", stage: "verified" }, // 75
      ],
    );
    expect(epicProgress("E1", data)).toBe(62);
  });

  it("ignores steps belonging to other epics", () => {
    const data = makeEpicsFile(
      [{ id: "E1" }, { id: "E2" }],
      [
        { id: "S1", epicId: "E1", stage: "observed" },
        { id: "S2", epicId: "E2", stage: "pending" }, // should not affect E1
      ],
    );
    expect(epicProgress("E1", data)).toBe(100);
  });

  // ── Hierarchy: parent derives from children ──

  it("derives parent progress as the mean of child epic progress", () => {
    // E1 (top) → E1a (steps avg 62) + E1b (steps avg 100) → mean = 81
    const epics: EpicSeed[] = [
      { id: "E1" },
      { id: "E1a", parentId: "E1" },
      { id: "E1b", parentId: "E1" },
    ];
    const steps: StepSeed[] = [
      { id: "Sa1", epicId: "E1a", stage: "coded" },
      { id: "Sa2", epicId: "E1a", stage: "verified" },
      { id: "Sb1", epicId: "E1b", stage: "observed" },
    ];
    const data = makeEpicsFile(epics, steps);
    expect(epicProgress("E1a", data)).toBe(62);
    expect(epicProgress("E1b", data)).toBe(100);
    expect(epicProgress("E1", data)).toBe(81);
  });

  it("child with no steps contributes 0 to parent mean", () => {
    // E1 → E1a (observed, 100) + E1b (empty, 0) → mean = 50
    const epics: EpicSeed[] = [
      { id: "E1" },
      { id: "E1a", parentId: "E1" },
      { id: "E1b", parentId: "E1" },
    ];
    const steps: StepSeed[] = [
      { id: "Sa1", epicId: "E1a", stage: "observed" },
    ];
    const data = makeEpicsFile(epics, steps);
    expect(epicProgress("E1", data)).toBe(50);
  });

  it("three-level nesting: top derives from children, children from steps", () => {
    // E1 → E1a → [S: 100]; E1 → E1b → [S: 50]
    // E1a = 100, E1b = 50, E1 = 75
    const epics: EpicSeed[] = [
      { id: "E1" },
      { id: "E1a", parentId: "E1" },
      { id: "E1b", parentId: "E1" },
    ];
    const steps: StepSeed[] = [
      { id: "Sa1", epicId: "E1a", stage: "observed" },
      { id: "Sb1", epicId: "E1b", stage: "coded" },
    ];
    const data = makeEpicsFile(epics, steps);
    expect(epicProgress("E1", data)).toBe(75);
  });

  it("never crashes for an epic id that does not exist (returns via no-match path)", () => {
    const data = makeEpicsFile([{ id: "E1" }], []);
    // Non-existent id → no children, no steps → 0
    expect(epicProgress("nonexistent", data)).toBe(0);
  });

  // ── Cycle defense (M1): .epics.json is user-editable; a circular parent_id
  //    chain must NOT cause unbounded recursion / stack overflow. ──

  it("does not crash on a self-referential parent_id (E1.parent_id = E1)", () => {
    // E1 lists itself as its own child. Without cycle defense this recurses forever.
    const data = makeEpicsFile(
      [{ id: "E1", parentId: "E1" }],
      [{ id: "S1", epicId: "E1", stage: "observed" }],
    );
    expect(() => epicProgress("E1", data)).not.toThrow();
    // E1 is its own only child; recursing into E1 hits the visited guard → 0.
    // E1 never reaches its own steps because it has a "child" (itself).
    // Result is the mean of its children = mean([0]) = 0. The point is
    // termination, not a meaningful progress value for malformed data.
    expect(epicProgress("E1", data)).toBe(0);
  });

  it("does not crash on a two-node parent_id cycle (E1 ↔ E2)", () => {
    // Regression test for the exact fixture that crashed before the fix:
    // E1.parent_id = E2 and E2.parent_id = E1.
    const data = makeEpicsFile(
      [
        { id: "E1", parentId: "E2" },
        { id: "E2", parentId: "E1" },
      ],
      [],
    );
    expect(() => epicProgress("E1", data)).not.toThrow();
    expect(() => epicProgress("E2", data)).not.toThrow();
    // Both nodes form a cycle with no steps → 0.
    expect(epicProgress("E1", data)).toBe(0);
  });

  it("terminates a three-node cycle and still counts valid siblings", () => {
    // E1 → E2 → E3 → E1 (cycle), plus a healthy child E1a of E1.
    // E1's children = [E2, E1a]. E2 recurses into E3 → E1 (visited, cut off, 0).
    // E1a has an observed step → 100. E1 = mean(E2=0, E1a=100) = 50.
    const data = makeEpicsFile(
      [
        { id: "E1" },
        { id: "E2", parentId: "E1" },
        { id: "E3", parentId: "E2" },
        { id: "E1a", parentId: "E1" },
      ],
      [{ id: "S1", epicId: "E1a", stage: "observed" }],
    );
    expect(() => epicProgress("E1", data)).not.toThrow();
    expect(epicProgress("E1", data)).toBe(50);
  });
});
