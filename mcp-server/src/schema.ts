export type ItemType = "knowhow" | "todo" | "idea";

export type ItemStatus =
  | "active"
  | "completed"
  | "archived"
  | "converted";

export type Priority = "high" | "medium" | "low";

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
