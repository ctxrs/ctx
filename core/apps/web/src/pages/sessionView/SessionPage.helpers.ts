import { type SessionTurn, type SubagentInvocationChild } from "../../api/client";
import {
  markdownToPlainText,
  normalizeTurnHeaderPlainText,
} from "@ctx/session-thread-layout";

export { markdownToPlainText };

const asRecord = (value: unknown): Record<string, unknown> => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, unknown>;
};

const readTrimmedString = (value: unknown): string => {
  if (typeof value !== "string") return "";
  return value.trim();
};

const normalizePlaceholderToolLabel = (value: string): string =>
  value.trim().toLowerCase().replace(/[\s._-]+/g, "");

export function isPlaceholderToolLabel(value?: string | null): boolean {
  const normalized = normalizePlaceholderToolLabel(String(value ?? ""));
  return normalized === "" || normalized === "unknown" || normalized === "tool" || normalized === "unknowntool";
}

export function normalizeDisplayToolLabel(value?: string | null): string {
  const trimmed = String(value ?? "").trim();
  if (!trimmed) return "";
  return trimmed.toLowerCase() === "agent" ? "Subagent" : trimmed;
}

function humanizeToolIdentifier(value: string): string {
  const trimmed = String(value ?? "").trim();
  if (!trimmed) return "";
  const spaced = trimmed
    .replace(/([a-z0-9])([A-Z])/g, "$1 $2")
    .replace(/[_-]+/g, " ")
    .replace(/\s+/g, " ")
    .trim();
  if (!spaced) return "";
  return spaced
    .split(" ")
    .map((part) => {
      if (!part) return part;
      if (part.length <= 4 && part.toUpperCase() === part) return part;
      return part[0]!.toUpperCase() + part.slice(1);
    })
    .join(" ");
}

export function attachmentDisplayName(name?: string | null) {
  const n = String(name ?? "").trim();
  if (!n) return "image";
  return n.split(/[\\/]/).pop() || "image";
}

export function appendSegment(base: string, addition: string): string {
  const trimmed = addition.trim();
  if (!trimmed) return base;
  if (!base) return trimmed;
  const needsSpace = /\S$/.test(base) && !/^[,.;!?]/.test(trimmed);
  return `${base}${needsSpace ? " " : ""}${trimmed}`;
}

export function appendFragment(base: string, fragment: string): string {
  const b = base ?? "";
  const f = fragment ?? "";
  if (!b) return f;
  if (!f) return b;
  if (f.startsWith(b)) return f;
  if (b.endsWith(f)) return b;
  return `${b}${f}`;
}

export { normalizeTurnHeaderPlainText };

export function parseIsoMs(value?: string | null): number | null {
  if (!value) return null;
  const parsed = Date.parse(value);
  return Number.isFinite(parsed) ? parsed : null;
}

export function formatElapsedMs(ms: number): string {
  const totalSeconds = Math.max(0, Math.floor(ms / 1000));
  const seconds = totalSeconds % 60;
  const minutes = Math.floor(totalSeconds / 60) % 60;
  const hours = Math.floor(totalSeconds / 3600);

  if (hours > 0) {
    return `${hours}h ${minutes}m ${seconds}s`;
  }
  if (minutes > 0) {
    return `${minutes}m ${seconds}s`;
  }
  return `${seconds}s`;
}

export function humanTurnStatus(status: SessionTurn["status"]): string {
  switch (status) {
    case "completed":
      return "Completed";
    case "interrupted":
      return "Interrupted";
    case "failed":
      return "Error";
    case "queued":
      return "Queued";
    case "running":
    default:
      return "Working";
  }
}

export function humanToolStatus(status: string): string {
  const s = String(status ?? "").trim().toLowerCase();
  switch (s) {
    case "pending":
    case "queued":
      return "Pending";
    case "running":
    case "in_progress":
    case "inprogress":
      return "Running";
    case "completed":
    case "complete":
    case "ok":
    case "success":
    case "succeeded":
      return "Completed";
    case "failed":
    case "error":
      return "Failed";
    default:
      return status ? String(status) : "";
  }
}

export function subagentChildLabel(child: SubagentInvocationChild): string {
  const label = child.label?.trim();
  if (label) return label;
  return `Subagent ${child.position + 1}`;
}

export function formatSubagentChildMeta(child: SubagentInvocationChild): string {
  const parts: string[] = [];
  if (child.harness) parts.push(child.harness);
  if (child.model) parts.push(child.model);
  if (child.reasoning_effort) parts.push(child.reasoning_effort);
  parts.push(`${child.prompt_length} chars`);
  return parts.join(" · ");
}

export function toolDisplayTitleFromPayload(payload: unknown): string {
  const update = asRecord(payload);
  const toolCall = asRecord(update.toolCall);
  for (const candidate of [
    normalizeDisplayToolLabel(readTrimmedString(update.title)),
    normalizeDisplayToolLabel(readTrimmedString(update.tool_label)),
    normalizeDisplayToolLabel(readTrimmedString(update.toolLabel)),
    normalizeDisplayToolLabel(readTrimmedString(toolCall.title)),
    normalizeDisplayToolLabel(readTrimmedString(toolCall.tool_label)),
    normalizeDisplayToolLabel(readTrimmedString(toolCall.toolLabel)),
    normalizeDisplayToolLabel(readTrimmedString(update.tool_name)),
    normalizeDisplayToolLabel(readTrimmedString(update.toolName)),
    normalizeDisplayToolLabel(readTrimmedString(update.name)),
    normalizeDisplayToolLabel(readTrimmedString(toolCall.name)),
  ]) {
    if (candidate && !isPlaceholderToolLabel(candidate)) return candidate;
  }
  return "";
}

export function toolKindIcon(kind: string): string {
  const k = String(kind ?? "").trim().toLowerCase();
  if (k === "execute" || k === "exec") return "$";
  if (k === "read" || k === "read_file") return "R";
  if (k === "search" || k === "list" || k === "list_files") return "S";
  if (k === "write" || k === "edit" || k === "apply_patch") return "W";
  return "·";
}

export function humanToolKind(kind: string): string {
  const raw = String(kind ?? "").trim();
  const k = raw.toLowerCase();
  if (k === "execute" || k === "exec") return "Run Command";
  if (k === "search") return "Search";
  if (k === "read" || k === "read_file") return "Read File";
  if (k === "edit" || k === "write" || k === "apply_patch") return "Edit File";
  if (k === "list" || k === "list_files") return "List Files";
  if (k === "fetch" || k === "http" || k === "curl") return "Fetch";
  if (k === "think") return "Think";
  if (k === "agent") return "Subagent";
  if (k === "error") return "Error";
  if (isPlaceholderToolLabel(raw)) return "Tool";
  return normalizeDisplayToolLabel(humanizeToolIdentifier(raw)) || "Tool";
}

export function formatToolInput(toolKind: string, input: unknown): string {
  const k = (toolKind || "").toLowerCase();
  const rec = asRecord(input);
  if (k === "execute" || k === "exec") {
    const cmd = Array.isArray(rec.command) ? rec.command.join(" ") : rec.command;
    const cwd = rec.cwd;
    const out: string[] = [];
    if (cwd) out.push(`cwd: ${cwd}`);
    if (cmd) out.push(`cmd: ${cmd}`);
    return out.join("\n") || JSON.stringify(input, null, 2);
  }
  if (typeof input === "string") return input;
  return JSON.stringify(input, null, 2);
}

type ToolDiffStats = {
  added?: number;
  removed?: number;
  files?: number;
};

function extractToolDiffStats(input: unknown): ToolDiffStats | null {
  const rec = asRecord(input);
  const raw = rec.diff_stats;
  if (!raw || typeof raw !== "object") return null;
  const rawRec = asRecord(raw);
  const added = Number(rawRec.added);
  const removed = Number(rawRec.removed);
  const files = Number(rawRec.files);
  const hasAny =
    Number.isFinite(added) || Number.isFinite(removed) || Number.isFinite(files);
  if (!hasAny) return null;
  return {
    added: Number.isFinite(added) ? added : undefined,
    removed: Number.isFinite(removed) ? removed : undefined,
    files: Number.isFinite(files) ? files : undefined,
  };
}

function formatToolDiffStats(input: unknown): string {
  const stats = extractToolDiffStats(input);
  if (!stats) return "";
  const parts: string[] = [];
  if (stats.added && stats.added > 0) parts.push(`+${stats.added}`);
  if (stats.removed && stats.removed > 0) parts.push(`-${stats.removed}`);
  if (!parts.length && stats.files && stats.files > 0) {
    parts.push(`${stats.files} files`);
  }
  return parts.length ? `(${parts.join(" ")})` : "";
}

function extractToolPaths(input: unknown): { paths: string[]; total?: number } {
  const rec = asRecord(input);
  const paths: string[] = [];
  const push = (value: unknown) => {
    if (typeof value !== "string") return;
    const trimmed = value.trim();
    if (trimmed) paths.push(trimmed);
  };
  push(rec.path);
  push(rec.file);
  push(rec.filename);
  push(rec.file_path);
  push(rec.filePath);
  push(rec.filepath);
  push(rec.target);
  if (Array.isArray(rec.paths)) rec.paths.forEach(push);
  if (Array.isArray(rec.files)) rec.files.forEach(push);
  if (Array.isArray(rec.file_paths)) rec.file_paths.forEach(push);
  if (Array.isArray(rec.filePaths)) rec.filePaths.forEach(push);
  if (Array.isArray(rec.parsed_cmd)) {
    rec.parsed_cmd.forEach((cmd) => push(asRecord(cmd).path));
  }
  const seen = new Set<string>();
  const unique = paths.filter((p) => {
    if (seen.has(p)) return false;
    seen.add(p);
    return true;
  });
  const total = typeof rec.paths_total === "number" ? rec.paths_total : unique.length;
  return { paths: unique, total };
}

function formatToolPathSummary(input: unknown): string {
  const { paths, total } = extractToolPaths(input);
  if (!paths.length) return "";
  const more = Math.max(0, (total ?? paths.length) - 1);
  const head = truncateMiddle(paths[0], 120);
  return more > 0 ? `${head} +${more} more` : head;
}

export function toolSummaryLine(toolKind: string, input: unknown): string {
  const k = (toolKind || "").toLowerCase();
  const rec = asRecord(input);
  const description = String(rec.description ?? "").trim();
  if (description) return truncateMiddle(description, 120);
  if (k.startsWith("mcp.")) {
    const server = String(rec.server ?? "").trim();
    const tool = String(rec.tool ?? "").trim();
    if (server && tool) return `${server}/${tool}`;
    if (server) return server;
    const parts = k.split(".").filter(Boolean);
    if (parts.length >= 3) return `${parts[1]}/${parts.slice(2).join(".")}`;
  }
  if (k === "execute" || k === "exec") {
    const cmd = Array.isArray(rec.command) ? rec.command.join(" ") : rec.command;
    return cmd ? truncateMiddle(String(cmd), 120) : "";
  }
  if (k === "bash" || k === "shell") {
    const cmd = Array.isArray(rec.command) ? rec.command.join(" ") : rec.command;
    return cmd ? truncateMiddle(String(cmd), 120) : "";
  }
  if (k === "search" || k === "web_search") {
    const q = rec.query ?? rec.pattern ?? rec.regex ?? rec.text;
    const path = formatToolPathSummary(input);
    const query = q ? truncateMiddle(String(q), 120) : "";
    if (query && path) return truncateMiddle(`${query} in ${path}`, 120);
    return query || path;
  }
  if (k === "grep") {
    const q = rec.pattern ?? rec.query ?? rec.regex ?? rec.text;
    const path = formatToolPathSummary(input);
    const query = q ? truncateMiddle(String(q), 120) : "";
    if (query && path) return truncateMiddle(`${query} in ${path}`, 120);
    return query || path;
  }
  if (k === "glob") {
    const pattern = rec.glob ?? rec.pattern;
    return pattern ? truncateMiddle(String(pattern), 120) : formatToolPathSummary(input);
  }
  if (k === "list" || k === "list_files") {
    return formatToolPathSummary(input);
  }
  if (k === "read" || k === "read_file") {
    return formatToolPathSummary(input);
  }
  if (k === "edit" || k === "write" || k === "apply_patch") {
    const path = formatToolPathSummary(input);
    const stats = formatToolDiffStats(input);
    if (path && stats) return `${path} ${stats}`;
    return path || stats;
  }
  if (k === "fetch" || k === "http" || k === "curl") {
    const method = String(rec.method ?? "GET").trim().toUpperCase();
    const url = rec.url ?? rec.uri ?? rec.href;
    if (url) return truncateMiddle(`${method} ${String(url)}`, 120);
    return method;
  }
  return "";
}

export function truncateMiddle(text: string, maxLen: number): string {
  const s = String(text ?? "");
  if (s.length <= maxLen) return s;
  const head = Math.max(10, Math.floor(maxLen * 0.6));
  const tail = Math.max(10, maxLen - head - 3);
  return `${s.slice(0, head)}...${s.slice(-tail)}`;
}

export function looksLikeMarkdown(text: string): boolean {
  const t = String(text ?? "");
  if (t.includes("```")) return true;
  if (/^#{1,6}\s/m.test(t)) return true;
  if (/^\s*[-*]\s+/m.test(t)) return true;
  if (/\[[^\]]+\]\([^)]+\)/.test(t)) return true;
  return false;
}
