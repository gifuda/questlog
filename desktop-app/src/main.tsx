import React from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import App from "./App";
import TimelinePanel from "./components/TimelinePanel";
import i18n from "./i18n";
import "./index.css";

// Forward JS errors to the Rust stdout log via diag_report_error.
// WKWebView console is invisible in release builds — without this bridge a
// render-time crash shows up as a silent white panel (the arb-board
// white-screen of 2026-08-31 took hours to diagnose for exactly this reason).
window.addEventListener("error", (e) => {
  void invoke("diag_report_error", {
    msg: `${e.message} @ ${e.filename}:${e.lineno}:${e.colno}\n${e.error?.stack ?? "no stack"}`,
  }).catch(() => {});
});
window.addEventListener("unhandledrejection", (e) => {
  const reason = e.reason as Error | string;
  void invoke("diag_report_error", {
    msg: `unhandled rejection: ${String(reason)}\n${(reason as Error)?.stack ?? "no stack"}`,
  }).catch(() => {});
});

async function bootstrap() {
  const label = getCurrentWindow().label;
  const isPopup = label === "popup" || window.location.hash === "#popup";

  // Popup follows system theme automatically
  if (isPopup) {
    const dark = window.matchMedia("(prefers-color-scheme: dark)").matches;
    if (!dark) document.documentElement.setAttribute("data-theme", "light");
  }

  // Cross-window language sync: frontend emit() (no target) broadcasts to every
  // webview, so other windows pick up the switch. The guard skips the window
  // that originated the change (its resolvedLanguage already matches).
  await listen<string>("lang-changed", (e) => {
    if (e.payload && e.payload !== i18n.resolvedLanguage) {
      void i18n.changeLanguage(e.payload);
    }
  });

  const root = document.getElementById("root")!;

  ReactDOM.createRoot(root).render(
    <React.StrictMode>
      {isPopup ? <TimelinePanel /> : <App />}
    </React.StrictMode>
  );
}

bootstrap();
