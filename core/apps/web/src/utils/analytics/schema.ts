import type { AnalyticsProperties, AnalyticsScalar } from "./types";

const MAX_PROPERTY_COUNT = 64;
const MAX_KEY_LENGTH = 80;
const MAX_STRING_LENGTH = 512;
const ALLOWED_TOKEN_METRIC_KEYS = new Set([
  "total_tokens_estimate",
  "input_tokens",
  "output_tokens",
  "context_window_tokens",
  "remaining_tokens_estimate",
]);

const FORBIDDEN_KEY_PATTERN =
  /(prompt|code|file.?path|repo.?name|branch|command|token|secret|api.?key|password|authorization|cookie)/i;

const FORBIDDEN_SCOPE_KEYS = new Set([
  "workspaceid",
  "taskid",
  "sessionid",
  "worktreeid",
  "runid",
  "turnid",
]);

const isAllowedScalar = (value: unknown): value is AnalyticsScalar =>
  typeof value === "string" || typeof value === "number" || typeof value === "boolean";

const normalizeString = (value: string): string => value.slice(0, MAX_STRING_LENGTH);

const normalizedKey = (key: string): string => key.replace(/[^a-z0-9]/gi, "").toLowerCase();

export const sanitizeAnalyticsProperties = (
  raw: Record<string, unknown>,
): AnalyticsProperties => {
  const out: AnalyticsProperties = {};
  for (const [key, value] of Object.entries(raw)) {
    if (Object.keys(out).length >= MAX_PROPERTY_COUNT) break;
    if (!key || key.length > MAX_KEY_LENGTH) continue;
    if (FORBIDDEN_SCOPE_KEYS.has(normalizedKey(key))) continue;
    if (FORBIDDEN_KEY_PATTERN.test(key) && !ALLOWED_TOKEN_METRIC_KEYS.has(key)) continue;
    if (!isAllowedScalar(value)) continue;
    if (typeof value === "number" && !Number.isFinite(value)) continue;
    out[key] = typeof value === "string" ? normalizeString(value) : value;
  }
  return out;
};
