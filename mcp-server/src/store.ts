import { promises as fs } from "node:fs";
import path from "node:path";
import os from "node:os";
import crypto from "node:crypto";
import { TodosFile, TodoItem, validateItem, validateTransition } from "./schema.js";
import { withFileLock } from "./file-lock.js";

const FILENAME = ".todos.json";
/**
 * Registry path. Lazily resolved on each access (not captured at import time)
 * so that tests can redirect it. Two redirection mechanisms:
 *   1. QUESTLOG_REGISTRY env var — absolute override (used by test setup, most
 *      reliable: no dependency on vi.mock hoisting timing).
 *   2. os.homedir() — the normal production path.
 * Relying solely on vi.mock("node:os") is fragile because store.js's eager
 * import can resolve the real os before the mock factory runs, leaking test
 * temp-dirs into the user's real ~/.todo-board.json.
 */
function registryPath(): string {
  if (process.env.QUESTLOG_REGISTRY) return process.env.QUESTLOG_REGISTRY;
  return path.join(os.homedir(), ".todo-board.json");
}

/**
 * Client cwd forwarded by the ssh wrapper (deploy/m4/todo-board-mcp-ssh.sh).
 * The authority-host MCP lands in HOME over ssh, so a remote session's
 * project dir is unknowable and auto-detection silently reads an empty
 * board. Mapping re-homes the client's path onto this host's home prefix.
 * Mapped paths still pass the unchanged registry/workspace allowlist below —
 * this adds a retry path, never a bypass.
 */
function clientHomePrefix(): string | null {
  const cwd = process.env.MCP_CLIENT_CWD;
  if (!cwd || !cwd.startsWith("/")) return null;
  const segs = cwd.split("/").filter(Boolean);
  if (segs.length < 2) return null;
  return `/${segs.slice(0, 2).join("/")}`;
}

/** Map a client-home-prefixed path onto this host's home; null when not under it. */
export function mapClientPath(p: string): string | null {
  const clientHome = clientHomePrefix();
  if (clientHome === null) return null;
  if (p !== clientHome && !p.startsWith(clientHome + "/")) return null;
  return path.join(os.homedir(), p.slice(clientHome.length));
}

/** Effective cwd for project resolution: mapped client cwd when present, else process.cwd(). */
export function effectiveCwd(): string {
  return mapClientPath(process.env.MCP_CLIENT_CWD || "") ?? process.cwd();
}
const SKIP_DIRS = new Set(["node_modules", ".git", ".vscode", ".idea", ".Trash", "__pycache__", "dist", "target", "build"]);

/** Weak/vague suggestion phrases that add no value. */
const WEAK_SUGGESTION_PATTERNS = [
  /^考虑/,
  /^建议/,
  /^后续/,
  /^进一步/,
  /^可以(再|去|尝试)?/,
  /^关注/,
  /^优化/,
  /^视情况/,
  /^根据情况/,
];

interface RegistryEntry { name: string; path: string }
interface Registry {
  projects: RegistryEntry[];
  feedback?: Record<string, { pushes?: number; archived_soon?: number }>;
  /** Monotonic revision for CAS. */
  revision?: number;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  [key: string]: any; // preserve unknown fields (e.g. Rust-side `trash`) on write-back
}

function resolveTodoPath(cwd: string): string {
  return path.join(cwd, FILENAME);
}

async function fileExists(filePath: string): Promise<boolean> {
  try { await fs.access(filePath); return true; } catch { return false; }
}

/** List immediate subdirectory names (skipping noise) */
async function getSubDirNames(parent: string): Promise<string[]> {
  try {
    const entries = await fs.readdir(parent, { withFileTypes: true });
    return entries
      .filter((e) => e.isDirectory() && !SKIP_DIRS.has(e.name) && !e.name.startsWith("."))
      .map((e) => e.name);
  } catch { return []; }
}

/**
 * Compute edit distance (Levenshtein). Used to detect typos in project names.
 * Returns 0 if strings are identical.
 */
function editDistance(a: string, b: string): number {
  const m = a.length, n = b.length;
  if (m === 0) return n;
  if (n === 0) return m;
  const dp: number[][] = Array.from({ length: m + 1 }, () => new Array(n + 1).fill(0));
  for (let i = 0; i <= m; i++) dp[i][0] = i;
  for (let j = 0; j <= n; j++) dp[0][j] = j;
  for (let i = 1; i <= m; i++) {
    for (let j = 1; j <= n; j++) {
      const cost = a[i - 1] === b[j - 1] ? 0 : 1;
      dp[i][j] = Math.min(dp[i - 1][j] + 1, dp[i][j - 1] + 1, dp[i - 1][j - 1] + cost);
    }
  }
  return dp[m][n];
}

/**
 * Auto-detect the correct project directory.
 * Signals (in priority order):
 *   1. .todos.json exists in cwd → already a project, use it
 *   2. A subdirectory name EXACTLY appears in title/tags → route to that subdirectory
 *      (no fuzzy matching — must be a complete word/segment match)
 *   3. Only one subdirectory exists → likely the project
 *   4. Fall back to cwd → item is unscoped (no auto-registration of new project)
 */
export async function resolveProjectDir(cwd: string, title: string, tags: string[]): Promise<string> {
  // 1. cwd already has .todos.json → established project
  if (await fileExists(path.join(cwd, FILENAME))) return cwd;

  // 2. Match subdirectory name against title/tags — EXACT match only
  const subs = await getSubDirNames(cwd);
  if (subs.length === 0) return cwd;

  const searchLower = [title, ...tags].map((s) => s.toLowerCase()).join(" ");
  for (const name of subs) {
    const nameLower = name.toLowerCase();
    // Match as a whole word/segment (surrounded by word boundaries or spaces)
    if (searchLower.includes(nameLower)) return path.join(cwd, name);
  }

  // 3. Single subdirectory → assume that's the project
  if (subs.length === 1) return path.join(cwd, subs[0]);

  // 4. Multiple subdirectories, no exact match → cwd (unscoped, no new project)
  return cwd;
}

/**
 * Check if a candidate name is a near-typo of an existing registered project.
 * Returns the suggested correction if edit distance ≤ 2, otherwise null.
 */
export async function suggestSimilarProject(name: string): Promise<string | null> {
  const registry = await readRegistry();
  const nameLower = name.toLowerCase();
  for (const entry of registry.projects) {
    if (editDistance(nameLower, entry.name.toLowerCase()) <= 2 && entry.name.toLowerCase() !== nameLower) {
      return entry.name;
    }
  }
  return null;
}

/** List subdirectories that have .todos.json (for list_todos workspace view) */
export async function findSubProjects(parent: string): Promise<string[]> {
  const names = await getSubDirNames(parent);
  const result: string[] = [];
  for (const name of names) {
    if (await fileExists(path.join(parent, name, FILENAME))) {
      result.push(path.join(parent, name));
    }
  }
  return result;
}

/**
 * Result of a defensive store read.
 * - `ok` → valid data returned.
 * - `notFound` → file does not exist (ENOENT) — caller may create it.
 * - `error` → permission denied, IO failure, or corrupt JSON — the caller
 *   MUST fail-closed: never fall back to empty data and write over the real
 *   file, because that would silently destroy the user's data. The corrupt
 *   file is preserved as a `.bak` backup for recovery.
 */
export type ReadResult<T> =
  | { ok: true; data: T }
  | { notFound: true }
  | { error: string };

/**
 * Read a JSON store with strict fail-closed semantics:
 * - File missing (ENOENT only) → `{ notFound: true }` — caller may create it.
 * - Permission denied / IO error → `{ error }` — caller must NOT write.
 * - Corrupt JSON → rename to `<name>.bak.<ts>`, return `{ error }` — caller
 *   must NOT write over the backup.
 * The original data is never silently overwritten; a corrupt file is preserved.
 */
export async function readStoreSafe(filePath: string): Promise<ReadResult<TodosFile>> {
  let raw: string;
  try {
    raw = await fs.readFile(filePath, "utf-8");
  } catch (err) {
    const code = (err as NodeJS.ErrnoException).code;
    if (code === "ENOENT") {
      return { notFound: true };
    }
    // Permission, IO, or any other non-ENOENT error → fail-closed.
    return { error: `read error (${code ?? "unknown"}): ${(err as Error).message}` };
  }
  try {
    return { ok: true, data: JSON.parse(raw) as TodosFile };
  } catch (err) {
    // Corrupt JSON — preserve the file as a backup, then signal error.
    await backupCorruptFile(filePath, "store");
    return { error: `corrupt JSON (backed up): ${(err as Error).message}` };
  }
}

/**
 * Convenience wrapper: returns the parsed data, or an empty default if the
 * file is genuinely missing (ENOENT). Throws on permission/IO/corrupt errors.
 *
 * The empty-on-ENOENT semantic is safe because a missing file has no data to
 * lose. All other error paths propagate, ensuring the caller never writes over
 * real user data after a failed read.
 */
export async function readStoreOrThrow(filePath: string): Promise<TodosFile> {
  const result = await readStoreSafe(filePath);
  if ("ok" in result) return result.data;
  if ("notFound" in result) {
    return { version: 1, project: path.basename(path.dirname(filePath)), items: [] };
  }
  throw new Error(result.error);
}

/** Rename a corrupt data file to `<name>.bak.<timestamp>` so it survives. */
export async function backupCorruptFile(filePath: string, label: string): Promise<void> {
  const bak = `${filePath}.bak.${Date.now()}`;
  try {
    await fs.rename(filePath, bak);
    console.warn(`[${label}] ${filePath} is corrupt — moved to ${bak}; starting fresh. Recover data from the backup.`);
  } catch (err) {
    console.warn(`[${label}] corrupt file at ${filePath} could not be backed up: ${(err as Error).message}`);
  }
}

/**
 * Write .todos.json atomically. Bumps revision for CAS.
 * Does NOT acquire a lock — callers needing cross-process safety must
 * wrap the read+write in withFileLock.
 */
async function writeStore(filePath: string, data: TodosFile): Promise<void> {
  data.revision = (data.revision ?? 0) + 1;
  const tmp = filePath + ".tmp-" + crypto.randomUUID();
  await fs.writeFile(tmp, JSON.stringify(data, null, 2), "utf-8");
  await fs.rename(tmp, filePath);
}

async function readRegistry(): Promise<Registry> {
  let raw: string;
  try {
    raw = await fs.readFile(registryPath(), "utf-8");
  } catch (err) {
    if ((err as NodeJS.ErrnoException).code === "ENOENT") return { projects: [] };
    console.warn(`[store] failed to read registry: ${(err as Error).message}`);
    return { projects: [] };
  }
  try {
    return JSON.parse(raw) as Registry;
  } catch {
    await backupCorruptFile(registryPath(), "registry");
    return { projects: [] };
  }
}

async function writeRegistry(registry: Registry): Promise<void> {
  const regPath = registryPath();
  registry.revision = (registry.revision ?? 0) + 1;
  const tmp = regPath + ".tmp-" + crypto.randomUUID();
  await fs.writeFile(tmp, JSON.stringify(registry, null, 2), "utf-8");
  await fs.rename(tmp, regPath);
}

/**
 * Lock-protected registry mutation: read, apply, write back.
 * Used for operations that need read-modify-write atomicity on the registry.
 */
async function mutateRegistry<T>(fn: (reg: Registry) => T): Promise<T> {
  return withFileLock(
    registryPath(),
    async () => {
      const reg = await readRegistry();
      const result = fn(reg);
      await writeRegistry(reg);
      return result;
    },
    10_000,
  );
}

/**
 * Resolve a directory's git common dir without spawning git (pure fs).
 * Handles both a `.git` directory and a `.git` file (linked worktree):
 *   - directory → realpath of the .git dir
 *   - file with `gitdir: <path>` → resolve relative to the .git file's dir;
 *     if that gitdir has a `commondir` file (linked worktree), resolve it
 *     relative to the gitdir to get the shared repo root.
 * Returns the canonical common dir, or null when the path is not in a repo.
 */
export async function resolveGitCommonDir(dir: string): Promise<string | null> {
  let cur = dir;
  for (;;) {
    const gitPath = path.join(cur, ".git");
    try {
      const st = await fs.stat(gitPath);
      if (st.isDirectory()) {
        return await fs.realpath(gitPath);
      }
      if (st.isFile()) {
        const content = await fs.readFile(gitPath, "utf-8");
        const m = content.match(/^gitdir:\s*(.+)$/m);
        if (!m) return null;
        const gitdir = path.resolve(cur, m[1].trim());
        // Linked worktree: gitdir/commondir points at the shared repo root.
        try {
          const commondirRaw = await fs.readFile(path.join(gitdir, "commondir"), "utf-8");
          return await fs.realpath(path.resolve(gitdir, commondirRaw.trim()));
        } catch {
          return await fs.realpath(gitdir);
        }
      }
    } catch {
      // no .git here
    }
    const parent = path.dirname(cur);
    if (parent === cur) return null;
    cur = parent;
  }
}

/**
 * Route a working directory to the registered project that shares its git
 * repository (e.g. a checkout/worktree of a registered project). This fixes
 * the "write-to-checkout, read-from-registry" split: registration calls land
 * in the .epics.json the desktop board actually reads.
 *
 * Matching order:
 *   1. cwd realpath equals a registered project path → that project.
 *   2. git common dir equals a registered project's git common dir → the
 *      first registered match (registration has one entry per repo today).
 * Returns the registered project's canonical path, or null when no match.
 */
export async function resolveRegisteredProject(cwd: string): Promise<string | null> {
  const registry = await readRegistry();
  const cwdCanonical = await fs.realpath(cwd).catch(() => cwd);

  // 1. exact path match
  for (const p of registry.projects) {
    const pc = await fs.realpath(p.path).catch(() => p.path);
    if (pc === cwdCanonical) return pc;
  }

  // 2. same git repository (common dir equality)
  const cwdGit = await resolveGitCommonDir(cwdCanonical);
  if (!cwdGit) return null;
  for (const p of registry.projects) {
    const pc = await fs.realpath(p.path).catch(() => p.path);
    const pg = await resolveGitCommonDir(pc);
    if (pg && pg === cwdGit) return pc;
  }
  return null;
}

/**
 * Validate + canonicalize an explicitly-supplied project directory.
 * Allowed set = the current working directory, its descendants, or a directory
 * already registered in ~/.todo-board.json. Rejects `..` escapes, outside
 * absolute paths, symlink escapes, and nonexistent paths.
 * Returns the canonical absolute path, or null when disallowed.
 */
export async function canonicalizeProjectDir(dir: string, cwd: string): Promise<string | null> {
  let canonical: string;
  try {
    canonical = await fs.realpath(dir);
  } catch (err) {
    // Remote client path (doesn't exist on this host): re-home it onto this
    // host's prefix and retry. Only "cannot exist" errors qualify — an
    // EACCES/ELOOP on a path that DOES exist must fail, not be re-anchored.
    const code = (err as NodeJS.ErrnoException).code;
    if (code !== "ENOENT" && code !== "ENOTDIR") return null;
    const mapped = mapClientPath(dir);
    if (mapped === null) return null;
    try {
      canonical = await fs.realpath(mapped);
    } catch {
      return null;
    }
  }
  const stat = await fs.stat(canonical).catch(() => null);
  if (!stat || !stat.isDirectory()) return null;

  // Allowed: within cwd (cwd itself or any descendant), or a registered project.
  const cwdCanonical = await fs.realpath(cwd).catch(() => cwd);
  const rel = path.relative(cwdCanonical, canonical);
  const insideCwd = rel === "" || (!rel.startsWith("..") && !path.isAbsolute(rel));
  if (insideCwd) return canonical;

  const registry = await readRegistry();
  const registered = await Promise.all(
    registry.projects.map(async (p) => {
      const pc = await fs.realpath(p.path).catch(() => p.path);
      return pc;
    })
  );
  if (registered.includes(canonical)) return canonical;

  console.warn(`[store] rejected project_dir outside workspace/registry: ${dir}`);
  return null;
}

/** Register a project in the central registry (~/.todo-board.json).
 *  Does NOT auto-register if cwd looks like a typo of an existing project.
 *  Uses lock-protected mutation for cross-process safety. */
export async function registerProject(cwd: string): Promise<void> {
  const name = path.basename(cwd);
  return mutateRegistry((registry) => {
    // Already registered by path
    if (registry.projects.some((p) => p.path === cwd)) return;

    // Check for typo — if the name is within edit distance 2 of an existing project,
    // don't create an orphan.
    const nameLower = name.toLowerCase();
    const isTypo = registry.projects.some(
      (p) => p.name.toLowerCase() !== nameLower && editDistance(nameLower, p.name.toLowerCase()) <= 2,
    );
    if (isTypo) return; // Silently skip

    registry.projects.push({ name, path: cwd });
  });
}

export function pushTodo(cwd: string, input: {
  type: TodoItem["type"];
  title: string;
  content?: string;
  suggestion?: string;
  tags?: string[];
  priority?: TodoItem["priority"];
  source_session?: string;
  /** Nightly auto-process opt-in (explicit user request only, todo type only). */
  auto_process?: boolean;
}): { item: TodoItem } | { error: string } {
  const content = input.content?.trim() ?? "";
  const suggestion = input.suggestion?.trim() ?? "";

  // The automated channel only processes research todos — an opt-in on
  // knowhow/idea is an invalid authorization, rejected at the door.
  if (input.auto_process && input.type !== "todo") {
    return { error: "auto_process opt-in is only valid for type='todo' items" };
  }

  // Format check (not a rejection): guide the AI to fill fields per template.
  // The user explicitly requested this push, so we never refuse — we only
  // point out missing structure so the AI can ask the user to fill it.
  if (content.length < 15) {
    return { error: `content 过短（${content.length} 字），未达到类型模板的信息量。请先问用户补齐：knowhow=背景+要点+适用边界；todo=目标+验收标准；idea=痛点+假设+收益+验证方式。用户明确说"别问了直接记"时才可按现有信息推送。` };
  }

  if (suggestion.length > 0 && WEAK_SUGGESTION_PATTERNS.some((re) => re.test(suggestion))) {
    return { error: `suggestion「${suggestion}」过于空泛。请向用户确认一个具体的下一步动作（动词 + 对象 + 预期结果），例如"对比 Redis vs Memcached 延迟基准"而非"建议后续关注性能"。用户明确说"别问了"时才可保留。` };
  }

  const now = new Date().toISOString();
  const item: TodoItem = {
    id: crypto.randomUUID(),
    type: input.type,
    title: input.title.trim(),
    content,
    suggestion,
    tags: input.tags ?? [],
    status: "active",
    priority: input.priority ?? "medium",
    source_session: input.source_session ?? "",
    processing: false,
    ...(input.auto_process ? { auto_process: true, auto_process_revision: 1 } : {}),
    created_at: now,
    updated_at: now,
  };

  const err = validateItem(item);
  if (err) return { error: err };

  return { item };
}

export async function listTodos(cwd: string): Promise<{ data: TodosFile } | { error: string }> {
  const result = await readStoreSafe(resolveTodoPath(cwd));
  if ("ok" in result) return { data: result.data };
  if ("notFound" in result) {
    return { data: { version: 1, project: path.basename(cwd), items: [] } };
  }
  return { error: result.error };
}

export async function addAndSave(cwd: string, item: TodoItem): Promise<{ ok: true } | { error: string }> {
  const filePath = resolveTodoPath(cwd);
  // Acquire cross-process lock for the read-modify-write.
  return withFileLock(
    filePath,
    async () => {
      // Fail-closed: a corrupt or unreadable file must not be overwritten.
      const result = await readStoreSafe(filePath);
      let data: TodosFile;
      if ("ok" in result) {
        data = result.data;
      } else if ("notFound" in result) {
        data = { version: 1, project: path.basename(cwd), items: [], revision: 0 };
      } else {
        return { error: result.error };
      }

      const dup = data.items.find((i) => i.status === "active" && i.title === item.title);
      if (dup) return { error: `active item with title '${item.title}' already exists (id: ${dup.id})` };

      data.items.push(item);
      data.project = path.basename(cwd);
      await writeStore(filePath, data);
      return { ok: true as const };
    },
    10_000,
  ).then(
    async (res) => {
      if ("error" in res) return res;
      // Registration and feedback recording happen outside the data lock
      // (they touch the registry, not the data file).
      await registerProject(cwd);
      await recordPush(cwd);
      return res;
    },
    (err) => ({ error: (err as Error).message }),
  );
}

/** Record a push in the quality feedback store (registry).
 *  Lock-protected for cross-process safety. */
async function recordPush(cwd: string): Promise<void> {
  return mutateRegistry((registry) => {
    registry.feedback = registry.feedback ?? {};
    const f = registry.feedback[cwd] ?? { pushes: 0, archived_soon: 0 };
    f.pushes = (f.pushes ?? 0) + 1;
    registry.feedback[cwd] = f;
  });
}

/** Read quality feedback for a project. */
export async function readFeedback(cwd: string): Promise<{ pushes: number; archived_soon: number }> {
  const registry = await readRegistry();
  const f = registry.feedback?.[cwd];
  return { pushes: f?.pushes ?? 0, archived_soon: f?.archived_soon ?? 0 };
}

/** Search active items across all registered projects by keyword */
export async function searchTodos(keyword: string): Promise<(TodoItem & { project: string; project_path: string })[]> {
  const registry = await readRegistry();
  const kw = keyword.toLowerCase();
  const results: (TodoItem & { project: string; project_path: string })[] = [];

  for (const entry of registry.projects) {
    // Use fail-closed reader: skip projects with corrupt/unreadable data
    // rather than treating them as empty (which would silently hide items).
    const result = await readStoreSafe(path.join(entry.path, FILENAME));
    let data: TodosFile | null = null;
    if ("ok" in result) {
      data = result.data;
    } else if ("notFound" in result) {
      continue; // project not initialized — skip
    } else {
      // Corrupt or unreadable — log and skip this project, don't crash search
      console.warn(`[store] skipping project '${entry.name}' during search: ${result.error}`);
      continue;
    }
    for (const item of data.items) {
      if (item.status !== "active") continue;
      const haystack = [item.title, item.content, item.suggestion, ...item.tags].join(" ").toLowerCase();
      if (haystack.includes(kw)) {
        results.push({ ...item, project: entry.name, project_path: entry.path });
      }
    }
  }

  // Sort by updated_at desc
  results.sort((a, b) => b.updated_at.localeCompare(a.updated_at));
  return results;
}

export async function updateAndSave(
  cwd: string,
  id: string,
  patch: Partial<Pick<TodoItem, "status" | "content" | "tags" | "priority" | "title" | "processing" | "auto_process" | "auto_process_revision">>
): Promise<{ ok: true } | { error: string }> {
  // 2026-09-23 注入防护（纵深）：字段长度硬上限——schema 层之外再拦一层，
  // 防拼接载荷经任何调用路径入库（事件存证 ~/.zcode/ymos-evidence-backup/injection-20260923/）。
  if (patch.title !== undefined && patch.title.length > 300)
    return { error: `title too long (${patch.title.length} > 300 chars) — rejected` };
  if (patch.content !== undefined && patch.content.length > 20000)
    return { error: `content too long (${patch.content.length} > 20000 chars) — rejected` };
  if (patch.tags !== undefined && (patch.tags.length > 12 || patch.tags.some((t) => t.length > 64)))
    return { error: "tags invalid (max 12 tags, 64 chars each) — rejected" };
  const filePath = resolveTodoPath(cwd);
  return withFileLock(
    filePath,
    async () => {
      // Fail-closed: a corrupt or unreadable file must not be overwritten.
      const result = await readStoreSafe(filePath);
      let data: TodosFile;
      if ("ok" in result) {
        data = result.data;
      } else if ("notFound" in result) {
        return { error: `item '${id}' not found (file does not exist)` };
      } else {
        return { error: result.error };
      }
      const item = data.items.find((i) => i.id === id);
      if (!item) return { error: `item '${id}' not found` };

      // Opt-in authorization guard: the automated channel only processes
      // research todos — an opt-in on knowhow/idea is invalid.
      const nextAuto = patch.auto_process !== undefined ? patch.auto_process : item.auto_process;
      if (nextAuto && item.type !== "todo") {
        return { error: "auto_process opt-in is only valid for type='todo' items" };
      }

      if (patch.status) {
        const err = validateTransition(item.type, patch.status);
        if (err) return { error: err };
        item.status = patch.status;
        // Completing an item also clears its in-progress flag
        if (patch.status === "completed" || patch.status === "archived" || patch.status === "converted") {
          item.processing = false;
        }
      }
      if (patch.content !== undefined) item.content = patch.content;
      if (patch.tags !== undefined) item.tags = patch.tags;
      if (patch.priority !== undefined) item.priority = patch.priority;
      if (patch.title !== undefined) item.title = patch.title;
      if (patch.processing !== undefined) item.processing = patch.processing;

      // Opt-in application: every false→true transition bumps
      // auto_process_revision so results from a previous authorization are
      // never reused (待办租户 plan §5.1). Turning OFF does not bump — the
      // next explicit re-enable gets a fresh revision.
      if (patch.auto_process !== undefined) {
        if (patch.auto_process && !item.auto_process) {
          item.auto_process_revision = (item.auto_process_revision ?? 0) + 1;
        }
        item.auto_process = patch.auto_process;
      }
      if (patch.auto_process_revision !== undefined) {
        // Explicit re-authorization of the SAME on-state (rare): only ever
        // moves forward, never backwards to reuse a closed result.
        item.auto_process_revision = Math.max(
          item.auto_process_revision ?? 0,
          patch.auto_process_revision,
        );
      }

      item.updated_at = new Date().toISOString();
      await writeStore(filePath, data);
      return { ok: true as const };
    },
    10_000,
  ).catch((err: Error) => ({ error: err.message }));
}
