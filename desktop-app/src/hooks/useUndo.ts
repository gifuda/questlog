import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";

interface UndoInfo {
  name: string;
  path: string;
}

/**
 * Shared undo-delete logic used by both the main window and the popup panel.
 * After removing a project, a yellow undo bar is shown for `timeoutMs`.
 * Calling undo() restores the project via undo_remove_project command.
 */
export function useUndo(timeoutMs = 12000) {
  const [undoInfo, setUndoInfo] = useState<UndoInfo | null>(null);

  useEffect(() => {
    if (!undoInfo) return;
    const t = setTimeout(() => setUndoInfo(null), timeoutMs);
    return () => clearTimeout(t);
  }, [undoInfo, timeoutMs]);

  const trigger = useCallback((info: UndoInfo) => {
    setUndoInfo(info);
  }, []);

  const undo = useCallback(async () => {
    if (!undoInfo) return;
    await invoke("undo_remove_project", {
      name: undoInfo.name,
      path: undoInfo.path,
    });
    setUndoInfo(null);
  }, [undoInfo]);

  const dismiss = useCallback(() => setUndoInfo(null), []);

  return { undoInfo, trigger, undo, dismiss };
}
