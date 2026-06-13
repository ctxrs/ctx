import { stripCitationMarkers } from "./citationMarkers";
import { markdownToPlainText } from "./markdownPlainText";
import type { WorkbenchTurnHeader } from "./transcriptTypes";

export function normalizeTurnHeaderPlainText(input: string): string {
  if (!input) return "";
  let text = stripCitationMarkers(input).replace(/\r/g, "");
  text = text.replace(/```[a-zA-Z0-9_-]*\n/g, "");
  text = text.replace(/```/g, "");
  text = text.replace(/`([^`]*)`/g, "$1");
  text = text.replace(/!\[([^\]]*)\]\(([^)]+)\)/g, "$1");
  text = text.replace(/\[([^\]]+)\]\(([^)]+)\)/g, "$1");
  text = text.replace(/[ \t]+\n/g, "\n").replace(/\n{3,}/g, "\n\n");
  return text.trim();
}

export function getWorkbenchTurnHeaderDisplayPlainText(header: WorkbenchTurnHeader): string {
  const explicitPlainText = typeof header.plain_text === "string" ? header.plain_text : "";
  if (explicitPlainText.length > 0) {
    return normalizeTurnHeaderPlainText(explicitPlainText);
  }
  return markdownToPlainText(header.content ?? "");
}
