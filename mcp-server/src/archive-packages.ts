/**
 * archive-packages — terminal old package archival (体检 P2-2).
 *
 * packages.json grows without bound (494 entries / 1.2MB at design time,
 * 46% superseded). The Runner full-reads it every poll. This moves packages
 * that nothing can ever need again into archive/packages-archive.json
 * (append-only, upsert by package_id keeping the highest plan_revision).
 *
 * Archival criteria — ALL must hold (conservative by design):
 *   1. status is terminal: superseded / cancelled / passed / failed / rejected
 *   2. not referenced by any execution in executions.json (history/UI joins)
 *   3. not the max-plan_revision package of its (epic_id, step_id) — the
 *      current working revision stays put for diagnostics
 *   4. sealed_at older than --min-age-days (default 30)
 *
 * Crash safety: the archive file is written BEFORE packages.json is trimmed.
 * A crash between the two leaves duplicated entries; the next run re-merges
 * idempotently. The reverse order could lose data and is never used.
 *
 * CLI: node dist/archive-packages.js [--dry-run] [--min-age-days N]
 * Prints one line of JSON: {archived, kept, archiveTotal, dryRun}.
 */
import { promises as fs } from "node:fs";
import path from "node:path";
import { withFileLock } from "./file-lock.js";
import { readPackagesSafe, writePackages, packagesPath, type PackagesFile } from "./runtime-store.js";
import type { TaskPackage } from "./task-package.js";

const TERMINAL_STATUSES = new Set(["superseded", "cancelled", "passed", "failed", "rejected"]);

export interface ArchiveOptions {
  dryRun?: boolean;
  minAgeDays?: number;
}

export interface ArchiveResult {
  archived: number;
  kept: number;
  archiveTotal: number;
  dryRun: boolean;
}

/** Pure partition — exported for tests. */
export function partitionForArchive(
  packages: TaskPackage[],
  referencedIds: Set<string>,
  nowMs: number,
  minAgeDays: number,
): { keep: TaskPackage[]; archive: TaskPackage[] } {
  const cutoff = nowMs - minAgeDays * 86_400_000;
  // Max plan_revision per (epic_id, step_id) — the working revision stays.
  const latestRev = new Map<string, number>();
  for (const p of packages) {
    const key = `${p.epic_id}/${p.step_id}`;
    latestRev.set(key, Math.max(latestRev.get(key) ?? 0, p.plan_revision ?? 0));
  }
  const keep: TaskPackage[] = [];
  const archive: TaskPackage[] = [];
  for (const p of packages) {
    const sealed = toMs(p.sealed_at);
    const isLatest = p.plan_revision === latestRev.get(`${p.epic_id}/${p.step_id}`);
    if (
      TERMINAL_STATUSES.has(p.status) &&
      !referencedIds.has(p.package_id) &&
      !isLatest &&
      sealed < cutoff &&
      // WARN-1: rows carrying origin.task_key are the todo-producer's
      // dedup backstop when the ledger is lost — they stay in the live file.
      !((p as { origin?: { task_key?: string } }).origin?.task_key)
    ) {
      archive.push(p);
    } else {
      keep.push(p);
    }
  }
  return { keep, archive };
}

function toMs(v: string | number | undefined): number {
  if (typeof v === "number") return v < 1e12 ? v * 1000 : v;
  if (typeof v === "string" && v) {
    const t = Date.parse(v);
    if (!Number.isNaN(t)) return t;
  }
  return Number.MAX_SAFE_INTEGER; // unknown age → treat as fresh, never archive
}

function archivePath(): string {
  const runtime = process.env.QUESTLOG_RUNTIME ?? path.join(process.env.HOME ?? ".", "questlog-runtime");
  return path.join(runtime, "archive", "packages-archive.json");
}

async function readExecutionsReferences(): Promise<Set<string>> {
  const runtime = process.env.QUESTLOG_RUNTIME ?? path.join(process.env.HOME ?? ".", "questlog-runtime");
  const refs = new Set<string>();
  const raw = await fs.readFile(path.join(runtime, "executions.json"), "utf-8").catch(() => null);
  if (!raw) return refs;
  try {
    const d = JSON.parse(raw);
    const list = Array.isArray(d) ? d : (d.executions ?? []);
    for (const e of list) {
      if (e && typeof e.package_id === "string") refs.add(e.package_id);
    }
  } catch {
    // Unreadable executions — fail-safe: keep everything referenced-or-not is
    // decided by the caller; an empty set here would over-archive, so bail
    // out loudly instead by throwing (the lock section catches it).
    throw new Error("executions.json unreadable — refusing to archive (cannot compute reference set)");
  }
  return refs;
}

export async function archivePackages(opts: ArchiveOptions = {}): Promise<ArchiveResult> {
  const minAgeDays = opts.minAgeDays ?? 30;
  const dryRun = opts.dryRun ?? false;
  const filePath = packagesPath();

  return withFileLock(
    filePath,
    async (): Promise<ArchiveResult> => {
      const result = await readPackagesSafe();
      if (!("ok" in result)) {
        throw new Error("error" in result ? result.error : "packages.json not found");
      }
      const file: PackagesFile = result.data;
      const referenced = await readExecutionsReferences();
      const { keep, archive } = partitionForArchive(file.packages, referenced, Date.now(), minAgeDays);

      if (archive.length === 0) {
        return { archived: 0, kept: keep.length, archiveTotal: await countArchive(), dryRun };
      }
      if (dryRun) {
        return { archived: archive.length, kept: keep.length, archiveTotal: await countArchive(), dryRun };
      }

      // 1) archive first (crash between steps only duplicates; next run merges)
      //    BLOCK-1 hardening: only ENOENT may initialize an empty archive —
      //    any read error, parse error, or wrong shape fails LOUD so an
      //    unreadable archive is never overwritten (its rename would destroy
      //    the only copy of already-archived packages).
      const aPath = archivePath();
      await fs.mkdir(path.dirname(aPath), { recursive: true });
      const existing: PackagesFile = await readArchiveStrict(aPath);
      const merged = new Map<string, TaskPackage>();
      for (const p of existing.packages ?? []) merged.set(p.package_id, p);
      for (const p of archive) {
        const prev = merged.get(p.package_id);
        if (!prev || (p.plan_revision ?? 0) >= (prev.plan_revision ?? 0)) merged.set(p.package_id, p);
      }
      const archiveFile: PackagesFile = {
        version: 1,
        packages: [...merged.values()],
        revision: (existing.revision ?? 0) + 1,
      };
      const tmpA = `${aPath}.tmp-${process.pid}`;
      // fsync before rename: the archive is (post-trim) the only live copy —
      // a rename that outlives its data on power loss must not be possible.
      const fh = await fs.open(tmpA, "w");
      try {
        await fh.writeFile(JSON.stringify(archiveFile, null, 2), "utf-8");
        await fh.sync();
      } finally {
        await fh.close();
      }
      await fs.rename(tmpA, aPath);

      // 2) then trim the live file (writePackages bumps revision, tmp+rename)
      file.packages = keep;
      await writePackages(file);

      return { archived: archive.length, kept: keep.length, archiveTotal: archiveFile.packages.length, dryRun };
    },
    10_000,
  );
}

async function countArchive(): Promise<number> {
  try {
    return (await readArchiveStrict(archivePath())).packages?.length ?? 0;
  } catch (err) {
    if ((err as NodeJS.ErrnoException).code === "ENOENT") return 0;
    return -1; // surfaced as -1 in dry-run output; the real run throws
  }
}

/** Archive read with BLOCK-1 semantics: ENOENT → empty; anything else → throw. */
async function readArchiveStrict(aPath: string): Promise<PackagesFile> {
  let raw: string;
  try {
    raw = await fs.readFile(aPath, "utf-8");
  } catch (err) {
    if ((err as NodeJS.ErrnoException).code === "ENOENT") {
      return { version: 1, packages: [] };
    }
    throw new Error(`packages-archive.json unreadable — refusing to archive: ${(err as Error).message}`);
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch (err) {
    throw new Error(`packages-archive.json corrupt — refusing to archive: ${(err as Error).message}`);
  }
  const shape = parsed as { packages?: unknown };
  if (!shape || typeof shape !== "object" || !Array.isArray(shape.packages)) {
    throw new Error("packages-archive.json wrong shape (packages[] missing) — refusing to archive");
  }
  return parsed as PackagesFile;
}

// ─── CLI ───
async function main(): Promise<number> {
  const args = process.argv.slice(2);
  const dryRun = args.includes("--dry-run");
  const minAgeIdx = args.indexOf("--min-age-days");
  const minAgeDays = minAgeIdx >= 0 ? Number(args[minAgeIdx + 1]) || 30 : 30;
  try {
    const res = await archivePackages({ dryRun, minAgeDays });
    console.log(JSON.stringify(res));
    return 0;
  } catch (err) {
    console.log(JSON.stringify({ error: err instanceof Error ? err.message : String(err) }));
    return 1;
  }
}

if (process.argv[1] && process.argv[1].endsWith("archive-packages.js")) {
  main().then((code) => process.exit(code));
}
