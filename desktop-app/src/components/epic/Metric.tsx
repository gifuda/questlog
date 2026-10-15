import type { ReactNode } from "react";

// Metric — 摘要条的大号等宽数字
// ══════════════════════════════════════════════
export function Metric({ num, label, numClass = "text-heading" }: { num: number; label: string; numClass?: string }) {
  return (
    <div className="flex items-baseline gap-1.5">
      <span className={`font-mono text-xl font-semibold leading-none tabular-nums ${numClass}`}>{num}</span>
      <span className="text-xs text-muted">{label}</span>
    </div>
  );
}

// ══════════════════════════════════════════════
