/** 方案追踪模块的共享常量。 */
import type { EpicStatus, StepStage } from "./types/epics";

/** 方案状态 → 徽章颜色（语义 token，颜色定义在 index.css @theme）。 */
export const epicStatusBadge: Record<EpicStatus, { dot: string; text: string; bg: string; label: string }> = {
  draft:    { dot: "bg-faint",        text: "text-muted",    bg: "bg-elevated",      label: "草稿" },
  active:   { dot: "bg-accent",       text: "text-accent",   bg: "bg-accent-bg",     label: "进行中" },
  verifying:{ dot: "bg-info",         text: "text-info",      bg: "bg-info-bg",        label: "验证中" },
  observing:{ dot: "bg-amber",        text: "text-amber",    bg: "bg-amber-bg",      label: "观察窗" },
  done:     { dot: "bg-success",      text: "text-success",  bg: "bg-success/10",    label: "已完成" },
  blocked:  { dot: "bg-danger",       text: "text-danger",   bg: "bg-danger/10",     label: "阻塞" },
  paused:   { dot: "bg-faint",        text: "text-faint",    bg: "bg-elevated",      label: "暂停" },
};

/** 步骤阶段顺序与颜色。 */
export const stageOrder: StepStage[] = ["pending", "designed", "coded", "verified", "observed"];

/** 方案状态选择项。 */
export const statusOptions: EpicStatus[] = [
  "draft", "active", "verifying", "observing", "done", "blocked", "paused",
];

/** 方案在路线图主视图的排序权重。 */
export function statusRank(s: EpicStatus): number {
  switch (s) {
    case "active": return 0;
    case "verifying": return 1;
    case "observing": return 2;
    case "blocked": return 3;
    case "draft": return 4;
    case "paused": return 5;
    case "done": return 6;
  }
}
