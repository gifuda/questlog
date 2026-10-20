/**
 * 服务器监控 (Infra Board) 纯逻辑 — 全部无副作用、可注入时钟/输入，
 * 供 infraLogic.test.ts 夹具覆盖（F5 异常态判定是验收红线，必须有单测）。
 * 组件只做渲染，判定一律来这里。
 */
import type { InfraBase, InfraProvider, InfraSnapshotFile } from "./types/infra";

/** dsh 日闸常数。来源：需求文档（契约 v1）而非数据文件本身——
 *  需求方改闸值需同步此常量。 */
export const DSH_DAILY_GATE = 50;

/** 新鲜度阈值：fetched_at 超 15 分钟视为数据源停摆。
 *  数据源（com.agent-base.snapshot-pull launchd 作业）约 5 分钟刷新一次，
 *  3 个周期无更新即按未就绪处理（F5）。 */
export const STALE_MS = 15 * 60 * 1000;

// ── F5 新鲜度与解析 ──────────────────────────────────────────

export type Staleness = "fresh" | "stale" | "unknown";

/** fetched_at 新鲜度：超 STALE_MS → stale；缺失/不可解析 → unknown。
 *  非 fresh（stale/unknown）一律按「数据源未就绪」降级，绝不白屏。 */
export function stalenessState(
  fetchedAt: string | null | undefined,
  now: number = Date.now(),
): Staleness {
  if (!fetchedAt || typeof fetchedAt !== "string") return "unknown";
  const ts = Date.parse(fetchedAt);
  if (Number.isNaN(ts)) return "unknown";
  return now - ts > STALE_MS ? "stale" : "fresh";
}

/** 防御式解析信封 data 原文：非 JSON / 非对象（数组、数字等）→ null。
 *  Rust 侧已验过一遍 JSON，这里兜底（信封 ok 但 body 再解析失败不炸视图）。 */
export function parseSnapshot(raw: string | null | undefined): InfraSnapshotFile | null {
  if (!raw || typeof raw !== "string") return null;
  try {
    const v = JSON.parse(raw) as unknown;
    if (v !== null && typeof v === "object" && !Array.isArray(v)) {
      return v as InfraSnapshotFile;
    }
  } catch {
    /* 损坏即未就绪，交由上层横幅呈现 */
  }
  return null;
}

// ── F3 健康指示行 ────────────────────────────────────────────

export interface HealthItem {
  key: string;
  value: string;
  /** 配对的 <key>_detail 值（存在时）；健康行 title 悬停展示 */
  detail: string | null;
}

/** health 键元数据：checked_at/status 是快照元信息，不是健康项；
 *  *_detail 是主键的配对明细，不单独成项。 */
const HEALTH_META_KEYS = new Set(["checked_at", "status"]);

/** 动态枚举健康项：实况 9 项（pg/worker/queue/codex_auth/proxy/
 *  zcode_env/zcode_live/dsh_budget/egress_ip），将来增减键自动跟上。 */
export function healthItems(
  health: Record<string, string> | null | undefined,
): HealthItem[] {
  if (!health || typeof health !== "object") return [];
  const out: HealthItem[] = [];
  for (const [key, value] of Object.entries(health)) {
    if (HEALTH_META_KEYS.has(key) || key.endsWith("_detail")) continue;
    if (typeof value !== "string") continue;
    const paired = health[`${key}_detail`];
    out.push({
      key,
      value,
      detail: typeof paired === "string" ? paired : null,
    });
  }
  return out;
}

export type HealthTone = "success" | "amber" | "danger" | "muted";

/** 健康值 → 语义色：ok→绿；warn/warning→黄；fail/error/down→红；
 *  非字符串（schema 漂移时的数字/布尔/对象）与未知值→灰——宁灰不误报，
 *  绝不因非字符串真值抛 TypeError（F5 白屏红线，全函数无异常路径）。 */
export function toneOf(value: unknown): HealthTone {
  if (typeof value !== "string") return "muted";
  switch (value.trim().toLowerCase()) {
    case "ok":
      return "success";
    case "warn":
    case "warning":
      return "amber";
    case "fail":
    case "error":
    case "down":
      return "danger";
    default:
      return "muted";
  }
}

// ── F1 LLM 额度 ─────────────────────────────────────────────

/** F1：剩余 <20% 转琥珀（边界：20 本身不告警，19 告警）。
 *  null/NaN/非数 → 不告警（无数据不算低额度）。 */
export function quotaWarn(pct: number | null | undefined): boolean {
  return typeof pct === "number" && !Number.isNaN(pct) && pct < 20;
}

/** 额度环色：剩余 100%→0% 由蓝（hue 220）连续过渡到红（hue 0），
 * 中途自然经过绿（50%）/黄橙（~20%）。null/非数 → null（无数据不画环）。 */
export function quotaHueColor(pct: number | null | undefined): string | null {
  if (typeof pct !== "number" || Number.isNaN(pct)) return null;
  const clamped = Math.min(100, Math.max(0, pct));
  return `hsl(${Math.round(220 * (clamped / 100))} 72% 55%)`;
}

/** providers[] 元素形态守卫（F5 白屏红线类）：null/数字/字符串/数组元素
 *  剔除，仅保留形态完整的供应商对象——schema 漂移时坏元素不渲染不连坐
 *  整卡，其余有效项照常展示。非数组输入 → 空列表。 */
export function providerList(raw: unknown): InfraProvider[] {
  if (!Array.isArray(raw)) return [];
  return raw.filter(
    (p): p is InfraProvider =>
      p !== null && typeof p === "object" && !Array.isArray(p),
  );
}

// ── F2 底座状态 ─────────────────────────────────────────────

/** queue 动态键值枚举：实况仅 dead_letter，契约自注「键动态」——
 *  有什么渲染什么，绝不写死 queued/running 三键假设。 */
export function queueEntries(
  queue: unknown,
): { key: string; value: number }[] {
  if (!queue || typeof queue !== "object") return [];
  const out: { key: string; value: number }[] = [];
  for (const [k, v] of Object.entries(queue)) {
    if (typeof v === "number" && !Number.isNaN(v)) out.push({ key: k, value: v });
  }
  return out;
}

/** dsh 今日用量水位（F2）：used/gate 百分比，钳 0..100；
 *  used 非数或 gate 非法 → null（显示 —）。 */
export function dshPct(
  used: number | null | undefined,
  gate: number = DSH_DAILY_GATE,
): number | null {
  if (typeof used !== "number" || Number.isNaN(used) || !(gate > 0)) return null;
  return Math.min(100, Math.max(0, (used / gate) * 100));
}

// ── F6 前置：14 天用量按日聚合 ───────────────────────────────

export interface UsageDay {
  day: string;
  ok: number;
  fail: number;
  secs: number;
  in_tok: number;
}

function numOr0(v: unknown): number {
  return typeof v === "number" && !Number.isNaN(v) ? v : 0;
}

/** usage_14d 跨 harness 按日合并。实况仅当天有数（稀疏天）——
 *  只聚合出现过的天，不补零；day 缺失/非字符串的行跳过，不炸。 */
export function aggUsage14d(rows: InfraBase["usage_14d"] | null): UsageDay[] {
  if (!Array.isArray(rows)) return [];
  const byDay = new Map<string, UsageDay>();
  for (const r of rows) {
    if (r === null || typeof r !== "object") continue;
    const day = (r as { day?: unknown }).day;
    if (typeof day !== "string" || day === "") continue;
    const cur = byDay.get(day) ?? { day, ok: 0, fail: 0, secs: 0, in_tok: 0 };
    const row = r as { ok?: unknown; fail?: unknown; secs?: unknown; in_tok?: unknown };
    cur.ok += numOr0(row.ok);
    cur.fail += numOr0(row.fail);
    cur.secs += numOr0(row.secs);
    cur.in_tok += numOr0(row.in_tok);
    byDay.set(day, cur);
  }
  return [...byDay.values()].sort((a, b) => a.day.localeCompare(b.day));
}

// ── F8 前置：NAS 帧龄 ───────────────────────────────────────

/** NAS 帧龄（nas_frame_age_secs，秒）→ 语义色。
 *  null/未知 → 红（无数据按最坏假设）；>600（10 分钟）→ 黄；其余 → 绿。 */
export function nasTone(secs: number | null | undefined): "danger" | "amber" | "success" {
  if (typeof secs !== "number" || Number.isNaN(secs)) return "danger";
  return secs > 600 ? "amber" : "success";
}

// ── 显示兜底 ────────────────────────────────────────────────

/** recent[].secs 可为 null（运行中/未知）——null/NaN → "—"，不炸。 */
export function fmtSecs(secs: number | null | undefined): string {
  return typeof secs === "number" && !Number.isNaN(secs) ? `${secs}s` : "—";
}

/** 百分比显示：null/NaN → "—"。 */
export function fmtPct(pct: number | null | undefined): string {
  return typeof pct === "number" && !Number.isNaN(pct) ? `${Math.round(pct)}%` : "—";
}

// ── 队列与任务流水（键动态/字段可缺，全守卫） ────────────────

/** queue 动态键 → 中文标签（未知键原样透出，契约漂移可见）。 */
const QUEUE_KEY_LABELS: Record<string, string> = {
  queued: "qQueued",
  running: "qRunning",
  dead_letter: "qDead",
};

export interface QueueChip {
  /** 原始键名（稳定 key）。 */
  key: string;
  /** i18n key（infra 命名空间内）。 */
  labelKey: string;
  value: number;
  /** dead_letter>0 → danger；running>0 → success；queued>0 → info；值 0 → muted。 */
  tone: "danger" | "success" | "info" | "muted";
}

export function queueChips(raw: unknown): QueueChip[] {
  const entries = queueEntries(raw);
  return entries.map((e) => {
    let tone: QueueChip["tone"] = "muted";
    if (e.value > 0) {
      if (e.key === "dead_letter") tone = "danger";
      else if (e.key === "running") tone = "success";
      else if (e.key === "queued") tone = "info";
    }
    return {
      key: e.key,
      labelKey: QUEUE_KEY_LABELS[e.key] ?? "",
      value: e.value,
      tone,
    };
  });
}

export interface RecentTask {
  id: number | null;
  harness: string;
  status: string;
  secs: number | null;
}

/** recent[] 形态守卫（F5 红线类）：null/非对象元素剔除，字段缺失降级。 */
export function recentList(raw: unknown): RecentTask[] {
  if (!Array.isArray(raw)) return [];
  return raw
    .filter((t): t is Record<string, unknown> => t !== null && typeof t === "object" && !Array.isArray(t))
    .map((t) => ({
      id: typeof t.id === "number" ? t.id : null,
      harness: typeof t.harness === "string" ? t.harness : "—",
      status: typeof t.status === "string" ? t.status : "—",
      secs: typeof t.secs === "number" ? t.secs : null,
    }));
}

/** 任务状态 → i18n key（infra 命名空间）；未知状态回退原样字符串（空串表示原样）。 */
export function taskStatusLabelKey(status: string): string {
  const map: Record<string, string> = {
    result_ready: "tsResultReady",
    dead_letter: "tsDeadLetter",
    running: "tsRunning",
    queued: "tsQueued",
    failed: "tsFailed",
    timeout: "tsTimeout",
  };
  return map[status] ?? "";
}

/** 任务状态 → 展示色。 */
export function taskStatusTone(status: string): "success" | "danger" | "accent" | "muted" {
  if (status === "result_ready") return "success";
  if (status === "dead_letter" || status === "failed" || status === "timeout") return "danger";
  if (status === "running") return "accent";
  return "muted";
}

// ── schema v2：在办/排队明细（全字段守卫） ────────────────────

export interface QueueItem {
  id: number | null;
  title: string;
  harness: string;
  /** 入队/开始时刻（ISO）；不可解析 → null（时长显示 —）。 */
  at: string | null;
}

function toQueueItems(raw: unknown): QueueItem[] {
  if (!Array.isArray(raw)) return [];
  return raw
    .filter((t): t is Record<string, unknown> => t !== null && typeof t === "object" && !Array.isArray(t))
    .map((t) => ({
      id: typeof t.id === "number" ? t.id : null,
      title: typeof t.title === "string" && t.title ? t.title : "（无标题）",
      harness: typeof t.harness === "string" ? t.harness : "—",
      at: typeof (t.enqueued_at ?? t.started_at) === "string" ? ((t.enqueued_at ?? t.started_at) as string) : null,
    }));
}

export function runningItems(raw: unknown): QueueItem[] {
  return toQueueItems(raw);
}

export function queuedItemsOf(raw: unknown): QueueItem[] {
  return toQueueItems(raw);
}

/** 距今时长字符串：ISO 可解析 → 「3分12秒/58秒/1时5分」；否则 —。 */
export function sinceLabel(iso: string | null, nowMs: number): string {
  if (!iso) return "—";
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return "—";
  const s = Math.max(0, Math.floor((nowMs - t) / 1000));
  if (s < 60) return `${s}秒`;
  if (s < 3600) return `${Math.floor(s / 60)}分${s % 60}秒`;
  return `${Math.floor(s / 3600)}时${Math.floor((s % 3600) / 60)}分`;
}
