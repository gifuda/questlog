import type {
  ObservedTaskMaintenance,
  TaskMaintenance,
  TaskMaintenanceLink,
  TaskMaintenanceLinkStatus,
  UnknownTaskMaintenance,
} from "./types/taskMaintenance";

const ENVELOPE_KEYS = [
  "schema_version", "observed_at", "source", "state", "reason", "independent_nas_ack",
  "assurance", "automatic_resume", "execution_allowed", "source_as_of", "summary", "coverage", "links",
].sort();
const SUMMARY_KEYS = ["execution_count", "linked_count", "candidate_count", "malformed_annotation_count"].sort();
const COVERAGE_KEYS = ["projected_links", "total_links", "truncated_links"].sort();
const LINK_KEYS = [
  "execution_id", "attempt_no", "case_id", "case_episode", "case_revision", "status", "reason",
  "decision_request_id", "recovery_candidate",
].sort();
const UUID = "[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}";
const EXECUTION_ID = new RegExp(`^exec_${UUID}$`);
const DECISION_ID = new RegExp(`^dec_${UUID}$`);
const CASE_ID = /^[A-Za-z0-9:._,-]{1,256}$/;
const REASON = /^[a-z0-9_]{1,80}$/;
const AWARE_ISO = /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?(?:Z|[+-]\d\d:\d\d)$/;
const LINK_STATUSES = new Set<TaskMaintenanceLinkStatus>([
  "unlinked", "operator_asserted", "retracted", "stale", "superseded", "UNKNOWN",
]);
const MAX_BYTES = 16 * 1024;
const MAX_AGE_MS = 180_000;

function isObject(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function exactKeys(value: Record<string, unknown>, expected: string[]): boolean {
  const actual = Object.keys(value).sort();
  return actual.length === expected.length && actual.every((key, index) => key === expected[index]);
}

function boundedInt(value: unknown, max: number): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 && value <= max;
}

function awareTimestamp(value: unknown, now: number): value is string {
  if (typeof value !== "string" || !AWARE_ISO.test(value)) return false;
  const timestamp = Date.parse(value);
  return Number.isFinite(timestamp) && timestamp <= now && now - timestamp <= MAX_AGE_MS;
}

function parseUnknown(value: Record<string, unknown>): UnknownTaskMaintenance | null {
  if (value.state !== "UNKNOWN" || value.reason !== "source_unavailable_or_stale"
      || value.source_as_of !== null || value.summary !== null || value.coverage !== null
      || !Array.isArray(value.links) || value.links.length !== 0) return null;
  return value as unknown as UnknownTaskMaintenance;
}

function parseLink(value: unknown): TaskMaintenanceLink | null {
  if (!isObject(value) || !exactKeys(value, LINK_KEYS)) return null;
  if (typeof value.execution_id !== "string" || !EXECUTION_ID.test(value.execution_id)
      || !boundedInt(value.attempt_no, 0xffffffff) || value.attempt_no === 0
      || (value.case_id !== null && (typeof value.case_id !== "string" || !CASE_ID.test(value.case_id)))
      || (value.case_episode !== null && (!boundedInt(value.case_episode, Number.MAX_SAFE_INTEGER) || value.case_episode === 0))
      || (value.case_revision !== null && (!boundedInt(value.case_revision, Number.MAX_SAFE_INTEGER) || value.case_revision === 0))
      || typeof value.status !== "string" || !LINK_STATUSES.has(value.status as TaskMaintenanceLinkStatus)
      || typeof value.reason !== "string" || !REASON.test(value.reason)
      || (value.decision_request_id !== null && (typeof value.decision_request_id !== "string" || !DECISION_ID.test(value.decision_request_id)))
      || typeof value.recovery_candidate !== "boolean") return null;
  if (value.recovery_candidate && (value.status !== "operator_asserted" || value.case_id === null
      || value.case_episode === null || value.case_revision === null || value.decision_request_id === null)) return null;
  return value as unknown as TaskMaintenanceLink;
}

/** Strictly validates the fixed read-only projection. Invalid or old input is UNKNOWN, never zero. */
export function parseTaskMaintenance(input: unknown, now = Date.now()): TaskMaintenance {
  try {
    if (!isObject(input) || !exactKeys(input, ENVELOPE_KEYS)) return unknownTaskMaintenance(now);
    const encoded = JSON.stringify(input);
    if (typeof encoded !== "string" || new TextEncoder().encode(encoded).byteLength > MAX_BYTES) {
      return unknownTaskMaintenance(now);
    }
    if (input.schema_version !== 1 || !awareTimestamp(input.observed_at, now)
        || input.source !== "mini-task-maintenance" || input.independent_nas_ack !== "NOT_PROVIDED"
        || input.assurance !== "operator_asserted" || input.automatic_resume !== false || input.execution_allowed !== false) {
      return unknownTaskMaintenance(now);
    }
    if (input.state === "UNKNOWN") {
      return parseUnknown(input) ?? unknownTaskMaintenance(now);
    }
    if (input.state !== "observed" || input.reason !== "readonly_projection"
        || !awareTimestamp(input.source_as_of, now) || !isObject(input.summary)
        || !exactKeys(input.summary, SUMMARY_KEYS) || !isObject(input.coverage)
        || !exactKeys(input.coverage, COVERAGE_KEYS) || !Array.isArray(input.links) || input.links.length > 4) {
      return unknownTaskMaintenance(now);
    }
    const summary = input.summary;
    const coverage = input.coverage;
    if (!boundedInt(summary.execution_count, 4096) || !boundedInt(summary.linked_count, 4096)
        || !boundedInt(summary.candidate_count, 4096) || !boundedInt(summary.malformed_annotation_count, 1024)
        || !boundedInt(coverage.projected_links, 4) || !boundedInt(coverage.total_links, 8192)
        || !boundedInt(coverage.truncated_links, 8192) || coverage.projected_links !== input.links.length
        || coverage.total_links < coverage.projected_links
        || coverage.truncated_links !== coverage.total_links - coverage.projected_links) {
      return unknownTaskMaintenance(now);
    }
    const links: TaskMaintenanceLink[] = [];
    for (const rawLink of input.links) {
      const link = parseLink(rawLink);
      if (!link) return unknownTaskMaintenance(now);
      links.push(link);
    }
    const shownLinked = links.filter((link) => link.status === "operator_asserted").length;
    const shownCandidates = links.filter((link) => link.recovery_candidate).length;
    if (summary.candidate_count > summary.linked_count || summary.linked_count > summary.execution_count
        || summary.linked_count > coverage.total_links || summary.candidate_count > coverage.total_links
        || summary.linked_count < shownLinked || summary.candidate_count < shownCandidates) {
      return unknownTaskMaintenance(now);
    }
    return input as unknown as ObservedTaskMaintenance;
  } catch {
    return unknownTaskMaintenance(now);
  }
}

export function unknownTaskMaintenance(now = Date.now()): UnknownTaskMaintenance {
  return {
    schema_version: 1,
    observed_at: new Date(now).toISOString(),
    source: "mini-task-maintenance",
    state: "UNKNOWN",
    reason: "source_unavailable_or_stale",
    independent_nas_ack: "NOT_PROVIDED",
    assurance: "operator_asserted",
    automatic_resume: false,
    execution_allowed: false,
    source_as_of: null,
    summary: null,
    coverage: null,
    links: [],
  };
}

export function matchTaskMaintenanceLink(
  projection: TaskMaintenance,
  executionId: string,
  attemptNo: number,
): TaskMaintenanceLink | null {
  if (projection.state !== "observed") return null;
  return projection.links.find((link) => link.execution_id === executionId && link.attempt_no === attemptNo) ?? null;
}

export function taskMaintenanceStatusLabel(status: TaskMaintenanceLinkStatus): string {
  switch (status) {
    case "operator_asserted": return "已显式关联";
    case "unlinked": return "明确未关联";
    case "retracted": return "关联已撤回";
    case "stale": return "关联已过期";
    case "superseded": return "关联已被替代";
    case "UNKNOWN": return "关联状态未知";
  }
}
