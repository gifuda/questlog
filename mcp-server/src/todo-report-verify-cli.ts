/**
 * Todo-report verifier CLI (待办租户 G3) — the entry point the G4
 * receipt-reconciliation calls on the MANAGER. Structured JSON on stdin,
 * verdict JSON on stdout. Reads the report text and context from stdin only;
 * never executes anything from the report.
 *
 *   {"stdout":"<raw executor stdout>","task_key":"…","snapshot_files":["src/a.rs",…],
 *    "exit_ok":true,"timed_out":false,"budget_exceeded":false,"changes_detected":false}
 *     | node <mcp-server>/dist/todo-report-verify-cli.js
 */

import { verifyTodoReport } from "./todo-report.js";
import { pathToFileURL } from "node:url";

interface CliInput {
  stdout: string;
  task_key: string;
  snapshot_files?: string[];
  exit_ok?: boolean;
  timed_out?: boolean;
  budget_exceeded?: boolean;
  changes_detected?: boolean;
}

async function main(): Promise<number> {
  const chunks: Buffer[] = [];
  for await (const chunk of process.stdin) {
    chunks.push(chunk as Buffer);
  }
  let input: CliInput;
  try {
    input = JSON.parse(Buffer.concat(chunks).toString("utf-8")) as CliInput;
  } catch (e) {
    process.stdout.write(
      JSON.stringify({ outcome: "rejected", reasons: [`bad stdin JSON: ${(e as Error).message}`] }),
    );
    return 1;
  }
  if (typeof input?.stdout !== "string" || typeof input?.task_key !== "string") {
    process.stdout.write(
      JSON.stringify({ outcome: "rejected", reasons: ["stdout and task_key are required"] }),
    );
    return 1;
  }

  const result = verifyTodoReport({
    stdout: input.stdout,
    task_key: input.task_key,
    snapshot_files: input.snapshot_files ?? [],
    exit_ok: input.exit_ok ?? false,
    timed_out: input.timed_out ?? false,
    budget_exceeded: input.budget_exceeded ?? false,
    changes_detected: input.changes_detected ?? false,
  });
  process.stdout.write(JSON.stringify(result));
  // Exit 0 for every classified verdict (including needs_review/rejected —
  // the verdict IS the result); 1 only for unusable input.
  return 0;
}

// Run only when executed directly — importing this module (e.g. by a test)
// must not block on stdin.
const isDirectRun = process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href;
if (isDirectRun) {
main()
  .then((code) => process.exit(code))
  .catch((err: Error) => {
    process.stdout.write(JSON.stringify({ outcome: "rejected", reasons: [err.message] }));
    process.exit(1);
  });
}

