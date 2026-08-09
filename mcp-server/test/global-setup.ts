import { promises as fs } from "node:fs";
import os from "node:os";
import path from "node:path";

/**
 * Global test setup: redirect the MCP registry to an isolated temp file so
 * tests NEVER write to the user's real ~/.todo-board.json.
 *
 * store.ts reads process.env.QUESTLOG_REGISTRY on every registry access
 * (see registryPath()). Setting it here, before any test module loads,
 * guarantees isolation regardless of vi.mock timing — fixing a leak where
 * test temp-dirs (questlog-integrity-*) were being registered as real
 * projects in the user's board.
 */
export async function setup(): Promise<void> {
  const tmpDir = await fs.mkdtemp(path.join(os.tmpdir(), "questlog-test-registry-"));
  const registryPath = path.join(tmpDir, ".todo-board.json");
  // Seed an empty registry so the first read doesn't create noise.
  await fs.writeFile(registryPath, JSON.stringify({ projects: [], feedback: {} }, null, 2), "utf-8");
  process.env.QUESTLOG_REGISTRY = registryPath;
}

export async function teardown(): Promise<void> {
  const registryPath = process.env.QUESTLOG_REGISTRY;
  if (registryPath) {
    await fs.rm(path.dirname(registryPath), { recursive: true, force: true }).catch(() => {});
  }
}
