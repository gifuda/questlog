export type ItemType = "knowhow" | "todo" | "idea";

export type ItemStatus =
  | "active"
  | "completed"
  | "archived"
  | "converted";

export type Priority = "high" | "medium" | "low";

/**
 * Closed outcome set for the nightly auto-process pipeline (待办租户 plan §5.3).
 * Machine-written by the trusted write-back path only.
 */
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

/** Structured result of the last automated attempt. */
export interface AutoResult {
  outcome: AutoOutcome;
  attempt_id?: string;
  report_ref?: string;
  report_hash?: string;
  finished_at?: string;
  reason_code?: string;
  verification_level?: string;
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
  /** True when the user copied the reference to process it in a conversation. */
  processing?: boolean;
  /**
   * Nightly auto-process opt-in (collector tenant pipeline). Explicit user
   * authorization only — defaults to false, never set by a model on its own
   * initiative. Only meaningful for type="todo" items.
   */
  auto_process?: boolean;
  /**
   * Bumped on every explicit (re)authorization. A closed result never applies
   * to a later revision: turning the opt-in off and back on must not reuse
   * the old result.
   */
  auto_process_revision?: number;
  /** Machine-written by the trusted write-back path only. */
  auto_result?: AutoResult;
  /** Written only when a CAS write-back actually completed active→completed. */
  auto_completed_at?: string;
  created_at: string;
  updated_at: string;
}

export interface TodosFile {
  version: 1;
  project: string;
  items: TodoItem[];
  /**
   * Monotonic revision counter for CAS concurrency control.
   * Old files without this field default to 0 on read (backward compat).
   */
  revision?: number;
}

const VALID_TYPES: ItemType[] = ["knowhow", "todo", "idea"];
const VALID_STATUSES: ItemStatus[] = ["active", "completed", "archived", "converted"];
const VALID_PRIORITIES: Priority[] = ["high", "medium", "low"];

const STATUS_TRANSITIONS: Record<ItemType, ItemStatus[]> = {
  knowhow: ["active", "archived"],
  todo: ["active", "completed", "archived"],
  idea: ["active", "converted", "archived"],
};

export function validateItem(partial: Partial<TodoItem>): string | null {
  if (!partial.title?.trim()) return "title is required";
  if (!partial.type || !VALID_TYPES.includes(partial.type)) return `type must be one of: ${VALID_TYPES.join(", ")}`;
  if (partial.status && !VALID_STATUSES.includes(partial.status)) return `status must be one of: ${VALID_STATUSES.join(", ")}`;
  if (partial.priority && !VALID_PRIORITIES.includes(partial.priority)) return `priority must be one of: ${VALID_PRIORITIES.join(", ")}`;
  return null;
}

export function validateTransition(type: ItemType, newStatus: ItemStatus): string | null {
  const allowed = STATUS_TRANSITIONS[type];
  if (!allowed.includes(newStatus)) {
    return `${type} items cannot transition to '${newStatus}'. Allowed: ${allowed.join(", ")}`;
  }
  return null;
}
