import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { useFleetHealth } from "../hooks/useFleetHealth";
import type { FleetHealth as FleetHealthData } from "../hooks/useFleetHealth";

/** 单个健康信号 chip：只有异常/待处理信号才渲染，正常态不占视觉。 */
function Chip({ tone, text, title }: { tone: "amber" | "red" | "blue"; text: string; title?: string }) {
  const cls =
    tone === "red"
      ? "bg-danger/15 text-danger"
      : tone === "amber"
        ? "bg-amber/15 text-amber"
        : "bg-accent/15 text-accent";
  return (
    <span title={title} className={`px-1.5 py-0.5 rounded text-[10px] font-medium whitespace-nowrap ${cls}`}>
      {text}
    </span>
  );
}

/** 昨晚结果一句话摘要（灯带与待确认区共用文案）。 */
export function digestText(h: FleetHealthData, t: TFunction): string | null {
  if (!h.digest) return null;
  return t("fleet.digest", {
    date: h.digest.date,
    total: h.digest.total,
    accepted: h.digest.accepted,
    review: h.digest.review,
    failed: h.digest.failed,
  });
}

/**
 * 跨机链路健康灯带：挂在方案追踪摘要条旁。
 * 管线未启用时显示灰态，正常时一个绿点，异常信号逐个亮 chip。
 */
export function FleetHealthBand() {
  const { t } = useTranslation();
  const { health } = useFleetHealth();
  if (!health) return null;

  if (!health.schedule_enabled) {
    return <span className="text-[10px] text-faint whitespace-nowrap">🌙 {t("fleet.off")}</span>;
  }

  const allClear =
    !health.window_missed &&
    health.outbox_pending === 0 &&
    health.receipts_pending === 0 &&
    health.receipts_unmatched === 0;
  const digest = digestText(health, t);

  return (
    <div className="flex items-center gap-1.5 flex-wrap">
      {allClear && (
        <span className="flex items-center gap-1 text-[10px] text-success whitespace-nowrap">
          <span className="w-1.5 h-1.5 rounded-full bg-success" />
          {t("fleet.ok")}
        </span>
      )}
      {health.window_missed && (
        <Chip tone="amber" text={t("fleet.missed", { date: health.sweep_state.last_missed_date })} />
      )}
      {health.outbox_pending > 0 && <Chip tone="amber" text={t("fleet.outbox", { count: health.outbox_pending })} />}
      {health.receipts_pending > 0 && (
        <Chip tone="blue" text={t("fleet.receipts", { count: health.receipts_pending })} />
      )}
      {health.receipts_unmatched > 0 && (
        <Chip tone="red" text={t("fleet.unmatched", { count: health.receipts_unmatched })} />
      )}
      {digest && <span className="text-[10px] text-faint whitespace-nowrap">{digest}</span>}
    </div>
  );
}

/** 待确认区头部的昨晚摘要行（仅在有 digest 时渲染）。 */
export function NightlyDigestLine() {
  const { t } = useTranslation();
  const { health } = useFleetHealth();
  if (!health) return null;
  const digest = digestText(health, t);
  if (!digest) return null;
  return <span className="text-[11px] text-accent/80 whitespace-nowrap">{digest}</span>;
}
