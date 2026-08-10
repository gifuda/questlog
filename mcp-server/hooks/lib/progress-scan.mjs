#!/usr/bin/env node
/**
 * progress-scan.mjs — pure scan logic for the Stop hook (capture-progress.mjs).
 *
 * Extracted so the hook's decision logic is unit-testable without stdin/fs.
 * The hook entry point reads the transcript + registry and delegates here.
 *
 * What it detects:
 *   1. Existing steps whose title appears in the transcript WITH a completion
 *      signal, but update_step was not called for them → "advance the stage".
 *   2. An epic whose title appears with completion signals but the epic has NO
 *      steps at all (steps=0) → "register steps first" (the HC-04 failure mode:
 *      work finished, board never had objects to register against).
 *   3. Steps created via push_step this session but never updated at all.
 */

// Completion signals per stage (same as before).
export const STAGE_SIGNALS = {
  pending: [/设计(完成|定稿|好了)|方案(定|确定|成型)/],
  designed: [/代码(完成|写完)|实现(完成|好了|落地)|改完|写完了|开发完成/],
  coded: [/验证通过|测试通过|跑通|通过(验证|测试)|verified|passed/],
  verified: [/观察(通过|期(过|满))|稳定(运行|了)|没问题|闭环|确认(稳定|无问题)/],
};

// "Do not register" skip signals.
export const SKIP_SIGNALS = [/不用(记|推|登记)|别(记|推|登记)|不(用|要)记录|不登记|不用了/];

export const NEXT_STAGE = {
  pending: "designed",
  designed: "coded",
  coded: "verified",
  verified: "observed",
};

export const STAGE_LABEL = {
  pending: "未启动",
  designed: "已设计",
  coded: "已实施",
  verified: "已验证",
  observed: "已观察",
};

/** Stage completion signal regex for a given stage. */
export function stageSignals(stage) {
  return STAGE_SIGNALS[stage] || [];
}

/** Stage → what the user claims happened (for human-readable reasons). */
export function stageClaim(stage) {
  switch (stage) {
    case "pending": return "设计完成";
    case "designed": return "实施完成";
    case "coded": return "验证通过";
    case "verified": return "稳定闭环";
    default: return "完成";
  }
}

/**
 * Collect tool calls + text from a transcript (array of JSON lines).
 * Returns { updatedIds, createdStepIds, createdEpicIds, texts }.
 */
export function parseTranscript(lines) {
  const updatedIds = new Set();    // update_step ids called this session
  const createdStepIds = new Set(); // push_step ids created this session
  const createdEpicTitles = [];     // push_epic titles created this session
  const texts = [];

  for (const line of lines) {
    try {
      const msg = JSON.parse(line);
      const content = msg?.message?.content;
      if (Array.isArray(content)) {
        for (const block of content) {
          if (block.type === "tool_use") {
            if (block.name === "update_step" && block.input?.id) updatedIds.add(block.input.id);
            if (block.name === "push_step" && block.input?.epic_id) createdStepIds.add(block.input.epic_id);
            if (block.name === "push_epic" && block.input?.title) createdEpicTitles.push(block.input.title);
          }
          if (block.type === "text" && block.text) texts.push(block.text);
        }
      }
      if (typeof content === "string") texts.push(content);
    } catch { /* skip malformed line */ }
  }
  return { updatedIds, createdStepIds, createdEpicTitles, texts };
}

/**
 * Core scan. `registry` is the parsed ~/.todo-board.json ({ projects: [...] }),
 * each project entry having { name, path }. `readEpicsFile(path)` returns the
 * parsed .epics.json or null (injected so tests can use in-memory data).
 * Returns an array of candidate reminders:
 *   { kind: "advance", stepId, stepTitle, epicTitle, fromStage, toStage }
 *   { kind: "no-steps", epicId, epicTitle }
 *   { kind: "created-never-updated", epicId, epicTitle }
 */
export async function scanForUnregisteredProgress({ transcriptLines, registry, readEpicsFile }) {
  const { updatedIds, createdStepIds, createdEpicTitles, texts } = parseTranscript(transcriptLines);
  if (texts.some((t) => SKIP_SIGNALS.some((re) => re.test(t)))) return [];

  const recentText = texts.join("\n");
  const candidates = [];

  for (const entry of registry.projects || []) {
    const data = await readEpicsFile(entry.path);
    if (!data || !Array.isArray(data.epics)) continue;

    const steps = Array.isArray(data.steps) ? data.steps : [];
    const epicsById = new Map(data.epics.map((e) => [e.id, e]));
    const stepsByEpic = new Map();
    for (const step of steps) {
      const list = stepsByEpic.get(step.epic_id) || [];
      list.push(step);
      stepsByEpic.set(step.epic_id, list);
    }

    // ── Case 1: existing steps mentioned as done, not registered ──
    for (const step of steps) {
      if (step.stage === "observed") continue;      // closed, nothing to advance
      if (updatedIds.has(step.id)) continue;        // already registered this session
      if (!step.title) continue;
      if (!recentText.includes(step.title)) continue;
      const hit = stageSignals(step.stage).some((re) => re.test(recentText));
      if (!hit) continue;
      const epic = epicsById.get(step.epic_id);
      candidates.push({
        kind: "advance",
        stepId: step.id,
        stepTitle: step.title,
        epicTitle: epic?.title || step.epic_id,
        fromStage: step.stage,
        toStage: NEXT_STAGE[step.stage] || "observed",
      });
    }

    // ── Case 2: epic mentioned as done but has NO steps (the HC-04 mode) ──
    for (const epic of data.epics) {
      const epicSteps = stepsByEpic.get(epic.id) || [];
      if (epicSteps.length > 0) continue;           // has steps → case 1 covers it
      if (createdStepIds.has(epic.id)) continue;    // steps being added this session
      if (!epic.title || !recentText.includes(epic.title)) continue;
      // Any completion-ish signal about the epic (work done) with nothing registered
      const allSignals = Object.values(STAGE_SIGNALS).flat();
      if (!allSignals.some((re) => re.test(recentText))) continue;
      candidates.push({ kind: "no-steps", epicId: epic.id, epicTitle: epic.title });
    }
  }

  return candidates;
}
