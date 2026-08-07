import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { pushTodo } from "../src/store.js";

// pushTodo(cwd, input):
//   - content trimmed; if length < 15 → error (format gate, not a hard reject)
//   - suggestion matched against WEAK_SUGGESTION_PATTERNS → error
//   - otherwise constructs a TodoItem with status "active", processing false,
//     timestamps from new Date(), id from crypto.randomUUID()
//   - finally runs validateItem on the result.
//
// Non-determinism (crypto, Date) is pinned via module mock + fake timers.
// store.ts does `import crypto from "node:crypto"` and calls crypto.randomUUID(),
// so we mock the module's default export.

vi.mock("node:crypto", () => ({
  default: { randomUUID: () => "fixed-uuid" },
}));

describe("pushTodo", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-01-01T00:00:00.000Z"));
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  // ── content gate ──

  it("rejects content shorter than 15 characters", () => {
    const res = pushTodo("/tmp/proj", {
      type: "todo",
      title: "Do thing",
      content: "too short", // 8 chars
    });
    expect("error" in res).toBe(true);
    expect((res as { error: string }).error).toMatch(/content 过短|too short|未达到/i);
  });

  it("accepts content that meets the 15-char minimum", () => {
    const res = pushTodo("/tmp/proj", {
      type: "todo",
      title: "Do thing",
      content: "this is long enough to pass", // 28 chars
    });
    expect(res).toMatchObject({
      item: expect.objectContaining({
        id: "fixed-uuid",
        title: "Do thing",
        type: "todo",
        content: "this is long enough to pass",
      }),
    });
  });

  // ── weak suggestion gate ──

  it("rejects a suggestion starting with a weak verb (建议)", () => {
    const res = pushTodo("/tmp/proj", {
      type: "todo",
      title: "Do thing",
      content: "this is long enough to pass",
      suggestion: "建议后续关注性能",
    });
    expect("error" in res).toBe(true);
    expect((res as { error: string }).error).toMatch(/空泛|过于/i);
  });

  it("rejects a suggestion starting with 考虑", () => {
    const res = pushTodo("/tmp/proj", {
      type: "todo",
      title: "Do thing",
      content: "this is long enough to pass",
      suggestion: "考虑加个缓存",
    });
    expect("error" in res).toBe(true);
  });

  it("accepts a concrete, verb-first suggestion", () => {
    const res = pushTodo("/tmp/proj", {
      type: "todo",
      title: "Do thing",
      content: "this is long enough to pass",
      suggestion: "对比 Redis 与 Memcached 的延迟基准",
    });
    expect(res).toMatchObject({
      item: expect.objectContaining({
        id: "fixed-uuid",
        title: "Do thing",
        suggestion: "对比 Redis 与 Memcached 的延迟基准",
      }),
    });
  });

  it("accepts an empty suggestion (no gate when nothing proposed)", () => {
    const res = pushTodo("/tmp/proj", {
      type: "todo",
      title: "Do thing",
      content: "this is long enough to pass",
      suggestion: "",
    });
    expect(res).toMatchObject({
      item: expect.objectContaining({
        id: "fixed-uuid",
        title: "Do thing",
        suggestion: "",
      }),
    });
  });

  // ── constructed item shape ──

  it("fills defaults: status active, processing false, priority medium, deterministic id/timestamps", () => {
    const res = pushTodo("/tmp/proj", {
      type: "knowhow",
      title: "  Trim me  ", // title should be trimmed
      content: "background and key points here",
      tags: ["a", "b"],
    });
    expect("item" in res).toBe(true);
    if (!("item" in res)) return;
    const item = res.item;
    expect(item.id).toBe("fixed-uuid");
    expect(item.title).toBe("Trim me");
    expect(item.status).toBe("active");
    expect(item.processing).toBe(false);
    expect(item.priority).toBe("medium");
    expect(item.source_session).toBe("");
    expect(item.created_at).toBe("2026-01-01T00:00:00.000Z");
    expect(item.updated_at).toBe("2026-01-01T00:00:00.000Z");
    expect(item.tags).toEqual(["a", "b"]);
  });

  it("passes through explicitly provided priority and source_session", () => {
    const res = pushTodo("/tmp/proj", {
      type: "todo",
      title: "X",
      content: "this is long enough to pass",
      priority: "high",
      source_session: "sess-42",
    });
    expect("item" in res).toBe(true);
    if (!("item" in res)) return;
    expect(res.item.priority).toBe("high");
    expect(res.item.source_session).toBe("sess-42");
  });

  // ── invalid type flows through to validateItem ──

  it("returns an error for an invalid type (validateItem catches it)", () => {
    const res = pushTodo("/tmp/proj", {
      type: "bug" as "todo",
      title: "X",
      content: "this is long enough to pass",
    });
    expect("error" in res).toBe(true);
  });
});
