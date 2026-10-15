import { useState } from "react";
import { useTranslation } from "react-i18next";
import { statusOptions } from "../../epicConstants";
import type { Epic, EpicStatus, StepStage } from "../../types/epics";
import { epicStatusKey, type EpicPatch } from "./helpers";

// NewEpicForm — 新建方案
// ══════════════════════════════════════════════
export function NewEpicForm({ existingTitles, onCancel, onSubmit, compact }: {
  existingTitles: { id: string; title: string }[];
  onCancel: () => void;
  onSubmit: (title: string, opts: { description?: string; priority?: string; dependsOn?: string[]; acceptance?: string }) => void;
  compact?: boolean;
}) {
  const { t } = useTranslation();
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [priority, setPriority] = useState("P1");
  const [dependsOn, setDependsOn] = useState<string[]>([]);
  const [acceptance, setAcceptance] = useState("");

  return (
    <div className={`rounded-lg border border-accent/40 bg-accent-bg/30 ${compact ? "p-2" : "p-3"}`}>
      <input
        autoFocus
        className="w-full bg-transparent border-b border-line px-1 py-1 text-[13px] text-heading focus:outline-none focus:border-accent"
        placeholder={t("epic.titlePlaceholder")}
        value={title}
        onChange={(e) => setTitle(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && title.trim()) onSubmit(title.trim(), { description: description.trim() || undefined, priority, dependsOn: dependsOn.length ? dependsOn : undefined, acceptance: acceptance.trim() || undefined });
          if (e.key === "Escape") onCancel();
        }}
      />
      <input
        className="w-full bg-transparent px-1 py-1 mt-1 text-xs text-muted focus:outline-none"
        placeholder={t("epic.descPlaceholder")}
        value={description}
        onChange={(e) => setDescription(e.target.value)}
      />
      <input
        className="w-full bg-transparent px-1 py-1 mt-0.5 text-xs text-amber/80 focus:outline-none"
        placeholder={t("epic.acceptancePlaceholder")}
        value={acceptance}
        onChange={(e) => setAcceptance(e.target.value)}
      />
      <div className="flex items-center gap-2 mt-2">
        <select
          className="bg-elevated text-xs text-body rounded px-1.5 py-0.5 focus:outline-none"
          value={priority}
          onChange={(e) => setPriority(e.target.value)}
        >
          <option value="P0">P0</option>
          <option value="P1">P1</option>
          <option value="P2">P2</option>
          <option value="P3">P3</option>
        </select>
        {existingTitles.length > 0 && (
          <select
            multiple
            className="flex-1 bg-elevated text-xs text-body rounded px-1 py-0.5 focus:outline-none h-12"
            value={dependsOn}
            onChange={(e) => {
              const selected = Array.from(e.target.selectedOptions).map((o) => o.value);
              setDependsOn(selected);
            }}
            title={t("epic.cmdCtrlMultiSelect")}
          >
            {existingTitles.map((t) => (
              <option key={t.id} value={t.id}>{t.title}</option>
            ))}
          </select>
        )}
      </div>
      <div className="flex items-center gap-3 mt-2">
        <button
          className="text-xs px-2 py-1 bg-accent text-white rounded hover:opacity-90 disabled:opacity-40"
          disabled={!title.trim()}
          onClick={() => onSubmit(title.trim(), { description: description.trim() || undefined, priority, dependsOn: dependsOn.length ? dependsOn : undefined, acceptance: acceptance.trim() || undefined })}
        >
          {t("epic.create")}
        </button>
        <button className="text-xs text-faint hover:text-body" onClick={onCancel}>{t("epic.cancel")}</button>
        <span className="text-[11px] text-faint ml-auto">{t("epic.cmdMultiSelect")}</span>
      </div>
    </div>
  );
}

// ══════════════════════════════════════════════
// EpicEditForm — 编辑方案的详情
// ══════════════════════════════════════════════
export function EpicEditForm({ epic, allEpics, onSubmit, onCancel }: {
  epic: Epic;
  allEpics: Epic[];
  onSubmit: (patch: { title?: string; description?: string; status?: EpicStatus; priority?: string; dependsOn?: string[]; linksDoc?: string; linksBranch?: string; linksNote?: string; acceptance?: string; closeReason?: string }) => void;
  onCancel: () => void;
}) {
  const { t } = useTranslation();
  const [title, setTitle] = useState(epic.title);
  const [description, setDescription] = useState(epic.description);
  const [status, setStatus] = useState<EpicStatus>(epic.status);
  const [priority, setPriority] = useState(epic.priority);
  const [dependsOn, setDependsOn] = useState<string[]>(epic.depends_on);
  const [linksDoc, setLinksDoc] = useState(epic.links.doc ?? "");
  const [linksBranch, setLinksBranch] = useState(epic.links.branch ?? "");
  const [linksNote, setLinksNote] = useState(epic.links.note ?? "");
  const [acceptance, setAcceptance] = useState(epic.acceptance ?? "");
  const [closeReason, setCloseReason] = useState(epic.close_reason ?? "");

  return (
    <div className="flex flex-col gap-2 py-1">
      <input
        autoFocus
        className="bg-transparent border-b border-line px-1 py-1 text-[13px] text-heading focus:outline-none focus:border-accent"
        value={title}
        onChange={(e) => setTitle(e.target.value)}
      />
      <textarea
        className="bg-transparent px-1 py-1 text-xs text-muted focus:outline-none resize-none"
        rows={2}
        placeholder={t("epic.epicDescPlaceholder")}
        value={description}
        onChange={(e) => setDescription(e.target.value)}
      />
      <div className="flex items-center gap-2">
        <label className="text-xs text-faint">{t("epic.status")}</label>
        <select
          className="bg-elevated text-xs text-body rounded px-1.5 py-0.5 focus:outline-none"
          value={status}
          onChange={(e) => setStatus(e.target.value as EpicStatus)}
        >
          {statusOptions.map((s) => (
            <option key={s} value={s}>{t(epicStatusKey[s])}</option>
          ))}
        </select>
        <label className="text-xs text-faint ml-2">{t("epic.priority")}</label>
        <select
          className="bg-elevated text-xs text-body rounded px-1.5 py-0.5 focus:outline-none"
          value={priority}
          onChange={(e) => setPriority(e.target.value)}
        >
          <option value="P0">P0</option>
          <option value="P1">P1</option>
          <option value="P2">P2</option>
          <option value="P3">P3</option>
        </select>
      </div>
      {/* Dependencies (multi-select) */}
      <div className="flex items-center gap-2">
        <label className="text-xs text-faint shrink-0">{t("epic.deps")}</label>
        <select
          multiple
          className="flex-1 bg-elevated text-xs text-body rounded px-1 py-0.5 focus:outline-none h-14"
          value={dependsOn}
          onChange={(e) => setDependsOn(Array.from(e.target.selectedOptions).map((o) => o.value))}
        >
          {allEpics.filter((e) => e.id !== epic.id).map((e) => (
            <option key={e.id} value={e.id}>{e.priority} {e.title}</option>
          ))}
        </select>
      </div>
      <div className="flex flex-col gap-1">
        <input className="bg-transparent border-b border-line px-1 py-0.5 text-xs text-muted focus:outline-none" placeholder={t("epic.docPlaceholder")} value={linksDoc} onChange={(e) => setLinksDoc(e.target.value)} />
        <input className="bg-transparent border-b border-line px-1 py-0.5 text-xs text-muted focus:outline-none" placeholder={t("epic.branchPlaceholder")} value={linksBranch} onChange={(e) => setLinksBranch(e.target.value)} />
        <input className="bg-transparent border-b border-line px-1 py-0.5 text-xs text-muted focus:outline-none" placeholder={t("epic.notePlaceholder")} value={linksNote} onChange={(e) => setLinksNote(e.target.value)} />
      </div>
      {/* 验收条件 + 收尾原因 */}
      <div className="flex flex-col gap-1">
        <textarea
          className="bg-transparent border-b border-line px-1 py-0.5 text-xs text-amber/90 focus:outline-none resize-none"
          rows={2}
          placeholder={t("epic.acceptancePlaceholder")}
          value={acceptance}
          onChange={(e) => setAcceptance(e.target.value)}
        />
        {status === "done" && (
          <input
            className="bg-transparent border-b border-line px-1 py-0.5 text-xs text-amber/80 focus:outline-none"
            placeholder={t("epic.closeReasonPlaceholder")}
            value={closeReason}
            onChange={(e) => setCloseReason(e.target.value)}
          />
        )}
      </div>
      <div className="flex items-center gap-3">
        <button
          className="text-xs px-2 py-1 bg-accent text-white rounded hover:opacity-90 disabled:opacity-40"
          disabled={!title.trim()}
          onClick={() => onSubmit({
            title: title.trim(), description,
            status, priority,
            dependsOn,
            linksDoc, linksBranch, linksNote,
            acceptance, closeReason: closeReason,
          })}
        >
          {t("epic.save")}
        </button>
        <button className="text-xs text-faint hover:text-body" onClick={onCancel}>{t("epic.cancel")}</button>
      </div>
    </div>
  );
}

// StepAddForm
// ══════════════════════════════════════════════
export function StepAddForm({ onCancel, onSubmit, existingSteps }: {
  onCancel: () => void;
  onSubmit: (title: string, desc: string | undefined, dependsOn: string[]) => void;
  existingSteps: { id: string; title: string }[];
}) {
  const { t } = useTranslation();
  const [title, setTitle] = useState("");
  const [desc, setDesc] = useState("");
  const [dependsOn, setDependsOn] = useState<string[]>([]);
  return (
    <div className="mt-1.5 flex flex-col gap-1.5">
      <div className="flex items-center gap-2">
        <input
          autoFocus
          className="flex-1 bg-elevated/50 rounded px-2 py-1 text-[13px] text-heading focus:outline-none focus:ring-1 focus:ring-accent"
          placeholder={t("epic.stepTitlePlaceholder")}
          value={title}
          onChange={(e) => setTitle(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && title.trim()) onSubmit(title.trim(), desc.trim() || undefined, dependsOn);
            if (e.key === "Escape") onCancel();
          }}
        />
        <button
          className="text-xs px-1.5 py-0.5 bg-accent text-white rounded disabled:opacity-40"
          disabled={!title.trim()}
          onClick={() => onSubmit(title.trim(), desc.trim() || undefined, dependsOn)}
        >
          {t("epic.addStep")}
        </button>
        <button className="text-xs text-faint hover:text-body" onClick={onCancel}>{t("epic.cancel")}</button>
      </div>
      {existingSteps.length > 0 && (
        <div className="flex items-center gap-1.5">
          <span className="text-[11px] text-faint shrink-0">{t("epic.depsOptional")}</span>
          <select
            multiple
            className="flex-1 bg-elevated/50 text-[11px] text-body rounded px-1 py-0.5 focus:outline-none h-10"
            value={dependsOn}
            onChange={(e) => setDependsOn(Array.from(e.target.selectedOptions).map((o) => o.value))}
            title={t("epic.stepDepsTitle")}
          >
            {existingSteps.map((s) => (
              <option key={s.id} value={s.id}>{s.title}</option>
            ))}
          </select>
        </div>
      )}
    </div>
  );
}
