/**
 * 夜间执行报告的解析与 verdict 映射（纯函数，供 ReportViewer 渲染）。
 *
 * 报告形态：markdown 正文 + 末尾一个 ```json 围栏块（schema
 * questlog-todo-report），围栏内是结构化字段：verdict / summary /
 * findings / unresolved / evidence_refs。阅读重点在「结果和影响」——
 * verdict+summary 是结果，unresolved 是影响（后续要发生什么）。
 */

export interface ReportStructured {
  verdict?: string;
  summary?: string;
  findings?: string[];
  unresolved?: string[];
  evidence_refs?: string[];
}

export interface ParsedReport {
  /** 末尾围栏里的结构化数据；围栏缺失或不可解析时为 null。 */
  structured: ReportStructured | null;
  /** 围栏之前的 markdown 正文（已去除围栏块与首尾空白）。 */
  body: string;
}

/** 提取最后一个 ```json 围栏并从正文里移除（报告只写一个，取最后一个防串）。 */
export function parseReport(raw: string): ParsedReport {
  const fence = /```json\s*([\s\S]*?)```/g;
  let lastMatch: RegExpExecArray | null = null;
  let m: RegExpExecArray | null;
  while ((m = fence.exec(raw)) !== null) lastMatch = m;
  if (!lastMatch) return { structured: null, body: raw.trim() };
  let structured: ReportStructured | null = null;
  try {
    const v = JSON.parse(lastMatch[1]) as Partial<ReportStructured>;
    if (typeof v === "object" && v !== null && ("verdict" in v || "summary" in v)) {
      structured = {
        verdict: typeof v.verdict === "string" ? v.verdict : undefined,
        summary: typeof v.summary === "string" ? v.summary : undefined,
        findings: Array.isArray(v.findings) ? v.findings.filter((x) => typeof x === "string") : undefined,
        unresolved: Array.isArray(v.unresolved) ? v.unresolved.filter((x) => typeof x === "string") : undefined,
        evidence_refs: Array.isArray(v.evidence_refs) ? v.evidence_refs.filter((x) => typeof x === "string") : undefined,
      };
    }
  } catch {
    structured = null;
  }
  const body = (raw.slice(0, lastMatch.index) + raw.slice(lastMatch.index + lastMatch[0].length)).trim();
  return { structured, body };
}

export type VerdictTone = "success" | "amber" | "muted";

export interface VerdictView {
  /** 展示文案；未知 verdict 原样透出，不猜。 */
  label: string;
  tone: VerdictTone;
}

const VERDICT_MAP: Record<string, { label: string; tone: VerdictTone }> = {
  CANNOT_AUTO: { label: "无法自动处理", tone: "amber" },
  NEEDS_REVIEW: { label: "需人工复核", tone: "amber" },
  ACCEPTED: { label: "可采纳", tone: "success" },
  OK: { label: "可采纳", tone: "success" },
  PASS: { label: "通过", tone: "success" },
  DONE: { label: "完成", tone: "success" },
};

export function verdictView(verdict: string | undefined): VerdictView | null {
  if (!verdict) return null;
  return VERDICT_MAP[verdict] ?? { label: verdict, tone: "muted" };
}
