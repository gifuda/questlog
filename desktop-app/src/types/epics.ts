/** 方案追踪模块的类型定义 — 与 Rust models.rs 对齐。 */

export type EpicStatus =
  | "draft"
  | "active"
  | "verifying"
  | "observing"
  | "done"
  | "blocked"
  | "paused";

export type StepStage = "pending" | "designed" | "coded" | "verified" | "observed";

export interface EpicStep {
  id: string;
  epic_id: string;
  title: string;
  description: string;
  order: number;
  depends_on: string[];
  depends_on_titles: string[];
  stage: StepStage;
  stage_label: string;
  stage_pct: number;
  stage_icon: string;
  /** true = 前置已满足、可以现在推进（可并行） */
  actionable: boolean;
  todo_ref?: string | null;
  doc?: string | null;
  updated_at: string;
}

export interface EpicLinks {
  doc?: string | null;
  branch?: string | null;
  note?: string | null;
}

export interface Epic {
  id: string;
  title: string;
  description: string;
  status: EpicStatus;
  status_label: string;
  priority: string;
  /** 父方案 id。undefined = 顶级方案。最多三层：顶级 → 子 → 步骤。 */
  parent_id?: string | null;
  depends_on: string[];
  depends_on_titles: string[];
  blocks: string[];
  blocks_titles: string[];
  progress: number;
  step_count: number;
  child_count: number;
  links: EpicLinks;
  max_active?: number | null;
  created_at: string;
  updated_at: string;
  steps: EpicStep[];
  /** 子方案（按创建时间倒序，由后端组装） */
  children: Epic[];
}

export interface EpicsSummary {
  total: number;
  active: number;
  observing: number;
  blocked: number;
  done: number;
  focus: { id: string; title: string; progress: number } | null;
  next: { id: string; title: string; status: EpicStatus; waiting_for: string[] } | null;
}

export interface EpicsFile {
  version: number;
  project: string;
  project_path: string;
  epics: Epic[];
  summary: EpicsSummary;
}

/** Global epic (across all projects) — for the dashboard aggregate. */
export interface GlobalEpic extends Epic {
  project: string;
  project_path: string;
}
