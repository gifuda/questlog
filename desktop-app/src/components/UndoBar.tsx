import { useTranslation } from "react-i18next";

/**
 * 撤销条（项目移除后的 12 秒挽回窗）——主窗口与托盘弹窗共用。
 * 配 useUndo：onUndo 恢复，onDismiss 立即关闭。
 */
export default function UndoBar({
  name,
  onUndo,
  onDismiss,
  edge = "top",
}: {
  name: string;
  onUndo: () => void;
  onDismiss: () => void;
  /** top：条在面板顶部（border-b）；bottom：主窗底部（border-t）。 */
  edge?: "top" | "bottom";
}) {
  const { t } = useTranslation();
  return (
    <div
      className={`flex items-center gap-2 px-4 py-1.5 bg-amber-bg ${
        edge === "top" ? "border-b" : "border-t"
      } border-amber/30`}
    >
      <span className="text-[11px] text-amber flex-1 truncate">
        {t("app.undoRemove", { name })}
      </span>
      <button className="text-[11px] text-amber font-bold underline" onClick={onUndo}>
        {t("app.undo")}
      </button>
      <button
        className="text-[11px] text-amber/70 hover:text-amber px-1"
        onClick={onDismiss}
        title={t("app.dismiss")}
      >
        ✕
      </button>
    </div>
  );
}
