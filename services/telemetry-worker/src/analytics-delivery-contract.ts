import {
  COUNT_BUCKETS,
  DURATION_BUCKETS,
  type TelemetryScalar,
  rejectUnknownKeys,
  requireEnum,
  requireRecord,
  schemaError,
} from "./telemetry-contract";

const ANALYTICS_DELIVERY_REQUIRED_PROPERTY_KEYS = [
  "queued_count_bucket",
  "retry_attempt_count_bucket",
  "dropped_count_bucket",
  "oldest_queued_age_bucket",
  "failure_class",
];
export const ANALYTICS_DELIVERY_PROPERTY_KEYS = new Set([
  ...ANALYTICS_DELIVERY_REQUIRED_PROPERTY_KEYS, "delivery_failure_reason",
]);
const DELIVERY_REASONS_BY_CLASS = new Map([
  ["transport", new Set([
    "request_dns", "request_connect", "request_timeout", "request_io", "response_status_408",
    "response_body_timeout", "response_body_io",
  ])],
  ["local_io", new Set([
    "file_open", "file_write", "file_flush", "outbox_corrupt", "outbox_expired",
    "outbox_capacity", "outbox_clock", "outbox_oversized",
  ])],
]);
const ANALYTICS_DELIVERY_FAILURE_CLASSES = new Set([
  "none",
  "transport",
  "rate_limited",
  "client_rejection",
  "server",
  "local_io",
  "configuration",
  "unknown",
]);

export function parseAnalyticsDeliveryProperties(
  value: unknown,
  outcome: string,
  surface: string,
  operation: string,
): Record<string, TelemetryScalar> {
  if (surface !== "cli" || operation !== "outbox") {
    throw schemaError("invalid_analytics_delivery_surface");
  }
  const properties = requireRecord(value, "invalid_properties");
  rejectUnknownKeys(
    properties,
    ANALYTICS_DELIVERY_PROPERTY_KEYS,
    "unknown_analytics_delivery_property",
  );
  for (const key of ANALYTICS_DELIVERY_REQUIRED_PROPERTY_KEYS) {
    if (!Object.hasOwn(properties, key)) {
      throw schemaError("incomplete_analytics_delivery_observation");
    }
  }
  const out: Record<string, TelemetryScalar> = {
    queued_count_bucket: requireEnum(
      properties.queued_count_bucket,
      COUNT_BUCKETS,
      "invalid_queued_count_bucket",
    ),
    retry_attempt_count_bucket: requireEnum(
      properties.retry_attempt_count_bucket,
      COUNT_BUCKETS,
      "invalid_retry_attempt_count_bucket",
    ),
    dropped_count_bucket: requireEnum(
      properties.dropped_count_bucket,
      COUNT_BUCKETS,
      "invalid_dropped_count_bucket",
    ),
    oldest_queued_age_bucket: requireEnum(
      properties.oldest_queued_age_bucket,
      DURATION_BUCKETS,
      "invalid_oldest_queued_age_bucket",
    ),
    failure_class: requireEnum(
      properties.failure_class,
      ANALYTICS_DELIVERY_FAILURE_CLASSES,
      "invalid_analytics_delivery_failure_class",
    ),
  };
  const degraded = out.queued_count_bucket !== "0"
    || out.dropped_count_bucket !== "0"
    || out.failure_class !== "none";
  if ((outcome === "failure") !== degraded) {
    throw schemaError("inconsistent_analytics_delivery_outcome");
  }
  if (Object.hasOwn(properties, "delivery_failure_reason")) {
    const reasons = DELIVERY_REASONS_BY_CLASS.get(String(out.failure_class));
    if (outcome !== "failure" || !reasons) {
      throw schemaError("inconsistent_delivery_failure_reason");
    }
    out.delivery_failure_reason = requireEnum(
      properties.delivery_failure_reason, reasons, "invalid_delivery_failure_reason",
    );
  }
  return out;
}
