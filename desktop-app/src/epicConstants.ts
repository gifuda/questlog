/** 方案追踪模块的共享常量。 */
import type { EpicStatus, StepStage } from "./types/epics";

/** 方案状态 → 徽章颜色（Tailwind class fragments）。 */
export const epicStatusBadge: Record<EpicStatus, { dot: string; text: string; bg: string; label: string }> = {
  draft:    { dot: "bg-gray-400",   text: "text-gray-300",   bg: "bg-gray-500/15",  label: "草稿" },
  active:   { dot: "bg-blue-500",   text: "text-blue-300",   bg: "bg-blue-500/15",  label: "进行中" },
  verifying:{ dot: "bg-cyan-500",   text: "text-cyan-300",   bg: "bg-cyan-500/15",  label: "验证中" },
  observing:{ dot: "bg-yellow-500", text: "text-yellow-300", bg: "bg-yellow-500/15", label: "观察窗" },
  done:     { dot: "bg-green-500",  text: "text-green-300",  bg: "bg-green-500/15", label: "已完成" },
  blocked:  { dot: "bg-red-500",    text: "text-red-300",    bg: "bg-red-500/15",   label: "阻塞" },
  paused:   { dot: "bg-gray-500",   text: "text-gray-400",   bg: "bg-gray-500/15",  label: "暂停" },
};

/** 步骤阶段顺序与颜色。 */
export const stageOrder: StepStage[] = ["pending", "designed", "coded", "verified", "observed"];

export const stageLabel: Record<StepStage, string> = {
  pending: "未启动",
  designed: "已设计",
  coded: "已实施",
  verified: "已验证",
  observed: "已观察",
};

export const stagePct: Record<StepStage, number> = {
  pending: 0,
  designed: 25,
  coded: 50,
  verified: 75,
  observed: 100,
};

export const stageIcon: Record<StepStage, string> = {
  pending: "☐",
  designed: "◐",
  coded: "◑",
  verified: "◒",
  observed: "☑",
};

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
