import { classifyProvider, type ProviderClassification } from "./provider-contract";
import {
  INSTALL_STAGE_V1_KEYS,
  LEGACY_INSTALL_STAGE_KEYS,
  RELEASE_VERSION_PATTERN,
  V1_BATCH_KEYS,
  V1_EVENT_KEYS,
  isKnownOperationPropertyKey,
} from "./telemetry-contract";
import { isKnownSurfacePropertyKey } from "./telemetry-surface-contract";

export type RejectionSizeBucket =
  | "lt_1kb"
  | "1kb_8kb"
  | "8kb_64kb"
  | "64kb_256kb"
  | "256kb_plus";

export type TelemetryRejectionDiagnostics = Readonly<{
  app_version?: string;
  field_shape: readonly string[];
  field_shape_overflow: boolean;
  provider_classification: ProviderClassification | "mixed";
  size_bucket: RejectionSizeBucket;
}>;

const MAX_FIELD_SHAPE_ENTRIES = 128;

export function buildRejectionDiagnostics(
  payload: unknown,
  byteLength: number,
  endpoint: "telemetry_batch" | "install_stage",
): TelemetryRejectionDiagnostics {
  const shape = new Set<string>();
  const providerClassifications = new Set<ProviderClassification>();

  if (isRecord(payload)) {
    if (endpoint === "install_stage") {
      addRecordShape(shape, "install", payload, new Set([
        ...INSTALL_STAGE_V1_KEYS,
        ...LEGACY_INSTALL_STAGE_KEYS,
      ]));
    } else {
      addRecordShape(shape, "batch", payload, V1_BATCH_KEYS);
      if (Array.isArray(payload.events)) {
        for (const candidate of payload.events) {
          if (!isRecord(candidate)) continue;
          addRecordShape(shape, "event", candidate, V1_EVENT_KEYS);
          if (!isRecord(candidate.properties)) continue;
          for (const [key, value] of Object.entries(candidate.properties)) {
            if (!isKnownOperationPropertyKey(key) && !isKnownSurfacePropertyKey(key)) continue;
            shape.add(`properties.${key}:${jsonType(value)}`);
          }
          for (const key of ["provider", "provider_filter"] as const) {
            if (Object.hasOwn(candidate.properties, key)) {
              providerClassifications.add(classifyProvider(candidate.properties[key]));
            }
          }
        }
      }
    }
  }

  const fieldShape = [...shape].sort();
  return {
    ...(containsBlameShape(payload) ? {} : { app_version: appVersion(payload) }),
    field_shape: fieldShape.slice(0, MAX_FIELD_SHAPE_ENTRIES),
    field_shape_overflow: fieldShape.length > MAX_FIELD_SHAPE_ENTRIES,
    provider_classification: combinedProviderClassification(providerClassifications),
    size_bucket: rejectionSizeBucket(byteLength),
  };
}

// Exact Core versions are restricted Blame receipt facts. Suppress them for
// both valid and malformed Blame-shaped batches before rejection observations
// can reach operational logs.
function containsBlameShape(payload: unknown): boolean {
  if (!isRecord(payload) || !Array.isArray(payload.events)) return false;
  return payload.events.some((candidate) => {
    if (!isRecord(candidate)) return false;
    if (candidate.operation === "blame") return true;
    if (!isRecord(candidate.properties)) return false;
    return Object.keys(candidate.properties).some((key) => key.startsWith("blame_"));
  });
}

export function rejectionSizeBucket(byteLength: number): RejectionSizeBucket {
  if (byteLength < 1024) return "lt_1kb";
  if (byteLength < 8 * 1024) return "1kb_8kb";
  if (byteLength < 64 * 1024) return "8kb_64kb";
  if (byteLength < 256 * 1024) return "64kb_256kb";
  return "256kb_plus";
}

function appVersion(payload: unknown): string {
  if (!isRecord(payload)) return "unknown";
  const value = payload.app_version ?? payload.broker_app_version ?? payload.version;
  return typeof value === "string" && RELEASE_VERSION_PATTERN.test(value) ? value : "unknown";
}

function addRecordShape(
  shape: Set<string>,
  prefix: string,
  value: Record<string, unknown>,
  knownKeys: ReadonlySet<string>,
): void {
  for (const [key, entry] of Object.entries(value)) {
    if (knownKeys.has(key)) shape.add(`${prefix}.${key}:${jsonType(entry)}`);
  }
}

function combinedProviderClassification(
  values: ReadonlySet<ProviderClassification>,
): ProviderClassification | "mixed" {
  if (values.size === 0) return "neutral";
  return values.size === 1 ? [...values][0] : "mixed";
}

function jsonType(value: unknown): string {
  if (value === null) return "null";
  if (Array.isArray(value)) return "array";
  return typeof value === "object" ? "object" : typeof value;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
