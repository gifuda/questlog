/** 统一徽章：单一字号（11px 下限）+ 语义 tone，替代散落的 9px/10px 硬编码徽章。 */
const tones = {
  accent: "bg-accent-bg text-accent",
  info: "bg-info-bg text-info",
  amber: "bg-amber-bg text-amber",
  success: "bg-success/15 text-success",
  danger: "bg-danger/15 text-danger",
  muted: "bg-elevated text-faint",
} as const;

export type BadgeTone = keyof typeof tones;

export default function Badge({
  tone,
  children,
  title,
  className = "",
  onClick,
}: {
  tone: BadgeTone;
  children: React.ReactNode;
  title?: string;
  className?: string;
  /** 给了 onClick 徽章即按钮（用于可切换状态的徽章，如夜间自动）。 */
  onClick?: () => void;
}) {
  const cls = `text-[11px] px-1.5 py-0.5 rounded whitespace-nowrap ${tones[tone]} ${className}`;
  if (onClick) {
    return (
      <button
        type="button"
        title={title}
        onClick={(e) => {
          // 徽章常嵌在可点击的标题/卡片里，先阻断冒泡再触发自身动作。
          e.stopPropagation();
          onClick();
        }}
        className={`${cls} hover:brightness-125 transition-colors`}
      >
        {children}
      </button>
    );
  }
  return (
    <span title={title} className={cls}>
      {children}
    </span>
  );
}
