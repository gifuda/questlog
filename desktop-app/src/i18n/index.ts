/**
 * i18n bootstrap — i18next + react-i18next + browser language detector.
 *
 * - Language auto-detected (navigator.language) and cached in localStorage
 *   under `i18nextLng` by i18next-browser-languagedetector.
 * - Fallback language: zh (missing keys render Chinese rather than blank).
 * - <html lang> stays in sync so screen readers / font fallback follow the UI.
 * - Cross-window sync: frontend emit() (no target) broadcasts to every
 *   webview; each window's listener syncs unless it originated the change.
 *   (No Rust forwarding — that created an emit/listen event loop.)
 */
import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import LanguageDetector from "i18next-browser-languagedetector";

import zh from "./locales/zh.json";
import en from "./locales/en.json";

export type AppLanguage = "zh" | "en";

/** Switch language everywhere: local i18next state + cross-window broadcast. */
export async function setLanguage(lng: AppLanguage): Promise<void> {
  await i18n.changeLanguage(lng);
  try {
    const { emit } = await import("@tauri-apps/api/event");
    await emit("lang-changed", lng);
  } catch {
    // Non-Tauri environment (dev in plain browser) — local change is enough.
  }
}

void i18n
  .use(LanguageDetector)
  .use(initReactI18next)
  .init({
    resources: {
      zh: { translation: zh },
      en: { translation: en },
    },
    fallbackLng: "zh",
    supportedLngs: ["zh", "en"],
    interpolation: { escapeValue: false },
    detection: {
      order: ["localStorage", "navigator"],
      caches: ["localStorage"],
      lookupLocalStorage: "i18nextLng",
    },
  });

// Keep <html lang> in sync with the active language (a11y + font fallback).
i18n.on("languageChanged", (lng) => {
  document.documentElement.lang = lng.startsWith("zh") ? "zh-CN" : "en";
});

export default i18n;
