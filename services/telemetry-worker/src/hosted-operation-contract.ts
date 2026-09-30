import {
  rejectUnknownKeys,
  requireEnum,
  schemaError,
  SHARED_PROPERTY_KEYS,
  type TelemetryScalar,
  validateSharedProperties,
} from "./telemetry-contract";

export const HOSTED_OPERATIONS = new Set([
  "archive_export", "archive_restore", "remote_connect", "remote_share", "remote_sync",
  "server_init", "server_invite", "server_grant", "server_revoke", "server_withdraw",
  "server_backup", "server_restore",
]);
export const HOSTED_PROPERTY_KEYS = new Set(["output", "hosted_failure_stage", "failure_type"]);
const FAILURE_TYPES = new Set([
  "invalid_request", "unauthorized", "forbidden", "not_found", "conflict", "credentials",
  "policy_denied", "unavailable", "capacity", "io", "invalid_archive", "other",
]);

// Shared by HTTP ingress and normalized Queue revalidation. These terminals
// describe a finite command, never a server process or an individual record.
export function parseHostedProperties(
  properties: Record<string, unknown>,
  outcome: string,
): Record<string, TelemetryScalar> {
  rejectUnknownKeys(properties, new Set([...SHARED_PROPERTY_KEYS, ...HOSTED_PROPERTY_KEYS]),
    "unknown_hosted_operation_property");
  const out: Record<string, TelemetryScalar> = {
    ...validateSharedProperties(properties),
    output: requireEnum(properties.output, new Set(["human", "json"]), "missing_output"),
  };
  const failed = outcome === "failure";
  if (
    Object.hasOwn(properties, "hosted_failure_stage") !== failed ||
    Object.hasOwn(properties, "failure_type") !== failed
  ) throw schemaError("invalid_hosted_failure_shape");
  if (failed) {
    out.hosted_failure_stage = requireEnum(properties.hosted_failure_stage,
      new Set(["operation", "output"]), "invalid_hosted_failure_stage");
    out.failure_type = requireEnum(properties.failure_type, FAILURE_TYPES, "invalid_hosted_failure_type");
    if (out.hosted_failure_stage === "output" && out.failure_type !== "io") {
      throw schemaError("invalid_hosted_output_failure");
    }
  }
  return out;
}
