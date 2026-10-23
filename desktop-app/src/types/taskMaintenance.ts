export type TaskMaintenanceLinkStatus =
  | "unlinked"
  | "operator_asserted"
  | "retracted"
  | "stale"
  | "superseded"
  | "UNKNOWN";

export interface TaskMaintenanceLink {
  execution_id: string;
  attempt_no: number;
  case_id: string | null;
  case_episode: number | null;
  case_revision: number | null;
  status: TaskMaintenanceLinkStatus;
  reason: string;
  decision_request_id: string | null;
  recovery_candidate: boolean;
}

export interface TaskMaintenanceSummary {
  execution_count: number;
  linked_count: number;
  candidate_count: number;
  malformed_annotation_count: number;
}

export interface TaskMaintenanceCoverage {
  projected_links: number;
  total_links: number;
  truncated_links: number;
}

interface TaskMaintenanceEnvelopeBase {
  schema_version: 1;
  observed_at: string;
  source: "mini-task-maintenance";
  state: "observed" | "UNKNOWN";
  reason: string;
  independent_nas_ack: "NOT_PROVIDED";
  assurance: "operator_asserted";
  automatic_resume: false;
  execution_allowed: false;
}

export interface ObservedTaskMaintenance extends TaskMaintenanceEnvelopeBase {
  state: "observed";
  reason: "readonly_projection";
  source_as_of: string;
  summary: TaskMaintenanceSummary;
  coverage: TaskMaintenanceCoverage;
  links: TaskMaintenanceLink[];
}

export interface UnknownTaskMaintenance extends TaskMaintenanceEnvelopeBase {
  state: "UNKNOWN";
  reason: "source_unavailable_or_stale";
  source_as_of: null;
  summary: null;
  coverage: null;
  links: [];
}

export type TaskMaintenance = ObservedTaskMaintenance | UnknownTaskMaintenance;
