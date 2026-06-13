const DIFF_LINE_GUARD_LIMIT = 10000;
const DIFF_FILE_GUARD_LIMIT = 200;

const readDiffSummaryNumber = (summary: Record<string, unknown> | null, keys: string[]): number | null => {
  if (!summary) return null;
  for (const key of keys) {
    const raw = summary[key];
    const value = Number(raw);
    if (Number.isFinite(value)) return value;
  }
  return null;
};

export const getDiffSummaryStats = (summary: Record<string, unknown> | null) => {
  const fileCount = readDiffSummaryNumber(summary, ["file_count", "files", "fileCount"]);
  const additions = readDiffSummaryNumber(summary, ["line_additions", "additions", "lineAdditions"]);
  const deletions = readDiffSummaryNumber(summary, ["line_deletions", "deletions", "lineDeletions"]);
  const lineCount =
    additions !== null && deletions !== null ? additions + deletions : additions ?? deletions ?? null;
  return { fileCount, additions, deletions, lineCount };
};

export const isDiffSummaryTooLarge = (summary: Record<string, unknown> | null) => {
  const { fileCount, lineCount } = getDiffSummaryStats(summary);
  if (lineCount !== null && lineCount > DIFF_LINE_GUARD_LIMIT) return true;
  if (fileCount !== null && fileCount > DIFF_FILE_GUARD_LIMIT) return true;
  return false;
};
