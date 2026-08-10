import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { promises as fs } from "node:fs";
import os from "node:os";
import path from "node:path";
import { listTodos, addAndSave, pushTodo, canonicalizeProjectDir } from "../src/store.js";
import { listEpics } from "../src/epic-store.js";

/**
 * Data-integrity tests (P0): corrupt JSON must be backed up, never silently
 * overwritten; ENOENT yields empty; valid files round-trip untouched.
 */

// Redirect the registry (~/.todo-board.json) into a temp dir so these tests
// never touch the real user registry. REGISTRY_PATH is captured at import time
// from os.homedir(), so we mock node:os before importing store.js.
let fakeHome = "";

vi.mock("node:os", async (importOriginal) => {
  const real = await importOriginal<typeof import("node:os")>();
  return {
    ...real,
    homedir: () => fakeHome,
  };
});

let dir: string;

beforeEach(async () => {
  dir = await fs.mkdtemp(path.join(os.tmpdir(), "questlog-integrity-"));
  fakeHome = dir; // registry writes land in the temp dir, never the real home
});

afterEach(async () => {
  await fs.rm(dir, { recursive: true, force: true });
  vi.restoreAllMocks();
});

function corruptFile(p: string, content = "{ not valid json !!") {
  return fs.writeFile(p, content, "utf-8");
}

describe("corrupt .todos.json", () => {
  it("backs up the corrupt file, returns error, and does not overwrite", async () => {
    const file = path.join(dir, ".todos.json");
    const original = "{ broken data that must survive }";
    await fs.writeFile(file, original, "utf-8");

    // Warning is expected — the test asserts on filesystem state, not logs.
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});

    // G0 fail-closed: corrupt data must surface as error, NOT empty data.
    // Returning empty would allow the next write to silently overwrite
    // the user's real data.
    const res = await listTodos(dir);
    expect(res).toHaveProperty("error");
    expect((res as { error: string }).error).toContain("corrupt");

    // Original content must still exist in a .bak file, never overwritten.
    const entries = await fs.readdir(dir);
    const baks = entries.filter((e) => e.startsWith(".todos.json.bak."));
    expect(baks.length).toBe(1);
    expect(await fs.readFile(path.join(dir, baks[0]), "utf-8")).toBe(original);
    warn.mockRestore();
  });

  it("ENOENT (no file) returns empty without creating anything", async () => {
    const res = await listTodos(dir);
    const data = res as { data: { items: unknown[] } };
    expect(data.data.items).toEqual([]);
    const entries = await fs.readdir(dir);
    expect(entries).toEqual([]);
  });

  it("valid file round-trips unchanged", async () => {
    const valid = { version: 1, project: "p", items: [{ id: "x", title: "hi" }] };
    await fs.writeFile(path.join(dir, ".todos.json"), JSON.stringify(valid), "utf-8");

    const res = await listTodos(dir);
    const data = res as { data: { items: { title: string }[] } };
    expect(data.data.items[0].title).toBe("hi");
    // No backup created for a valid file
    const entries = await fs.readdir(dir);
    expect(entries.filter((e) => e.includes(".bak."))).toEqual([]);
  });
});

describe("corrupt .epics.json", () => {
  it("backs up the corrupt file and returns error (fail-closed)", async () => {
    const file = path.join(dir, ".epics.json");
    const original = "{ corrupted epic data }";
    await fs.writeFile(file, original, "utf-8");
    vi.spyOn(console, "warn").mockImplementation(() => {});

    // G0 fail-closed: corrupt .epics.json returns error, not empty data.
    const res = await listEpics(dir);
    expect("error" in res).toBe(true);

    const entries = await fs.readdir(dir);
    const baks = entries.filter((e) => e.startsWith(".epics.json.bak."));
    expect(baks.length).toBe(1);
    expect(await fs.readFile(path.join(dir, baks[0]), "utf-8")).toBe(original);
  });
});

describe("write after corruption is rejected (fail-closed)", () => {
  it("addAndSave after a corrupt read is rejected, backup preserved, no overwrite", async () => {
    await corruptFile(path.join(dir, ".todos.json"));
    vi.spyOn(console, "warn").mockImplementation(() => {});

    const item = pushTodo(dir, { type: "todo", title: "t", content: "long enough content here" });
    if ("error" in item) throw new Error(item.error);

    // G0 fail-closed: write after corrupt read must be REJECTED, not allowed.
    // Allowing it would silently overwrite the user's data with a fresh file.
    const save = await addAndSave(dir, item.item);
    expect("error" in save).toBe(true);

    // Backup exists
    const entries = await fs.readdir(dir);
    expect(entries.some((e) => e.startsWith(".todos.json.bak."))).toBe(true);
    // The .todos.json file must NOT have been created (only the .bak exists).
    expect(entries.some((e) => e === ".todos.json")).toBe(false);
  });
});

describe("canonicalizeProjectDir (P0 path guard)", () => {
  it("allows cwd itself and descendants", async () => {
    const sub = path.join(dir, "sub");
    await fs.mkdir(sub);
    expect(await canonicalizeProjectDir(dir, dir)).toBe(await fs.realpath(dir));
    expect(await canonicalizeProjectDir(sub, dir)).toBe(await fs.realpath(sub));
  });

  it("rejects .. escape outside cwd", async () => {
    const outside = path.join(dir, "..");
    expect(await canonicalizeProjectDir(outside, dir)).toBeNull();
  });

  it("rejects nonexistent paths", async () => {
    expect(await canonicalizeProjectDir(path.join(dir, "nope"), dir)).toBeNull();
  });

  it("rejects files (not directories)", async () => {
    const f = path.join(dir, "afile.txt");
    await fs.writeFile(f, "x");
    expect(await canonicalizeProjectDir(f, dir)).toBeNull();
  });
});
