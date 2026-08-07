#!/usr/bin/env node
/**
 * capture-progress.mjs — 桌面清单贴 Stop Hook
 *
 * 职责：捕捉"对话中明确提到某步骤已完成，但没调用 update_step 登记进度"
 * 的漏洞，阻止对话结束并提醒 Claude 补登记。
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

const REGISTRY = path.join(os.homedir(), ".todo-board.json");
const STATE_DIR = path.join(os.homedir(), ".questlog-hook");

// 每个阶段对应的"完成信号"（检测到这些词 → 该步骤可能已推进）
const STAGE_SIGNALS = {
  pending: [/设计(完成|定稿|好了)|方案(定|确定|成型)/],
  designed: [/代码(完成|写完)|实现(完成|好了|落地)|改完|写完了|开发完成/],
  coded: [/验证通过|测试通过|跑通|通过(验证|测试)|verified|passed/],
  verified: [/观察(通过|期(过|满))|稳定(运行|了)|没问题|闭环|确认(稳定|无问题)/],
};

const SKIP_SIGNALS = [/不用(记|推|登记)|别(记|推|登记)|不(用|要)记录|不登记|不用了/];

// 每阶段推进到的下一阶段
const NEXT_STAGE = {
  pending: "designed",
  designed: "coded",
  coded: "verified",
  verified: "observed",
};

const STAGE_PCT = { pending: 0, designed: 25, coded: 50, verified: 75, observed: 100 };
const STAGE_LABEL = { pending: "未启动", designed: "已设计", coded: "已实施", verified: "已验证", observed: "已观察" };

function loadJsonSafe(p, fallback) {
  return fs.readFile(p, "utf-8").then((raw) => {
    try { return JSON.parse(raw); } catch { return fallback; }
  }).catch(() => fallback);
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
    const lines = transcript.trim().split("\n").filter(Boolean);
    const recent = lines.slice(-80); // 只看最近 80 条，避免历史噪音

    // 1. 收集本对话调用过 update_step 的步骤 id
    const updatedIds = new Set();
    // 2. 收集所有文本（user + assistant），并检测显式跳过信号
    const texts = [];
    let skipAll = false;
    for (const line of recent) {
      try {
        const msg = JSON.parse(line);
        const content = msg?.message?.content;
        if (Array.isArray(content)) {
          for (const block of content) {
            if (block.type === "tool_use" && block.name === "update_step" && block.input?.id) {
              updatedIds.add(block.input.id);
            }
            if (block.type === "text" && block.text) texts.push(block.text);
          }
        }
        // 用户消息可能是字符串
        if (typeof content === "string") texts.push(content);
      } catch { /* skip malformed line */ }
    }
    if (texts.some((t) => SKIP_SIGNALS.some((re) => re.test(t)))) return;

    const recentText = texts.join("\n");

    // 3. 遍历所有注册项目，读 .epics.json
    const registry = await loadJsonSafe(REGISTRY, { projects: [] });
    const candidates = [];

    for (const entry of registry.projects || []) {
      const epicsFile = path.join(entry.path, ".epics.json");
      const data = await loadJsonSafe(epicsFile, null);
      if (!data?.epics?.length || !data?.steps?.length) continue;

      const epicsById = new Map(data.epics.map((e) => [e.id, e]));
      for (const step of data.steps) {
        if (step.stage === "observed") continue; // 已闭环，无需推进
        if (updatedIds.has(step.id)) continue;   // 已登记过
        if (!step.title) continue;

        // 步骤标题出现在对话里？
        if (!recentText.includes(step.title)) continue;

        // 且该阶段的完成信号出现？
        const signals = STAGE_SIGNALS[step.stage] || [];
        const hit = signals.some((re) => re.test(recentText));
        if (!hit) continue;

        const epic = epicsById.get(step.epic_id);
        candidates.push({
          stepId: step.id,
          stepTitle: step.title,
          epicTitle: epic?.title || step.epic_id,
          fromStage: step.stage,
          toStage: NEXT_STAGE[step.stage] || "observed",
          progress: STAGE_PCT[step.stage],
        });
      }
    }

    if (candidates.length === 0) return;

    // 4. marker：每个步骤每会话只提醒一次
    await fs.mkdir(STATE_DIR, { recursive: true });
    const stateFile = path.join(STATE_DIR, `${sessionId}.json`);
    const state = await loadJsonSafe(stateFile, { reminded: [] });
    const fresh = candidates.filter((c) => !state.reminded.includes(c.stepId));
    if (fresh.length === 0) return;

    // 清理 7 天前的 marker
    const oldFiles = await fs.readdir(STATE_DIR).catch(() => []);
    const weekAgo = Date.now() - 7 * 24 * 3600 * 1000;
    for (const f of oldFiles) {
      const p = path.join(STATE_DIR, f);
      const st = await fs.stat(p).catch(() => null);
      if (st && st.mtimeMs < weekAgo) await fs.rm(p, { force: true });
    }

    state.reminded.push(...fresh.map((c) => c.stepId));
    await fs.writeFile(stateFile, JSON.stringify(state, null, 2));

    // 5. block：阻止结束，提醒 Claude 补登记
    const linesOut = fresh.map((c) => {
      return `- 「${c.stepTitle}」（属于 ${c.epicTitle}）当前 ${STAGE_LABEL[c.fromStage]}，对话中提到已${c.fromStage === "pending" ? "设计完成" : c.fromStage === "designed" ? "实施完成" : c.fromStage === "coded" ? "验证通过" : "稳定闭环"}，但未调用 update_step 登记。`;
    });
    const reason = [
      `检测到以下方案步骤已完成但未登记进度：`,
      ...linesOut,
      ``,
      `请逐一确认：若确实完成，调用 update_step 推进阶段（如 update_step({id, stage:'${fresh[0].toStage}'})），进度条会自动重算；若尚未真正完成，忽略此提醒。`,
      `（每个步骤只提醒一次）`,
    ].join("\n");

    process.stdout.write(JSON.stringify({ decision: "block", reason }));
  } catch {
    // 任何异常 → 放行
  }
}

main();
