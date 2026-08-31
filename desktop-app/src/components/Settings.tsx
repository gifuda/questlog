import { useState, useRef, useEffect } from "react";
import { useTranslation } from "react-i18next";
import type { Theme } from "../hooks/useTheme";
import { setLanguage, type AppLanguage } from "../i18n";
import { setArbBoard } from "../hooks/useArbBoard";

interface SettingsProps {
  theme: Theme;
  onThemeChange: (t: Theme) => void;
  /** 当前选中项目（null = 未选中）。用于项目级「套利机会栏」开关。 */
  activeProject?: { name: string; path: string; arb_enabled: boolean } | null;
  /** 开关变更后通知父组件刷新项目列表。 */
  onProjectsChanged?: () => void;
}

export default function Settings({ theme, onThemeChange, activeProject, onProjectsChanged }: SettingsProps) {
  const { t, i18n } = useTranslation();
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const [boardPath, setBoardPath] = useState("exports/board.json");

  const themeOptions: { value: Theme; label: string; icon: string }[] = [
    { value: "dark", label: t("app.themeDark"), icon: "🌙" },
    { value: "light", label: t("app.themeLight"), icon: "☀️" },
    { value: "system", label: t("app.themeSystem"), icon: "💻" },
  ];

  const langOptions: { value: AppLanguage; label: string; icon: string }[] = [
    { value: "zh", label: t("app.langZh"), icon: "🇨🇳" },
    { value: "en", label: t("app.langEn"), icon: "🇺🇸" },
  ];

  useEffect(() => {
    if (!open) return;
    function onClick(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    }
    document.addEventListener("mousedown", onClick);
    return () => document.removeEventListener("mousedown", onClick);
  }, [open]);

  return (
    <div className="relative" ref={ref}>
      <button
        className="p-1.5 rounded-md text-muted hover:text-heading hover:bg-elevated transition-colors"
        onClick={() => setOpen(!open)}
        title={t("app.settings")}
      >
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          {/* Standard 8-tooth gear (lucide-style) — crisp at small sizes */}
          <path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z"/>
          <circle cx="12" cy="12" r="3"/>
        </svg>
      </button>

      {open && (
        <div className="absolute right-0 top-full mt-1 w-44 bg-surface border border-line rounded-lg shadow-xl py-2 z-50">
          <div className="px-3 pb-2 text-[10px] text-faint uppercase tracking-wider">{t("app.appearance")}</div>
          {themeOptions.map((opt) => (
            <button
              key={opt.value}
              className={`w-full flex items-center gap-2 px-3 py-1.5 text-xs transition-colors ${
                theme === opt.value ? "text-heading bg-elevated" : "text-body hover:bg-elevated"
              }`}
              onClick={() => { onThemeChange(opt.value); setOpen(false); }}
            >
              <span>{opt.icon}</span>
              <span>{opt.label}</span>
              {theme === opt.value && (
                <svg className="ml-auto w-3.5 h-3.5 text-accent" viewBox="0 0 16 16" fill="currentColor">
                  <path d="M13.78 4.22a.75.75 0 010 1.06l-7.25 7.25a.75.75 0 01-1.06 0L2.22 9.28a.75.75 0 011.06-1.06L6 10.94l6.72-6.72a.75.75 0 011.06 0z" />
                </svg>
              )}
            </button>
          ))}

          <div className="my-1 border-t border-line" />

          <div className="px-3 pb-2 text-[10px] text-faint uppercase tracking-wider">{t("app.language")}</div>
          {langOptions.map((opt) => (
            <button
              key={opt.value}
              className={`w-full flex items-center gap-2 px-3 py-1.5 text-xs transition-colors ${
                i18n.resolvedLanguage?.startsWith(opt.value) ? "text-heading bg-elevated" : "text-body hover:bg-elevated"
              }`}
              onClick={() => { void setLanguage(opt.value); setOpen(false); }}
            >
              <span>{opt.icon}</span>
              <span>{opt.label}</span>
              {i18n.resolvedLanguage?.startsWith(opt.value) && (
                <svg className="ml-auto w-3.5 h-3.5 text-accent" viewBox="0 0 16 16" fill="currentColor">
                  <path d="M13.78 4.22a.75.75 0 010 1.06l-7.25 7.25a.75.75 0 01-1.06 0L2.22 9.28a.75.75 0 011.06-1.06L6 10.94l6.72-6.72a.75.75 0 011.06 0z" />
                </svg>
              )}
            </button>
          ))}

          {/* 项目级「套利机会栏」开关（需求 3.1/4.3：默认关，显式开启） */}
          {activeProject && (
            <>
              <div className="my-1 border-t border-line" />
              <div className="px-3 py-1.5">
                <div className="flex items-center justify-between">
                  <span className="text-xs text-body">📈 {t("app.arbTab")}</span>
                  <button
                    className={`w-8 h-4 rounded-full transition-colors relative ${
                      activeProject.arb_enabled ? "bg-accent" : "bg-elevated"
                    }`}
                    onClick={async () => {
                      const next = !activeProject.arb_enabled;
                      await setArbBoard(activeProject.path, next, boardPath);
                      onProjectsChanged?.();
                    }}
                    title={t("arb.toggleHint")}
                  >
                    <span
                      className={`absolute top-0.5 w-3 h-3 rounded-full bg-white transition-all ${
                        activeProject.arb_enabled ? "left-4" : "left-0.5"
                      }`}
                    />
                  </button>
                </div>
                <input
                  className="w-full mt-1.5 bg-elevated/50 rounded px-1.5 py-0.5 text-[10px] text-muted focus:outline-none"
                  value={boardPath}
                  onChange={(e) => setBoardPath(e.target.value)}
                  placeholder="exports/board.json"
                  title={t("arb.pathHint")}
                />
                <div className="text-[9px] text-faint mt-0.5 truncate">{activeProject.name}</div>
              </div>
            </>
          )}
        </div>
      )}
    </div>
  );
}
