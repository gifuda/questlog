import { useState, useMemo } from "react";
import { useTranslation } from "react-i18next";
import { useArbBoard, submitArbControl } from "../hooks/useArbBoard";
import type { ArbOpenHypothesis, ArbSettledHypothesis, ArbOutcome, ArbControl, ArbLead } from "../types/arb";

type ArbFilter = "all" | "open" | "settled";
type ArbSort = "diff" | "annualized";

/**
 * 套利机会栏 — GROS 假设库机会面的只读视图（app 级全局功能，不按项目区分）。
 * 纯只读：状态变更只能经 GROS CLI（复制 settle_hint 到终端执行）。
 */
export default function ArbBoard() {
  const { t } = useTranslation();
  const { data, loading, refresh } = useArbBoard();
  const [filter, setFilter] = useState<ArbFilter>("all");
  const [sort, setSort] = useState<ArbSort>("diff");

  const openItems = useMemo(() => {
    const arr = [...(Array.isArray(data?.open) ? data!.open : [])];
    // 数值兜底：null/缺失排末位（与年化排序规则一致）。
    const abs = (v: number | null | undefined): number =>
      typeof v === "number" && !Number.isNaN(v) ? Math.abs(v) : -1;
    // 需求 3.2：按价差 |price_diff| DESC；按年化时 null 排末位。
    if (sort === "diff") {
      arr.sort((a, b) => abs(b.price_diff) - abs(a.price_diff));
    } else {
      arr.sort((a, b) => abs(b.annualized) - abs(a.annualized));
    }
    return arr;
  }, [data, sort]);

  const settledItems = useMemo(
    () => (Array.isArray(data?.settled) ? data!.settled : []),
    [data]
  );

  if (loading && !data) {
    return <div className="flex items-center justify-center h-full text-faint text-sm">…</div>;
  }

  // ── 空态（需求 4.2）：off / missing / parse_error 都友好降级 ──
  if (!data || data.status === "off") {
    return (
      <div className="flex flex-col items-center justify-center h-full text-faint gap-2 px-6 text-center">
        <span className="text-3xl">📈</span>
        <span className="text-sm">{t("arb.offTitle")}</span>
        <span className="text-[11px] leading-relaxed">{t("arb.offDesc")}</span>
      </div>
    );
  }
  if (data.status === "missing" || data.status === "parse_error") {
    return (
      <div className="flex flex-col items-center justify-center h-full text-faint gap-2 px-6 text-center">
        <span className="text-3xl">📈</span>
        <span className="text-sm">{t("arb.emptyTitle")}</span>
        <span className="text-[11px] leading-relaxed whitespace-pre-line">
          {data.status === "missing"
            ? t("arb.emptyMissing", { path: data.board_path ?? "exports/board.json" })
            : t("arb.emptyParseError", { path: data.board_path ?? "", error: data.error ?? "" })}
        </span>
        <button className="text-[11px] text-accent hover:underline" onClick={() => refresh()}>
          {t("arb.retry")}
        </button>
      </div>
    );
  }

  const summary = data.summary ?? { open: 0, settled: 0, hit_rate: null, mean_bias: null, bias_samples: 0 };
  const showOpen = filter === "all" || filter === "open";
  const showSettled = filter === "all" || filter === "settled";

  return (
    <div className="flex flex-col h-full">
      {/* 汇总条（需求 3.5） */}
      <div className="px-4 py-2.5 border-b border-line bg-surface/30 flex items-center gap-3 text-[11px] text-muted flex-wrap">
        <span className="font-bold text-heading">📈 {t("arb.title")}</span>
        <span className="text-blue-400">{t("arb.summaryOpen", { count: summary.open })}</span>
        <span className="text-gray-400">{t("arb.summarySettled", { count: summary.settled })}</span>
        <span className="text-green-400">
          {t("arb.summaryHitRate")}: {summary.hit_rate === null ? "—" : `${Math.round(summary.hit_rate * 100)}%`}
        </span>
        <span className="text-yellow-400">
          {t("arb.summaryBias")}: {summary.mean_bias === null ? "—" : fmtSigned(summary.mean_bias, 4)}
        </span>
        <span className="text-faint ml-auto">{t("arb.readOnlyHint")}</span>
      </div>

      {/* 自动化控制条（契约 v3 B 控制区；GROS-01 增量 #2 未落地时置灰） */}
      <ControlBar control={data.control} />

      {/* 过滤 + 排序 */}
      <div className="px-4 py-2 border-b border-line flex items-center gap-2">
        {(["all", "open", "settled"] as ArbFilter[]).map((f) => (
          <button
            key={f}
            className={`text-[11px] px-2.5 py-1 rounded-full transition-colors ${
              filter === f ? "bg-accent text-white" : "text-muted hover:bg-elevated"
            }`}
            onClick={() => setFilter(f)}
          >
            {t(`arb.filter_${f}`)}
          </button>
        ))}
        <div className="ml-auto flex items-center gap-1.5">
          <span className="text-[10px] text-faint">{t("arb.sortBy")}</span>
          {(["diff", "annualized"] as ArbSort[]).map((s) => (
            <button
              key={s}
              className={`text-[11px] px-2 py-0.5 rounded transition-colors ${
                sort === s ? "bg-surface text-heading" : "text-faint hover:text-body"
              }`}
              onClick={() => setSort(s)}
            >
              {t(`arb.sort_${s}`)}
            </button>
          ))}
        </div>
      </div>

      {/* 侦察线索（契约 v3 A 线索段；GROS-01 增量 #2 未落地时置灰） */}
      <LeadsSection leads={data.leads} />

      {/* 卡片列表 */}
      <div className="flex-1 overflow-y-auto p-3 flex flex-col gap-2">
        {showOpen && openItems.length === 0 && (
          <div className="text-[11px] text-faint text-center py-4">{t("arb.noOpen")}</div>
        )}
        {showOpen && openItems.map((h) => (
          <OpenCard key={h.id} item={h} />
        ))}
        {showSettled && settledItems.length === 0 && filter === "settled" && (
          <div className="text-[11px] text-faint text-center py-4">{t("arb.noSettled")}</div>
        )}
        {showSettled && settledItems.map((h) => (
          <SettledCard key={h.id} item={h} />
        ))}
      </div>
    </div>
  );
}

/**
 * 数值显示兜底：board.json 是 GROS 的外部导出，任何数值字段都可能出现
 * null/缺失（如实测中出现过 price_diff/fair_value 为 null 的假设）。
 * 契约类型标 number 但运行时必须防 null——坏一个字段只显示 "—"，
 * 绝不让整张卡片（进而整个面板）崩溃。
 */
function fmtSigned(v: number | null | undefined, digits = 4): string {
  if (v === null || v === undefined || typeof v !== "number" || Number.isNaN(v)) return "—";
  const s = v.toFixed(digits);
  return v >= 0 ? `+${s}` : s;
}

function fmtNum(v: number | null | undefined, digits = 1): string {
  if (v === null || v === undefined || typeof v !== "number" || Number.isNaN(v)) return "—";
  return v.toFixed(digits);
}

// ══════════════════════════════════════════════
// OpenCard — open 假设卡片（可展开）
// ══════════════════════════════════════════════
function OpenCard({ item }: { item: ArbOpenHypothesis }) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(false);
  const [copied, setCopied] = useState(false);

  async function copySettle() {
    try {
      await navigator.clipboard.writeText(item.settle_hint);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch { /* clipboard unavailable — ignore */ }
  }

  return (
    <div className="rounded-lg border border-line bg-surface/50 hover:border-accent/30 transition-colors">
      <div className="px-3 py-2.5 cursor-pointer" onClick={() => setExpanded(!expanded)}>
        <div className="flex items-center gap-2">
          <span className="text-[9px] px-1.5 py-0.5 rounded bg-blue-500/15 text-blue-300 shrink-0">
            {t("arb.statusOpen")}
          </span>
          <span className="text-xs font-medium flex-1 truncate text-heading">{item.title}</span>
          <span className="text-xs font-mono font-bold text-heading shrink-0">
            {fmtSigned(item.price_diff, 4)}
          </span>
          <span className="text-[10px] text-faint shrink-0">{expanded ? "▾" : "▸"}</span>
        </div>
        <div className="mt-1.5 flex items-center gap-3 text-[10px] text-muted flex-wrap">
          <span title={t("arb.annualized")}>
            {t("arb.annualizedShort")}: {fmtSigned(item.annualized)}
          </span>
          <span>{t("arb.ageDays", { days: fmtNum(item.age_days) })}</span>
          <span className="font-mono">
            {t("arb.fairVsObserved", { fair: fmtNum(item.fair_value, 2), observed: fmtNum(item.observed_value, 2) })}
          </span>
          <span className="text-green-400/80">✓{item.evidence?.support ?? 0}</span>
          <span className="text-red-400/80">✗{item.evidence?.oppose ?? 0}</span>
          {item.horizon_days != null && (
            <span className="text-faint">{t("arb.horizonDays", { days: item.horizon_days })}</span>
          )}
        </div>
        {/* 证伪条件摘要（需求 3.3：截断） */}
        {Array.isArray(item.falsification_triggers) && item.falsification_triggers.length > 0 && !expanded && (
          <div className="mt-1 text-[10px] text-faint truncate">
            ⚡ {item.falsification_triggers[0]}
          </div>
        )}
      </div>

      {expanded && (
        <div className="px-3 pb-3 border-t border-line/50 pt-2.5 flex flex-col gap-2">
          {/* 证伪条件全文 */}
          {Array.isArray(item.falsification_triggers) && item.falsification_triggers.length > 0 && (
            <div>
              <div className="text-[10px] text-faint mb-0.5">⚡ {t("arb.falsification")}</div>
              {item.falsification_triggers.map((tr, i) => (
                <div key={i} className="text-[11px] text-body leading-relaxed">· {tr}</div>
              ))}
            </div>
          )}
          {/* 下一步 */}
          {item.next_action && (
            <div>
              <div className="text-[10px] text-faint mb-0.5">→ {t("arb.nextAction")}</div>
              <div className="text-[11px] text-body leading-relaxed">{item.next_action}</div>
            </div>
          )}
          {/* 复制结算命令（需求 3.7） */}
          <div className="flex items-center gap-2">
            <code className="flex-1 text-[10px] font-mono text-muted bg-elevated/50 rounded px-2 py-1 truncate">
              {item.settle_hint}
            </code>
            <button
              className="text-[10px] px-2 py-1 rounded bg-accent text-white hover:opacity-90 shrink-0"
              onClick={(e) => { e.stopPropagation(); copySettle(); }}
            >
              {copied ? t("arb.copied") : t("arb.copySettle")}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

// ══════════════════════════════════════════════
// SettledCard — 已结算假设卡片
// ══════════════════════════════════════════════
function SettledCard({ item }: { item: ArbSettledHypothesis }) {
  const { t } = useTranslation();
  const outcomeBadge: Record<ArbOutcome, string> = {
    confirmed: "bg-green-500/15 text-green-300",
    falsified: "bg-red-500/15 text-red-300",
    expired: "bg-gray-500/15 text-gray-400",
  };
  return (
    <div className="rounded-lg border border-line bg-surface/30 px-3 py-2 opacity-80">
      <div className="flex items-center gap-2">
        <span className={`text-[9px] px-1.5 py-0.5 rounded shrink-0 ${outcomeBadge[item.outcome] ?? "bg-gray-500/15 text-gray-400"}`}>
          {t(`arb.outcome_${item.outcome}`)}
        </span>
        <span className="text-xs flex-1 truncate text-muted line-through">{item.title}</span>
        <span className="text-[10px] font-mono text-faint shrink-0">
          {t("arb.realized")}: {fmtNum(item.realized_value, 2)}
        </span>
        <span className="text-[10px] text-faint shrink-0">
          {typeof item.settled_at === "string" ? item.settled_at.slice(0, 10) : "—"}
        </span>
      </div>
      {item.close_reason && (
        <div className="mt-1 text-[10px] text-faint truncate">{item.close_reason}</div>
      )}
    </div>
  );
}

// ══════════════════════════════════════════════
// ControlBar — 自动化控制条（契约 v3 B 控制区）
// 渲染值 = board.json control 段回显（唯一真值）。
// 控件变更 → 参数卡经薄壳入账房 → 采集机 config-sync 应用 →
// 下一轮导出回显。UI 显示"已提交"但不本地改值（防环，契约 v3）。
// ══════════════════════════════════════════════
function ControlBar({ control }: { control?: ArbControl }) {
  const { t } = useTranslation();
  const [submitted, setSubmitted] = useState<string | null>(null);

  if (!control) {
    // undefined = GROS-01 增量 #2 未落地 → 按约定置灰
    return (
      <div className="px-4 py-1.5 border-b border-line/50 text-[10px] text-faint/70 flex items-center gap-2">
        <span>🎛 {t("arb.controlTitle")}</span>
        <span className="px-1.5 py-px rounded bg-elevated text-faint/70">{t("arb.controlPending")}</span>
      </div>
    );
  }

  async function submit(param: string, value: string) {
    try {
      await submitArbControl(param, value);
      setSubmitted(param);
      setTimeout(() => setSubmitted((s) => (s === param ? null : s)), 4000);
    } catch (e) {
      // 薄壳写入失败（gros 缺失/账房错误）——把错误顶到标题提示，2.5s 恢复
      setSubmitted(`__error__:${String(e)}`);
      setTimeout(() => setSubmitted(null), 6000);
    }
  }

  const model = control.current_model ?? "";
  const models = Array.isArray(control.models) ? control.models : [];
  const quota = control.research_quota_per_week ?? 0;
  const step = (param: string, cur: number, delta: number, lo: number, hi: number) =>
    submit(param, String(Math.min(hi, Math.max(lo, cur + delta))));

  return (
    <div className="px-4 py-1.5 border-b border-line/50 flex items-center gap-3 text-[11px] flex-wrap">
      <span className="text-faint shrink-0">🎛 {t("arb.controlTitle")}</span>

      {/* 雷达模型 */}
      <label className="flex items-center gap-1 shrink-0">
        <span className="text-muted">{t("arb.controlModel")}</span>
        <select
          className="bg-elevated text-body rounded px-1.5 py-0.5 text-[11px] focus:outline-none disabled:opacity-40"
          value={model}
          disabled={models.length === 0}
          onChange={(e) => submit("current_model", e.target.value)}
        >
          {models.length === 0 && <option value="">—</option>}
          {models.map((m) => (
            <option key={m} value={m}>{m}</option>
          ))}
        </select>
      </label>

      {/* 自动探索（总闸） */}
      <button
        className={`px-2 py-0.5 rounded-full font-medium transition-colors ${
          control.autonomy
            ? "bg-success/15 text-success hover:bg-success/25"
            : "bg-danger/15 text-danger hover:bg-danger/25"
        }`}
        onClick={() => submit("autonomy", control.autonomy ? "false" : "true")}
        title={t("arb.controlAutonomyHint")}
      >
        {control.autonomy ? "▶" : "⏸"} {t("arb.controlAutonomy")} {control.autonomy ? "ON" : "OFF"}
      </button>

      {/* 线索上限步进器（1..=20，契约硬上限） */}
      <label className="flex items-center gap-1 shrink-0">
        <span className="text-muted">{t("arb.controlLeadsPerRun")}</span>
        <Stepper value={control.leads_per_run ?? 3} lo={1} hi={20}
          onStep={(d) => step("leads_per_run", control.leads_per_run ?? 3, d, 1, 20)} />
      </label>

      {/* 每周自动深研配额步进器（0..=50） */}
      <label className="flex items-center gap-1 shrink-0">
        <span className="text-muted">{t("arb.controlQuota")}</span>
        <Stepper value={quota} lo={0} hi={50}
          onStep={(d) => step("research_quota_per_week", quota, d, 0, 50)} />
      </label>

      {/* 提交反馈 */}
      {submitted?.startsWith("__error__") ? (
        <span className="text-danger ml-auto text-[10px] truncate max-w-[280px]" title={submitted.slice(10)}>
          ✗ {submitted.slice(10)}
        </span>
      ) : submitted ? (
        <span className="text-success ml-auto text-[10px] shrink-0">{t("arb.controlSubmitted")}</span>
      ) : (
        control.updated_at && (
          <span className="text-faint/70 ml-auto text-[10px] shrink-0">{control.updated_at.replace("T", " ").slice(0, 16)}</span>
        )
      )}
    </div>
  );
}

/** 小步进器：− N ＋，点击即提交。 */
function Stepper({ value, lo, hi, onStep }: {
  value: number; lo: number; hi: number; onStep: (delta: number) => void;
}) {
  return (
    <span className="inline-flex items-center gap-0.5">
      <button
        className="w-4 h-4 rounded bg-elevated text-faint hover:text-body hover:bg-line-hover disabled:opacity-30"
        disabled={value <= lo}
        onClick={() => onStep(-1)}
      >−</button>
      <span className="w-6 text-center font-mono tabular-nums text-body">{value}</span>
      <button
        className="w-4 h-4 rounded bg-elevated text-faint hover:text-body hover:bg-line-hover disabled:opacity-30"
        disabled={value >= hi}
        onClick={() => onStep(1)}
      >＋</button>
    </span>
  );
}

// ══════════════════════════════════════════════
// LeadsSection — 侦察线索（契约 v3 A 线索段）
// 只读：放行经既有审核流（gros research），app 不写线索状态。
// dismissed 默认隐藏（契约）；promoted 显示"已放行"徽章。
// ══════════════════════════════════════════════
function LeadsSection({ leads }: { leads?: ArbLead[] }) {
  const { t } = useTranslation();

  // hooks 规则：必须在 early return（置灰分支）之前调用
  const visible = useMemo(() => {
    if (!leads) return [];
    const arr = leads.filter((l) => l.status !== "dismissed");
    // 契约：按 date DESC；null 排末位（防御性排序，导出已排）
    return [...arr].sort((a, b) => (b.date ?? "").localeCompare(a.date ?? ""));
  }, [leads]);

  if (leads === undefined) {
    // undefined = GROS-01 增量 #2 未落地 → 按约定置灰
    return (
      <div className="px-4 py-2 border-b border-line/50 text-[10px] text-faint/70 flex items-center gap-2 shrink-0">
        <span>🛰 {t("arb.leadsTitle")}</span>
        <span className="px-1.5 py-px rounded bg-elevated text-faint/70">{t("arb.controlPending")}</span>
      </div>
    );
  }

  return (
    <div className="px-3 pt-2.5 pb-1 shrink-0">
      <div className="flex items-center gap-2 mb-1.5">
        <span className="text-[11px] font-medium text-heading">🛰 {t("arb.leadsTitle")}</span>
        <span className="text-[10px] text-faint">{visible.length}</span>
        <span className="text-[9px] text-faint/70 ml-auto">{t("arb.leadsHint")}</span>
      </div>
      {visible.length === 0 ? (
        <div className="text-[10px] text-faint py-1">{t("arb.noLeads")}</div>
      ) : (
        <div className="flex flex-col gap-1.5 max-h-56 overflow-y-auto">
          {visible.map((lead) => (
            <LeadCard key={lead.id} lead={lead} />
          ))}
        </div>
      )}
    </div>
  );
}

function LeadCard({ lead }: { lead: ArbLead }) {
  const { t } = useTranslation();
  const prioBadge =
    lead.priority === "high" ? "bg-red-500/15 text-red-300"
    : lead.priority === "medium" ? "bg-yellow-500/15 text-yellow-300"
    : "bg-gray-500/15 text-gray-400";

  return (
    <div className="rounded-lg border border-line bg-surface/40 px-2.5 py-1.5">
      <div className="flex items-center gap-2">
        {lead.status === "promoted" ? (
          <span className="text-[9px] px-1.5 py-px rounded bg-success/15 text-success shrink-0">
            ✓ {t("arb.leadPromoted")}
          </span>
        ) : (
          <span className={`text-[9px] px-1.5 py-px rounded shrink-0 ${prioBadge}`}>
            {lead.priority ?? "?"}
          </span>
        )}
        <span className="text-[11px] flex-1 truncate text-body" title={lead.phenomenon ?? ""}>
          {lead.phenomenon ?? "—"}
        </span>
        {lead.domain && (
          <span className="text-[9px] px-1.5 py-px rounded bg-accent/10 text-accent shrink-0">
            {lead.domain}
          </span>
        )}
        <span className="text-[9px] text-faint shrink-0">{lead.date ?? "—"}</span>
      </div>
      <div className="mt-0.5 flex items-center gap-3 text-[10px] text-muted flex-wrap">
        <span title={t("arb.leadCognitiveGap")}>
          💡 {lead.cognitive_gap ?? "—"}
        </span>
        <span className="font-mono text-faint" title={t("arb.leadDataPoint")}>
          📌 {lead.data_point ?? "—"}
        </span>
      </div>
    </div>
  );
}
