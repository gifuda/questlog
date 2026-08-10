#!/usr/bin/env node
/**
 * capture-progress.mjs — 桌面清单贴 Stop Hook
 *
 * 职责：捕捉"对话中明确提到工作已完成，但没调用 update_step 登记进度"
 * 的漏洞，阻止对话结束并提醒 Claude 补登记。
 *
 * 检测三类（见 hooks/lib/progress-scan.mjs）：
 *   1. 已有步骤：提到完成但未 update_step → 提醒推进阶段
 *   2. 方案无步骤（steps=0）但对话提到完成信号 → 提醒先 push_step 建立步骤
 *      （HC-04 模式：施工全完成但面板从未有登记对象）
 *   3. （预留）会话创建了方案但从未建步骤/推进
 *
 * 设计原则：
 * 1. 进度是显式登记的（Claude 调 update_step），不是关键词猜测的
 * 2. Hook 只做兜底——发现"说了完成但没登记"才提醒
 * 3. 每个步骤每会话只提醒一次（marker 文件），避免无限循环
 * 4. 用户明确说"不用记/别登记"时跳过
 * 5. 任何异常都放行（exit 0），绝不阻塞用户的正常对话
 */
import { promises as fs } from "node:fs";
import os from "node:os";
import path from "node:path";
import {
  scanForUnregisteredProgress,
  STAGE_LABEL,
  stageClaim,
} from "./lib/progress-scan.mjs";

const REGISTRY = path.join(os.homedir(), ".todo-board.json");
const STATE_DIR = path.join(os.homedir(), ".questlog-hook");

function loadJsonSafe(p, fallback) {
  return fs.readFile(p, "utf-8").then((raw) => {
    try { return JSON.parse(raw); } catch { return fallback; }
  }).catch(() => fallback);
}

async function readEpicsFile(projectPath) {
  return loadJsonSafe(path.join(projectPath, ".epics.json"), null);
}

async function main() {
  let input = "";
  process.stdin.on("data", (d) => (input += d));
  process.stdin.on("end", async () => run(JSON.parse(input || "{}")));
}

async function run(meta) {
  try {
    const sessionId = meta.session_id || "unknown";
    const transcriptPath = meta.transcript_path;
    if (!transcriptPath) return;

    const transcript = await fs.readFile(transcriptPath, "utf-8");
    const lines = transcript.trim().split("\n").filter(Boolean).slice(-80);

    const registry = await loadJsonSafe(REGISTRY, { projects: [] });
    const candidates = await scanForUnregisteredProgress({
      transcriptLines: lines,
      registry,
      readEpicsFile,
    });
    if (candidates.length === 0) return;

    // marker：每个步骤/方案每会话只提醒一次
    await fs.mkdir(STATE_DIR, { recursive: true });
    const stateFile = path.join(STATE_DIR, `${sessionId}.json`);
    const state = await loadJsonSafe(stateFile, { reminded: [] });
    const keyOf = (c) => (c.kind === "advance" ? `step:${c.stepId}` : `epic:${c.epicId}`);
    const fresh = candidates.filter((c) => !state.reminded.includes(keyOf(c)));
    if (fresh.length === 0) return;

    // 清理 7 天前的 marker
    const oldFiles = await fs.readdir(STATE_DIR).catch(() => []);
    const weekAgo = Date.now() - 7 * 24 * 3600 * 1000;
    for (const f of oldFiles) {
      const p = path.join(STATE_DIR, f);
      const st = await fs.stat(p).catch(() => null);
      if (st && st.mtimeMs < weekAgo) await fs.rm(p, { force: true });
    }

    state.reminded.push(...fresh.map(keyOf));
    await fs.writeFile(stateFile, JSON.stringify(state, null, 2));

    // block：阻止结束，提醒 Claude 补登记
    const linesOut = fresh.map((c) => {
      if (c.kind === "advance") {
        return `- 「${c.stepTitle}」（属于 ${c.epicTitle}）当前 ${STAGE_LABEL[c.fromStage]}，对话中提到已${stageClaim(c.fromStage)}，但未调用 update_step 登记。`;
      }
      if (c.kind === "no-steps") {
        return `- 方案「${c.epicTitle}」在面板中没有任何步骤（steps=0），但对话中提到工作已完成。请先 push_step 建立步骤并登记各阶段进度——否则面板将永远落后于实际施工。`;
      }
      return `- 方案「${c.epicTitle}」相关登记缺失。`;
    });
    const reason = [
      `检测到以下方案进度未登记：`,
      ...linesOut,
      ``,
      `请逐一确认：若确实完成，调用 update_step 推进阶段（如 update_step({id, stage:'${fresh[0].kind === "advance" ? fresh[0].toStage : "coded"}'})），进度条会自动重算；若方案无步骤，先 push_step 建立步骤再登记。若尚未真正完成，忽略此提醒。`,
      `（每项只提醒一次）`,
    ].join("\n");

    process.stdout.write(JSON.stringify({ decision: "block", reason }));
  } catch {
    // 任何异常 → 放行
  }
}

main();
