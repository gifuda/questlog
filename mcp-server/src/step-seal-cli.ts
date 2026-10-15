/**
 * step-seal-cli — app「▶ 启动」的一键封包入口。
 *   echo '{"epic_id":"…","step_id":"…",…}' | node dist/step-seal-cli.js --stdin
 * 或：node dist/step-seal-cli.js '<json>'
 * 输出单行 JSON（package_id / error）。
 */
import { prepareTaskPackage, validateTaskPackage, sealTaskPackage } from "./task-package-ops.js";
import type { PrepareInput } from "./task-package-ops.js";

async function main(): Promise<number> {
  let raw: string;
  if (process.argv[2] === "--stdin") {
    const chunks: Buffer[] = [];
    for await (const c of process.stdin) chunks.push(c as Buffer);
    raw = Buffer.concat(chunks).toString("utf-8");
  } else {
    raw = process.argv[2] ?? "";
  }
  let body: PrepareInput & { project_dir: string };
  try {
    body = JSON.parse(raw);
  } catch (e) {
    process.stdout.write(JSON.stringify({ error: `bad JSON: ${e instanceof Error ? e.message : String(e)}` }));
    return 1;
  }
  const { project_dir, ...input } = body;
  if (!project_dir || !input.step_id || !input.epic_id) {
    process.stdout.write(JSON.stringify({ error: "project_dir, epic_id, step_id are required" }));
    return 1;
  }
  try {
    const prep = await prepareTaskPackage(project_dir, input);
    if ("error" in prep) {
      process.stdout.write(JSON.stringify({ error: `prepare: ${prep.error}` }));
      return 1;
    }
    const val = await validateTaskPackage(project_dir, prep.package_id);
    if ("error" in val) {
      process.stdout.write(JSON.stringify({ error: `validate: ${val.error}` }));
      return 1;
    }
    const errs = val.issues.filter((i: { severity: string }) => i.severity === "error");
    if (errs.length > 0) {
      process.stdout.write(JSON.stringify({ error: `validate: ${errs.map((e: { message: string }) => e.message).join("; ")}` }));
      return 1;
    }
    const sealed = await sealTaskPackage(project_dir, prep.package_id);
    if ("error" in sealed) {
      process.stdout.write(JSON.stringify({ error: `seal: ${sealed.error}` }));
      return 1;
    }
    process.stdout.write(JSON.stringify({ ok: true, package_id: sealed.package_id, plan_revision: sealed.plan_revision ?? 0 }));
    return 0;
  } catch (e) {
    process.stdout.write(JSON.stringify({ error: `pipeline: ${e instanceof Error ? e.message : String(e)}` }));
    return 1;
  }
}

main()
  .then((code) => process.exit(code))
  .catch((err) => {
    process.stdout.write(JSON.stringify({ error: `internal: ${err.message}` }));
    process.exit(1);
  });
