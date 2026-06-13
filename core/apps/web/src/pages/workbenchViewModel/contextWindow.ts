import type { ContextWindowInfo } from "../../components/WorkbenchComposer";

const asRecord = (value: unknown): Record<string, unknown> =>
  value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};

function coerceNumber(value: unknown): number | null {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value === "string" && value.trim()) {
    const parsed = Number.parseFloat(value);
    return Number.isFinite(parsed) ? parsed : null;
  }
  return null;
}

export function normalizeContextWindowMetrics(metrics: unknown): ContextWindowInfo | null {
  const record = asRecord(metrics);
  if (!record) return null;
  const windowTokens = coerceNumber(record.context_window_tokens);
  if (!windowTokens || windowTokens <= 0) return null;

  const contextTokensEstimate = coerceNumber(record.context_tokens_estimate);

  let usedTokens: number | null = null;
  if (contextTokensEstimate != null) {
    usedTokens = contextTokensEstimate;
  }

  let remainingTokens = coerceNumber(record.remaining_tokens_estimate);
  let remainingFraction = coerceNumber(record.remaining_fraction);

  if (remainingFraction != null && remainingFraction > 1) {
    remainingFraction = remainingFraction <= 100 ? remainingFraction / 100 : null;
  }
  if (remainingFraction != null) {
    remainingFraction = Math.max(0, Math.min(1, remainingFraction));
  }

  if (remainingTokens == null && usedTokens != null) {
    remainingTokens = Math.max(0, windowTokens - usedTokens);
  }
  if (usedTokens == null && remainingTokens != null) {
    usedTokens = Math.max(0, windowTokens - remainingTokens);
  }
  if (remainingFraction == null && usedTokens != null) {
    remainingFraction = Math.max(0, Math.min(1, 1 - usedTokens / windowTokens));
  }
  if (usedTokens == null && remainingFraction != null) {
    usedTokens = Math.max(0, Math.round(windowTokens * (1 - remainingFraction)));
  }

  return {
    windowTokens,
    usedTokens: usedTokens ?? undefined,
    remainingTokens: remainingTokens ?? undefined,
    remainingFraction: remainingFraction ?? undefined,
  };
}
