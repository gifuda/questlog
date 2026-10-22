import { useState, useEffect, useRef, useCallback } from "react";
import { useTranslation } from "react-i18next";

/**
 * Two-click confirmation button.
 * First click → button turns red with confirmation text, auto-reverts after 3s.
 * Second click within 3s → fires onConfirm.
 */
export default function ConfirmButton({
  label,
  confirmLabel,
  onConfirm,
  className = "",
  title,
  onPendingChange,
  tone = "danger",
}: {
  label: React.ReactNode;
  confirmLabel?: string;
  onConfirm: () => void;
  className?: string;
  title?: string;
  /** 确认窗开合回调——调用方可借此在确认窗期间锁定同组其他动作。 */
  onPendingChange?: (pending: boolean) => void;
  /** danger=破坏性操作（红）；accent=知情授权（蓝，如夜间自动 opt-in）。 */
  tone?: "danger" | "accent";
}) {
  const { t } = useTranslation();
  const [pending, setPending] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);

  const setPendingBoth = useCallback(
    (p: boolean) => {
      setPending(p);
      onPendingChange?.(p);
    },
    [onPendingChange],
  );

  const reset = useCallback(() => setPendingBoth(false), [setPendingBoth]);

  useEffect(() => {
    if (pending) {
      timer.current = setTimeout(reset, 3000);
      return () => clearTimeout(timer.current);
    }
  }, [pending, reset]);

  function handleClick(e: React.MouseEvent) {
    e.stopPropagation();
    e.preventDefault();
    if (pending) {
      clearTimeout(timer.current);
      setPendingBoth(false);
      onConfirm();
    } else {
      setPendingBoth(true);
    }
  }

  if (pending) {
    // 保留调用方的尺寸/布局类，避免确认态布局跳动；配色与 hover 全部钉死。
    const toneCls =
      tone === "accent"
        ? "bg-accent! text-on-accent! hover:bg-accent!"
        : "bg-danger! text-on-accent! hover:bg-danger!";
    return (
      <button
        className={`${className} ${toneCls} animate-pulse`}
        onClick={handleClick}
        title={title}
      >
        {confirmLabel ?? t("app.confirm")}
      </button>
    );
  }

  return (
    <button
      className={className}
      onClick={handleClick}
      title={title}
    >
      {label}
    </button>
  );
}
