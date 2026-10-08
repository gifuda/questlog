import { useState, useMemo, useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { useArbBoard, submitArbControl, runArbDecision } from "../hooks/useArbBoard";
import Badge from "./Badge";
import type {
  ArbOpenHypothesis,
  ArbSettledHypothesis,
  ArbOutcome,
  ArbControl,
  ArbLead,
  ArbBrief,
  ArbDecisionItem,
} from "../types/arb";

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
        <span className="text-accent">{t("arb.summaryOpen", { count: summary.open })}</span>
        <span className="text-muted">{t("arb.summarySettled", { count: summary.settled })}</span>
        <span className="text-success">
          {t("arb.summaryHitRate")}: {summary.hit_rate === null ? "—" : `${Math.round(summary.hit_rate * 100)}%`}
        </span>
        <span className="text-amber">
          {t("arb.summaryBias")}: {summary.mean_bias === null ? "—" : fmtSigned(summary.mean_bias, 4)}
        </span>
        <span className="text-faint ml-auto">{t("arb.readOnlyHint")}</span>
      </div>

      {/* 自动化控制条（契约 v3 B 控制区；GROS-01 增量 #2 未落地时置灰） */}
      <ControlBar control={data.control} />

      {/* 晨报卡片（契约 v4；GROS 侧 brief/decisions 段未产出时置灰等待） */}
      <BriefingSection brief={data.brief} decisions={data.decisions} />

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
          <Badge tone="accent" className="shrink-0">
            {t("arb.statusOpen")}
          </Badge>
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
          <span className="text-success/80">✓{item.evidence?.support ?? 0}</span>
          <span className="text-danger/80">✗{item.evidence?.oppose ?? 0}</span>
          {item.horizon_days != null && (
            <span className="text-faint">{t("arb.horizonDays", { days: item.horizon_days })}</span>
          )}
        </div>
        {/* 证伪条件摘要（需求 3.3：截断） */}
        {Array.isArray(item.falsification_triggers) && item.falsification_triggers.length > 0 && !expanded && (
          <div className="mt-1 text-[11px] text-faint truncate">
            ⚡ {item.falsification_triggers[0]}
          </div>
        )}
      </div>

      {expanded && (
        <div className="px-3 pb-3 border-t border-line/50 pt-2.5 flex flex-col gap-2">
          {/* 证伪条件全文 */}
          {Array.isArray(item.falsification_triggers) && item.falsification_triggers.length > 0 && (
            <div>
              <div className="text-[11px] text-faint mb-0.5">⚡ {t("arb.falsification")}</div>
              {item.falsification_triggers.map((tr, i) => (
                <div key={i} className="text-[11px] text-body leading-relaxed">· {tr}</div>
              ))}
            </div>
          )}
          {/* 下一步 */}
          {item.next_action && (
            <div>
              <div className="text-[11px] text-faint mb-0.5">→ {t("arb.nextAction")}</div>
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
  const outcomeTone: Record<ArbOutcome, "success" | "danger" | "muted"> = {
    confirmed: "success",
    falsified: "danger",
    expired: "muted",
  };
  return (
    <div className="rounded-lg border border-line bg-surface/30 px-3 py-2 opacity-80">
      <div className="flex items-center gap-2">
        <Badge tone={outcomeTone[item.outcome] ?? "muted"} className="shrink-0">
          {t(`arb.outcome_${item.outcome}`)}
        </Badge>
        <span className="text-xs flex-1 truncate text-muted line-through">{item.title}</span>
        <span className="text-[10px] font-mono text-faint shrink-0">
          {t("arb.realized")}: {fmtNum(item.realized_value, 2)}
        </span>
        <span className="text-[10px] text-faint shrink-0">
          {typeof item.settled_at === "string" ? item.settled_at.slice(0, 10) : "—"}
        </span>
      </div>
      {item.close_reason && (
        <div className="mt-1 text-[11px] text-faint truncate">{item.close_reason}</div>
      )}
    </div>
  );
}

// ══════════════════════════════════════════════
// ControlBar — 自动化控制条（契约 v3 B 控制区）
// 渲染值 = board.json control 段回显（唯一真值）。
// 控件变更 → 参数卡经薄壳入账房 → 采集机 config-sync 应用 →
// 下一轮导出回显。防环纪律：UI 不以本地值覆盖回显——提交后进入
// pending 过渡态（显示「回显值 → 提交值 ⏳」），config-sync 应用后
// 回显追上，pending 自动清除；若采集机拒绝/未消费，pending 持续
// 显示「待应用」——那是真实的未生效状态，不是 UI 假成功。
// 连点防抖：同参数 1.2s 内只发最后一张卡（账房免噪音）。
// ══════════════════════════════════════════════
function ControlBar({ control }: { control?: ArbControl }) {
  const { t } = useTranslation();
  /** param → 已提交待回显的值（显示层 pending，非真值） */
  const [pending, setPending] = useState<Record<string, string>>({});
  const [error, setError] = useState<string | null>(null);
  const debounceRef = useRef<Record<string, ReturnType<typeof setTimeout>>>({});

  // 回显追上 → 清对应 pending（effect 依赖 control 各字段值）
  useEffect(() => {
    if (!control) return;
    setPending((p) => {
      const echoed: Record<string, string> = {
        current_model: control.current_model ?? "",
        autonomy: control.autonomy === true || control.autonomy === "on" ? "true" : "false",
        leads_per_run: String(control.leads_per_run ?? ""),
        research_quota_per_week: String(control.research_quota_per_week ?? ""),
      };
      const next = { ...p };
      let changed = false;
      for (const k of Object.keys(next)) {
        if (echoed[k] === next[k]) {
          delete next[k];
          changed = true;
        }
      }
      return changed ? next : p;
    });
  }, [control?.current_model, control?.autonomy, control?.leads_per_run, control?.research_quota_per_week]);

  if (!control) {
    // undefined = GROS-01 增量 #2 未落地 → 按约定置灰
    return (
      <div className="px-4 py-1.5 border-b border-line/50 text-[10px] text-faint/70 flex items-center gap-2">
        <span>🎛 {t("arb.controlTitle")}</span>
        <span className="px-1.5 py-px rounded bg-elevated text-faint/70">{t("arb.controlPending")}</span>
      </div>
    );
  }

  function submit(param: string, value: string) {
    // 乐观 pending：立即显示目标值（仅显示层；防环纪律不破坏）
    setPending((p) => ({ ...p, [param]: value }));
    // 同参数防抖：连点只发最后一张卡
    clearTimeout(debounceRef.current[param]);
    debounceRef.current[param] = setTimeout(async () => {
      try {
        await submitArbControl(param, value);
        setError(null);
      } catch (e) {
        // 薄壳写入失败 → 撤 pending、顶错误提示（6s 恢复）
        setPending((p) => {
          const n = { ...p };
          delete n[param];
          return n;
        });
        setError(String(e));
        setTimeout(() => setError(null), 6000);
      }
    }, 1200);
  }

  /** 显示值：有 pending 显示提交值（⏳ 样式由调用方判断），否则显示回显真值 */
  const disp = (param: string, echoed: string) => pending[param] ?? echoed;
  const isPending = (param: string) => param in pending;

  const models = Array.isArray(control.models) ? control.models : [];
  const echoedAutonomyOn = control.autonomy === true || control.autonomy === "on";
  const autonomyOn = disp("autonomy", echoedAutonomyOn ? "true" : "false") === "true";
  const autonomyPending = isPending("autonomy");
  const leadsVal = Number(disp("leads_per_run", String(control.leads_per_run ?? 3)));
  const quotaVal = Number(disp("research_quota_per_week", String(control.research_quota_per_week ?? 0)));
  const leadsPending = isPending("leads_per_run");
  const quotaPending = isPending("research_quota_per_week");
  const anyPending = Object.keys(pending).length > 0;

  const step = (param: string, cur: number, delta: number, lo: number, hi: number) =>
    submit(param, String(Math.min(hi, Math.max(lo, cur + delta))));

  return (
    <div className="px-4 py-1.5 border-b border-line/50 flex items-center gap-3 text-[11px] flex-wrap">
      <span className="text-faint shrink-0">🎛 {t("arb.controlTitle")}</span>

      {/* 雷达模型 */}
      <label className="flex items-center gap-1 shrink-0">
        <span className="text-muted">{t("arb.controlModel")}</span>
        <select
          className={`bg-elevated text-body rounded px-1.5 py-0.5 text-[11px] focus:outline-none disabled:opacity-40 ${
            isPending("current_model") ? "text-amber" : ""
          }`}
          value={disp("current_model", control.current_model ?? "")}
          disabled={models.length === 0}
          onChange={(e) => submit("current_model", e.target.value)}
        >
          {models.length === 0 && <option value="">—</option>}
          {models.map((m) => (
            <option key={m} value={m}>{m}</option>
          ))}
        </select>
      </label>

      {/* 自动探索（总闸）—— pending 时加 ⏳ 与琥珀色 */}
      <button
        className={`px-2 py-0.5 rounded-full font-medium transition-colors ${
          autonomyPending
            ? "bg-amber/15 text-amber"
            : autonomyOn
              ? "bg-success/15 text-success hover:bg-success/25"
              : "bg-danger/15 text-danger hover:bg-danger/25"
        }`}
        onClick={() => submit("autonomy", autonomyOn ? "false" : "true")}
        title={t("arb.controlAutonomyHint")}
      >
        {autonomyPending ? "⏳" : autonomyOn ? "▶" : "⏸"} {t("arb.controlAutonomy")} {autonomyOn ? "ON" : "OFF"}
      </button>

      {/* 线索上限步进器（1..=20，契约硬上限；pending 基准连点累加） */}
      <label className="flex items-center gap-1 shrink-0">
        <span className="text-muted">{t("arb.controlLeadsPerRun")}</span>
        <Stepper value={leadsVal} lo={1} hi={20} pending={leadsPending}
          onStep={(d) => step("leads_per_run", leadsVal, d, 1, 20)} />
      </label>

      {/* 每周自动深研配额步进器（0..=50） */}
      <label className="flex items-center gap-1 shrink-0">
        <span className="text-muted">{t("arb.controlQuota")}</span>
        <Stepper value={quotaVal} lo={0} hi={50} pending={quotaPending}
          onStep={(d) => step("research_quota_per_week", quotaVal, d, 0, 50)} />
      </label>

      {/* 提交反馈：错误 > 待应用 > 最后更新时间 */}
      {error ? (
        <span className="text-danger ml-auto text-[10px] truncate max-w-[280px]" title={error}>
          ✗ {error}
        </span>
      ) : anyPending ? (
        <span className="text-amber ml-auto text-[10px] shrink-0">{t("arb.controlSubmitted")}</span>
      ) : (
        control.updated_at && (
          <span className="text-faint/70 ml-auto text-[10px] shrink-0">{control.updated_at.replace("T", " ").slice(0, 16)}</span>
        )
      )}
    </div>
  );
}

/** 小步进器：− N ＋；pending 时数字琥珀色（已提交待回显）。 */
function Stepper({ value, lo, hi, pending, onStep }: {
  value: number; lo: number; hi: number; pending?: boolean; onStep: (delta: number) => void;
}) {
  return (
    <span className="inline-flex items-center gap-0.5">
      <button
        className="w-4 h-4 rounded bg-elevated text-faint hover:text-body hover:bg-line-hover disabled:opacity-30"
        disabled={value <= lo}
        onClick={() => onStep(-1)}
      >−</button>
      <span className={`w-7 text-center font-mono tabular-nums ${pending ? "text-amber" : "text-body"}`}>
        {value}{pending ? "⏳" : ""}
      </span>
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
        <span className="text-[11px] text-faint/70 ml-auto">{t("arb.leadsHint")}</span>
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
  const [copied, setCopied] = useState(false);
  // 优先级徽章：GROS-02 实测导出为中文"高/中/低"，兼容契约初稿英文枚举
  const p = lead.priority;
  const prioTone: "danger" | "amber" | "muted" =
    p === "high" || p === "高" ? "danger"
    : p === "medium" || p === "中" ? "amber"
    : "muted";

  /** "转研究"：复制一段可直接粘贴到 GROS 会话的研究请求（不直接写库，
   *  放行仍走既有审核流——gros research 由用户在会话内发起）。 */
  async function copyResearchPrompt() {
    const lines = [
      `请对以下侦察线索发起研究（gros research 流程）：`,
      `· 现象：${lead.phenomenon ?? "—"}`,
      `· 认知差：${lead.cognitive_gap ?? "—"}`,
      `· 可核数据：${lead.verifiable_data ?? "—"}`,
      `· 域：${lead.domain ?? "自由探索"}（来源 ${lead.conversation_id ?? "—"}，线索id ${lead.id.slice(0, 8)}）`,
      `请先 gros search 查重，再取证起草公允值/现值与证伪条件，落 hypothesis 前等我确认。`,
    ];
    try {
      await navigator.clipboard.writeText(lines.join("\n"));
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch { /* clipboard unavailable */ }
  }

  return (
    <div className="rounded-lg border border-line bg-surface/40 px-2.5 py-1.5">
      <div className="flex items-center gap-2">
        {lead.status === "promoted" ? (
          <Badge tone="success" className="shrink-0"
            title={lead.promoted_hypothesis_id ? `${t("arb.leadPromoted")}: ${lead.promoted_hypothesis_id}` : t("arb.leadPromoted")}>
            ✓ {t("arb.leadPromoted")}
          </Badge>
        ) : (
          <Badge tone={prioTone} className="shrink-0">
            {lead.priority ?? "?"}
          </Badge>
        )}
        <span className="text-[11px] flex-1 truncate text-body" title={lead.phenomenon ?? ""}>
          {lead.phenomenon ?? "—"}
        </span>
        {lead.domain && (
          <span className="text-[11px] px-1.5 py-px rounded bg-accent/10 text-accent shrink-0">
            {lead.domain}
          </span>
        )}
        <span className="text-[11px] text-faint shrink-0"
          title={`${t("arb.leadCreatedAt")}: ${lead.created_at ?? "—"}\n${lead.conversation_id ?? ""}`}>
          {lead.date ?? "—"}
        </span>
      </div>
      <div className="mt-0.5 flex items-center gap-3 text-[11px] text-muted flex-wrap">
        <span title={t("arb.leadCognitiveGap")}>
          💡 {lead.cognitive_gap ?? "—"}
        </span>
        <span className="font-mono text-faint" title={t("arb.leadVerifiableData")}>
          📌 {lead.verifiable_data ?? "—"}
        </span>
        {lead.status === "open" && (
          <button
            className="ml-auto text-[10px] px-1.5 py-px rounded bg-accent/15 text-accent hover:bg-accent/25 shrink-0"
            onClick={copyResearchPrompt}
            title={t("arb.leadToResearchHint")}
          >
            {copied ? t("arb.copied") : t("arb.leadToResearch")}
          </button>
        )}
      </div>
    </div>
  );
}

// ══════════════════════════════════════════════
// BriefingSection — 晨报卡片（契约 v4，2026-10-08）
// brief 段（概要 + 当日机会橱窗）+ decisions 段（D# 决策清单，点击处置）。
// 处置 = 本地执行 GROS 预生成的 cmd 草稿（app 不组装命令），白名单硬约束
// 在 Rust 侧 run_arb_decision 统一把守（唯一执行口）。写走薄壳（账房留痕），
// 读走下一轮 board.json 导出——已处置项自然消失，与控制区同一单向双通道。
// 降级铁律：brief/decisions 两段都 undefined（契约 v4 前旧数据）→ 整卡置灰
// 「等待 GROS 侧数据」；字段级 null 一律渲染 "—"，绝不崩溃。
// ══════════════════════════════════════════════
function BriefingSection({ brief, decisions }: { brief?: ArbBrief; decisions?: ArbDecisionItem[] }) {
  const { t } = useTranslation();

  if (brief === undefined && decisions === undefined) {
    // 旧数据（GROS 侧晨报段未产出）→ 按约定整卡置灰
    return (
      <div className="px-4 py-1.5 border-b border-line/50 text-[10px] text-faint/70 flex items-center gap-2 shrink-0">
        <span>📰 {t("arb.briefTitle")}</span>
        <span className="px-1.5 py-px rounded bg-elevated text-faint/70">{t("arb.briefWaiting")}</span>
      </div>
    );
  }

  // 字段级 null 容忍：数组非数组一律按空处理（外部导出不可信）
  const opps = Array.isArray(brief?.opportunities) ? brief!.opportunities! : [];
  const items = Array.isArray(decisions) ? decisions : [];

  return (
    <div className="px-3 pt-2.5 pb-2 border-b border-line/50 shrink-0 flex flex-col gap-2">
      {/* brief 段：概要一行 + 当日机会橱窗 */}
      <div>
        <div className="flex items-center gap-2 mb-1">
          <span className="text-[11px] font-medium text-heading">📰 {t("arb.briefTitle")}</span>
          {brief?.brief_date && (
            <span className="text-[10px] text-faint" title={t("arb.briefDate")}>
              {brief.brief_date}
            </span>
          )}
          <span className="text-[11px] text-faint/70 ml-auto shrink-0">{t("arb.briefHint")}</span>
        </div>
        {brief?.headline && (
          <div className="text-[11px] text-body truncate mb-1" title={brief.headline}>
            {brief.headline}
          </div>
        )}
        {opps.length > 0 && (
          <div className="flex flex-col gap-1">
            {opps.map((o, i) => (
              <div key={i} className="flex items-center gap-1.5 min-w-0">
                <Badge tone={leadPriorityTone(o.priority)} className="shrink-0">
                  {o.priority ?? "?"}
                </Badge>
                <span className="text-[11px] text-body truncate min-w-0" title={o.title ?? ""}>
                  {o.title ?? "—"}
                </span>
              </div>
            ))}
          </div>
        )}
      </div>

      {/* decisions 段：D# 决策清单（cmd 非空的项才有按钮） */}
      {items.length > 0 && (
        <div>
          <div className="flex items-center gap-2 mb-1">
            <span className="text-[11px] font-medium text-heading">☑ {t("arb.decisionsTitle")}</span>
            <span className="text-[10px] text-faint">{items.length}</span>
            <span className="text-[11px] text-faint/70 ml-auto shrink-0">{t("arb.decisionHint")}</span>
          </div>
          <div className="flex flex-col gap-1.5">
            {items.map((d, i) => (
              <DecisionRow key={d.id ?? `idx-${i}`} item={d} />
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

/** 优先级徽章 tone：兼容中文"高/中/低"与契约初稿英文枚举（同 LeadCard）。 */
function leadPriorityTone(p: string | null | undefined): "danger" | "amber" | "muted" {
  if (p === "high" || p === "高") return "danger";
  if (p === "medium" || p === "中") return "amber";
  return "muted";
}

/** 截止临期判定：今天起 3 天内（含已逾期）→ true；日期缺失/不可解析 → false。 */
function isDeadlineNear(deadline: string | null | undefined): boolean {
  if (!deadline || typeof deadline !== "string") return false;
  const m = /^(\d{4})-(\d{2})-(\d{2})/.exec(deadline.trim());
  if (!m) return false;
  const dl = new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3])).getTime();
  if (Number.isNaN(dl)) return false;
  const now = new Date();
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  const DAY = 24 * 60 * 60 * 1000;
  return dl - today <= 3 * DAY;
}

// ══════════════════════════════════════════════
// DecisionRow — 单条决策（D# 编号制）
// 按钮态机：idle → running（禁用）→ done（已处置 ✓，双按钮禁用）；
// 失败：错误输出原样显示、不自动重试（按钮保留，用户可自行再点——
// "不重试"指 app 不自动重试，禁死会让环境类失败无法人工恢复）。
// 仅 cmd 非空的项渲染对应按钮；cmd 均空 = 系统事项，纯展示。
// ══════════════════════════════════════════════
function DecisionRow({ item: d }: { item: ArbDecisionItem }) {
  const { t } = useTranslation();
  const [state, setState] = useState<"idle" | "running" | "done">("idle");
  const [error, setError] = useState<string | null>(null);

  async function run(kind: "approve" | "reject") {
    const cmd = kind === "approve" ? d.cmd_approve : d.cmd_reject;
    if (!cmd || state === "running" || state === "done") return;
    setState("running");
    setError(null);
    try {
      await runArbDecision(cmd);
      setState("done");
    } catch (e) {
      // 失败原样显示错误输出；不自动重试
      setState("idle");
      setError(String(e));
    }
  }

  const canApprove = d.cmd_approve != null && d.cmd_approve !== "";
  const canReject = d.cmd_reject != null && d.cmd_reject !== "";
  const deadlineNear = isDeadlineNear(d.deadline);
  const busy = state === "running";

  return (
    <div className={`rounded-lg border px-2.5 py-1.5 ${state === "done" ? "border-success/30 bg-success/5" : "border-line bg-surface/40"}`}>
      <div className="flex items-center gap-2 min-w-0">
        <Badge tone={state === "done" ? "success" : "accent"} className="shrink-0 font-mono">
          {d.id ?? "?"}
        </Badge>
        <span className="text-[11px] flex-1 truncate text-body min-w-0" title={d.item ?? ""}>
          {d.item ?? "—"}
        </span>
        {/* 已挂 N 天角标（pending_days） */}
        {typeof d.pending_days === "number" && !Number.isNaN(d.pending_days) && (
          <span className="text-[10px] text-faint shrink-0" title={t("arb.decisionPendingDays")}>
            {t("arb.decisionPendingDays", { days: d.pending_days })}
          </span>
        )}
      </div>
      <div className="mt-0.5 flex items-center gap-2 text-[11px] text-muted flex-wrap min-w-0">
        <span className="truncate min-w-0 flex-1" title={d.advice ?? ""}>
          💡 {d.advice ?? "—"}
        </span>
        {d.deadline && (
          <span
            className={`shrink-0 ${deadlineNear ? "text-danger font-medium" : "text-faint"}`}
            title={t("arb.decisionDeadline")}
          >
            ⏰ {t("arb.decisionDeadline")}: {d.deadline}
          </span>
        )}
        {/* 处置按钮（仅 cmd 非空的项；已处置显示 ✓ 徽章） */}
        {state === "done" ? (
          <Badge tone="success" className="shrink-0">✓ {t("arb.decisionDone")}</Badge>
        ) : (
          <>
            {canApprove && (
              <button
                className="text-[10px] px-2 py-0.5 rounded bg-success/15 text-success hover:bg-success/25 disabled:opacity-40 shrink-0"
                disabled={busy}
                onClick={() => run("approve")}
                title={d.cmd_approve ?? ""}
              >
                {busy ? "⏳" : t("arb.decisionApprove")}
              </button>
            )}
            {canReject && (
              <button
                className="text-[10px] px-2 py-0.5 rounded bg-danger/15 text-danger hover:bg-danger/25 disabled:opacity-40 shrink-0"
                disabled={busy}
                onClick={() => run("reject")}
                title={d.cmd_reject ?? ""}
              >
                {busy ? "⏳" : t("arb.decisionReject")}
              </button>
            )}
          </>
        )}
      </div>
      {/* 失败：错误输出原样显示（不重试） */}
      {error && (
        <div className="mt-1 text-[10px] text-danger whitespace-pre-wrap break-all max-h-24 overflow-y-auto" title={error}>
          ✗ {error}
        </div>
      )}
    </div>
  );
}
