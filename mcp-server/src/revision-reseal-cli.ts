/**
 * revision-reseal-cli — Rust 侧 revise_scope 决议的后台桥（fire-and-forget）。
 *   node dist/revision-reseal-cli.js <execution_id>
 * 输出单行 JSON {ok, note, package_id?}；exit 0=修订成功，1=拒绝/失败
 * （失败时包保持 blocked 等人工——fail-open 回退旧行为）。
 */
import { autoResealRevision } from "./scope-revise.js";

async function main(): Promise<number> {
  const executionId = process.argv[2];
  if (!executionId) {
    process.stdout.write(JSON.stringify({ ok: false, note: "usage: revision-reseal-cli.js <execution_id>" }));
    return 1;
  }
  const result = await autoResealRevision(executionId);
  process.stdout.write(JSON.stringify(result));
  return result.ok ? 0 : 1;
}

main()
  .then((code) => process.exit(code))
  .catch((err) => {
    process.stdout.write(JSON.stringify({ ok: false, note: `internal: ${err.message}` }));
    process.exit(1);
  });
