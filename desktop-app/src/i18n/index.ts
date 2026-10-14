/**
 * i18n bootstrap — i18next + react-i18next.
 *
 * 2026-10-14 用户裁决：个人桌面工具，不做多语言——语言检测/切换/英文包
 * 全部移除，钉死中文。i18n 框架保留（全仓 t() 调用不动），只留 zh 资源。
 */
import i18n from "i18next";
import { initReactI18next } from "react-i18next";

import zh from "./locales/zh.json";

void i18n.use(initReactI18next).init({
  resources: { zh: { translation: zh } },
  lng: "zh",
  fallbackLng: "zh",
  interpolation: { escapeValue: false },
});

document.documentElement.lang = "zh-CN";

export default i18n;
