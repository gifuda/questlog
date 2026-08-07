/**
 * QuestLog logo — the three-checkmark mark extracted from the app icon.
 * Rendered from the PNG asset so it matches the menu bar tray icon exactly.
 */
export default function Logo({ size = 20 }: { size?: number }) {
  return (
    <img
      src="/logo-check.png"
      alt="QuestLog"
      width={size}
      height={size}
      draggable={false}
      className="select-none"
    />
  );
}
