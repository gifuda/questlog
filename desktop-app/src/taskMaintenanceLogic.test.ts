import { describe, expect, it } from "vitest";
import { matchTaskMaintenanceLink, parseTaskMaintenance } from "./taskMaintenanceLogic";

const now = Date.parse("2026-10-23T12:00:00Z");
const fixture = {
  schema_version: 1,
  observed_at: "2026-10-23T12:00:00+00:00",
  source: "mini-task-maintenance",
  state: "observed",
  reason: "readonly_projection",
  independent_nas_ack: "NOT_PROVIDED",
  assurance: "operator_asserted",
  automatic_resume: false,
  execution_allowed: false,
  source_as_of: "2026-10-23T11:59:59Z",
  summary: { execution_count: 2, linked_count: 1, candidate_count: 1, malformed_annotation_count: 0 },
  coverage: { projected_links: 1, total_links: 2, truncated_links: 1 },
  links: [{
    execution_id: "exec_11111111-1111-4111-8111-111111111111",
    attempt_no: 1,
    case_id: "infrastructure::mini::evidence_gap",
    case_episode: 1,
    case_revision: 4,
    status: "operator_asserted",
    reason: "review_required",
    decision_request_id: "dec_22222222-2222-4222-8222-222222222222",
    recovery_candidate: true,
  }],
};

describe("parseTaskMaintenance", () => {
  it("accepts a fresh, exact synthetic projection and matches exact id plus attempt", () => {
    const result = parseTaskMaintenance(fixture, now);
    expect(result.state).toBe("observed");
    expect(matchTaskMaintenanceLink(result, fixture.links[0].execution_id, 1)?.case_id).toBe(fixture.links[0].case_id);
    expect(matchTaskMaintenanceLink(result, fixture.links[0].execution_id, 2)).toBeNull();
  });

  it.each([
    ["old source", { ...fixture, source_as_of: "2026-10-23T11:56:59Z" }],
    ["future source", { ...fixture, source_as_of: "2026-10-23T12:00:01Z" }],
    ["naive timestamp", { ...fixture, source_as_of: "2026-10-23T12:00:00" }],
    ["extra field", { ...fixture, extra: true }],
    ["automatic resume enabled", { ...fixture, automatic_resume: true }],
    ["bad counts", { ...fixture, summary: { ...fixture.summary, candidate_count: 2 } }],
    ["inconsistent coverage", { ...fixture, coverage: { ...fixture.coverage, truncated_links: 0 } }],
    ["oversized payload", { ...fixture, reason: "x".repeat(17_000) }],
    ["linked count exceeds total links", { ...fixture, coverage: { ...fixture.coverage, total_links: 0, truncated_links: 0 } }],
  ])("returns UNKNOWN for %s", (_name, input) => {
    expect(parseTaskMaintenance(input, now).state).toBe("UNKNOWN");
  });

  it("keeps an UNKNOWN envelope empty instead of turning it into zero counts", () => {
    const input = {
      ...fixture,
      state: "UNKNOWN",
      reason: "source_unavailable_or_stale",
      source_as_of: null,
      summary: null,
      coverage: null,
      links: [],
    };
    expect(parseTaskMaintenance(input, now)).toMatchObject({ state: "UNKNOWN", summary: null, links: [] });
  });
});
