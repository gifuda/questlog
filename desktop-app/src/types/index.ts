export type ItemType = "knowhow" | "todo" | "idea";
export type ItemStatus = "active" | "completed" | "archived" | "converted";
export type Priority = "high" | "medium" | "low";

/** Closed outcome set for the nightly auto-process pipeline (待办租户 plan §5.3). */
export type AutoOutcome =
  | "accepted_report"
  | "cannot_auto"
  | "needs_review"
  | "execution_failed"
  | "timed_out"
  | "result_pending_writeback"
  | "stale"
  | "cancelled"
  | "preflight_blocked";

export interface AutoResult {
  outcome: AutoOutcome;
  attempt_id?: string;
  report_ref?: string;
  report_hash?: string;
  finished_at?: string;
  reason_code?: string;
  verification_level?: string;
  /** 人工复核裁决（needs_review 出口）：accepted=认可报告，rejected=人工驳回。 */
  review_verdict?: "accepted" | "rejected";
  reviewed_at?: string;
}

export interface TodoItem {
  id: string;
  type: ItemType;
  title: string;
  content: string;
  suggestion: string;
  tags: string[];
  status: ItemStatus;
  priority: Priority;
  source_session: string;
  processing?: boolean;
  /** Nightly auto-process opt-in (collector tenant pipeline), todo items only. */
  auto_process?: boolean;
  /** Bumped on every explicit (re)authorization; old results never reused. */
  auto_process_revision?: number;
  /** Machine-written by the trusted write-back path only. */
  auto_result?: AutoResult;
  /** Written only when a CAS write-back completed active→completed. */
  auto_completed_at?: string;
  /** User confirmation of a nightly-completed result (review gate). */
  auto_ack_at?: string;
  created_at: string;
  updated_at: string;
}

export interface TodosFile {
  version: number;
  project: string;
  items: TodoItem[];
}

export interface ProjectInfo {
  name: string;
  path: string;
  counts: { knowhow: number; todo: number; idea: number };
  /** 未完成（非 done）的顶级方案数，方案追踪 tab 的徽标。 */
  active_epics: number;
  last_active_at: string;
}
