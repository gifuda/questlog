import { useMemo, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import Badge from "./Badge";
import { parseReport, verdictView } from "../reportFormat";

/**
 * 夜间执行报告阅读视图：重点前置——结果（verdict+summary）与影响
 * （unresolved）置顶成摘要卡，正文 markdown 轻量渲染，证据引用折叠。
 * 全部构建 React 节点渲染，不使用 innerHTML（无注入面）。
 */

/** 行内 markdown：**粗体** 与 `代码`。 */
function inline(text: string, keyBase: string): ReactNode[] {
  return text
    .split(/(\*\*[^*]+\*\*|`[^`]+`)/g)
    .filter(Boolean)
    .map((p, i) => {
      const key = `${keyBase}-${i}`;
      if (p.startsWith("**") && p.endsWith("**")) {
        return (
          <strong key={key} className="text-heading font-medium">
            {p.slice(2, -2)}
          </strong>
        );
      }
      if (p.startsWith("`") && p.endsWith("`")) {
        return (
          <code key={key} className="font-mono text-[12px] bg-elevated px-1 rounded">
            {p.slice(1, -1)}
          </code>
        );
      }
      return <span key={key}>{p}</span>;
    });
}

/** 块级 markdown 轻量渲染：标题/有序无序列表/表格/代码块/段落。 */
function renderBody(body: string): ReactNode[] {
  const out: ReactNode[] = [];
  const lines = body.split("\n");
  let keySeed = 0;
  const k = () => `blk${keySeed++}`;
  let i = 0;
  const isBlockStart = (s: string) =>
    /^(```|#{1,4}\s|\s*[-*]\s|\s*\d+\.\s)/.test(s) || s.trimStart().startsWith("|");
  while (i < lines.length) {
    const line = lines[i] ?? "";
    if (line.startsWith("```")) {
      const buf: string[] = [];
      i += 1;
      while (i < lines.length && !(lines[i] ?? "").startsWith("```")) {
        buf.push(lines[i] ?? "");
        i += 1;
      }
      i += 1;
      out.push(
        <pre
          key={k()}
          className="font-mono text-[12px] text-muted bg-elevated rounded-md px-3 py-2 overflow-x-auto whitespace-pre"
        >
          {buf.join("\n")}
        </pre>
      );
      continue;
    }
    const h = /^(#{1,4})\s+(.*)$/.exec(line);
    if (h) {
      out.push(
        <div key={k()} className="text-[14px] font-medium text-heading pt-1">
          {inline(h[2] ?? "", "h")}
        </div>
      );
      i += 1;
      continue;
    }
    if (/^\s*[-*]\s+/.test(line)) {
      const items: string[] = [];
      while (i < lines.length && /^\s*[-*]\s+/.test(lines[i] ?? "")) {
        items.push((lines[i] ?? "").replace(/^\s*[-*]\s+/, ""));
        i += 1;
      }
      out.push(
        <ul key={k()} className="space-y-1">
          {items.map((it, j) => (
            <li key={j} className="flex gap-1.5">
              <span className="text-faint shrink-0">•</span>
              <span>{inline(it, `ul${j}`)}</span>
            </li>
          ))}
        </ul>
      );
      continue;
    }
    if (/^\s*\d+\.\s+/.test(line)) {
      const items: string[] = [];
      while (i < lines.length && /^\s*\d+\.\s+/.test(lines[i] ?? "")) {
        items.push((lines[i] ?? "").replace(/^\s*\d+\.\s+/, ""));
        i += 1;
      }
      out.push(
        <div key={k()} className="space-y-1">
          {items.map((it, j) => (
            <div key={j} className="flex gap-1.5">
              <span className="text-faint shrink-0 w-4 text-right">{j + 1}.</span>
              <span>{inline(it, `ol${j}`)}</span>
            </div>
          ))}
        </div>
      );
      continue;
    }
    if (line.trimStart().startsWith("|")) {
      const buf: string[] = [];
      while (i < lines.length && (lines[i] ?? "").trimStart().startsWith("|")) {
        buf.push(lines[i] ?? "");
        i += 1;
      }
      out.push(
        <pre
          key={k()}
          className="font-mono text-[11px] text-muted bg-elevated rounded-md px-3 py-2 overflow-x-auto whitespace-pre"
        >
          {buf.join("\n")}
        </pre>
      );
      continue;
    }
    if (line.trim() === "") {
      i += 1;
      continue;
    }
    const buf: string[] = [];
    while (i < lines.length && (lines[i] ?? "").trim() !== "" && !isBlockStart(lines[i] ?? "")) {
      buf.push(lines[i] ?? "");
      i += 1;
    }
    out.push(<p key={k()}>{inline(buf.join(""), "p")}</p>);
  }
  return out;
}

export default function ReportViewer({ text }: { text: string }) {
  const { t } = useTranslation();
  const { structured, body } = useMemo(() => parseReport(text), [text]);
  const v = verdictView(structured?.verdict);

  return (
    <div className="space-y-3">
      {structured && (
        <div className="space-y-2.5">
          {v && (
            <div className="flex items-center gap-2">
              <Badge tone={v.tone}>{v.label}</Badge>
              <span className="text-[11px] text-faint font-mono">verdict: {structured.verdict}</span>
            </div>
          )}
          {structured.summary && (
            <div className="rounded-md border border-line bg-surface-dim px-3 py-2.5">
              <div className="text-[11px] text-faint mb-1">{t("card.reportResult")}</div>
              <p className="text-[13px] text-body leading-relaxed">{structured.summary}</p>
            </div>
          )}
          {structured.unresolved && structured.unresolved.length > 0 && (
            <div className="rounded-md border border-line bg-surface-dim px-3 py-2.5">
              <div className="text-[11px] text-faint mb-1">{t("card.reportImpact")}</div>
              <ul className="space-y-1">
                {structured.unresolved.map((u, i) => (
                  <li key={i} className="text-[13px] text-body leading-relaxed flex gap-1.5">
                    <span className="text-amber shrink-0">⏳</span>
                    <span>{u}</span>
                  </li>
                ))}
              </ul>
            </div>
          )}
        </div>
      )}

      {(body || !structured) && (
        <div>
          {structured && <div className="text-[11px] text-faint mb-1.5">{t("card.reportDetail")}</div>}
          <div className="text-[13px] text-body leading-relaxed space-y-2">{renderBody(body)}</div>
        </div>
      )}

      {structured?.evidence_refs && structured.evidence_refs.length > 0 && (
        <details className="text-[12px] text-muted">
          <summary className="cursor-pointer text-faint hover:text-muted select-none">
            {t("card.reportEvidence", { n: structured.evidence_refs.length })}
          </summary>
          <ul className="mt-1 space-y-0.5 pl-3">
            {structured.evidence_refs.map((e, i) => (
              <li key={i} className="font-mono text-[11px] text-faint break-all">
                {e}
              </li>
            ))}
          </ul>
        </details>
      )}
    </div>
  );
}
