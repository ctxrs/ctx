import {
  COUNT_BUCKETS,
  DURATION_BUCKETS,
  requireBoolean,
  requireEnum,
  schemaError,
  type TelemetryScalar,
} from "./telemetry-contract";

// Ordinary CLI/MCP facts share the existing operation_completed@1 envelope.
// This set is deliberately separate from the retained signed pro_host contract.
export const ORDINARY_BLAME_PROPERTY_KEYS = new Set([
  "blame_target_kind", "blame_request_kind", "blame_query_duration_bucket",
  "blame_result_state", "blame_result_count_bucket", "blame_freshness", "blame_has_more",
  "blame_failure_class", "blame_failure_phase", "blame_output_served",
]);

const RESULT_KEYS = [
  "blame_result_state", "blame_result_count_bucket", "blame_freshness", "blame_has_more",
];
const FAILURE_KEYS = ["blame_failure_class", "blame_failure_phase"];
const ENUMS = new Map([
  ["blame_target_kind", new Set(["file", "commit", "pull_request"])],
  ["blame_request_kind", new Set(["first_request", "continuation"])],
  ["blame_result_state", new Set(["proven", "possible", "conflicting", "none"])],
  ["blame_freshness", new Set(["current", "stale_committed"])],
  ["blame_failure_class", new Set([
    "invalid_request", "source", "repository", "stale", "ambiguous", "corruption",
    "cancelled", "output", "other",
  ])],
  ["blame_failure_phase", new Set(["setup", "query", "presentation", "output"])],
]);

export function parseOrdinaryBlameProperties(
  properties: Record<string, unknown>,
  outcome: string,
  required: boolean,
): Record<string, TelemetryScalar> {
  const entries = Object.entries(properties).filter(([key]) => ORDINARY_BLAME_PROPERTY_KEYS.has(key));
  // Historical MCP terminals did not have Blame sidecars. Keep accepting them.
  if (!required && entries.length === 0) return {};
  if (!Object.hasOwn(properties, "blame_target_kind")) {
    throw schemaError("missing_blame_target_kind");
  }
  for (const group of [RESULT_KEYS, FAILURE_KEYS]) {
    const count = group.filter((key) => Object.hasOwn(properties, key)).length;
    if (count !== 0 && count !== group.length) throw schemaError("incomplete_blame_facts");
  }
  const out: Record<string, TelemetryScalar> = {};
  for (const [key, value] of entries) {
    const allowed = ENUMS.get(key)
      ?? (key === "blame_result_count_bucket" ? COUNT_BUCKETS : undefined)
      ?? (key === "blame_query_duration_bucket" ? DURATION_BUCKETS : undefined);
    out[key] = allowed
      ? requireEnum(value, allowed, `invalid_${key}`)
      : requireBoolean(value, `invalid_${key}`);
  }
  if (out.blame_query_duration_bucket === "unknown") throw schemaError("invalid_blame_query_duration_bucket");
  if (outcome === "success" && Object.hasOwn(out, "blame_failure_class")) {
    throw schemaError("invalid_blame_failure_outcome");
  }
  if (out.blame_output_served === true && (outcome !== "success" || !Object.hasOwn(out, "blame_result_state"))) {
    throw schemaError("invalid_blame_output_served");
  }
  return out;
}
