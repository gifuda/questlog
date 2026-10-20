import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { useInfraSnapshot } from "../hooks/useInfraSnapshot";
import {
  DSH_DAILY_GATE,
  dshPct,
  fmtPct,
  fmtSecs,
  healthItems,
  parseSnapshot,
  providerList,
  queueChips,
  recentList,
  taskStatusLabelKey,
  taskStatusTone,
  quotaHueColor,
  stalenessState,
  toneOf,
} from "../infraLogic";
import type { HealthTone } from "../infraLogic";
import type {
  InfraBase,
  InfraProvider,
  InfraQuotaWindow,
  InfraSnapshotFile,
} from "../types/infra";

/** HealthTone → 点/文字/徽章色（配色全走 index.css 既有 token）。 */
const dotCls: Record<HealthTone, string> = {
  success: "bg-success",
  amber: "bg-amber",
  danger: "bg-danger",
  muted: "bg-faint",
};
const toneTextCls: Record<HealthTone, string> = {
  success: "text-success",
  amber: "text-amber",
  danger: "text-danger",
  muted: "text-faint",
};

/** fetched_at（ISO UTC）→ 本地 MM-DD HH:mm；不可解析 → null。 */
function fmtFetched(iso: string | null | undefined): string | null {
  if (!iso || typeof iso !== "string") return null;
  const ts = Date.parse(iso);
  if (Number.isNaN(ts)) return null;
  return new Date(ts).toLocaleString("zh-CN", {
    hour12: false,
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/**
 * 服务器监控 — 家庭服务器 agent-base snapshot-local.json 的只读视图
 * （app 级全局功能，不按项目区分；命名对齐 ArbBoard/FleetHealth）。
 * P0 = F1 额度卡 / F2 底座状态卡 / F3 健康指示行 / F4 手动刷新 /
 * F5 未就绪降级（文件缺失/JSON 损坏/fetched_at 超 15 分钟 → 横幅不白屏）。
 */
export default function InfraBoard() {
  const { t } = useTranslation();
  const { envelope, loaded, refresh } = useInfraSnapshot();

  // 信封 ok 时才解析 body；解析失败（防御兜底）→ null → 未就绪
  const file = useMemo<InfraSnapshotFile | null>(
    () => (envelope?.status === "ok" ? parseSnapshot(envelope.data) : null),
    [envelope],
  );
  const staleness = stalenessState(file?.fetched_at);

  // ── F5 未就绪成因（null = 就绪）──
  const notReadyCause: string | null =
    envelope === null
      ? t("infra.unreachableDesc")
      : envelope.status === "missing"
        ? t("infra.missingDesc", { path: envelope.snapshot_path ?? "—" })
        : envelope.status === "parse_error"
          ? t("infra.parseErrorDesc", { error: envelope.error ?? "—" })
          : file === null
            ? t("infra.parseErrorDesc", { error: "body" })
            : staleness === "stale"
              ? t("infra.staleDesc")
              : staleness === "unknown"
                ? t("infra.noTimestampDesc")
                : null;

  const lastAtText = file?.fetched_at
    ? t("infra.lastAt", { time: fmtFetched(file.fetched_at) ?? file.fetched_at })
    : t("infra.lastUnknown");

  if (!loaded) {
    return <div className="flex items-center justify-center h-full text-faint text-sm">…</div>;
  }

  // 页头（含 F4 手动刷新按钮，任何状态下常驻）
  const header = (
    <div className="px-4 py-2.5 border-b border-line bg-surface/30 flex items-center gap-3 text-[11px] flex-wrap shrink-0">
      <span className="text-[13px] font-bold text-heading">🖥 {t("infra.title")}</span>
      <span className="text-faint">{t("infra.readOnlyHint")}</span>
      <span className="ml-auto text-faint tabular-nums" title={file?.fetched_at ?? ""}>
        {file?.fetched_at
          ? t("infra.fetchedAt", { time: fmtFetched(file.fetched_at) ?? file.fetched_at })
          : t("infra.lastUnknown")}
      </span>
      <button
        className="text-[11px] px-2.5 py-1 rounded-md bg-surface text-muted hover:text-heading transition-colors shrink-0"
        onClick={() => refresh()}
      >
        ↻ {t("infra.refresh")}
      </button>
    </div>
  );

  // ── F5 完全无数据（invoke 失败/文件缺失/解析失败）：居中降级面板，不白屏 ──
  if (notReadyCause && !file) {
    return (
      <div className="flex flex-col h-full">
        {header}
        <div className="flex flex-col items-center justify-center flex-1 text-faint gap-2 px-6 text-center">
          <span className="text-3xl">🖥</span>
          <span className="text-sm text-body">{t("infra.notReadyTitle")}</span>
          <span className="text-[11px] leading-relaxed max-w-md break-all">{notReadyCause}</span>
          <span className="text-[11px]">{lastAtText}</span>
          <button className="text-[11px] text-accent hover:underline" onClick={() => refresh()}>
            {t("infra.retry")}
          </button>
        </div>
      </div>
    );
  }

  // providers 元素形态守卫（对抗审查 F5 红线类）：null/非对象元素在纯函数层
  // 剔除——下游 QuotaCard/ProviderCard 的 p.id/p.name 不可能再撞 null。
  const providers = providerList(file?.llm_quota?.providers);

  return (
    <div className="flex flex-col h-full">
      {header}

      {/* F5 有数据但超龄：琥珀横幅（含「上次：…」）+ 数据降透明度，不白屏 */}
      {notReadyCause && (
        <div className="flex items-center gap-2 px-4 py-1.5 bg-amber-bg text-amber text-[11px] border-b border-amber/30 flex-wrap">
          <span className="inline-block w-2 h-2 rounded-full bg-amber shrink-0" />
          <span className="font-medium shrink-0">{t("infra.notReadyTitle")}</span>
          <span className="min-w-0">{notReadyCause}</span>
          <span className="shrink-0">{lastAtText}</span>
          <button className="ml-auto hover:underline shrink-0" onClick={() => refresh()}>
            {t("infra.retry")}
          </button>
        </div>
      )}

      <div className={`flex-1 overflow-y-auto p-3 flex flex-col gap-3 ${notReadyCause ? "opacity-60" : ""}`}>
        {/* F1 顶部 LLM 额度卡：四家 × 5h/周双水位条 */}
        <QuotaCard providers={providers} updatedLocal={file?.llm_quota?.updated_local} />

        {/* F2 底座状态卡：worker 运行态 / queue 动态键值 / dsh 今日水位 */}
        <BaseStatusCard base={file?.snapshot?.base} />

        {/* F3 健康指示行：● 点阵，warn/fail 项 title 悬停显 _detail */}
        <HealthStrip health={file?.snapshot?.base?.health} />

        {/* P1 下半区插槽（预留，P0 不实现）：
            F6 用量趋势（aggUsage14d 按日聚合，容忍稀疏天）
            F7 最近任务流水（recent[].secs 可 null → fmtSecs）
            F8 NAS 卡（nasTone：null 红 / >600 黄）
            F9 告警联动 */}
      </div>
    </div>
  );
}

// ════════════════════════════════════════════════════════════
// F1 — LLM 额度卡
// ════════════════════════════════════════════════════════════

function QuotaCard({
  providers,
  updatedLocal,
}: {
  providers: InfraProvider[];
  updatedLocal?: string | null;
}) {
  const { t } = useTranslation();
  return (
    <section className="rounded-lg border border-line bg-surface/30 p-3">
      <div className="flex items-center gap-2 mb-1.5">
        <h2 className="text-[13px] font-medium text-heading">🔋 {t("infra.quotaTitle")}</h2>
        <span className="text-[11px] text-faint">{t("infra.quotaLegend")}</span>
        {updatedLocal && (
          <span className="text-[11px] text-faint ml-auto tabular-nums">{updatedLocal}</span>
        )}
      </div>
      {providers.length === 0 ? (
        <div className="text-[11px] text-faint py-2">{t("infra.quotaNoData")}</div>
      ) : (
        <div className="grid grid-cols-4 gap-2">
          {providers.map((p, i) => (
            <ProviderCard key={p.id ?? `idx-${i}`} provider={p} />
          ))}
        </div>
      )}
    </section>
  );
}

function ProviderCard({ provider: p }: { provider: InfraProvider }) {
  const { t } = useTranslation();
  const five = typeof p.five_hour?.remaining_pct === "number" ? p.five_hour.remaining_pct : null;
  const week = typeof p.weekly?.remaining_pct === "number" ? p.weekly.remaining_pct : null;
  const tip =
    `${t("infra.quota5h")} ${fmtPct(five)}` +
    (p.five_hour?.reset_local ? `（重置于 ${p.five_hour.reset_local}）` : "") +
    ` · ${t("infra.quotaWeek")} ${fmtPct(week)}` +
    (p.weekly?.reset_local ? `（重置于 ${p.weekly.reset_local}）` : "") +
    (p.error ? ` · ${p.error}` : "") +
    (p.state ? ` · ${p.state}` : "");
  return (
    <div className="flex flex-col items-center min-w-0" title={tip}>
      <RingGauge innerPct={five} outerPct={week} />
      <div className="mt-1 flex items-center gap-1 min-w-0">
        <span className="text-[11px] font-medium text-heading truncate">
          {p.name ?? p.id ?? "—"}
        </span>
        {p.error ? <span className="text-[11px] text-amber shrink-0">⚠</span> : null}
      </div>
      <div className="text-[11px] font-mono tabular-nums text-body">
        {fmtPct(five)} <span className="text-faint">/</span> {fmtPct(week)}
      </div>
    </div>
  );
}

/**
 * 同心圆双环仪表：外环=周额度、内环=5h 额度（图例在卡头）。
 * 环色随剩余额度连续过渡：100% 蓝 → 50% 绿 → 0% 红（quotaHueColor）。
 * pct null 时只画轨道（无数据不着色）。
 */
function RingGauge({ innerPct, outerPct }: { innerPct: number | null; outerPct: number | null }) {
  const R_OUT = 30;
  const R_IN = 21.5;
  const C_OUT = 2 * Math.PI * R_OUT;
  const C_IN = 2 * Math.PI * R_IN;
  const clamp = (v: number | null) => (v === null ? null : Math.min(100, Math.max(0, v)));
  const i = clamp(innerPct);
  const o = clamp(outerPct);
  const ring = (r: number, c: number, pct: number | null) => (
    <>
      <circle cx="36" cy="36" r={r} fill="none" strokeWidth="5.5" className="stroke-elevated" />
      {pct !== null && (
        <circle
          cx="36"
          cy="36"
          r={r}
          fill="none"
          strokeWidth="5.5"
          strokeLinecap="round"
          stroke={quotaHueColor(pct) ?? undefined}
          strokeDasharray={`${(pct / 100) * c} ${c}`}
          transform="rotate(-90 36 36)"
        />
      )}
    </>
  );
  return (
    <svg width="72" height="72" viewBox="0 0 72 72" role="img">
      {ring(R_OUT, C_OUT, o)}
      {ring(R_IN, C_IN, i)}
    </svg>
  );
}

// ════════════════════════════════════════════════════════════
// F2 — 底座状态卡
// ════════════════════════════════════════════════════════════

function BaseStatusCard({ base }: { base?: InfraBase }) {
  const { t } = useTranslation();
  // 边界收窄（对抗审查 F5 红线类）：health 声明为 Record<string,string>，
  // 但 schema 漂移时 worker 可能是非字符串真值（数字/布尔/对象）——
  // 非字符串一律按缺失处理（显示 —、灰点），绝不带进渲染。
  const workerRaw = base?.health?.worker;
  const worker = typeof workerRaw === "string" ? workerRaw : undefined;
  const workerTone = toneOf(worker);
  const workerStateText: Record<string, string> = {
    ok: t("infra.stateOk"),
    warn: t("infra.stateWarn"),
    fail: t("infra.stateFail"),
  };
  const workerText =
    worker != null ? (workerStateText[worker] ?? worker) : "—";

  const queues = queueChips(base?.queue);
  const recents = recentList(base?.recent);
  const dsh = typeof base?.dsh_today === "number" ? base.dsh_today : null;
  const dshBar = dshPct(dsh);

  return (
    <section className="rounded-lg border border-line bg-surface/30 p-3">
      <h2 className="text-[13px] font-medium text-heading mb-2">🧱 {t("infra.baseTitle")}</h2>
      <div className="grid grid-cols-1 sm:grid-cols-3 gap-3">
        {/* worker 运行态 */}
        <div className="min-w-0">
          <div className="text-[11px] text-faint mb-1">{t("infra.workerLabel")}</div>
          <div className="flex items-center gap-1.5">
            <span className={`w-2 h-2 rounded-full shrink-0 ${dotCls[workerTone]}`} />
            <span className={`text-[13px] font-medium ${toneTextCls[workerTone]}`}>{workerText}</span>
          </div>
        </div>

        {/* queue 计数：中文标签 + 语义色（键动态，未知键原样透出） */}
        <div className="min-w-0">
          <div className="text-[11px] text-faint mb-1">{t("infra.queueLabel")}</div>
          {queues.length === 0 ? (
            <span className="text-[13px] text-faint">{t("infra.queueEmpty")}</span>
          ) : (
            <div className="flex items-center gap-1.5 flex-wrap">
              {queues.map((q) => {
                const toneCls =
                  q.tone === "danger" ? "text-danger bg-danger/10" :
                  q.tone === "success" ? "text-success bg-success/10" :
                  q.tone === "info" ? "text-info bg-info/10" : "text-faint bg-elevated";
                return (
                  <span key={q.key} className={`text-[11px] px-1.5 py-0.5 rounded font-mono tabular-nums ${toneCls}`}>
                    {q.labelKey ? t(`infra.${q.labelKey}`) : q.key} {q.value}
                  </span>
                );
              })}
            </div>
          )}
        </div>

        {/* dsh 今日用量水位（日闸 50 来自需求文档，常数在 infraLogic.ts 顶部） */}
        <div className="min-w-0">
          <div className="text-[11px] text-faint mb-1">{t("infra.dshToday")}</div>
          <div className="text-[13px] font-mono tabular-nums text-body">
            {t("infra.dshUsage", { used: dsh ?? "—", gate: DSH_DAILY_GATE })}
          </div>
          <div className="mt-1 h-1.5 rounded-full bg-elevated overflow-hidden">
            <div
              className="h-full rounded-full bg-accent"
              style={{ width: `${dshBar ?? 0}%` }}
            />
          </div>
        </div>
      </div>

      {/* F7 提前实装：最近任务流水——「刚在办什么」的唯一契约内来源 */}
      <div className="mt-2.5 min-w-0">
        <div className="text-[11px] text-faint mb-1">{t("infra.recentTitle")}</div>
        {recents.length === 0 ? (
          <div className="text-[11px] text-faint">{t("infra.recentEmpty")}</div>
        ) : (
          <div className="flex flex-col gap-0.5">
            {recents.map((r, i) => {
              const tone = taskStatusTone(r.status);
              const toneCls =
                tone === "success" ? "text-success" : tone === "danger" ? "text-danger" :
                tone === "accent" ? "text-accent" : "text-faint";
              const labelKey = taskStatusLabelKey(r.status);
              return (
                <div key={r.id ?? `idx-${i}`} className="flex items-center gap-2 text-[11px] font-mono tabular-nums min-w-0">
                  <span className="text-faint w-8 shrink-0">#{r.id ?? "?"}</span>
                  <span className="text-body truncate shrink min-w-0" title={r.harness}>{r.harness}</span>
                  <span className={`${toneCls} shrink-0`}>{labelKey ? t(`infra.${labelKey}`) : r.status}</span>
                  <span className="ml-auto text-faint shrink-0">{fmtSecs(r.secs)}</span>
                </div>
              );
            })}
          </div>
        )}
      </div>
    </section>
  );
}

// ════════════════════════════════════════════════════════════
// F3 — 健康指示行（● 点阵）
// ════════════════════════════════════════════════════════════

function HealthStrip({ health }: { health?: Record<string, string> }) {
  const { t } = useTranslation();
  const items = healthItems(health);
  return (
    <section className="rounded-lg border border-line bg-surface/30 p-3">
      <div className="flex items-center gap-2 mb-2">
        <h2 className="text-[13px] font-medium text-heading">🩺 {t("infra.healthTitle")}</h2>
        {typeof health?.checked_at === "string" && (
          <span className="text-[11px] text-faint ml-auto tabular-nums">{health.checked_at}</span>
        )}
      </div>
      {items.length === 0 ? (
        <div className="text-[11px] text-faint">{t("infra.healthEmpty")}</div>
      ) : (
        <div className="flex items-center gap-x-3 gap-y-1.5 flex-wrap">
          {items.map((it) => {
            const tone = toneOf(it.value);
            // title：key: value — detail（warn 项悬停显 _detail，F3/验收第 3 条）
            const title = `${it.key}: ${it.value}${it.detail ? ` — ${it.detail}` : ""}`;
            return (
              <span key={it.key} title={title} className="flex items-center gap-1 text-[11px] whitespace-nowrap">
                <span className={`w-1.5 h-1.5 rounded-full ${dotCls[tone]}`} />
                <span className={tone === "success" ? "text-faint" : `${toneTextCls[tone]} font-medium`}>
                  {it.key}
                </span>
              </span>
            );
          })}
        </div>
      )}
    </section>
  );
}
