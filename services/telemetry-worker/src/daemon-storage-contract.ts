import {
  type TelemetryScalar,
  requireEnum,
  schemaError,
} from "./telemetry-contract";

export const DAEMON_STORAGE_PROPERTY_KEYS = [
  "filesystem_total_bytes_bucket",
  "filesystem_available_bytes_bucket",
  "filesystem_available_fraction_bucket",
  "core_active_logical_bytes_bucket",
  "core_certified_source_bytes_bucket",
  "core_logical_amplification_bucket",
  "filesystem_available_to_active_core_ratio_bucket",
] as const;

const STORAGE_BYTE_BUCKETS = new Set([
  "0", "lt_100mb", "100mb-1gb", "1gb-5gb", "5gb-10gb", "10gb-25gb",
  "25gb-50gb", "50gb-100gb", "100gb-250gb", "250gb-500gb", "500gb-1tb",
  "1tb-2tb", "2tb-5tb", "5tb+",
]);
const FILESYSTEM_AVAILABLE_FRACTION_BUCKETS = new Set([
  "0", "lt_5pct", "5pct-10pct", "10pct-20pct", "20pct-40pct", "40pct-60pct",
  "60pct+",
]);
const CORE_LOGICAL_AMPLIFICATION_BUCKETS = new Set([
  "lt_0_10x", "0_10x-0_25x", "0_25x-0_35x", "0_35x-0_50x", "0_50x-1x",
  "1x-2x", "2x+",
]);
const FILESYSTEM_AVAILABLE_TO_ACTIVE_CORE_RATIO_BUCKETS = new Set([
  "lt_0_5x", "0_5x-1x", "1x-1_25x", "1_25x-2x", "2x-4x", "4x+",
]);

export function parseDaemonStorageProperties(
  out: Record<string, TelemetryScalar>,
  properties: Record<string, unknown>,
): void {
  const filesystemKeys = [
    "filesystem_total_bytes_bucket",
    "filesystem_available_bytes_bucket",
    "filesystem_available_fraction_bucket",
  ] as const;
  const coreStockKeys = [
    "core_active_logical_bytes_bucket",
    "core_certified_source_bytes_bucket",
  ] as const;
  const filesystemCount = filesystemKeys.filter((key) => Object.hasOwn(properties, key)).length;
  if (filesystemCount !== 0 && filesystemCount !== filesystemKeys.length) {
    throw schemaError("incomplete_filesystem_storage_snapshot");
  }
  const coreStockCount = coreStockKeys.filter((key) => Object.hasOwn(properties, key)).length;
  if (coreStockCount !== 0 && coreStockCount !== coreStockKeys.length) {
    throw schemaError("incomplete_core_storage_snapshot");
  }

  if (filesystemCount !== 0) {
    out.filesystem_total_bytes_bucket = requireEnum(
      properties.filesystem_total_bytes_bucket,
      STORAGE_BYTE_BUCKETS,
      "invalid_filesystem_total_bytes_bucket",
    );
    out.filesystem_available_bytes_bucket = requireEnum(
      properties.filesystem_available_bytes_bucket,
      STORAGE_BYTE_BUCKETS,
      "invalid_filesystem_available_bytes_bucket",
    );
    out.filesystem_available_fraction_bucket = requireEnum(
      properties.filesystem_available_fraction_bucket,
      FILESYSTEM_AVAILABLE_FRACTION_BUCKETS,
      "invalid_filesystem_available_fraction_bucket",
    );
  }

  if (coreStockCount !== 0) {
    out.core_active_logical_bytes_bucket = requireEnum(
      properties.core_active_logical_bytes_bucket,
      STORAGE_BYTE_BUCKETS,
      "invalid_core_active_logical_bytes_bucket",
    );
    out.core_certified_source_bytes_bucket = requireEnum(
      properties.core_certified_source_bytes_bucket,
      STORAGE_BYTE_BUCKETS,
      "invalid_core_certified_source_bytes_bucket",
    );
  }

  const hasAmplification = Object.hasOwn(properties, "core_logical_amplification_bucket");
  const amplificationRequired = coreStockCount !== 0
    && out.core_certified_source_bytes_bucket !== "0";
  if (hasAmplification !== amplificationRequired) {
    throw schemaError("inconsistent_core_logical_amplification");
  }
  if (hasAmplification) {
    out.core_logical_amplification_bucket = requireEnum(
      properties.core_logical_amplification_bucket,
      CORE_LOGICAL_AMPLIFICATION_BUCKETS,
      "invalid_core_logical_amplification_bucket",
    );
  }

  const hasAvailableToCoreRatio = Object.hasOwn(
    properties,
    "filesystem_available_to_active_core_ratio_bucket",
  );
  const availableToCoreRatioAllowed = filesystemCount !== 0
    && coreStockCount !== 0
    && out.core_active_logical_bytes_bucket !== "0";
  if (hasAvailableToCoreRatio && !availableToCoreRatioAllowed) {
    throw schemaError("inconsistent_filesystem_available_to_active_core_ratio");
  }
  if (hasAvailableToCoreRatio) {
    out.filesystem_available_to_active_core_ratio_bucket = requireEnum(
      properties.filesystem_available_to_active_core_ratio_bucket,
      FILESYSTEM_AVAILABLE_TO_ACTIVE_CORE_RATIO_BUCKETS,
      "invalid_filesystem_available_to_active_core_ratio_bucket",
    );
  }
}
