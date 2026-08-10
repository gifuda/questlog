#!/usr/bin/env node
/**
 * TaskPackage compliance hook — Stop hook for Codex / Claude Code.
 *
 * NON-AUTHORITATIVE: this hook provides reminders and interception only.
 * The MCP server's prepare/validate/seal/enqueue tools are the real hard
 * gates. If this hook is disabled or unsupported by the client, the
 * server-side validation still prevents unsealed/invalid packages from
 * being enqueued.
 *
 * What it does:
 * - SessionStart: inject a reminder about active TaskPackages and blocked steps
 * - PreToolUse: if enqueue_step is called, check that the step has a sealed package
 * - Stop: if the user asked for an executable plan but no sealed package was
 *   produced, remind to seal it
 *
 * Usage: configure as a hook in the MCP client's settings.
 * Input: JSON on stdin (hook event payload).
 * Output: JSON on stdout (hook response with optional message).
 */

import { readFileSync, existsSync } from "node:fs";
import { join } from "node:path";
import { homedir } from "node:os";

const RUNTIME_DIR = process.env.QUESTLOG_RUNTIME || join(homedir(), "questlog-runtime");
const DECISIONS_PATH = join(RUNTIME_DIR, "decision-requests.json");

function readJsonSafe(filePath) {
  if (!existsSync(filePath)) return null;
  try {
    return JSON.parse(readFileSync(filePath, "utf-8"));
  } catch {
    return null;
  }
}

async function main() {
  let input = "";
  for await (const chunk of process.stdin) {
    input += chunk;
  }

  let event;
  try {
    event = JSON.parse(input);
  } catch {
    process.exit(0); // Not a valid hook event — let the client proceed
  }

  const hookType = event.hook_type || event.type || "";

  switch (hookType) {
    case "SessionStart": {
      // Check for unresolved decision requests
      const decisions = readJsonSafe(DECISIONS_PATH);
      if (decisions && decisions.requests) {
        const unresolved = decisions.requests.filter((r) => !r.resolved);
        if (unresolved.length > 0) {
          console.log(JSON.stringify({
            message: `🔔 QuestLog: ${unresolved.length} unresolved decision request(s) pending. Run list_decision_requests to see them.`,
          }));
        }
      }
      break;
    }

    case "PreToolUse": {
      // Intercept enqueue_step if no sealed package exists
      const toolName = event.tool_name || event.tool || "";
      if (toolName === "enqueue_step") {
        // The MCP server will validate this anyway — this is just an early
        // reminder. We don't block; the server's hard gate does.
        const stepId = event.tool_input?.step_id;
        if (stepId) {
          // Just log a reminder — the server is authoritative
          console.log(JSON.stringify({
            message: `ℹ️ enqueue_step for '${stepId}': the server will verify the package is sealed and valid.`,
          }));
        }
      }
      break;
    }

    case "Stop": {
      // Check if the conversation mentioned executable plans but didn't seal
      const transcript = event.transcript || event.conversation || "";
      if (typeof transcript === "string") {
        const mentionsExecution = /executable|task.?package|auto.?exec|enqueue|prepare_task/i.test(transcript);
        const mentionsSeal = /seal_task_package|sealed/i.test(transcript);
        if (mentionsExecution && !mentionsSeal) {
          console.log(JSON.stringify({
            message: "⚠️ QuestLog: You mentioned creating an executable task, but no TaskPackage was sealed. Remember: prepare_task_package → validate_task_package → seal_task_package → enqueue_step. Only sealed packages can be auto-executed.",
          }));
        }
      }
      break;
    }

    default:
      // Unknown hook type — do nothing
      break;
  }

  process.exit(0);
}

main().catch(() => process.exit(0));
