import { describe, it, expect } from "vitest";
import { validateItem, validateTransition } from "../src/schema.js";
import type { ItemType, ItemStatus } from "../src/schema.js";

// ─── validateItem ───
// Pure validation against the ItemType / ItemStatus / Priority enums.

describe("validateItem", () => {
  it("returns null for a minimal valid item (title + type)", () => {
    expect(validateItem({ title: "x", type: "todo" })).toBeNull();
  });

  it("rejects empty/whitespace title", () => {
    expect(validateItem({ title: "   ", type: "todo" })).toMatch(/title is required/i);
    expect(validateItem({ type: "todo" })).toMatch(/title is required/i);
  });

  it("accepts all three valid types", () => {
    const types: ItemType[] = ["knowhow", "todo", "idea"];
    for (const t of types) {
      expect(validateItem({ title: "x", type: t })).toBeNull();
    }
  });

  it("rejects an invalid type", () => {
    expect(validateItem({ title: "x", type: "bug" as ItemType })).toMatch(/type must be one of/);
  });

  it("accepts all four valid statuses (status is optional)", () => {
    const statuses: ItemStatus[] = ["active", "completed", "archived", "converted"];
    for (const s of statuses) {
      expect(validateItem({ title: "x", type: "todo", status: s })).toBeNull();
    }
  });

  it("rejects an invalid status", () => {
    expect(
      validateItem({ title: "x", type: "todo", status: "closed" as ItemStatus }),
    ).toMatch(/status must be one of/);
  });

  it("accepts all three valid priorities", () => {
    for (const p of ["high", "medium", "low"] as const) {
      expect(validateItem({ title: "x", type: "todo", priority: p })).toBeNull();
    }
  });

  it("rejects an invalid priority", () => {
    expect(
      validateItem({ title: "x", type: "todo", priority: "urgent" as never }),
    ).toMatch(/priority must be one of/);
  });
});

// ─── validateTransition ───
// Per-type state machine:
//   knowhow: active ↔ archived
//   todo:    active → completed → archived
//   idea:    active → converted → archived

describe("validateTransition", () => {
  it("knowhow: allows active and archived only", () => {
    expect(validateTransition("knowhow", "active")).toBeNull();
    expect(validateTransition("knowhow", "archived")).toBeNull();
  });

  it("knowhow: rejects completed and converted", () => {
    expect(validateTransition("knowhow", "completed")).toMatch(/cannot transition/);
    expect(validateTransition("knowhow", "converted")).toMatch(/cannot transition/);
  });

  it("todo: allows active, completed, archived", () => {
    expect(validateTransition("todo", "active")).toBeNull();
    expect(validateTransition("todo", "completed")).toBeNull();
    expect(validateTransition("todo", "archived")).toBeNull();
  });

  it("todo: rejects converted (todos convert to nothing)", () => {
    expect(validateTransition("todo", "converted")).toMatch(/cannot transition/);
  });

  it("idea: allows active, converted, archived", () => {
    expect(validateTransition("idea", "active")).toBeNull();
    expect(validateTransition("idea", "converted")).toBeNull();
    expect(validateTransition("idea", "archived")).toBeNull();
  });

  it("idea: rejects completed (ideas don't get 'completed')", () => {
    expect(validateTransition("idea", "completed")).toMatch(/cannot transition/);
  });
});
