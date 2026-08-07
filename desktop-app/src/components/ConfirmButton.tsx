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
}: {
  label: React.ReactNode;
  confirmLabel?: string;
  onConfirm: () => void;
  className?: string;
  title?: string;
}) {
  const { t } = useTranslation();
  const [pending, setPending] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);

  const reset = useCallback(() => setPending(false), []);

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
      setPending(false);
      onConfirm();
    } else {
      setPending(true);
    }
  }

  if (pending) {
    return (
      <button
        className="bg-red-500 text-white font-bold rounded px-2 py-0.5 animate-pulse"
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
