import {
  type TelemetryScalar,
  rejectUnknownKeys,
  requireBoolean,
  requireEnum,
  requireExact,
  requireInteger,
  requireRecord,
  requireString,
  schemaError,
} from "./telemetry-contract";

const BLAME_PRODUCT_V1_PROPERTY_KEYS = new Set([
  "blame_schema_version",
  "blame_semantics_version",
  "blame_surface",
  "blame_target_kind",
  "blame_request_kind",
  "blame_access_state",
  "blame_result_state",
  "blame_failure_class",
  "blame_freshness",
  "blame_has_more",
  "blame_output_served",
  "blame_pro_version",
  "blame_pro_protocol_version",
]);

const BLAME_PRODUCT_V2_PROPERTY_KEYS = new Set([
  "blame_schema_version",
  "blame_surface",
  "blame_target_kind",
  "blame_request_kind",
  "blame_query_duration_bucket",
  "blame_result_state",
  "blame_result_count_bucket",
  "blame_freshness",
  "blame_has_more",
  "blame_failure_class",
  "blame_failure_phase",
]);

export const BLAME_PRODUCT_PROPERTY_KEYS = new Set([
  ...BLAME_PRODUCT_V1_PROPERTY_KEYS,
  ...BLAME_PRODUCT_V2_PROPERTY_KEYS,
]);

const SURFACES = new Set(["cli"]);
const V2_SURFACES = new Set(["cli", "mcp"]);
const TARGET_KINDS = new Set(["file", "commit", "pull_request"]);
const REQUEST_KINDS = new Set(["first_request", "continuation"]);
const ACCESS_STATES = new Set([
  "trial", "active", "canceling_paid", "offline_grace", "locked", "unavailable",
]);
const RESULT_STATES = new Set(["proven", "possible", "conflicting", "none"]);
const RESULT_COUNT_BUCKETS = new Set([
  "0", "1", "2-5", "6-20", "21-100", "101-1k", "1k-10k", "10k-100k",
  "100k-1m", "1m+",
]);
const MEASURED_DURATION_BUCKETS = new Set([
  "lt_100ms", "lt_1s", "lt_5s", "lt_30s", "lt_2m", "lt_10m", "lt_1h", "gte_1h",
]);
const FRESHNESS_STATES = new Set(["current", "stale_committed"]);
const FAILURE_PHASES = new Set(["setup", "query", "presentation"]);
export const BLAME_FAILURE_CLASSES = [
  "commercial", "installation", "authorization", "key_store", "protocol", "source",
  "repository", "stale", "ambiguous", "invalid_request", "invalid_response", "cancelled",
  "helper_crashed", "helper_timeout", "output", "other",
] as const;
const FAILURE_CLASSES = new Set<string>(BLAME_FAILURE_CLASSES);
const V2_FAILURE_CLASSES = new Set<string>(
  BLAME_FAILURE_CLASSES.filter((value) => value !== "output"),
);
export function isCurrentBlameProductContract(properties: Record<string, unknown>): boolean {
  return Object.hasOwn(properties, "blame_schema_version");
}

export function parseCurrentBlameProductProperties(
  value: unknown,
  outcome: string,
): Record<string, TelemetryScalar> {
  const properties = requireRecord(value, "invalid_properties");
  return properties.blame_schema_version === 2
    ? parseBlameProductV2Properties(properties, outcome)
    : parseBlameProductV1Properties(properties, outcome);
}

function parseBlameProductV1Properties(
  properties: Record<string, unknown>,
  outcome: string,
): Record<string, TelemetryScalar> {
  rejectUnknownKeys(
    properties,
    BLAME_PRODUCT_V1_PROPERTY_KEYS,
    "unknown_blame_product_property",
  );
  requireExact(properties.blame_schema_version, 1, "invalid_blame_schema_version");
  requireExact(properties.blame_semantics_version, 1, "invalid_blame_semantics_version");

  const out: Record<string, TelemetryScalar> = {
    blame_schema_version: 1,
    blame_semantics_version: 1,
    blame_surface: requireEnum(properties.blame_surface, SURFACES, "invalid_blame_surface"),
    blame_target_kind: requireEnum(
      properties.blame_target_kind,
      TARGET_KINDS,
      "invalid_blame_target_kind",
    ),
    blame_request_kind: requireEnum(
      properties.blame_request_kind,
      REQUEST_KINDS,
      "invalid_blame_request_kind",
    ),
  };
  addOptionalEnum(out, properties, "blame_access_state", ACCESS_STATES);
  addOptionalPatch(out, properties, "blame_pro_version");
  addOptionalProtocol(out, properties, "blame_pro_protocol_version");

  if (!Object.hasOwn(properties, "blame_output_served")) {
    throw schemaError("missing_blame_output_served");
  }
  out.blame_output_served = requireBoolean(
    properties.blame_output_served,
    "invalid_blame_output_served",
  );
  if (outcome === "success") {
    if (
      out.blame_output_served !== true
      || Object.hasOwn(properties, "blame_failure_class")
    ) {
      throw schemaError("contradictory_blame_success");
    }
    out.blame_result_state = requireEnum(
      properties.blame_result_state,
      RESULT_STATES,
      "invalid_blame_result_state",
    );
    out.blame_freshness = requireEnum(
      properties.blame_freshness,
      FRESHNESS_STATES,
      "invalid_blame_freshness",
    );
    out.blame_has_more = requireBoolean(properties.blame_has_more, "invalid_blame_has_more");
    return out;
  }
  if (outcome !== "failure") throw schemaError("invalid_blame_outcome");
  if (
    out.blame_output_served !== false
    || ["blame_result_state", "blame_freshness", "blame_has_more"].some(
      (key) => Object.hasOwn(properties, key),
    )
  ) {
    throw schemaError("contradictory_blame_failure");
  }
  out.blame_failure_class = requireEnum(
    properties.blame_failure_class,
    FAILURE_CLASSES,
    "invalid_blame_failure_class",
  );
  return out;
}

function parseBlameProductV2Properties(
  properties: Record<string, unknown>,
  outcome: string,
): Record<string, TelemetryScalar> {
  rejectUnknownKeys(
    properties,
    BLAME_PRODUCT_V2_PROPERTY_KEYS,
    "unknown_blame_product_property",
  );
  requireExact(properties.blame_schema_version, 2, "invalid_blame_schema_version");

  const out: Record<string, TelemetryScalar> = {
    blame_schema_version: 2,
    blame_surface: requireEnum(properties.blame_surface, V2_SURFACES, "invalid_blame_surface"),
    blame_target_kind: requireEnum(
      properties.blame_target_kind,
      TARGET_KINDS,
      "invalid_blame_target_kind",
    ),
    blame_request_kind: requireEnum(
      properties.blame_request_kind,
      REQUEST_KINDS,
      "invalid_blame_request_kind",
    ),
  };
  addOptionalEnum(out, properties, "blame_query_duration_bucket", MEASURED_DURATION_BUCKETS);

  if (outcome === "success") {
    if (
      Object.hasOwn(properties, "blame_failure_class")
      || Object.hasOwn(properties, "blame_failure_phase")
    ) throw schemaError("contradictory_blame_success");
    requireMeasuredDuration(out, properties, "blame_query_duration_bucket");
    out.blame_result_state = requireEnum(
      properties.blame_result_state,
      RESULT_STATES,
      "invalid_blame_result_state",
    );
    out.blame_result_count_bucket = requireEnum(
      properties.blame_result_count_bucket,
      RESULT_COUNT_BUCKETS,
      "invalid_blame_result_count_bucket",
    );
    out.blame_freshness = requireEnum(
      properties.blame_freshness,
      FRESHNESS_STATES,
      "invalid_blame_freshness",
    );
    out.blame_has_more = requireBoolean(properties.blame_has_more, "invalid_blame_has_more");
    return out;
  }
  if (outcome !== "failure") throw schemaError("invalid_blame_outcome");
  if (
    [
      "blame_result_state",
      "blame_result_count_bucket",
      "blame_freshness",
      "blame_has_more",
    ].some((key) => Object.hasOwn(properties, key))
  ) throw schemaError("contradictory_blame_failure");
  out.blame_failure_class = requireEnum(
    properties.blame_failure_class,
    V2_FAILURE_CLASSES,
    "invalid_blame_failure_class",
  );
  out.blame_failure_phase = requireEnum(
    properties.blame_failure_phase,
    FAILURE_PHASES,
    "invalid_blame_failure_phase",
  );
  if (out.blame_failure_phase === "setup") {
    if (Object.hasOwn(properties, "blame_query_duration_bucket")) {
      throw schemaError("contradictory_blame_failure_phase");
    }
  } else {
    requireMeasuredDuration(out, properties, "blame_query_duration_bucket");
  }
  return out;
}

function requireMeasuredDuration(
  out: Record<string, TelemetryScalar>,
  properties: Record<string, unknown>,
  key: string,
): void {
  out[key] = requireEnum(properties[key], MEASURED_DURATION_BUCKETS, `invalid_${key}`);
}

function addOptionalEnum(
  out: Record<string, TelemetryScalar>,
  properties: Record<string, unknown>,
  key: string,
  allowed: ReadonlySet<string>,
): void {
  if (!Object.hasOwn(properties, key)) return;
  out[key] = requireEnum(properties[key], allowed, `invalid_${key}`);
}

function addOptionalPatch(
  out: Record<string, TelemetryScalar>,
  properties: Record<string, unknown>,
  key: string,
): void {
  if (!Object.hasOwn(properties, key)) return;
  const value = requireString(properties[key], `invalid_${key}`);
  if (!isBoundedExactBlameVersion(value)) throw schemaError(`invalid_${key}`);
  out[key] = value;
}

export function isBoundedExactBlameVersion(value: string): boolean {
  if (value.length === 0 || value.length > 64 || !/^[\x20-\x7e]+$/u.test(value)) return false;
  const plus = value.indexOf("+");
  if (plus !== value.lastIndexOf("+")) return false;
  const withoutBuild = plus === -1 ? value : value.slice(0, plus);
  const build = plus === -1 ? undefined : value.slice(plus + 1);
  const dash = withoutBuild.indexOf("-");
  const core = dash === -1 ? withoutBuild : withoutBuild.slice(0, dash);
  const prerelease = dash === -1 ? undefined : withoutBuild.slice(dash + 1);
  const coreParts = core.split(".");
  return coreParts.length === 3
    && coreParts.every((part) => (
      /^(?:0|[1-9][0-9]{0,4})$/u.test(part)
    ))
    && (prerelease === undefined || validVersionIdentifiers(prerelease, true))
    && (build === undefined || validVersionIdentifiers(build, false));
}

function validVersionIdentifiers(value: string, rejectNumericLeadingZero: boolean): boolean {
  return value.length > 0 && value.split(".").every((part) => (
    part.length > 0
    && /^[0-9A-Za-z-]+$/u.test(part)
    && !(
      rejectNumericLeadingZero
      && part.length > 1
      && part.startsWith("0")
      && /^[0-9]+$/u.test(part)
    )
  ));
}

function addOptionalProtocol(
  out: Record<string, TelemetryScalar>,
  properties: Record<string, unknown>,
  key: string,
): void {
  if (!Object.hasOwn(properties, key)) return;
  const value = requireInteger(properties[key], 1, 65_535, `invalid_${key}`);
  out[key] = value;
}
