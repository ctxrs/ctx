import { requireEnum, schemaError, type TelemetryScalar } from "./telemetry-contract";

export const REFRESH_FAILURE_DIAGNOSTIC_KEYS = [
  "refresh_failure_stage", "refresh_failure_kind",
  "refresh_coverage_reason", "refresh_source_failure_class",
  "refresh_failure_reason",
] as const;

const IO_REASONS = [
  "io_not_found", "io_permission_denied", "io_storage_full", "io_read_only_filesystem",
  "io_out_of_memory", "io_timed_out",
];
const REASONS_BY_KIND = new Map([
  ["io", new Set(IO_REASONS)],
  ["provider", new Set([...IO_REASONS, "route_output_limit", "route_scratch_limit"])],
  ["index", new Set([...IO_REASONS, "index_memory_limit", "index_scratch_limit", "index_writer_invariant"])],
]);

export function parseRefreshFailureDiagnostic(
  properties: Record<string, unknown>,
  surface: string,
  outcome: string,
): Record<string, TelemetryScalar> {
  const hasStage = Object.hasOwn(properties, "refresh_failure_stage");
  const hasKind = Object.hasOwn(properties, "refresh_failure_kind");
  if (hasStage !== hasKind) throw schemaError("incomplete_refresh_failure_diagnostic");
  const out: Record<string, TelemetryScalar> = {};
  if (hasStage) {
    if (surface !== "daemon" || outcome !== "failure" || !Object.hasOwn(properties, "failure_code")) {
      throw schemaError("inconsistent_refresh_failure_diagnostic");
    }
    out.refresh_failure_stage = requireEnum(
      properties.refresh_failure_stage,
      new Set(["admission", "execution", "verification", "finalization"]),
      "invalid_refresh_failure_stage",
    );
    out.refresh_failure_kind = requireEnum(
      properties.refresh_failure_kind,
      new Set(["io", "index", "provider", "unknown"]),
      "invalid_refresh_failure_kind",
    );
  }
  if (Object.hasOwn(properties, "refresh_coverage_reason")) {
    if (!hasStage || out.refresh_failure_kind !== "provider"
      || properties.failure_code !== "all_provider_terminal_coverage_unavailable") {
      throw schemaError("inconsistent_refresh_coverage_reason");
    }
    out.refresh_coverage_reason = requireEnum(
      properties.refresh_coverage_reason,
      new Set([
        "catalog_unavailable", "unsafe_root", "missing_terminal_authority",
        "route_failed", "invalid_route_identity", "missing_empty_authority",
      ]),
      "invalid_refresh_coverage_reason",
    );
  }
  if (Object.hasOwn(properties, "refresh_failure_reason")) {
    const reasons = REASONS_BY_KIND.get(String(out.refresh_failure_kind));
    if (!hasStage || properties.refresh_result !== "failure"
      || typeof properties.retryable !== "boolean" || !reasons) {
      throw schemaError("inconsistent_refresh_failure_reason");
    }
    out.refresh_failure_reason = requireEnum(
      properties.refresh_failure_reason, reasons, "invalid_refresh_failure_reason",
    );
  }
  if (Object.hasOwn(properties, "refresh_source_failure_class")) {
    if (surface !== "daemon" || outcome !== "success" || properties.refresh_result !== "partial"
      || (properties.failure_scope !== "source" && properties.failure_scope !== "mixed")) {
      throw schemaError("inconsistent_refresh_source_failure_class");
    }
    out.refresh_source_failure_class = requireEnum(
      properties.refresh_source_failure_class,
      new Set(["unavailable", "source_changed", "unreadable", "incompatible", "mixed"]),
      "invalid_refresh_source_failure_class",
    );
  }
  return out;
}
