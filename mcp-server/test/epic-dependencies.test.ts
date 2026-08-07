import { describe, it, expect, beforeEach } from "vitest";
import { deriveDependencies } from "../src/epic-store.js";
import { makeEpicsFile, resetIds, type EpicSeed } from "./fixtures.js";

// deriveDependencies(data) mutates in place:
//   1. Rebuild `blocks` (reverse of depends_on) for every epic.
//   2. If an epic has any non-done dependency → force status "blocked"
//      (unless already done/paused).
//   3. If dependencies are all done AND epic is currently "blocked" →
//      release back to "draft" (user picks next).

describe("deriveDependencies", () => {
  beforeEach(resetIds);

  // ── blocks reverse graph ──

  it("rebuilds blocks as the reverse of depends_on", () => {
    // B depends on A → A.blocks should contain B.
    const data = makeEpicsFile([
      { id: "A", status: "done" },
      { id: "B", status: "draft", dependsOn: ["A"] },
    ]);
    deriveDependencies(data);
    const a = data.epics.find((e) => e.id === "A")!;
    expect(a.blocks).toContain("B");
  });

  it("accumulates multiple dependents into one epic's blocks", () => {
    const data = makeEpicsFile([
      { id: "A", status: "done" },
      { id: "B", status: "draft", dependsOn: ["A"] },
      { id: "C", status: "draft", dependsOn: ["A"] },
    ]);
    deriveDependencies(data);
    const a = data.epics.find((e) => e.id === "A")!;
    expect(a.blocks.sort()).toEqual(["B", "C"]);
  });

  it("clears stale blocks before rebuilding", () => {
    // Seed A with a stale blocks entry that should be wiped.
    const seeds: EpicSeed[] = [
      { id: "A", status: "done", blocks: ["STALE"] },
      { id: "B", status: "done" },
    ];
    const data = makeEpicsFile(seeds);
    deriveDependencies(data);
    const a = data.epics.find((e) => e.id === "A")!;
    expect(a.blocks).toEqual([]);
  });

  // ── auto-blocked when dependency not done ──

  it("forces an epic to blocked when its dependency is not done", () => {
    const data = makeEpicsFile([
      { id: "A", status: "active" }, // not done
      { id: "B", status: "active", dependsOn: ["A"] },
    ]);
    deriveDependencies(data);
    const b = data.epics.find((e) => e.id === "B")!;
    expect(b.status).toBe("blocked");
  });

  it("leaves an epic unblocked when its dependency is done", () => {
    const data = makeEpicsFile([
      { id: "A", status: "done" },
      { id: "B", status: "active", dependsOn: ["A"] },
    ]);
    deriveDependencies(data);
    const b = data.epics.find((e) => e.id === "B")!;
    expect(b.status).toBe("active");
  });

  it("blocks when ANY one of multiple dependencies is not done", () => {
    const data = makeEpicsFile([
      { id: "A", status: "done" },
      { id: "B", status: "active" }, // not done
      { id: "C", status: "active", dependsOn: ["A", "B"] },
    ]);
    deriveDependencies(data);
    const c = data.epics.find((e) => e.id === "C")!;
    expect(c.status).toBe("blocked");
  });

  // ── release blocked → draft when deps become satisfied ──

  it("releases a blocked epic back to draft when all deps are done", () => {
    const data = makeEpicsFile([
      { id: "A", status: "done" },
      { id: "B", status: "blocked", dependsOn: ["A"] },
    ]);
    deriveDependencies(data);
    const b = data.epics.find((e) => e.id === "B")!;
    expect(b.status).toBe("draft");
  });

  // ── done / paused are immune ──

  it("does not force-block a done epic even if its deps are unmet", () => {
    const data = makeEpicsFile([
      { id: "A", status: "active" },
      { id: "B", status: "done", dependsOn: ["A"] },
    ]);
    deriveDependencies(data);
    const b = data.epics.find((e) => e.id === "B")!;
    expect(b.status).toBe("done");
  });

  it("does not force-block a paused epic", () => {
    const data = makeEpicsFile([
      { id: "A", status: "active" },
      { id: "B", status: "paused", dependsOn: ["A"] },
    ]);
    deriveDependencies(data);
    const b = data.epics.find((e) => e.id === "B")!;
    expect(b.status).toBe("paused");
  });

  it("does not release a paused epic even when deps are satisfied", () => {
    const data = makeEpicsFile([
      { id: "A", status: "done" },
      { id: "B", status: "paused", dependsOn: ["A"] },
    ]);
    deriveDependencies(data);
    const b = data.epics.find((e) => e.id === "B")!;
    expect(b.status).toBe("paused");
  });

  // ── edge cases ──

  it("ignores orphan dependency ids (pointing to nonexistent epics)", () => {
    // B depends on a ghost id — should not crash, and B should be blocked
    // because the ghost is never "done".
    const data = makeEpicsFile([
      { id: "B", status: "active", dependsOn: ["GHOST"] },
    ]);
    expect(() => deriveDependencies(data)).not.toThrow();
    const b = data.epics.find((e) => e.id === "B")!;
    expect(b.status).toBe("blocked");
  });

  // ── Cycle / self-reference (M4): depends_on cycles are malformed but must
  //    not crash. These tests pin the CURRENT behavior so future changes are
  //    intentional. deriveDependencies is iterative (not recursive), so it
  //    terminates by construction — these tests guard the outcome. ──

  it("self-referential depends_on (A→A) forces A to blocked and never releases it", () => {
    // A depends on itself. A is never "done" (its own dep is unmet), so the
    // algorithm forces A to blocked. A cannot self-release: releasing requires
    // deps to be done, but A's only dep is A itself, which is now blocked.
    const data = makeEpicsFile([
      { id: "A", status: "active", dependsOn: ["A"] },
    ]);
    expect(() => deriveDependencies(data)).not.toThrow();
    expect(data.epics.find((e) => e.id === "A")!.status).toBe("blocked");
  });

  it("two-node depends_on cycle (A→B, B→A) blocks both, then releases B when A is marked done", () => {
    // Start: both active, mutual deps → both blocked.
    const data = makeEpicsFile([
      { id: "A", status: "active", dependsOn: ["B"] },
      { id: "B", status: "active", dependsOn: ["A"] },
    ]);
    deriveDependencies(data);
    expect(data.epics.find((e) => e.id === "A")!.status).toBe("blocked");
    expect(data.epics.find((e) => e.id === "B")!.status).toBe("blocked");

    // Mark A done and re-derive: A is immune (done); B's dep A is now met →
    // B releases from blocked to draft.
    data.epics.find((e) => e.id === "A")!.status = "done";
    deriveDependencies(data);
    expect(data.epics.find((e) => e.id === "A")!.status).toBe("done");
    expect(data.epics.find((e) => e.id === "B")!.status).toBe("draft");
  });

  it("leaves independent epics (no depends_on) untouched", () => {
    const data = makeEpicsFile([
      { id: "A", status: "active" },
      { id: "B", status: "verifying" },
    ]);
    deriveDependencies(data);
    expect(data.epics.find((e) => e.id === "A")!.status).toBe("active");
    expect(data.epics.find((e) => e.id === "B")!.status).toBe("verifying");
  });
});
