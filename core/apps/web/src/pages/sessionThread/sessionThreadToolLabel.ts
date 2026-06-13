import type { ThreadItem } from "@ctx/session-thread-layout";
import {
  humanToolKind,
  isPlaceholderToolLabel,
  normalizeDisplayToolLabel,
  toolSummaryLine,
  truncateMiddle,
} from "../sessionView/SessionPage.helpers";

const INLINE_SUMMARY_VERBS = new Set([
  "Read",
  "Explored",
  "Searched",
  "Wrote",
  "Edited",
  "Ran",
  "Run",
  "Fetch",
  "Fetched",
  "Search",
  "List",
  "Write",
  "Edit",
]);

const PREFIX_VERBS = [
  "Read",
  "Explored",
  "Searched",
  "Wrote",
  "Edited",
  "Run",
  "Fetch",
  "Search",
  "List",
  "Write",
  "Edit",
  "Subagent",
];

type LabelParts = {
  verb: string;
  rest: string;
  label: string;
};

export type WorkbenchToolLabel = {
  verb: string;
  inlineTail: string;
};

const asRecord = (value: unknown): Record<string, unknown> => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, unknown>;
};

const normalizeWorktreePath = (path?: string) => {
  const value = String(path ?? "").trim();
  if (!value) return "";
  return value.replace(/\/home\/[^/]+\/\.ctx\/worktrees\/[0-9a-f-]+\/[0-9a-f-]+\//g, "");
};

const shortPath = (path?: string) => {
  const value = normalizeWorktreePath(path).trim();
  if (!value) return "";
  const parts = value.split(/[\\/]/).filter(Boolean);
  if (parts.length <= 2) return value;
  return `${parts[parts.length - 2]}/${parts[parts.length - 1]}`;
};

const makeParts = (verb: string, rest?: string): LabelParts => {
  const trimmedRest = String(rest ?? "").trim();
  return {
    verb,
    rest: trimmedRest,
    label: trimmedRest ? `${verb} ${trimmedRest}` : verb,
  };
};

const parsePrefixed = (value: string, verbs: string[]): LabelParts | null => {
  const trimmed = String(value ?? "").trim();
  if (!trimmed) return null;
  for (const verb of verbs) {
    if (trimmed === verb) return makeParts(verb);
    if (trimmed.startsWith(`${verb} `)) return makeParts(verb, trimmed.slice(verb.length + 1));
  }
  return null;
};

const isInlineSummaryVerb = (value: string) => INLINE_SUMMARY_VERBS.has(String(value ?? "").trim());

const shortCommand = (raw: string) => {
  let command = String(raw ?? "").trim();
  if (!command) return "";
  command = command.replace(/^\/bin\/bash\s+-lc\s+/, "");
  command = command.replace(/^bash\s+-lc\s+/, "");
  command = command.replace(/^set\s+-euo\s+pipefail\s*;?\s*/i, "");
  command = command.replace(/^\s*&&\s*/, "");
  command = command.replace(/^"(.+)"$/, "$1");
  command = command.replace(/\/home\/[^/]+\/\.ctx\/worktrees\/[0-9a-f-]+\/[0-9a-f-]+\//g, "");
  const supercatMatch = command.match(/(?:^|\s)(\.?\/?scripts\/supercat\.sh)\s+([^\s&;]+)/);
  if (supercatMatch) return `./scripts/supercat.sh ${shortPath(supercatMatch[2])}`;
  const readManyMatch =
    command.match(/rg\s+--files\s+([^\s|&;]+)\s*\|\s*sort\b[\s\S]*xargs[\s\S]*\bcat\b/) ??
    command.match(/find\s+([^\s|&;]+)\s+.*xargs[\s\S]*\bcat\b/);
  if (readManyMatch) return `Read ${shortPath(readManyMatch[1])}`;
  const catMatch = command.match(/(?:^|[;&|]\s*)cat\s+([^\s|&;]+)(?:\s|$)/);
  if (catMatch) return `Read ${shortPath(catMatch[1])}`;
  const lsMatch = command.match(/(?:^|[;&|]\s*)(?:ls|ls\s+-la|ls\s+-l)\s+([^\s|&;]+)(?:\s|$)/);
  if (lsMatch) return `Explored ${shortPath(lsMatch[1])}`;
  const rgMatch = command.match(/(?:^|[;&|]\s*)rg\s+([^\s]+)\s+([^\s|&;]+)(?:\s|$)/);
  if (rgMatch) return `Searched ${truncateMiddle(rgMatch[1], 60)}`;
  const first = command.split(/\s+/).slice(0, 4).join(" ");
  return truncateMiddle(first, 80);
};

export function buildWorkbenchToolLabel(item: Extract<ThreadItem, { kind: "tool" }>): WorkbenchToolLabel {
  const kind = String(item.tool_kind ?? "").toLowerCase();
  const pathFromLoc = item.locations?.[0]?.path;
  const title = String(item.title ?? "").trim();
  const summary = String(item.subtitle ?? "").trim() || toolSummaryLine(kind, item.input);

  const labelParts = (() => {
    const normalizedTitle = normalizeWorktreePath(title);
    const displayTitle = isPlaceholderToolLabel(normalizedTitle) ? "" : normalizeDisplayToolLabel(normalizedTitle);
    const providerToolName = normalizeWorktreePath(String(item.provider_tool_name ?? "").trim());
    const displayProviderToolName = isPlaceholderToolLabel(providerToolName)
      ? ""
      : normalizeDisplayToolLabel(providerToolName);
    const titlePrefixed = parsePrefixed(displayTitle, PREFIX_VERBS);
    const titleRest = titlePrefixed?.rest ?? "";

    if (displayTitle && displayTitle !== "Tool") {
      if (summary && isInlineSummaryVerb(displayTitle)) {
        return makeParts(displayTitle, summary);
      }
      if (titlePrefixed) return titlePrefixed;
      return makeParts(displayTitle);
    }

    const providerPrefixed = parsePrefixed(displayProviderToolName, PREFIX_VERBS);
    if (displayProviderToolName) {
      if (providerPrefixed) return providerPrefixed;
      return makeParts(displayProviderToolName);
    }

    const inputRecord = asRecord(item.input);
    const parsedCmd = inputRecord.parsed_cmd;
    const parsed = Array.isArray(parsedCmd) ? parsedCmd.map((cmd) => asRecord(cmd)) : [];
    if (parsed.length > 0) {
      const firstParsed = parsed[0] ?? {};
      const parsedPath = typeof firstParsed.path === "string" ? firstParsed.path : "";
      if (firstParsed.type === "list_files" && parsedPath) return makeParts("Explored", shortPath(parsedPath));
      if (firstParsed.type === "read_file" && parsedPath) return makeParts("Read", shortPath(parsedPath));
      if (firstParsed.type === "search") {
        const query = String(firstParsed.query ?? firstParsed.pattern ?? firstParsed.regex ?? firstParsed.text ?? "").trim();
        if (query) return makeParts("Searched", truncateMiddle(query, 90));
        if (parsedPath) return makeParts("Searched", shortPath(parsedPath));
        return makeParts("Searched");
      }
    }
    if (kind === "search") {
      const query = String(
        inputRecord.query ??
          inputRecord.pattern ??
          inputRecord.regex ??
          inputRecord.text ??
          summary ??
          titleRest ??
          "",
      ).trim();
      return query ? makeParts("Searched", truncateMiddle(query, 90)) : makeParts("Searched");
    }
    if (kind === "execute") {
      const command = Array.isArray(inputRecord.command) ? inputRecord.command.join(" ") : inputRecord.command;
      const short = shortCommand(String(command ?? ""));
      const parsedShort = parsePrefixed(short, ["Read", "Explored", "Searched", "Wrote", "Edited"]);
      if (parsedShort) return parsedShort;
      return short ? makeParts("Run", short) : makeParts("Run");
    }
    if (kind === "read_file" || kind === "read") {
      const path = pathFromLoc ?? summary ?? titleRest;
      return path ? makeParts("Read", shortPath(path)) : makeParts("Read");
    }
    if (kind === "list" || kind === "list_files") {
      const path = summary || pathFromLoc || titleRest;
      return path ? makeParts("Explored", shortPath(path)) : makeParts("Explored");
    }
    if (kind === "write" || kind === "edit" || kind === "apply_patch") {
      const path = summary || pathFromLoc || titleRest;
      const verb = kind === "write" ? "Wrote" : "Edited";
      return path ? makeParts(verb, shortPath(path)) : makeParts(verb);
    }
    if (kind === "fetch" || kind === "http") {
      const path = summary || titleRest;
      return path ? makeParts("Fetch", path) : makeParts("Fetch");
    }
    if (kind === "error") return makeParts("Error");

    const fallbackVerb = humanToolKind(item.tool_kind);
    if (summary && fallbackVerb === "Tool") {
      return makeParts("Tool", summary);
    }
    const fallbackLabel = displayTitle || displayProviderToolName || fallbackVerb;
    const parsedLabel = parsePrefixed(fallbackLabel, ["Read", "Explored", "Searched", "Wrote", "Edited", "Run"]);
    if (parsedLabel) return parsedLabel;
    if (fallbackLabel && fallbackLabel !== fallbackVerb) {
      return makeParts(fallbackVerb, fallbackLabel);
    }
    return makeParts(fallbackVerb);
  })();

  const description = (() => {
    const trimmed = String(summary ?? "").trim();
    if (!trimmed) return "";
    if (trimmed === labelParts.rest || trimmed === labelParts.label || trimmed === title) return "";
    return trimmed;
  })();
  const inlineTail = [labelParts.rest, description]
    .filter((part) => String(part).trim().length > 0)
    .join(" · ");
  return {
    verb: labelParts.verb,
    inlineTail,
  };
}
