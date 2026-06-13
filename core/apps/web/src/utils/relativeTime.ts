export function formatRelativeAgeShort(iso: string | null | undefined, nowMs: number = Date.now()): string {
  const raw = String(iso ?? "").trim();
  if (!raw) return "";
  const t = Date.parse(raw);
  if (!Number.isFinite(t)) return "";

  const diffMs = Math.max(0, nowMs - t);
  const diffSec = Math.floor(diffMs / 1000);
  if (diffSec < 45) return "Now";

  const diffMin = Math.floor(diffSec / 60);
  if (diffMin < 60) return `${Math.max(1, diffMin)}m`;

  const diffHr = Math.floor(diffMin / 60);
  if (diffHr < 24) return `${diffHr}h`;

  const diffDay = Math.floor(diffHr / 24);
  if (diffDay < 7) return `${diffDay}d`;

  const diffWeek = Math.floor(diffDay / 7);
  if (diffWeek < 52) return `${diffWeek}w`;

  const diffYear = Math.floor(diffWeek / 52);
  return `${Math.max(1, diffYear)}y`;
}
