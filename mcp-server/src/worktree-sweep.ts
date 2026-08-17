/**
 * worktree-sweep — bounded governance for QuestLog's historical worktrees
 * (epic_55f92675 S7).
 *
 * DRY-RUN BY DEFAULT: the tool classifies candidates and reports; only an
 * explicit `apply: true` mutates anything, and even then three guards must
 * pass first (master switch off, executions.json lock held with an in-lock
 * re-classification, git-native removal).
 *
 * Classification verdicts (fail-open — anything not provably safe is kept):
 *   reclaimable          — QuestLog-owned, terminal, unreferenced, no
 *                          preserved-scene obligations, no unmerged candidate
 *                          lineage, and past the TTL on BOTH the directory
 *                          mtime and the branch tip date.
 *   keep_active          — referenced by a queued/running/candidate/
 *                          verifying execution.
 *   keep_evidence        — referenced by an execution with
 *                          scene_preserved=false (the worktree IS the
 *                          remaining evidence).
 *   keep_unmerged_candidate — an execution's candidate_sha is not an
 *                          ancestor of HEAD; deleting the branch would break
 *                          the PredecessorCandidate lineage.
 *   keep_fresh           — activity within the TTL.
 *   unknown              — not provably QuestLog-owned (e.g. the codex dev
 *                          worktree) or classification failed → keep.
 */

import { promises as fs } from "node:fs";
import path from "node:path";
import os from "node:os";
import { execFile, execFileSync } from "node:child_process";
import { withFileLock } from "./file-lock.js";

/** The Runner's own namespace: branch_name = `questlog/<exec_id>` is BOTH the
 *  branch and the directory name under the worktree base — so the worktree
 *  LEAF is two levels deep: <base>/questlog/exec_<uuid>/. */
const WORKTREE_BASE = path.join(os.tmpdir(), "questlog-worktrees");
const WORKTREE_LEAF_GLOB_DIR = path.join(WORKTREE_BASE, "questlog");
const BRANCH_PREFIX = "questlog/";
/** Branch names that are worktree leaves: questlog/exec_<uuid>. */
const BRANCH_RE = /^questlog\/exec_[0-9a-f-]+$/;

const ACTIVE_STATUSES = new Set(["queued", "running", "candidate", "verifying"]);

/** Node-side default is 5s; the Rust side holds the executions lock for up
 *  to 10s — match it so a long Runner write doesn't fail the sweep. */
const LOCK_TIMEOUT_MS = 10_000;

export const DEFAULT_TTL_DAYS = 7;

export interface SweepExecutionRow {
  id: string;
  package_id: string;
  status: string;
  worktree_path: string | null;
  branch: string | null;
  candidate_sha: string | null;
  updated_at: number;
  scene_preserved?: boolean | null;
}

export type Verdict =
  | "reclaimable"
  | "keep_active"
  | "keep_evidence"
  | "keep_unmerged_candidate"
  | "keep_fresh"
  | "unknown";

export interface SweepTarget {
  /** `questlog/exec_<uuid>` — the unit of governance. */
  branch: string;
  /** Worktree leaf directory, when it still exists on disk. */
  dir: string | null;
  /** Directory mtime (epoch seconds). */
  dirMtime: number | null;
  /** Branch tip commit date (epoch seconds). */
  tipDate: number | null;
}

export interface SweepClassification {
  target: SweepTarget;
  verdict: Verdict;
  reason: string;
  referencedBy: string[];
  /** Candidate SHAs anchoring the keep_unmerged_candidate verdict. */
  unmergedCandidates: string[];
}

export interface SweepInput {
  targets: SweepTarget[];
  executions: SweepExecutionRow[];
  /** Pre-computed: is the given SHA an ancestor of HEAD? */
  isAncestorOfHead: (sha: string) => boolean;
  now: number;
  ttlSeconds: number;
}

/** Pure classifier — fully unit-testable. */
export function classifyTargets(input: SweepInput): SweepClassification[] {
  return input.targets.map((target) =>
    classifyOne(target, input.executions, input.isAncestorOfHead, input.now, input.ttlSeconds),
  );
}

function classifyOne(
  target: SweepTarget,
  executions: SweepExecutionRow[],
  isAncestorOfHead: (sha: string) => boolean,
  now: number,
  ttlSeconds: number,
): SweepClassification {
  if (!BRANCH_RE.test(target.branch)) {
    return {
      target,
      verdict: "unknown",
      reason: `branch name '${target.branch}' is not a QuestLog execution worktree (questlog/exec_*)`,
      referencedBy: [],
      unmergedCandidates: [],
    };
  }

  const dirName = path.basename(target.branch);
  const references = executions.filter(
    (e) =>
      (e.branch ?? null) === target.branch ||
      (e.worktree_path !== null && path.basename(e.worktree_path) === dirName),
  );

  const active = references.filter((e) => ACTIVE_STATUSES.has(e.status));
  if (active.length > 0) {
    return {
      target,
      verdict: "keep_active",
      reason: `referenced by in-flight execution(s) ${active.map((e) => e.id).join(", ")}`,
      referencedBy: active.map((e) => e.id),
      unmergedCandidates: [],
    };
  }

  const evidence = references.filter((e) => e.scene_preserved === false);
  if (evidence.length > 0) {
    return {
      target,
      verdict: "keep_evidence",
      reason: `worktree is the remaining evidence for scene-incomplete execution(s) ${evidence.map((e) => e.id).join(", ")}`,
      referencedBy: evidence.map((e) => e.id),
      unmergedCandidates: [],
    };
  }

  const unmerged = references.filter((e) => {
    if (!e.candidate_sha) return false;
    return !isAncestorOfHead(e.candidate_sha);
  });
  if (unmerged.length > 0) {
    return {
      target,
      verdict: "keep_unmerged_candidate",
      reason: `carries unmerged candidate lineage: ${unmerged
        .map((e) => `${e.id}@${e.candidate_sha}`)
        .join(", ")} — deleting would break the PredecessorCandidate chain`,
      referencedBy: unmerged.map((e) => e.id),
      unmergedCandidates: unmerged.map((e) => e.candidate_sha ?? ""),
    };
  }

  const lastActivity = Math.max(target.dirMtime ?? 0, target.tipDate ?? 0);
  if (lastActivity === 0) {
    return {
      target,
      verdict: "unknown",
      reason: "no activity signal available (neither directory mtime nor branch tip)",
      referencedBy: [],
      unmergedCandidates: [],
    };
  }
  if (now - lastActivity < ttlSeconds) {
    return {
      target,
      verdict: "keep_fresh",
      reason: `last activity ${new Date(lastActivity * 1000).toISOString()} is within the TTL`,
      referencedBy: [],
      unmergedCandidates: [],
    };
  }

  return {
    target,
    verdict: "reclaimable",
    reason: `terminal, unreferenced, no unmerged candidates, last activity ${new Date(lastActivity * 1000).toISOString()} is past the TTL`,
    referencedBy: [],
    unmergedCandidates: [],
  };
}

// ─── Collection (reads git + filesystem; no mutation) ───

function git(cwd: string, args: string[]): Promise<string> {
  return new Promise((resolve, reject) => {
    execFile("git", args, { cwd }, (err, stdout, stderr) => {
      if (err) reject(new Error(`git ${args.join(" ")}: ${stderr || err.message}`));
      else resolve(stdout);
    });
  });
}

async function collectTargets(repoRoot: string): Promise<SweepTarget[]> {
  const byBranch = new Map<string, SweepTarget>();

  // Branches: questlog/exec_*
  const branchList = await git(repoRoot, [
    "for-each-ref",
    "--format=%(refname:short) %(committerdate:unix)",
    `refs/heads/${BRANCH_PREFIX}*`,
  ]);
  for (const line of branchList.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    const [branch, date] = trimmed.split(" ");
    byBranch.set(branch, {
      branch,
      dir: null,
      dirMtime: null,
      tipDate: date ? Number(date) : null,
    });
  }

  // Worktree leaves: <base>/questlog/exec_*/ (two levels deep — the branch
  // name already carries the questlog/ prefix).
  try {
    const leaves = await fs.readdir(WORKTREE_LEAF_GLOB_DIR);
    for (const leaf of leaves) {
      if (!/^exec_[0-9a-f-]+$/.test(leaf)) continue;
      const branch = `${BRANCH_PREFIX}${leaf}`;
      const dir = path.join(WORKTREE_LEAF_GLOB_DIR, leaf);
      const st = await fs.stat(dir).catch(() => null);
      const existing = byBranch.get(branch);
      if (existing) {
        existing.dir = dir;
        existing.dirMtime = st ? Math.floor(st.mtimeMs / 1000) : null;
      } else {
        byBranch.set(branch, {
          branch,
          dir,
          dirMtime: st ? Math.floor(st.mtimeMs / 1000) : null,
          tipDate: null,
        });
      }
    }
  } catch {
    // Base dir missing entirely — branches only.
  }

  return [...byBranch.values()];
}

// ─── Sweep (dry-run default; apply is triple-guarded) ───

export interface SweepResultItem extends SweepClassification {
  /** apply-mode only: what happened to this target. */
  action?: string;
  actionError?: string;
}

export async function sweepWorktrees(
  repoRoot: string,
  options: { apply?: boolean; ttlDays?: number } = {},
): Promise<
  | {
      mode: "dry-run" | "apply";
      repo_root: string;
      ttl_days: number;
      items: SweepResultItem[];
      summary: Record<Verdict, number> & { removed: number; failed: number };
    }
  | { error: string }
> {
  const ttlDays = options.ttlDays ?? DEFAULT_TTL_DAYS;
  const ttlSeconds = ttlDays * 86400;
  const now = Math.floor(Date.now() / 1000);

  if (options.apply) {
    // Guard 1: the master kill switch must be OFF — sweeping a live Runner
    // races poll_once's create_worktree (remove-then-add) against ours.
    const runtimeDir = process.env.QUESTLOG_RUNTIME ?? path.join(os.homedir(), "questlog-runtime");
    try {
      const config = JSON.parse(
        await fs.readFile(path.join(runtimeDir, "runner-config.json"), "utf-8"),
      ) as { global_enabled?: boolean };
      if (config.global_enabled === true) {
        return {
          error:
            "refusing to apply while the master kill switch (global_enabled) is ON — turn it off first (emergency stop), run the sweep, then re-enable",
        };
      }
    } catch {
      // Missing/corrupt config: fail-closed for apply.
      return { error: "cannot read runner-config.json to verify the kill switch — refusing to apply" };
    }
  }

  // Guard 2: hold the executions lock across classification (and, in apply
  // mode, the removals) so a just-queued execution cannot appear mid-sweep.
  return withFileLock(
    path.join(
      process.env.QUESTLOG_RUNTIME ?? path.join(os.homedir(), "questlog-runtime"),
      "executions.json",
    ),
    async () => {
      const targets = await collectTargets(repoRoot);
      let executions: SweepExecutionRow[] = [];
      try {
        const raw = await fs.readFile(
          path.join(
            process.env.QUESTLOG_RUNTIME ?? path.join(os.homedir(), "questlog-runtime"),
            "executions.json",
          ),
          "utf-8",
        );
        executions = (JSON.parse(raw) as { executions?: SweepExecutionRow[] }).executions ?? [];
      } catch {
        executions = [];
      }

      // Synchronous by design: the classifier is a pure function over the
      // ancestry predicate; a per-call git spawn keeps it honest.
      const isAncestorOfHead = (sha: string): boolean => {
        try {
          execFileSync("git", ["merge-base", "--is-ancestor", sha, "HEAD"], {
            cwd: repoRoot,
            stdio: "pipe",
          });
          return true;
        } catch {
          return false;
        }
      };

      let items: SweepResultItem[] = classifyTargets({
        targets,
        executions,
        isAncestorOfHead,
        now,
        ttlSeconds,
      });

      const summary = {
        reclaimable: 0,
        keep_active: 0,
        keep_evidence: 0,
        keep_unmerged_candidate: 0,
        keep_fresh: 0,
        unknown: 0,
        removed: 0,
        failed: 0,
      } as Record<Verdict, number> & { removed: number; failed: number };
      for (const item of items) summary[item.verdict] += 1;

      if (!options.apply) {
        return { mode: "dry-run" as const, repo_root: repoRoot, ttl_days: ttlDays, items, summary };
      }

      // Guard 3: git-native removal only.
      for (const item of items) {
        if (item.verdict !== "reclaimable") continue;
        try {
          if (item.target.dir) {
            await git(repoRoot, ["worktree", "remove", "--force", item.target.dir]);
            item.action = "worktree removed";
          }
          await git(repoRoot, ["branch", "-D", item.target.branch]);
          item.action = (item.action ? `${item.action}; ` : "") + "branch deleted";
          summary.removed += 1;
        } catch (err) {
          item.actionError = (err as Error).message;
          summary.failed += 1;
        }
      }
      // Clean up any orphaned worktree metadata after the removals.
      await git(repoRoot, ["worktree", "prune"]).catch(() => {});

      return { mode: "apply" as const, repo_root: repoRoot, ttl_days: ttlDays, items, summary };
    },
    LOCK_TIMEOUT_MS,
  ).catch((err: Error) => ({ error: err.message }) as { error: string });
}
