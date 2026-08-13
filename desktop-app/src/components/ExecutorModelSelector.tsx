import { useState, useRef, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { useExecutorOptions } from "../hooks/useExecutions";

/**
 * ExecutorModelSelector — pick which model the Runner's executor uses.
 * Options come from ~/.zcode/cli/config.json; the selection is injected into
 * each worktree as a project-level .zcode/config.json.
 *
 * The thinking variant is shown read-only: the ZCode CLI 0.16.3 has no
 * runtime thinking switch for custom providers (verified), so the selected
 * model's API-side default applies.
 */
export default function ExecutorModelSelector() {
  const { t } = useTranslation();
  const { options, select } = useExecutorOptions();
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    function onClick(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    }
    document.addEventListener("mousedown", onClick);
    return () => document.removeEventListener("mousedown", onClick);
  }, [open]);

  const current = options?.current_model ?? null;
  // Short label for the collapsed pill: "deepseek/deepseek-v4-flash" → "deepseek-v4-flash"
  const shortLabel = current ? current.split("/").slice(1).join("/") : t("execution.modelDefault");
  const currentModelDef = options?.providers
    .flatMap((p) => p.models)
    .find((m) => m.id === current);

  return (
    <div className="relative shrink-0" ref={ref}>
      <button
        className="px-2 py-0.5 rounded-full text-[9px] font-medium transition-colors bg-elevated text-body hover:bg-line-hover"
        onClick={() => setOpen(!open)}
        title={t("execution.modelSelectorHint")}
      >
        🤖 {shortLabel}
      </button>

      {open && (
        <div className="absolute right-0 top-full mt-1 w-60 bg-surface border border-line rounded-lg shadow-xl py-2 z-50">
          <div className="px-3 pb-2 text-[10px] text-faint uppercase tracking-wider">
            {t("execution.modelSelectorTitle")}
          </div>
          {options && options.providers.length > 0 ? (
            options.providers.map((provider) => (
              <div key={provider.name}>
                <div className="px-3 pt-1.5 pb-0.5 text-[10px] text-faint">{provider.display_name}</div>
                {provider.models.map((model) => (
                  <button
                    key={model.id}
                    className={`w-full flex items-center gap-2 px-3 py-1.5 text-xs transition-colors ${
                      current === model.id ? "text-heading bg-elevated" : "text-body hover:bg-elevated"
                    }`}
                    onClick={() => {
                      void select(model.id);
                      setOpen(false);
                    }}
                  >
                    <span className="truncate">{model.name}</span>
                    {model.thinking_default && (
                      <span className="ml-auto text-[9px] text-faint shrink-0">
                        {t("execution.thinkingDefault", { variant: model.thinking_default })}
                      </span>
                    )}
                    {current === model.id && (
                      <svg className="w-3.5 h-3.5 text-accent shrink-0" viewBox="0 0 16 16" fill="currentColor">
                        <path d="M13.78 4.22a.75.75 0 010 1.06l-7.25 7.25a.75.75 0 01-1.06 0L2.22 9.28a.75.75 0 011.06-1.06L6 10.94l6.72-6.72a.75.75 0 011.06 0z" />
                      </svg>
                    )}
                  </button>
                ))}
              </div>
            ))
          ) : (
            <div className="px-3 py-2 text-xs text-faint">{t("execution.modelNoneFound")}</div>
          )}

          <div className="my-1 border-t border-line" />

          <button
            className={`w-full flex items-center gap-2 px-3 py-1.5 text-xs transition-colors ${
              current === null ? "text-heading bg-elevated" : "text-body hover:bg-elevated"
            }`}
            onClick={() => {
              void select(null);
              setOpen(false);
            }}
          >
            <span>{t("execution.modelDefault")}</span>
            {current === null && (
              <svg className="ml-auto w-3.5 h-3.5 text-accent" viewBox="0 0 16 16" fill="currentColor">
                <path d="M13.78 4.22a.75.75 0 010 1.06l-7.25 7.25a.75.75 0 01-1.06 0L2.22 9.28a.75.75 0 011.06-1.06L6 10.94l6.72-6.72a.75.75 0 011.06 0z" />
              </svg>
            )}
          </button>

          <div className="px-3 pt-2 text-[9px] text-faint leading-relaxed border-t border-line mt-1">
            {currentModelDef?.thinking_default
              ? t("execution.thinkingReadOnlyHint", { variant: currentModelDef.thinking_default })
              : t("execution.modelSelectorHint")}
          </div>
        </div>
      )}
    </div>
  );
}
