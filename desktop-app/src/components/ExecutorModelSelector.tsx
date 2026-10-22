import { useState, useRef, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { useExecutorOptions, useNightlyModel } from "../hooks/useExecutions";

function Check({ on }: { on: boolean }) {
  if (!on) return null;
  return (
    <svg className="ml-auto w-3.5 h-3.5 text-accent shrink-0" viewBox="0 0 16 16" fill="currentColor">
      <path d="M13.78 4.22a.75.75 0 010 1.06l-7.25 7.25a.75.75 0 01-1.06 0L2.22 9.28a.75.75 0 011.06-1.06L6 10.94l6.72-6.72a.75.75 0 011.06 0z" />
    </svg>
  );
}

/**
 * ExecutorModelSelector — pick which model AND thinking level the Runner's
 * executor uses. The two are decoupled: options come from
 * ~/.zcode/cli/config.json (level lists follow the user's ZCode config
 * automatically), selections are stored in runner-config.json and injected
 * into each worktree as a project-level .zcode/config.json.
 *
 * Thinking injection normalizes every declared level's providerOptionsByLevel
 * to the chosen level's params, so the choice holds regardless of the CLI's
 * global runtime reasoningLevel setting.
 */
function ModelPicker({
  options, current, thinking, onSelect, onSelectThinking, showDefault, titleKey,
}: {
  options: import("../hooks/useExecutions").ExecutorOptions | null;
  current: string | null;
  thinking: string | null;
  onSelect: (id: string) => void;
  onSelectThinking: (level: string | null) => void;
  showDefault: boolean;
  titleKey: string;
}) {
  const { t } = useTranslation();
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

  // Short label for the collapsed pill: "deepseek/deepseek-v4-flash" → "deepseek-v4-flash"
  const shortLabel = current ? current.split("/").slice(1).join("/") : t("execution.modelDefault");
  const currentModelDef = options?.providers
    .flatMap((p) => p.models)
    .find((m) => m.id === current);
  const levels = currentModelDef?.thinking_levels ?? [];

  return (
    <div className="relative shrink-0" ref={ref}>
      <button
        className="px-2 py-0.5 rounded-full text-[11px] font-medium transition-colors bg-elevated text-body hover:bg-line-hover"
        onClick={() => setOpen(!open)}
        title={t("execution.modelSelectorHint")}
      >
        🤖 {shortLabel}
        {thinking && <span className="text-faint"> · {thinking}</span>}
      </button>

      {open && (
        <div className="absolute right-0 top-full mt-1 w-64 bg-surface border border-line rounded-lg shadow-xl py-2 z-[60]">
          <div className="px-3 pb-2 text-[11px] text-faint uppercase tracking-wider">
            {t(titleKey)}
          </div>
          {options && options.providers.length > 0 ? (
            options.providers.map((provider) => (
              <div key={provider.name}>
                <div className="px-3 pt-1.5 pb-0.5 text-[11px] text-faint">{provider.display_name}</div>
                {provider.models.map((model) => (
                  <button
                    key={model.id}
                    className={`w-full flex items-center gap-2 px-3 py-1.5 text-xs transition-colors ${
                      current === model.id ? "text-heading bg-elevated" : "text-body hover:bg-elevated"
                    }`}
                    onClick={() => {
                      onSelect(model.id);
                      // 思考档随模型切换：新模型档位不含当前值时复位
                      if (thinking && !model.thinking_levels.includes(thinking)) {
                        onSelectThinking(null);
                      }
                    }}
                  >
                    <span className="truncate">{model.name}</span>
                    <Check on={current === model.id} />
                  </button>
                ))}
              </div>
            ))
          ) : (
            <div className="px-3 py-2 text-xs text-faint">{t("execution.modelNoneFound")}</div>
          )}

          <div className="my-1 border-t border-line" />

          {showDefault && (
            <button
              className={`w-full flex items-center gap-2 px-3 py-1.5 text-xs transition-colors ${
                current === null ? "text-heading bg-elevated" : "text-body hover:bg-elevated"
              }`}
              onClick={() => void onSelect("__default__")}
            >
              <span>{t("execution.modelDefault")}</span>
              <Check on={current === null} />
            </button>
          )}

          <div className="my-1 border-t border-line" />

          <div className="px-3 pb-1 pt-1.5 text-[11px] text-faint uppercase tracking-wider">
            {t("execution.thinkingTitle")}
          </div>
          {levels.length > 0 ? (
            <>
              {levels.map((level) => (
                <button
                  key={level}
                  className={`w-full flex items-center gap-2 px-3 py-1.5 text-xs transition-colors ${
                    thinking === level ? "text-heading bg-elevated" : "text-body hover:bg-elevated"
                  }`}
                  onClick={() => onSelectThinking(level)}
                >
                  <span className="truncate">{t("execution.thinkingLevel", { level })}</span>
                  {currentModelDef?.thinking_default === level && (
                    <span className="text-[11px] text-faint shrink-0">
                      {t("execution.thinkingIsDefault")}
                    </span>
                  )}
                  <Check on={thinking === level} />
                </button>
              ))}
              <button
                className={`w-full flex items-center gap-2 px-3 py-1.5 text-xs transition-colors ${
                  thinking === null ? "text-heading bg-elevated" : "text-body hover:bg-elevated"
                }`}
                onClick={() => onSelectThinking(null)}
              >
                <span>{t("execution.thinkingFollowDefault")}</span>
                <Check on={thinking === null} />
              </button>
            </>
          ) : (
            <button
              className={`w-full flex items-center gap-2 px-3 py-1.5 text-xs transition-colors ${
                thinking === null ? "text-heading bg-elevated" : "text-body hover:bg-elevated"
              }`}
              onClick={() => onSelectThinking(null)}
            >
              <span>{t("execution.thinkingFollowDefault")}</span>
              <Check on={thinking === null} />
            </button>
          )}

          <div className="px-3 pt-2 text-[11px] text-faint leading-relaxed border-t border-line mt-1">
            {t("execution.thinkingSelectorHint")}
          </div>
        </div>
      )}
    </div>
  );
}


/** 施工执行器选择器（方案步骤自动执行用）——挂在全局设置。 */
export default function ExecutorModelSelector() {
  const { options, select, selectThinking } = useExecutorOptions();
  return (
    <ModelPicker
      options={options}
      current={options?.current_model ?? null}
      thinking={options?.current_thinking ?? null}
      onSelect={(id) => { void select(id === "__default__" ? null : id); }}
      onSelectThinking={(lv) => { void selectThinking(lv); }}
      showDefault
      titleKey="execution.modelSelectorTitle"
    />
  );
}

/** 夜间研究链选择器（待办自动处理用）——挂在全局设置；夜间链恒有显式模型，
 *  无"默认"档。 */
export function NightlyModelSelector() {
  const { status, options, select, selectThinking } = useNightlyModel();
  return (
    <ModelPicker
      options={options}
      current={status?.model ?? null}
      thinking={status?.thinking ?? null}
      onSelect={(id) => { void select(id); }}
      onSelectThinking={(lv) => { void selectThinking(lv); }}
      showDefault={false}
      titleKey="execution.nightlySelectorTitle"
    />
  );
}
