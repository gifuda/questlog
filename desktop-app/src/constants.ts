/** Shared constants for item types, used across main window and popup panel.
 *  Type labels are i18n'd — use t(`card.itemType${Type}`) from the locales. */

export const typeIcons: Record<string, string> = {
  knowhow: "📘",
  todo: "✅",
  idea: "💡",
};

export const priorityDot: Record<string, string> = {
  high: "bg-red-500",
  medium: "bg-yellow-500",
  low: "bg-gray-500",
};

export const typeOrder = ["todo", "knowhow", "idea"] as const;

/** How many days before a project is auto-archived as "inactive". */
export const ARCHIVE_DAYS = 30;
