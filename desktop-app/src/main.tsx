import React from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import App from "./App";
import TimelinePanel from "./components/TimelinePanel";
import i18n from "./i18n";
import "./index.css";

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
