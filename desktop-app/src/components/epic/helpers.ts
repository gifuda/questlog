import { statusRank } from "../../epicConstants";
import type { Epic, EpicStatus, StepStage } from "../../types/epics";

export const epicStatusKey: Record<EpicStatus, string> = {
  draft: "epic.statusDraft",
  active: "epic.statusActive",
  verifying: "epic.statusVerifying",
  observing: "epic.statusObserving",
  done: "epic.statusDone",
  blocked: "epic.statusBlocked",
  paused: "epic.statusPaused",
};

/** i18n keys for step stage labels. */
export const stepStageKey: Record<StepStage, string> = {
  pending: "epic.stagePending",
  designed: "epic.stageDesigned",
  coded: "epic.stageCoded",
  verified: "epic.stageVerified",
  observed: "epic.stageObserved",
};

/** 总表四列的栅格模板（表头与行共用，保证对齐）。
 *  焦点列给 1fr：SignalCell 内容常只 2-4 字，1.8fr 在宽窗下留大片空白。 */

export const GRID =
  "grid grid-cols-[minmax(240px,2.2fr)_110px_150px_minmax(200px,1fr)] items-center gap-3 px-4";

/** 在办步骤 = 处于中段（已设计/已实施/已验证）的步骤。 */
export const IN_PROGRESS_STAGES: StepStage[] = ["designed", "coded", "verified"];

/** 排序：按方案自身状态权重（done 严格沉底，与状态徽章一致），同级按创建时间倒序。
 *  子方案跟随父方案渲染，不参与顶级排序；父方案 done 但子方案未完属数据不一致，
 *  徽章与位置保持一致比掩盖更重要。 */

export function sortEpics(list: Epic[]): Epic[] {
  return [...list].sort((a, b) => {
    const r = statusRank(a.status) - statusRank(b.status);
    if (r !== 0) return r;
    return (b.created_at || "").localeCompare(a.created_at || "");
  });
}

export type EpicPatch = { title?: string; description?: string; status?: EpicStatus; priority?: string; dependsOn?: string[]; linksDoc?: string; linksBranch?: string; linksNote?: string; acceptance?: string; closeReason?: string };
