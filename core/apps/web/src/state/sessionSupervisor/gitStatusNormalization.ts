import type { GitStatusSummary } from "../../api/client";

const readString = (value: unknown): string | undefined => {
  if (typeof value === "string") return value;
  return undefined;
};

const readBool = (value: unknown): boolean | undefined => {
  if (typeof value === "boolean") return value;
  return undefined;
};

const readNumber = (value: unknown): number | undefined => {
  const num = typeof value === "number" ? value : Number(value);
  if (!Number.isFinite(num)) return undefined;
  return num;
};

export const normalizeGitStatusSummaryInput = (
  value: unknown,
  entries?: unknown,
): Partial<GitStatusSummary> => {
  if (!value || typeof value !== "object") {
    return Array.isArray(entries) ? { entries: entries as GitStatusSummary["entries"] } : {};
  }
  const src = value as Record<string, unknown>;
  const out: Partial<GitStatusSummary> = {};
  const raw = readString(src.raw);
  if (raw !== undefined) out.raw = raw;
  const summaryLine = readString(src.summary_line);
  if (summaryLine !== undefined) out.summary_line = summaryLine;
  const summaryLineAlt = readString(src.summaryLine);
  if (summaryLineAlt !== undefined) out.summaryLine = summaryLineAlt;
  const summary = readString(src.summary);
  if (summary !== undefined) out.summary = summary;
  const status = readString(src.status);
  if (status !== undefined) out.status = status;
  if (Array.isArray(src.lines)) out.lines = src.lines as string[];
  const branch = readString(src.branch);
  if (branch !== undefined) out.branch = branch;
  const upstream = readString(src.upstream);
  if (upstream !== undefined) out.upstream = upstream;
  const ahead = readNumber(src.ahead);
  if (ahead !== undefined) out.ahead = ahead;
  const behind = readNumber(src.behind);
  if (behind !== undefined) out.behind = behind;
  const detached = readBool(src.detached);
  if (detached !== undefined) out.detached = detached;
  const staged = readNumber(src.staged);
  if (staged !== undefined) out.staged = staged;
  const unstaged = readNumber(src.unstaged);
  if (unstaged !== undefined) out.unstaged = unstaged;
  const untracked = readNumber(src.untracked);
  if (untracked !== undefined) out.untracked = untracked;
  if (Array.isArray(src.entries)) out.entries = src.entries as GitStatusSummary["entries"];
  if (Array.isArray(entries)) out.entries = entries as GitStatusSummary["entries"];
  return out;
};
