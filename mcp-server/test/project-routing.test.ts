import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { promises as fs } from "node:fs";
import os from "node:os";
import path from "node:path";
import { resolveRegisteredProject, resolveGitCommonDir } from "../src/store.js";
import { epicCwd } from "../src/index.js";

/**
 * Registered-project routing tests (P0 "write-point unification"):
 * epic-registration calls should land in the registered project that shares
 * the cwd's git repository, so the desktop board (which reads the registry)
 * sees the progress. All git repos here are fabricated with plain files —
 * no real `git` invocation.
 */

let tmp: string;
let registryPath: string;

async function makeRepo(repoName: string): Promise<string> {
  // A "bare-like" shared repo: a directory that .git files point at.
  const repo = path.join(tmp, "repos", repoName);
  await fs.mkdir(repo, { recursive: true });
  return repo;
}

/** Normal worktree: `.git` is a real directory. */
async function makeWorktree(worktreeName: string, repo: string): Promise<string> {
  const wt = path.join(tmp, "worktrees", worktreeName);
  await fs.mkdir(wt, { recursive: true });
  await fs.symlink(repo, path.join(wt, ".git"), "dir");
  return wt;
}

/** Linked worktree: `.git` is a file `gitdir: <path>` and gitdir has commondir. */
async function makeLinkedWorktree(worktreeName: string, repo: string): Promise<string> {
  const wt = path.join(tmp, "worktrees", worktreeName);
  const gitdir = path.join(tmp, "gitdirs", worktreeName);
  await fs.mkdir(wt, { recursive: true });
  await fs.mkdir(gitdir, { recursive: true });
  await fs.writeFile(path.join(wt, ".git"), `gitdir: ${gitdir}\n`, "utf-8");
  // Real git semantics: commondir content is relative to the gitdir directory.
  // gitdir = tmp/gitdirs/<name> → repo = tmp/repos/<name> needs ../../repos/<name>.
  await fs.writeFile(path.join(gitdir, "commondir"), `../../repos/${path.basename(repo)}\n`, "utf-8");
  return wt;
}

async function seedRegistry(projects: { name: string; path: string }[]): Promise<void> {
  await fs.writeFile(registryPath, JSON.stringify({ projects, feedback: {} }, null, 2), "utf-8");
}

beforeEach(async () => {
  tmp = await fs.mkdtemp(path.join(os.tmpdir(), "questlog-routing-"));
  registryPath = path.join(tmp, ".todo-board.json");
  process.env.QUESTLOG_REGISTRY = registryPath;
  await seedRegistry([]);
});

afterEach(async () => {
  delete process.env.QUESTLOG_REGISTRY;
  await fs.rm(tmp, { recursive: true, force: true });
});

describe("resolveGitCommonDir", () => {
  it("resolves a normal .git directory", async () => {
    const repo = await makeRepo("r1");
    const wt = await makeWorktree("wt-normal", repo);
    expect(await resolveGitCommonDir(wt)).toBe(await fs.realpath(repo));
  });

  it("resolves a linked worktree through .git file + commondir", async () => {
    const repo = await makeRepo("r2");
    const wt = await makeLinkedWorktree("wt-linked", repo);
    expect(await resolveGitCommonDir(wt)).toBe(await fs.realpath(repo));
  });

  it("returns null for a directory without git", async () => {
    const plain = path.join(tmp, "norepo");
    await fs.mkdir(plain, { recursive: true });
    expect(await resolveGitCommonDir(plain)).toBeNull();
  });
});

describe("resolveRegisteredProject", () => {
  it("returns the registered project when cwd is inside it (exact path match)", async () => {
    const repo = await makeRepo("r3");
    const registered = await makeWorktree("registered-main", repo);
    await seedRegistry([{ name: "ymos", path: registered }]);
    expect(await resolveRegisteredProject(registered)).toBe(await fs.realpath(registered));
  });

  it("routes a checkout/worktree to the registered project sharing the repo", async () => {
    const repo = await makeRepo("r4");
    const registered = await makeWorktree("registered-main", repo);
    const checkout = await makeLinkedWorktree("checkout-batch", repo);
    await seedRegistry([{ name: "ymos", path: registered }]);
    expect(await resolveRegisteredProject(checkout)).toBe(await fs.realpath(registered));
  });

  it("returns null when cwd is not in a git repo", async () => {
    const registered = await makeWorktree("registered-main", await makeRepo("r5"));
    await seedRegistry([{ name: "ymos", path: registered }]);
    const plain = path.join(tmp, "plaindir");
    await fs.mkdir(plain, { recursive: true });
    expect(await resolveRegisteredProject(plain)).toBeNull();
  });

  it("returns null when no registered project shares the repo", async () => {
    const repo = await makeRepo("r6");
    const other = await makeRepo("r7");
    await seedRegistry([{ name: "other", path: await makeWorktree("other-main", other) }]);
    const checkout = await makeLinkedWorktree("checkout-other", repo);
    expect(await resolveRegisteredProject(checkout)).toBeNull();
  });
});

describe("epicCwd", () => {
  it("honors an explicit project_dir (regression: was ignored, landing in cwd project)", async () => {
    const target = await makeWorktree("ymos-target", await makeRepo("ymos-repo"));
    const elsewhere = await makeWorktree("elsewhere", await makeRepo("other-repo"));
    await seedRegistry([{ name: "ymos", path: target }]);

    // Even when cwd is NOT inside the target project, explicit project_dir
    // must route there (canonicalizeProjectDir accepts registered projects).
    const resolved = await epicCwd(() => elsewhere, { project_dir: target });
    expect("error" in resolved).toBe(false);
    if ("error" in resolved) return;
    // canonicalizeProjectDir may resolve symlinks (/var → /private/var on macOS)
    expect(await fs.realpath(resolved.cwd)).toBe(await fs.realpath(target));
  });

  it("falls back to cwd default when no project_dir given", async () => {
    const fallback = await makeWorktree("fb", await makeRepo("fb-repo"));
    const resolved = await epicCwd(() => fallback);
    expect("error" in resolved).toBe(false);
    if ("error" in resolved) return;
    expect(resolved.cwd).toBe(fallback);
  });
});
