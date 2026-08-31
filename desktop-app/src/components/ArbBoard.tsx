import { useState, useMemo } from "react";
import { useTranslation } from "react-i18next";
import { useArbBoard } from "../hooks/useArbBoard";
import type { ArbOpenHypothesis, ArbSettledHypothesis, ArbOutcome } from "../types/arb";

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
    const arr = [...(data?.open ?? [])];
    // 需求 3.2：按价差 |price_diff| DESC；按年化时 null 排末位。
    if (sort === "diff") {
      arr.sort((a, b) => Math.abs(b.price_diff) - Math.abs(a.price_diff));
    } else {
      arr.sort((a, b) => {
        if (a.annualized === null && b.annualized === null) return 0;
        if (a.annualized === null) return 1;
        if (b.annualized === null) return -1;
        return Math.abs(b.annualized) - Math.abs(a.annualized);
      });
    }
    return arr;
  }, [data, sort]);

  const settledItems = useMemo(() => data?.settled ?? [], [data]);

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

/** +0.5500 带符号 4 位小数（需求 3.3）。 */
function fmtSigned(v: number, digits: number): string {
  const s = v.toFixed(digits);
  return v >= 0 ? `+${s}` : s;
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
            {t("arb.annualizedShort")}: {item.annualized === null ? "—" : fmtSigned(item.annualized, 4)}
          </span>
          <span>{t("arb.ageDays", { days: item.age_days.toFixed(1) })}</span>
          <span className="font-mono">
            {t("arb.fairVsObserved", { fair: item.fair_value, observed: item.observed_value })}
          </span>
          <span className="text-green-400/80">✓{item.evidence.support}</span>
          <span className="text-red-400/80">✗{item.evidence.oppose}</span>
          {item.horizon_days !== null && (
            <span className="text-faint">{t("arb.horizonDays", { days: item.horizon_days })}</span>
          )}
        </div>
        {/* 证伪条件摘要（需求 3.3：截断） */}
        {item.falsification_triggers.length > 0 && !expanded && (
          <div className="mt-1 text-[10px] text-faint truncate">
            ⚡ {item.falsification_triggers[0]}
          </div>
        )}
      </div>

      {expanded && (
        <div className="px-3 pb-3 border-t border-line/50 pt-2.5 flex flex-col gap-2">
          {/* 证伪条件全文 */}
          <div>
            <div className="text-[10px] text-faint mb-0.5">⚡ {t("arb.falsification")}</div>
            {item.falsification_triggers.map((tr, i) => (
              <div key={i} className="text-[11px] text-body leading-relaxed">· {tr}</div>
            ))}
          </div>
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
        <span className={`text-[9px] px-1.5 py-0.5 rounded shrink-0 ${outcomeBadge[item.outcome]}`}>
          {t(`arb.outcome_${item.outcome}`)}
        </span>
        <span className="text-xs flex-1 truncate text-muted line-through">{item.title}</span>
        <span className="text-[10px] font-mono text-faint shrink-0">
          {t("arb.realized")}: {item.realized_value}
        </span>
        <span className="text-[10px] text-faint shrink-0">{item.settled_at.slice(0, 10)}</span>
      </div>
      {item.close_reason && (
        <div className="mt-1 text-[10px] text-faint truncate">{item.close_reason}</div>
      )}
    </div>
  );
}
