import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { useArbBoard } from "../hooks/useArbBoard";
import { useFleetHealth } from "../hooks/useFleetHealth";
import { useDecisionRequests } from "../hooks/useExecutions";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useState, useCallback, useEffect } from "react";
import { useRefreshFallback } from "../hooks/useRefreshFallback";
import Badge from "./Badge";
import { epicStatusBadge } from "../epicConstants";
import type { EpicStatus } from "../types/epics";

/** 时间戳 → 距今小时数。解析失败返回 null。 */
function hoursAgo(dateStr: string | null | undefined): number | null {
  if (!dateStr) return null;
  const t = Date.parse(dateStr.trim());
  if (Number.isNaN(t)) return null;
  return (Date.now() - t) / 3600000;
}

/** 日期 → 距今天的天数（本地时区，日历日差）。解析失败返回 null。 */
function daysAgo(dateStr: string | null | undefined): number | null {
  if (!dateStr) return null;
  const m = /^(\d{4})-(\d{2})-(\d{2})/.exec(dateStr.trim());
  if (!m) return null;
  const d = new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3])).getTime();
  if (Number.isNaN(d)) return null;
  const now = new Date();
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  return Math.round((today - d) / 86400000);
}

/** 时效徽章：今天=绿、昨天=灰、更久=琥珀色标年龄。日期缺失/不可解析不渲染。 */
function AgeBadge({ date }: { date: string | null | undefined }) {
  const { t } = useTranslation();
  const age = daysAgo(date);
  if (age === null) return null;
  if (age <= 0) return <Badge tone="success">{t("morning.today")}</Badge>;
  if (age === 1) return <Badge tone="muted">{t("morning.yesterday")}</Badge>;
  return <Badge tone="amber">{t("morning.daysAgo", { days: age })}</Badge>;
}
/** 时效告警：晨报内容超龄 / 导出停滞 / 决策逾期。无告警返回空数组。 */
function useStalenessAlerts(
  arb: ReturnType<typeof useArbBoard>["data"],
): { tone: "amber" | "danger"; text: string }[] {
  const { t } = useTranslation();
  const alerts: { tone: "amber" | "danger"; text: string }[] = [];
  const briefAge = daysAgo(arb?.brief?.brief_date);
  if (briefAge !== null && briefAge >= 2) {
    alerts.push({
      tone: "amber",
      text: t("morning.alertStaleBrief", { days: briefAge, date: arb?.brief?.brief_date }),
    });
  }
  // 36h 阈值与采集机镜像脚本的 W10.10 生产者停滞判定对齐：daily-export
  // 每天 09:35 跑，日期粒度的"≥1 天"会在每天 00:00–09:35 必然误报。
  // 36h = 完整漏跑一天以上才告警。
  const exportHours = hoursAgo(arb?.generated_at);
  if (exportHours !== null && exportHours >= 36) {
    alerts.push({
      tone: "danger",
      text: t("morning.alertExportStale", { ts: arb?.generated_at?.slice(0, 10) }),
    });
  }
  for (const d of arb?.decisions ?? []) {
    const overdue = daysAgo(d.deadline);
    if (overdue !== null && overdue > 0) {
      alerts.push({
        tone: "danger",
        text: t("morning.alertOverdue", { id: d.id ?? "?", days: overdue }),
      });
    }
  }
  return alerts;
}

/** get_epics_global 的行形状（后端返回 Epic 子集 + project 归属）。 */
interface OverviewEpic {
  id: string;
  title: string;
  status: EpicStatus;
  status_label: string;
  priority: string;
  progress: number;
  project: string;
}

interface ActiveItem {
  id: string;
  type: string;
  project: string;
}

interface ProjectInfo {
  name: string;
  path: string;
}

interface MorningProps {
  projects: ProjectInfo[];
  /** 跳转到指定 tab；可选带项目（待确认跳转需要定位项目）。 */
  onGoto: (tab: "todos" | "epics" | "arb", projectName?: string) => void;
}

/**
 * 晨报首屏（需求 v1 · D1-D4）——app 默认页。纯只读：
 * 晨报卡（GROS brief + QuestLog 夜间 digest 合一）、待我决断三卡（计数+跳转）、
 * 各项目焦点行（吞并原总览 tab）。任何处置动作都发生在目标页。
 */
export default function MorningBriefing({ projects, onGoto }: MorningProps) {
  const { t } = useTranslation();
  const { data: arb } = useArbBoard();
  const { health } = useFleetHealth();
  const { requests } = useDecisionRequests();

  // 跨项目焦点（原 Overview 的数据面）
  const [epics, setEpics] = useState<OverviewEpic[]>([]);
  const [items, setItems] = useState<ActiveItem[]>([]);
  const refresh = useCallback(async () => {
    try {
      const [eg, ai] = await Promise.all([
        invoke<{ epics: OverviewEpic[] }>("get_epics_global"),
        invoke<ActiveItem[]>("get_all_active"),
      ]);
      setEpics(eg.epics);
      setItems(ai);
    } catch {
      // 保持上次快照
    }
  }, []);
  useEffect(() => {
    refresh();
    const un1 = listen("todos-changed", refresh);
    const un2 = listen("epics-changed", refresh);
    return () => {
      un1.then((f) => f());
      un2.then((f) => f());
    };
  }, [refresh]);
  useRefreshFallback(refresh);

  // ── 决断计数 ──
  const arbDecisions = (arb?.decisions ?? []).filter(
    (d) => (d.cmd_approve ?? "") !== "" || (d.cmd_reject ?? "") !== "",
  );
  const pendingAck = health?.pending_ack ?? { count: 0, items: [] };
  const drqCount = requests.length;

  // ── 项目焦点行（原 Overview 排序：空项目沉底、阻塞优先、在办总量） ──
  const rows = useMemo(() => {
    return projects
      .map((p) => {
        const projItems = items.filter((i) => i.project === p.name);
        const projEpics = epics.filter((e) => e.project === p.name && e.status !== "done");
        return {
          name: p.name,
          todoCount: projItems.filter((i) => i.type === "todo").length,
          knowhowCount: projItems.filter((i) => i.type === "knowhow").length,
          ideaCount: projItems.filter((i) => i.type === "idea").length,
          epicCount: projEpics.length,
          blocked: projEpics.some((e) => e.status === "blocked"),
          focus: projEpics[0],
        };
      })
      .sort((a, b) => {
        const ea = a.epicCount + a.todoCount + a.knowhowCount + a.ideaCount === 0 ? 1 : 0;
        const eb = b.epicCount + b.todoCount + b.knowhowCount + b.ideaCount === 0 ? 1 : 0;
        if (ea !== eb) return ea - eb;
        if (a.blocked !== b.blocked) return a.blocked ? -1 : 1;
        return b.epicCount + b.todoCount - (a.epicCount + a.todoCount);
      });
  }, [projects, epics, items]);

  const brief = arb?.brief;
  const alerts = useStalenessAlerts(arb);

  return (
    <div className="h-full overflow-y-auto px-4 py-3 flex flex-col gap-3">
      {/* ── 时效告警条：数据停滞/决策逾期在最顶端 ── */}
      {alerts.length > 0 && (
        <div className="rounded-lg border border-amber/30 bg-amber-bg px-3 py-2 flex flex-col gap-1">
          {alerts.map((a, i) => (
            <div key={i} className={`flex items-center gap-2 text-[11px] ${a.tone === "danger" ? "text-danger" : "text-amber"}`}>
              <span>{a.tone === "danger" ? "⛔" : "⚠️"}</span>
              <span>{a.text}</span>
            </div>
          ))}
        </div>
      )}
      {/* ── 晨报卡（D2 合一；各源独立降级） ── */}
      <div className="rounded-xl border border-line bg-surface">
        <div className="flex items-center gap-2 px-3.5 pt-2.5 pb-1.5">
          <span className="text-[14px] font-semibold text-heading">☀ {t("morning.briefTitle")}</span>
          <span className="text-[11px] font-mono text-faint">{brief?.brief_date ?? "—"}</span>
          <AgeBadge date={brief?.brief_date} />
          <span className="ml-auto text-[11px] text-faint">
            {t("morning.sources")}
            {arb?.generated_at && ` · ${t("morning.exportedAt", { ts: arb.generated_at.slice(0, 10) })}`}
          </span>
        </div>
        {brief === undefined || brief === null ? (
          <div className="px-3.5 pb-2.5 text-[11px] text-faint">{t("morning.briefWaiting")}</div>
        ) : (
          <>
            {brief.headline && (
              <div className="px-3.5 pb-2 text-[13px] text-body">{brief.headline}</div>
            )}
            {(brief.opportunities ?? []).length > 0 && (
              <div className="px-3.5 pb-2.5 border-t border-line/50 pt-2">
                <div className="text-[11px] text-faint mb-1">{t("morning.opps")}</div>
                {(brief.opportunities ?? []).map((o, i) => (
                  <div key={i} className="flex items-center gap-1.5 py-0.5 min-w-0">
                    <Badge tone={o.priority === "高" || o.priority === "high" ? "danger" : o.priority === "中" || o.priority === "medium" ? "amber" : "muted"}>
                      {o.priority ?? "?"}
                    </Badge>
                    <span className="text-[13px] text-body truncate">{o.title ?? "—"}</span>
                  </div>
                ))}
              </div>
            )}
          </>
        )}
        <div className="px-3.5 pb-2.5 border-t border-line/50 pt-2">
          <div className="flex items-center gap-2 text-[11px] text-faint mb-1">
            {/* digest 语义诚实：只有真的是昨夜才说"昨夜"，否则标明是最近一次 */}
            <span>
              {health?.digest
                ? daysAgo(health.digest.date) === 0
                  ? t("morning.nightly")
                  : daysAgo(health.digest.date) === 1
                    ? t("morning.nightlyYesterday")
                    : t("morning.nightlyRecent")
                : t("morning.nightly")}
            </span>
            {health?.digest && daysAgo(health.digest.date) !== null && daysAgo(health.digest.date)! > 1 && (
              <AgeBadge date={health.digest.date} />
            )}
          </div>
          {health?.digest ? (
            <div className="text-[13px] text-body">
              🌙 <span className="font-mono">{health.digest.date}</span>
              {" · "}{t("morning.digestLine", {
                total: health.digest.total,
                accepted: health.digest.accepted,
                review: health.digest.review,
                failed: health.digest.failed,
              })}
            </div>
          ) : (
            <div className="text-[13px] text-faint">{t("morning.noNightly")}</div>
          )}
        </div>
      </div>

      {/* ── 待我决断（D3：计数+跳转，处置回各自页面） ── */}
      <div className="grid grid-cols-3 gap-2.5">
        <DecisionCard
          count={arbDecisions.length}
          tone="amber"
          title={t("morning.decArb")}
          sub={arbDecisions[0] ? `${arbDecisions[0].id} ${arbDecisions[0].advice ?? ""}` : ""}
          action={t("morning.decArbGo")}
          onClick={() => onGoto("arb")}
        />
        <DecisionCard
          count={pendingAck.count}
          tone="accent"
          title={t("morning.decNightly")}
          sub={pendingAck.items[0] ? String(pendingAck.items[0].title ?? "") : ""}
          action={t("morning.decNightlyGo")}
          onClick={() => onGoto("todos", pendingAck.items[0]?.project as string | undefined)}
        />
        <DecisionCard
          count={drqCount}
          tone="plain"
          title={t("morning.decDrq")}
          sub={drqCount === 0 ? t("morning.decDrqNone") : ""}
          action={drqCount > 0 ? t("morning.decDrqGo") : "—"}
          onClick={drqCount > 0 ? () => onGoto("epics") : undefined}
        />
      </div>

      {/* ── 各项目焦点（D4） ── */}
      <div>
        <div className="text-[11px] text-faint tracking-wide mb-1.5">{t("morning.focusTitle")}</div>
        <div className="flex flex-col gap-1.5">
          {rows.map((r) => {
            const empty = r.epicCount + r.todoCount + r.knowhowCount + r.ideaCount === 0;
            // Rust 端新增状态时前端未同步的兜底：落到 paused 徽章而非崩溃。
            const badge = r.focus ? (epicStatusBadge[r.focus.status] ?? epicStatusBadge.paused) : null;
            return (
              <button
                key={r.name}
                disabled={empty}
                onClick={() => onGoto("epics", r.name)}
                title={t("overview.jumpTitle")}
                className={`w-full text-left px-3 py-2 rounded-lg border border-line bg-surface/60 transition-colors ${
                  empty ? "opacity-45 cursor-default" : "hover:bg-surface hover:border-accent/40"
                }`}
              >
                <div className="flex items-center gap-3 min-w-0">
                  <span className="text-[13px] font-medium text-heading shrink-0">{r.name}</span>
                  <span className="flex items-center gap-2 text-[11px] text-muted shrink-0 tabular-nums">
                    {r.todoCount > 0 && <span>✅ {r.todoCount}</span>}
                    {r.knowhowCount > 0 && <span>📘 {r.knowhowCount}</span>}
                    {r.ideaCount > 0 && <span>💡 {r.ideaCount}</span>}
                    {r.epicCount > 0 && (
                      <span className="text-accent">{t("overview.epicCount", { count: r.epicCount })}</span>
                    )}
                  </span>
                  {r.focus && badge && (
                    <span className="flex items-center gap-1.5 text-[11px] text-muted min-w-0 ml-auto">
                      <span className={`w-1.5 h-1.5 rounded-full shrink-0 ${badge.dot}`} />
                      <span className="shrink-0">{r.focus.status_label}</span>
                      <span className="truncate text-body">{r.focus.title}</span>
                      <span className="shrink-0 tabular-nums text-faint">{r.focus.progress}%</span>
                    </span>
                  )}
                </div>
              </button>
            );
          })}
        </div>
      </div>
    </div>
  );
}

/** 决断卡：计数 + 一行摘要 + 跳转。0 且无内容时置灰不可点。 */
function DecisionCard({
  count,
  tone,
  title,
  sub,
  action,
  onClick,
}: {
  count: number;
  tone: "amber" | "accent" | "plain";
  title: string;
  sub: string;
  action: string;
  onClick?: () => void;
}) {
  const empty = count === 0;
  return (
    <button
      disabled={empty && !onClick}
      onClick={onClick}
      className={`flex items-center gap-2.5 rounded-lg border border-line bg-surface px-3 py-2.5 text-left transition-colors ${
        empty ? "opacity-45 cursor-default" : "hover:bg-elevated hover:border-accent/40"
      }`}
    >
      <span
        className={`text-[20px] font-bold font-mono tabular-nums ${
          tone === "amber" ? "text-amber" : tone === "accent" ? "text-accent" : "text-heading"
        }`}
      >
        {count}
      </span>
      <span className="flex-1 min-w-0">
        <span className="block text-[13px] text-heading">{title}</span>
        <span className="block text-[11px] text-faint truncate">{sub}</span>
      </span>
      <span className="text-[13px] text-accent shrink-0">{action}</span>
    </button>
  );
}
