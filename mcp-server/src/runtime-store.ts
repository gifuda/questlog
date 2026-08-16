/**
 * Runtime Store — local-only storage for TaskPackages and execution state.
 *
 * This data lives OUTSIDE the project directory (in the OS app data dir) so:
 * 1. It is never committed to Git (contains absolute paths, CLI sessions).
 * 2. High-frequency execution events don't race with .epics.json edits.
 *
 * The .epics.json (Plan Store) only stores a portable summary: package_id,
 * revision, hash, and objective. The full package with all execution details
 * is stored here.
 *
 * Layout:
 *   <app-data>/questlog-runtime/
 *   ├── packages.json       — all TaskPackages (full content)
 *   └── packages.json.lock  — cross-process lock
 */

import { promises as fs } from "node:fs";
import path from "node:path";
import os from "node:os";
import crypto from "node:crypto";
import { withFileLock } from "./file-lock.js";
import { TaskPackage, PackageSummary, computePackageHash } from "./task-package.js";

const RUNTIME_DIRNAME = "questlog-runtime";
const PACKAGES_FILENAME = "packages.json";

/**
 * Resolve the runtime data directory.
 * Priority: QUESTLOG_RUNTIME env var > OS local app data > home/.questlog-runtime
 */
function runtimeDir(): string {
  if (process.env.QUESTLOG_RUNTIME) return process.env.QUESTLOG_RUNTIME;
  // Use platform-specific local app data
  const home = os.homedir();
  return path.join(home, RUNTIME_DIRNAME);
}

function packagesPath(): string {
  return path.join(runtimeDir(), PACKAGES_FILENAME);
}

interface PackagesFile {
  version: number;
  packages: TaskPackage[];
  revision?: number;
}

async function ensureRuntimeDir(): Promise<void> {
  await fs.mkdir(runtimeDir(), { recursive: true });
}

/**
 * Read all packages from the Runtime Store.
 * Fail-closed: corrupt JSON returns error, not empty.
 */
async function readPackagesSafe(): Promise<
  { ok: true; data: PackagesFile } | { notFound: true } | { error: string }
> {
  const filePath = packagesPath();
  let raw: string;
  try {
    raw = await fs.readFile(filePath, "utf-8");
  } catch (err) {
    const code = (err as NodeJS.ErrnoException).code;
    if (code === "ENOENT") return { notFound: true };
    return { error: `read error (${code}): ${(err as Error).message}` };
  }
  try {
    return { ok: true, data: JSON.parse(raw) as PackagesFile };
  } catch (err) {
    // Backup corrupt file
    const bak = `${filePath}.bak.${Date.now()}`;
    await fs.rename(filePath, bak).catch(() => {});
    return { error: `corrupt packages.json (backed up to ${bak}): ${(err as Error).message}` };
  }
}

async function writePackages(data: PackagesFile): Promise<void> {
  await ensureRuntimeDir();
  const filePath = packagesPath();
  data.revision = (data.revision ?? 0) + 1;
  const tmp = `${filePath}.tmp-${crypto.randomUUID()}`;
  await fs.writeFile(tmp, JSON.stringify(data, null, 2), "utf-8");
  await fs.rename(tmp, filePath);
}

// ─── Package operations ───

/**
 * Save a package to the Runtime Store (upsert by package_id + plan_revision).
 * If a package with the same package_id exists, the new one supersedes it.
 */
export async function savePackage(pkg: TaskPackage): Promise<{ ok: true } | { error: string }> {
  await ensureRuntimeDir();
  const filePath = packagesPath();
  return withFileLock(
    filePath,
    async () => {
      const result = await readPackagesSafe();
      let file: PackagesFile;
      if ("ok" in result) {
        file = result.data;
      } else if ("notFound" in result) {
        file = { version: 1, packages: [], revision: 0 };
      } else {
        return { error: result.error };
      }

      // Upsert: if package_id exists, mark old as superseded
      for (const existing of file.packages) {
        if (existing.package_id === pkg.package_id && existing.plan_revision < pkg.plan_revision) {
          existing.status = "superseded";
        }
      }

      // Remove any existing entry with the same id+revision (replace)
      file.packages = file.packages.filter(
        (p) => !(p.package_id === pkg.package_id && p.plan_revision === pkg.plan_revision),
      );
      file.packages.push(pkg);
      await writePackages(file);
      return { ok: true as const };
    },
    10_000,
  ).catch((err: Error) => ({ error: err.message }));
}

/**
 * Get a specific package by ID. Returns the latest revision.
 */
export async function getPackage(
  packageId: string,
): Promise<{ pkg: TaskPackage } | { error: string }> {
  const result = await readPackagesSafe();
  if ("error" in result) return { error: result.error };
  if ("notFound" in result) return { error: "no packages stored" };
  // Find the latest non-superseded revision of this package
  const candidates = result.data.packages
    .filter((p) => p.package_id === packageId && p.status !== "superseded")
    .sort((a, b) => b.plan_revision - a.plan_revision);
  if (candidates.length === 0) return { error: `package '${packageId}' not found` };
  return { pkg: candidates[0] };
}

/**
 * List all packages for a given step (latest revisions only).
 *
 * When multiple package_ids exist for the same step (e.g. a previous
 * attempt was blocked and a new package was sealed), prefer the one
 * that is actually actionable (sealed/ready) over terminal states
 * (blocked/cancelled/passed/failed). Among equally-actionable packages,
 * the most recently sealed wins.
 */
export async function getPackagesForStep(stepId: string): Promise<TaskPackage[]> {
  const result = await readPackagesSafe();
  if (!("ok" in result)) return [];
  const all = result.data.packages
    .filter((p) => p.step_id === stepId && p.status !== "superseded");
  if (all.length === 0) return [];
  // Sort: actionable (sealed/ready) first, then by sealed_at desc (newest seal wins)
  const actionableStatuses = new Set(["sealed", "ready"]);
  all.sort((a, b) => {
    const aActionable = actionableStatuses.has(a.status) ? 1 : 0;
    const bActionable = actionableStatuses.has(b.status) ? 1 : 0;
    if (aActionable !== bActionable) return bActionable - aActionable;
    // Both same actionability → newest sealed_at wins
    return (b.sealed_at || "").localeCompare(a.sealed_at || "");
  });
  return [all[0]];
}

/**
 * List all non-superseded packages belonging to an epic (any status).
 *
 * FAIL-CLOSED, unlike getPackagesForStep/getReadyPackages: a corrupt or
 * unreadable store returns `{ error }`, NEVER an empty list. Callers use this
 * for rollback (cancel everything this epic created) — treating a corrupt
 * store as "nothing to cancel" would delete the epic while sealed packages
 * still sit in the store.
 */
export async function getPackagesForEpic(
  epicId: string,
): Promise<{ pkgs: TaskPackage[] } | { error: string }> {
  const result = await readPackagesSafe();
  if ("error" in result) return { error: result.error };
  if ("notFound" in result) return { pkgs: [] };
  return {
    pkgs: result.data.packages.filter(
      (p) => p.epic_id === epicId && p.status !== "superseded",
    ),
  };
}

/**
 * Fail-closed readability probe. publishPlan calls this BEFORE creating
 * anything, so a store it could not read (and therefore could not roll back
 * over) is rejected up front instead of failing half-created.
 *
 * Note: readPackagesSafe rotates a corrupt file to a .bak backup, so probing
 * once at the start also guarantees a clean store for the rest of the run.
 */
export async function checkRuntimeStore(): Promise<{ ok: true } | { error: string }> {
  const result = await readPackagesSafe();
  if ("error" in result) return { error: result.error };
  return { ok: true };
}

/**
 * Get packages ready for execution (status = "sealed" or "ready").
 */
export async function getReadyPackages(): Promise<TaskPackage[]> {
  const result = await readPackagesSafe();
  if (!("ok" in result)) return [];
  return result.data.packages.filter((p) => p.status === "sealed" || p.status === "ready");
}

/**
 * Update a package's status (e.g., sealed → ready → cancelled).
 */
export async function updatePackageStatus(
  packageId: string,
  status: TaskPackage["status"],
): Promise<{ ok: true } | { error: string }> {
  await ensureRuntimeDir();
  const filePath = packagesPath();
  return withFileLock(
    filePath,
    async () => {
      const result = await readPackagesSafe();
      if ("error" in result) return { error: result.error };
      if ("notFound" in result) return { error: "no packages stored" };
      const pkg = result.data.packages.find(
        (p) => p.package_id === packageId && p.status !== "superseded",
      );
      if (!pkg) return { error: `package '${packageId}' not found` };
      pkg.status = status;
      await writePackages(result.data);
      return { ok: true as const };
    },
    10_000,
  ).catch((err: Error) => ({ error: err.message }));
}

/**
 * Create a portable summary from a full package (for .epics.json storage).
 */
export function packageSummary(pkg: TaskPackage): PackageSummary {
  return {
    package_id: pkg.package_id,
    plan_revision: pkg.plan_revision,
    package_hash: pkg.package_hash,
    objective: pkg.objective,
    executor: pkg.executor.preferred,
    status: pkg.status,
  };
}
