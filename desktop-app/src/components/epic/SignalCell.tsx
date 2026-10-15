import { useTranslation } from "react-i18next";
import { statusRank } from "../../epicConstants";
import type { Epic } from "../../types/epics";
import { stepStageKey } from "./helpers";

// SignalCell — 焦点列：每行只显示最重要的一个信号，且必须与真实进度一致
// 优先级：堵点 > 在办步骤 > 在办子方案 > 可做步骤 > 收尾语义
// 收尾语义三分：已闭环(done) / 观察期中(observing) / 待收尾(全部完成但未标 done)
// ══════════════════════════════════════════════
export function SignalCell({ epic, currentStep, nextStep }: {
  epic: Epic;
  currentStep: Epic["steps"][number] | undefined;
  nextStep: Epic["steps"][number] | undefined;
}) {
  const { t } = useTranslation();
  const children = epic.children ?? [];
  const nonDoneChildren = children.filter((c) => c.status !== "done");

  if (epic.status === "blocked") {
    return (
      <span className="text-danger">
        ⚠ {epic.depends_on_titles.length > 0
          ? t("epic.waitFor", { names: epic.depends_on_titles.join("、") })
          : t("epic.resolveDeps")}
      </span>
    );
  }
  if (epic.status === "paused") {
    return <span className="text-faint">{t("epic.statusPaused")}</span>;
  }
  // 在办：自己的中段步骤
  if (currentStep) {
    return (
      <>
        <span className="font-mono text-[11px] text-accent mr-1.5">{t(stepStageKey[currentStep.stage])}</span>
        <span className="text-body">{currentStep.title}</span>
      </>
    );
  }
  // 在办：还有未完成的子方案（父方案进度来自子方案均值）
  if (nonDoneChildren.length > 0) {
    const focusChild = [...nonDoneChildren].sort((a, b) => statusRank(a.status) - statusRank(b.status))[0];
    return (
      <>
        <span className="text-faint mr-1">→</span>
        <span className="text-body">{focusChild.title}</span>
        <span className="text-faint text-xs ml-1.5 whitespace-nowrap">
          {t("epic.closedCount", { done: children.length - nonDoneChildren.length, total: children.length })}
        </span>
      </>
    );
  }
  // 下一步：可做的自己的步骤
  if (nextStep) {
    return (
      <>
        <span className="text-faint mr-1">→</span>
        <span className="text-body">{nextStep.title}</span>
      </>
    );
  }
  // 收尾语义（自己的步骤全部 observed 或没有，子方案全部 done 或没有）
  if (epic.status === "done") {
    return <span className="text-faint">{t("epic.closed")}</span>;
  }
  if (epic.status === "observing") {
    return (
      <span className="text-amber break-words">
        ⏱ {epic.acceptance ? t("epic.observingWith", { acceptance: epic.acceptance }) : t("epic.observingPeriod")}
      </span>
    );
  }
  if (epic.progress >= 100 || (epic.steps.length > 0 && epic.steps.every((s) => s.stage === "observed"))) {
    return <span className="text-amber">{t("epic.pendingClose")}</span>;
  }
  if (epic.steps.length === 0 || epic.steps.every((s) => s.stage === "pending")) {
    return <span className="text-faint">{t("epic.notStarted")}</span>;
  }
  return <span className="text-faint">{t("epic.none")}</span>;
}

// ══════════════════════════════════════════════
// EpicRow — 总表中的一行（顶级方案或子方案），可展开看详情
// ══════════════════════════════════════════════
