/**
 * ZCode CLI e2e fixture tests (G3 acceptance).
 *
 * These tests run the REAL ZCode CLI against temporary fixture git projects.
 * Prerequisites: ZCode CLI at /Applications/ZCode.app/Contents/Resources/glm/zcode.cjs
 * and a valid provider config in ~/.zcode/cli/config.json.
 *
 * Two chains:
 * 1. Read-only: ZCode reads a file and reports its content. No worktree write.
 * 2. Write chain: ZCode modifies a file in an isolated worktree, Runner
 *    verifies the diff and creates a candidate commit.
 */

import { describe, it, expect, beforeAll } from "vitest";
import { promises as fs, existsSync } from "node:fs";
import { execSync, execFileSync } from "node:child_process";
import path from "node:path";
import os from "node:os";

const ZCODE_CLI = "/Applications/ZCode.app/Contents/Resources/glm/zcode.cjs";
const NODE_BIN = process.execPath; // absolute path to the running node binary
const ZCODE_CMD = [NODE_BIN, ZCODE_CLI];

function zcodeAvailable(): boolean {
  try {
    if (!existsSync(ZCODE_CLI)) {
      console.log("[probe] CLI file does not exist:", ZCODE_CLI);
      return false;
    }
    const out = execSync(`${NODE_BIN} ${ZCODE_CLI} --version`, { stdio: "pipe", timeout: 5000 });
    console.log("[probe] version output:", out.toString().trim());
    return true;
  } catch (e) {
    console.log("[probe] error:", (e as Error).message.split("\n")[0]);
    return false;
  }
}

const ZCODE_OK = zcodeAvailable();

async function createFixtureRepo(name: string): Promise<string> {
  const dir = await fs.mkdtemp(path.join(os.tmpdir(), `zcode-${name}-`));
  execFileSync("git", ["init"], { cwd: dir, stdio: "pipe" });
  execFileSync("git", ["config", "user.email", "test@test.com"], { cwd: dir, stdio: "pipe" });
  execFileSync("git", ["config", "user.name", "Test"], { cwd: dir, stdio: "pipe" });
  await fs.writeFile(path.join(dir, "README.md"), "# Fixture Project\n\nInitial content.\n");
  await fs.writeFile(path.join(dir, "src.ts"), "export const value = 1;\n");
  execFileSync("git", ["add", "."], { cwd: dir, stdio: "pipe" });
  execFileSync("git", ["commit", "-m", "initial"], { cwd: dir, stdio: "pipe" });
  return dir;
}

describe.skipIf(!ZCODE_OK)("ZCode CLI e2e", () => {
  it(
    "read-only: ZCode reads a file and reports its content",
    { timeout: 120_000 },
    async () => {
      const dir = await createFixtureRepo("readonly");
      try {
        const output = execFileSync(
          ZCODE_CMD[0],
          [...ZCODE_CMD.slice(1),
            "--prompt", "Read src.ts and tell me what number value is set to. Reply with just the number.",
            "--cwd", dir,
            "--mode", "plan"],
          { stdio: ["pipe", "pipe", "pipe"], timeout: 90_000, encoding: "utf-8" },
        );

        // ZCode should respond with the value
        expect(output.trim()).toContain("1");

        // Verify no files were modified
        const status = execFileSync("git", ["status", "--porcelain"], {
          cwd: dir, encoding: "utf-8",
        });
        expect(status.trim()).toBe("");
      } finally {
        await fs.rm(dir, { recursive: true, force: true });
      }
    },
  );

  it(
    "write: ZCode modifies a file in worktree, commit has QuestLog trailers",
    { timeout: 120_000 },
    async () => {
      const dir = await createFixtureRepo("write");
      try {
        const baseSha = execFileSync("git", ["rev-parse", "HEAD"], {
          cwd: dir, encoding: "utf-8",
        }).trim();

        const worktreePath = path.join(os.tmpdir(), `zcode-wt-${Date.now()}`);
        execFileSync("git", ["worktree", "add", "--detach", worktreePath, baseSha], {
          cwd: dir, stdio: "pipe",
        });

        // Run ZCode in the worktree to modify src.ts
        const output = execFileSync(
          ZCODE_CMD[0],
          [...ZCODE_CMD.slice(1),
            "--prompt", "Change the value in src.ts from 1 to 42. Only modify src.ts.",
            "--cwd", worktreePath,
            "--mode", "yolo"],
          { stdio: ["pipe", "pipe", "pipe"], timeout: 90_000, encoding: "utf-8" },
        );

        // Verify src.ts was modified
        const srcContent = await fs.readFile(path.join(worktreePath, "src.ts"), "utf-8");
        expect(srcContent).toContain("42");

        // Check git diff
        const diff = execFileSync("git", ["diff"], {
          cwd: worktreePath, encoding: "utf-8",
        });
        expect(diff).toContain("42");

        // Create candidate commit with QuestLog trailers
        execFileSync("git", ["add", "src.ts"], { cwd: worktreePath, stdio: "pipe" });
        execFileSync("git", ["commit", "-m",
          "QuestLog auto-execution\n\nQuestLog-Epic: test-epic\nQuestLog-Step: test-step\nQuestLog-Package: test-pkg@1\nQuestLog-Execution: test-exec"],
          { cwd: worktreePath, stdio: "pipe" });

        // Verify trailers
        const commitMsg = execFileSync("git", ["log", "-1", "--format=%B"], {
          cwd: worktreePath, encoding: "utf-8",
        });
        expect(commitMsg).toContain("QuestLog-Epic: test-epic");
        expect(commitMsg).toContain("QuestLog-Step: test-step");
        expect(commitMsg).toContain("QuestLog-Package: test-pkg@1");

        // Verify main workspace untouched
        const mainContent = await fs.readFile(path.join(dir, "src.ts"), "utf-8");
        expect(mainContent).toContain("1");
        expect(mainContent).not.toContain("42");

        // Cleanup
        execFileSync("git", ["worktree", "remove", "--force", worktreePath], {
          cwd: dir, stdio: "pipe",
        });
      } finally {
        await fs.rm(dir, { recursive: true, force: true });
      }
    },
  );
});

describe("ZCode probe status", () => {
  it("reports CLI availability", () => {
    console.log(`ZCode CLI: ${ZCODE_OK ? "✓ available" : "✗ not installed"}`);
    if (ZCODE_OK) {
      const version = execSync(`${NODE_BIN} ${ZCODE_CLI} --version`, { encoding: "utf-8" }).trim();
      console.log(`Version: ${version}`);
    }
    expect(typeof ZCODE_OK).toBe("boolean");
  });
});
