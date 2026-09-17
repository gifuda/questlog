/**
 * Todo-producer CLI (待办租户 G2) — the fixed local entry point the G4
 * scheduler calls. Structured JSON on stdin, JSON on stdout; the todo's free
 * text NEVER appears in argv (plan §4.2: 不把标题/建议拼入 shell).
 *
 *   echo '{"project_dir":"/abs/path","todo_id":"…"}' \
 *     | node <mcp-server>/dist/todo-producer-cli.js
 *
 * Exit codes: 0 = produced or replayed; 1 = refused/failed (JSON on stdout
 * carries reason_code).
 */

import { produceTodoPackage } from "./todo-producer.js";

interface CliInput {
  project_dir: string;
  todo_id: string;
  acceptance_criteria?: string;
  input_scope?: string;
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
    process.stdout.write(JSON.stringify({ ok: false, error: `bad stdin JSON: ${(e as Error).message}`, reason_code: "bad_input" }));
    return 1;
  }
  if (!input?.project_dir || !input?.todo_id) {
    process.stdout.write(
      JSON.stringify({ ok: false, error: "project_dir and todo_id are required", reason_code: "bad_input" }),
    );
    return 1;
  }

  const result = await produceTodoPackage(input.project_dir, input.todo_id, {
    acceptance_criteria: input.acceptance_criteria,
    input_scope: input.input_scope,
  });
  process.stdout.write(JSON.stringify(result));
  return result.ok ? 0 : 1;
}

main()
  .then((code) => process.exit(code))
  .catch((err: Error) => {
    process.stdout.write(JSON.stringify({ ok: false, error: err.message, reason_code: "internal_error" }));
    process.exit(1);
  });
