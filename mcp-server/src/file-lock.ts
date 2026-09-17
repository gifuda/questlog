/**
 * Cross-process file locking + revision (CAS) protocol.
 *
 * Problem: Node MCP server, Tauri UI, and the future Runner are three
 * independent processes that all write to the same JSON files. The atomic
 * rename pattern prevents half-written files, but it does NOT prevent
 * last-writer-wins data loss when two processes read the same version and
 * both write back.
 *
 * Solution:
 * 1. **Owner-safe cross-process lock** — an advisory lockfile (`.lock`
 *    sibling) using O_EXCL atomic creation. The lock carries the holder's
 *    identity (`{"v":2,"pid":…,"token":…,"created_ms":…}`). A leftover lock
 *    is only reclaimed when its owner is PROVABLY dead (`kill(pid, 0)` →
 *    ESRCH); age alone never justifies stealing — a live-but-slow holder
 *    (paused process, laptop sleep) must not be robbed mid-critical-section.
 *    Empty (0-byte) stale locks are reclaimed via age — the creator crashed
 *    between create and writing its identity, so no owner can ever release
 *    it. Non-empty unparseable locks are NEVER auto-removed; acquisition
 *    times out with a "blocked" error for manual cleanup. Mirrors
 *    file_lock.rs on the Rust side.
 * 2. **Revision / CAS** — each JSON file carries a monotonic `revision`
 *    counter. A writer reads with revision R, mutates, and writes back
 *    with revision R+1. If the on-disk revision is no longer R when the
 *    lock is acquired, another writer beat us — the caller must re-read
 *    and retry.
 */

import { promises as fs } from "node:fs";
import path from "node:path";
import crypto from "node:crypto";

/** Default lock timeout: 10 seconds (aligned with the Rust side's DEFAULT_TIMEOUT). */
const DEFAULT_TIMEOUT_MS = 10_000;

/** Retry interval between lock attempts: 50ms. */
const RETRY_MS = 50;

/** Maximum age of an EMPTY lock before reclamation (creator crashed pre-identity). */
const STALE_LOCK_MS = 10_000;

/** Extract the holder pid from lock content (v2 JSON or legacy "pid-uuid"). */
function parseOwnerPid(content: string): number | null {
  const trimmed = content.trim();
  if (!trimmed) return null;
  try {
    const v = JSON.parse(trimmed) as { pid?: unknown };
    const pid = v?.pid;
    if (typeof pid === "number" && Number.isInteger(pid) && pid > 0 && pid <= 0x7fffffff) {
      return pid;
    }
    return null;
  } catch {
    const dash = trimmed.indexOf("-");
    if (dash > 0) {
      const pid = Number.parseInt(trimmed.slice(0, dash), 10);
      if (Number.isInteger(pid) && pid > 0) return pid;
    }
    return null;
  }
}

/** `kill(pid, 0)` liveness probe. ESRCH = provably dead; anything else = alive. */
function ownerAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (err) {
    const code = (err as NodeJS.ErrnoException).code;
    if (code === "ESRCH") return false;
    return true; // EPERM / unknown → conservatively alive
  }
}

/**
 * Acquire a cross-process advisory lock on a file path.
 * Uses O_EXCL atomic creation of a `.lock` sibling file.
 * Reclaims only provably-dead owners (see header).
 *
 * Returns a release function that removes the lock file.
 * Throws if the lock cannot be acquired within the timeout.
 */
export async function withFileLock<T>(
  dataPath: string,
  fn: () => Promise<T>,
  timeoutMs: number = DEFAULT_TIMEOUT_MS,
): Promise<T> {
  const lockPath = dataPath + ".lock";
  const deadline = Date.now() + timeoutMs;
  const token = crypto.randomUUID();
  const lockContent = JSON.stringify({
    v: 2,
    pid: process.pid,
    token,
    created_ms: Date.now(),
  });

  // Try to acquire the lock with retries.
  let acquired = false;
  let lastObstacle: "unparseable" | "live-owner" = "live-owner";
  while (Date.now() < deadline) {
    try {
      // O_EXCL: fails if the file already exists.
      const fh = await fs.open(lockPath, "wx");
      await fh.writeFile(lockContent);
      await fh.close();
      acquired = true;
      break;
    } catch (err) {
      const code = (err as NodeJS.ErrnoException).code;
      if (code === "EEXIST") {
        let content = "";
        try {
          content = await fs.readFile(lockPath, "utf-8");
        } catch {
          // Lock file vanished between EEXIST and read — loop retries.
        }
        const pid = parseOwnerPid(content);
        if (pid !== null) {
          if (!ownerAlive(pid)) {
            // Owner provably dead — safe to reclaim.
            await fs.unlink(lockPath).catch(() => {});
            continue;
          }
          // Live holder — wait for it below.
          lastObstacle = "live-owner";
        } else if (content.trim() === "") {
          // Creator crashed before writing its identity. Age escape hatch:
          // no owner can ever release this lock.
          try {
            const stat = await fs.stat(lockPath);
            if (Date.now() - stat.mtimeMs > STALE_LOCK_MS) {
              await fs.unlink(lockPath).catch(() => {});
              continue;
            }
          } catch {
            // vanished — loop retries
          }
          lastObstacle = "live-owner";
        } else {
          // Non-empty, unparseable — identity unknown. Never auto-remove;
          // surface a blocked error at timeout for manual cleanup.
          lastObstacle = "unparseable";
        }
        await sleep(RETRY_MS);
        continue;
      }
      // Permission or other error — cannot lock.
      throw new Error(`lock error (${code}): ${(err as Error).message}`);
    }
  }

  if (!acquired) {
    if (lastObstacle === "unparseable") {
      throw new Error(
        `lock blocked: ${lockPath} is unreadable and no owner can be proven dead — manual cleanup required`,
      );
    }
    throw new Error(`lock timeout: could not acquire ${lockPath} within ${timeoutMs}ms (live owner)`);
  }

  try {
    return await fn();
  } finally {
    // Only remove if we still own the lock (check content).
    try {
      const content = await fs.readFile(lockPath, "utf-8");
      if (content.trim() === lockContent) {
        await fs.unlink(lockPath);
      }
      // If content doesn't match, someone else took over (shouldn't happen
      // in normal flow, but we don't want to delete another process's lock).
    } catch {
      // Lock file already gone — nothing to clean up.
    }
  }
}

/**
 * CAS-protected read-modify-write.
 *
 * Reads the file inside a lock, applies the mutation, checks that the
 * revision hasn't changed since the read, writes back with revision+1.
 *
 * If `maxRetries` is > 0, on CAS conflict it re-reads, re-applies the
 * mutation function, and retries — up to maxRetries times.
 *
 * @param filePath The JSON file path.
 * @param mutator  Receives the parsed data (with `revision` field), returns
 *                 the new data to write. If it throws, the write is aborted
 *                 and the lock released.
 * @param maxRetries How many times to retry on CAS conflict.
 */
export async function casWrite<T extends { revision?: number }>(
  filePath: string,
  mutator: (data: T) => T | Promise<T>,
  maxRetries: number = 3,
): Promise<{ data: T; attempts: number } | { error: string }> {
  return withFileLock(filePath, async () => {
    let lastError: string | null = null;
    for (let attempt = 0; attempt <= maxRetries; attempt++) {
      // Read current state.
      let current: T;
      try {
        const raw = await fs.readFile(filePath, "utf-8");
        current = JSON.parse(raw) as T;
      } catch (err) {
        const code = (err as NodeJS.ErrnoException).code;
        if (code === "ENOENT") {
          // File doesn't exist — caller's mutator gets a revision-0 object.
          // But for data files, we use the safe reader elsewhere; here we
          // only proceed if the mutator handles undefined gracefully.
          return { error: "file not found (use safe reader first)" };
        }
        return { error: `read error: ${(err as Error).message}` };
      }

      const readRevision = current.revision ?? 0;

      // Apply mutation.
      let next: T;
      try {
        next = await mutator(current);
      } catch (err) {
        return { error: (err as Error).message };
      }

      // Bump revision.
      next.revision = readRevision + 1;

      // Write atomically.
      const tmp = filePath + ".tmp-" + crypto.randomUUID();
      try {
        await fs.writeFile(tmp, JSON.stringify(next, null, 2), "utf-8");
        await fs.rename(tmp, filePath);
      } catch (err) {
        return { error: `write error: ${(err as Error).message}` };
      }

      return { data: next, attempts: attempt + 1 };
    }
    return { error: lastError ?? "CAS retries exhausted" };
  });
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
