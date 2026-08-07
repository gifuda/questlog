import { useState, useEffect, useCallback } from "react";

export type Theme = "dark" | "light" | "system";

const STORAGE_KEY = "todo-board-theme";

function resolve(theme: Theme): "dark" | "light" {
  if (theme !== "system") return theme;
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

export function useTheme() {
  const [theme, setThemeRaw] = useState<Theme>(() => {
    return (localStorage.getItem(STORAGE_KEY) as Theme) ?? "dark";
  });

  const apply = useCallback((t: Theme) => {
    document.documentElement.setAttribute("data-theme", resolve(t));
  }, []);

  useEffect(() => { apply(theme); }, [theme, apply]);

  // Listen to system changes when theme is "system"
  useEffect(() => {
    if (theme !== "system") return;
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const handler = () => apply("system");
    mq.addEventListener("change", handler);
    return () => mq.removeEventListener("change", handler);
  }, [theme, apply]);

  function setTheme(t: Theme) {
    setThemeRaw(t);
    localStorage.setItem(STORAGE_KEY, t);
  }

  return { theme, setTheme };
}
