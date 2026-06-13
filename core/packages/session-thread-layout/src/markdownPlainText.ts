import { stripCitationMarkers } from "./citationMarkers";

const PLAIN_TEXT_CACHE_LIMIT = 500;
const plainTextCache = new Map<string, string>();

export function markdownToPlainText(input: string): string {
  if (!input) return "";
  const cached = plainTextCache.get(input);
  if (cached != null) return cached;
  let text = stripCitationMarkers(input).replace(/\r/g, "");
  text = text.replace(/```[a-zA-Z0-9_-]*\n/g, "");
  text = text.replace(/```/g, "");
  text = text.replace(/`([^`]*)`/g, "$1");
  text = text.replace(/!\[([^\]]*)\]\([^)]+\)/g, "$1");
  text = text.replace(/\[([^\]]+)\]\([^)]+\)/g, "$1");
  text = text
    .split("\n")
    .map((line) => line.replace(/^\s*(?:#{1,6}|>|[*+-]|\d+\.)\s+/, ""))
    .join("\n");
  text = text.replace(/\n{3,}/g, "\n\n");
  const trimmed = text.trim();
  if (plainTextCache.size >= PLAIN_TEXT_CACHE_LIMIT) {
    const oldest = plainTextCache.keys().next().value as string | undefined;
    if (oldest) plainTextCache.delete(oldest);
  }
  plainTextCache.set(input, trimmed);
  return trimmed;
}
