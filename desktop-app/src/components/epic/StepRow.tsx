import { useTranslation } from "react-i18next";
import { useState } from "react";
import { stageOrder } from "../../epicConstants";
import type { Epic, StepStage } from "../../types/epics";
import ConfirmButton from "../ConfirmButton";
import ExecutionPanel from "../ExecutionPanel";
import { stepStageKey } from "./helpers";

type StepVisual = "todo" | "running" | "done" | "error";

function stepVisual(step: Epic["steps"][number]): StepVisual {
  if (step.stage === "verified" || step.stage === "observed") return "done";
  // 有封存包 = 正在系统管道中（排队/执行中/待验收）→ 一律显示 🔄
  if (step.package?.status === "sealed") return "running";
  if (step.stage === "coded") return "running";
  return "todo";
}

const stepVisualConfig: Record<StepVisual, { icon: string; label: string; cls: string }> = {
  todo: { icon: "⬜", label: "待做", cls: "text-faint" },
  running: { icon: "🔄", label: "执行中", cls: "text-accent" },
  done: { icon: "✅", label: "完成", cls: "text-success" },
  error: { icon: "⚠️", label: "需处理", cls: "text-danger" },
};

export function StepRow({ step, onUpdate, onDelete, onExecute, projectPath }: {
  step: Epic["steps"][number];
  onUpdate: (patch: { title?: string; stage?: StepStage }) => void;
  onDelete: () => void;
  onExecute?: () => void;
  projectPath?: string;
}) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(false);
  const [sealing, setSealing] = useState(false);
  const [feedback, setFeedback] = useState<string | null>(null);

  const visual = stepVisual(step);
  const cfg = stepVisualConfig[visual];
  const isDone = visual === "done";

  return (
    <div className="rounded hover:bg-elevated/40 group">
      <div
        className="flex items-center gap-2.5 py-1.5 px-2 cursor-pointer"
        onClick={() => setExpanded(!expanded)}
      >
        {/* 三态图标 */}
        <span className={`text-sm shrink-0 ${cfg.cls}`} title={cfg.label}>
          {cfg.icon}
        </span>

        {/* 步骤进度条（5 阶段 → 百分比） */}
        <span className="shrink-0 flex items-center gap-1" title={cfg.label}>
          <div className="w-12 h-1 bg-line rounded-sm overflow-hidden">
            <div
              className={`h-full rounded-sm transition-all ${
                isDone ? "bg-success" : visual === "running" ? "bg-accent animate-pulse" : "bg-accent/60"
              }`}
              style={{ width: `${step.stage_pct}%` }}
            />
          </div>
          <span className="font-mono text-[10px] text-faint tabular-nums w-8">{step.stage_pct}%</span>
        </span>

        {/* 标题 */}
        <span className={`text-[13px] flex-1 truncate ${isDone ? "text-faint" : "text-body"}`}>
          {step.title}
        </span>

        {/* ▶ 启动按钮（仅待做步骤，始终可见） */}
        {onExecute && !isDone && visual === "todo" && (
          <button
            className="text-[11px] text-accent bg-accent-bg px-1.5 py-px rounded shrink-0 hover:bg-accent hover:text-white transition-colors disabled:opacity-40"
            title={t("epic.executeHint")}
            disabled={sealing}
            onClick={async (e) => {
              e.stopPropagation();
              setSealing(true);
              setFeedback(null);
              try {
                await onExecute();
                setFeedback("✅ 已启动");
                setTimeout(() => setFeedback(null), 4000);
              } catch (err) {
                const msg = err instanceof Error ? err.message : String(err);
                setFeedback(`❌ ${msg.length > 40 ? msg.slice(0, 40) + "…" : msg}`);
                setTimeout(() => setFeedback(null), 6000);
              } finally { setSealing(false); }
            }}
          >
            {sealing ? "⏳" : feedback ?? "▶"}
          </button>
        )}

        {/* 反馈信息（非按钮位置） */}
        {feedback && feedback.startsWith("❌") && (
          <span className="text-[10px] text-danger truncate max-w-[180px]">{feedback.slice(2)}</span>
        )}

        {/* 依赖标记 */}
        {step.depends_on_titles.length > 0 && !isDone && (
          <span className="text-[11px] text-faint shrink-0" title={t("epic.waitFor", { names: step.depends_on_titles.join("、") })}>
            🔗{step.depends_on_titles.length}
          </span>
        )}

        {/* 删除 */}
        <ConfirmButton
          label="×"
          confirmLabel="✓"
          onConfirm={() => onDelete()}
          className="w-4 h-4 flex items-center justify-center text-[11px] text-faint hover:text-danger rounded-full opacity-0 group-hover:opacity-100"
        />
      </div>

      {/* 白盒详情 — 展开可见执行过程/包信息/diff */}
      {expanded && (
        <div className="pl-10 pr-2 pb-2">
          {/* 阶段胶囊（高级用户可点击推进） */}
          <div className="flex items-center gap-1 mb-1.5">
            {stageOrder.map((st, i) => (
              <button
                key={st}
                title={t(stepStageKey[st])}
                onClick={() => onUpdate({ stage: st })}
                className={`px-1.5 py-px text-[10px] rounded transition-colors ${
                  i <= stageOrder.indexOf(step.stage)
                    ? step.stage === "observed" ? "bg-success/15 text-success" : "bg-accent-bg text-accent"
                    : "bg-elevated text-faint hover:bg-line-hover"
                }`}
              >
                {t(stepStageKey[st])}
              </button>
            ))}
          </div>
          {step.description && (
            <p className="text-xs text-muted leading-relaxed mb-1">{step.description}</p>
          )}
          {/* 执行面板（包详情+运行时间线） */}
          {step.package && <ExecutionPanel stepId={step.id} />}
        </div>
      )}
    </div>
  );
}

// ══════════════════════════════════════════════
