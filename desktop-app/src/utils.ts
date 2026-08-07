/**
 * Strip markdown formatting for clean plain-text display.
 * Keeps content readable but removes **bold**, ## headings, `code`, etc.
 */
export function stripMd(text: string): string {
  return text
    .replace(/^#{1,6}\s+/gm, "")       // ## heading
    .replace(/\*\*(.+?)\*\*/g, "$1")    // **bold**
    .replace(/\*(.+?)\*/g, "$1")        // *italic*
    .replace(/`([^`]+)`/g, "$1")        // `code`
    .replace(/~~(.+?)~~/g, "$1")        // ~~strike~~
    .replace(/\[(.+?)\]\(.+?\)/g, "$1") // [link](url)
    .replace(/^[-*]\s+/gm, "• ")        // - list item
    .replace(/^\d+\.\s+/gm, "")         // 1. numbered list
    .replace(/^>\s+/gm, "")             // > blockquote
    .replace(/\n{2,}/g, "\n")           // collapse multi-blank lines
    .trim();
}

/**
 * Locale-aware date formatting, driven by the UI language (not the system),
 * so a manual language switch also switches date formats.
 */
export function formatDate(ts: string | number, lang: string): string {
  const locale = lang.startsWith("zh") ? "zh-CN" : "en-US";
  return new Date(ts).toLocaleDateString(locale);
}
