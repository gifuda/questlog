/**
 * Shared test fixtures for QuestLog MCP server tests.
 *
 * Factory functions that build Epic / Step / EpicsFile objects with sane
 * defaults. Tests override only the fields they care about — this keeps the
 * "data skeleton" of each test readable and consistent across the suite.
 *
 * The same data shapes are mirrored in the Rust tests
 * (desktop-app/src-tauri/src/epic_store.rs `#[cfg(test)]`) so a change to the
 * algorithm that breaks one suite tends to break the other.
 */
import type { Epic, Step, EpicsFile, EpicStatus, StepStage } from "../src/epic-store.js";

let idCounter = 0;
/** Deterministic id generator so tests don't depend on crypto. */
export function nextId(prefix = "id"): string {
  idCounter += 1;
  return `${prefix}-${idCounter}`;
}

/** Reset the id counter between tests for stable, predictable ids. */
export function resetIds(): void {
  idCounter = 0;
}

export interface EpicSeed {
  id: string;
  title?: string;
  status?: EpicStatus;
  parentId?: string;
  dependsOn?: string[];
  blocks?: string[];
  priority?: string;
  acceptance?: string;
  closeReason?: string;
}

export interface StepSeed {
  id: string;
  epicId: string;
  stage?: StepStage;
  order?: number;
  dependsOn?: string[];
  title?: string;
}

const BASE_TS = "2026-01-01T00:00:00.000Z";

/** Build a single Epic with defaults. */
export function makeEpic(seed: EpicSeed): Epic {
  return {
    id: seed.id,
    title: seed.title ?? `Epic ${seed.id}`,
    description: "",
    status: seed.status ?? "draft",
    priority: seed.priority ?? "P1",
    parent_id: seed.parentId,
    depends_on: seed.dependsOn ?? [],
    blocks: seed.blocks ?? [],
    links: {},
    acceptance: seed.acceptance ?? "",
    close_reason: seed.closeReason ?? "",
    created_at: BASE_TS,
    updated_at: BASE_TS,
  };
}

/** Build a single Step with defaults. */
export function makeStep(seed: StepSeed): Step {
  return {
    id: seed.id,
    epic_id: seed.epicId,
    title: seed.title ?? `Step ${seed.id}`,
    description: "",
    order: seed.order ?? 0,
    depends_on: seed.dependsOn,
    stage: seed.stage ?? "pending",
    links: {},
    created_at: BASE_TS,
    updated_at: BASE_TS,
  };
}

/** Build an EpicsFile from arrays of epic seeds and step seeds. */
export function makeEpicsFile(
  epicSeeds: EpicSeed[],
  stepSeeds: StepSeed[] = [],
  project = "test-project",
): EpicsFile {
  return {
    version: 1,
    project,
    epics: epicSeeds.map(makeEpic),
    steps: stepSeeds.map(makeStep),
  };
}
