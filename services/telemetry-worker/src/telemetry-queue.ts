import { parseAnalyticsDeliveryProperties } from "./analytics-delivery-contract";
import {
  isBoundedExactBlameVersion,
  isCurrentBlameProductContract,
  parseCurrentBlameProductProperties,
} from "./blame-product-contract";
import type {
  TelemetryEventFamily,
  TelemetryIngestEndpoint,
} from "./database";
import type { BlameProductReceipt } from "./blame-product-receipt";
import {
  LEGACY_DURATION_BUCKETS,
  legacyCliRelease,
  parseLegacyCliProperties,
} from "./legacy-cli-contract";
import { PROVIDERS } from "./provider-contract";
import {
  classifyActivity,
  type InstallStageRow,
  type TelemetryRow,
} from "./telemetry-ingest";
import {
  ARCHITECTURES,
  DURATION_BUCKETS,
  EVENT_NAMES,
  INSTALL_ARCHITECTURES,
  INSTALL_PLATFORMS,
  INSTALL_SCRIPT_FAMILIES,
  INSTALL_STAGES,
  INSTALL_STATUSES,
  LEGACY_INSTALL_CHANNELS,
  LEGACY_INSTALL_STAGES,
  OPERATING_SYSTEMS,
  OUTCOMES,
  RELEASE_VERSION_PATTERN,
  SURFACES,
  UUID_PATTERN,
  parseOperationProperties,
  validateInstallError,
  validateInstallStatus,
  validateOperation,
  validateProviderRefreshOperation,
  validateRuntimeOperation,
} from "./telemetry-contract";
import {
  parseProviderRefreshProperties,
  parseRuntimeProperties,
  parseSurfaceOperationProperties,
  validateSharedPropertyConsistency,
} from "./telemetry-surface-contract";

export const TELEMETRY_QUEUE_FORMAT_VERSION = 1 as const;
const TELEMETRY_QUEUE_REDRIVE_FORMAT_VERSION = 1 as const;
export const CLOUDFLARE_QUEUE_MESSAGE_LIMIT_BYTES = 128_000;
// Cloudflare documents approximately 100 bytes of internal metadata inside
// the message limit. Keep a second 100-byte margin rather than relying on an
// approximate boundary.
export const MAX_TELEMETRY_QUEUE_BODY_BYTES =
  CLOUDFLARE_QUEUE_MESSAGE_LIMIT_BYTES - 200;
export const MAX_TELEMETRY_QUEUE_UNCOMPRESSED_BYTES = 64 * 1024;
const MAX_TELEMETRY_QUEUE_REDRIVE_BASE64_BYTES =
  Math.ceil(MAX_TELEMETRY_QUEUE_UNCOMPRESSED_BYTES / 3) * 4;
const TELEMETRY_QUEUE_RETRY_BASE_SECONDS = 300;
const TELEMETRY_QUEUE_RETRY_MAX_SECONDS = 12 * 60 * 60;

export type TelemetryQueueCollisionVisibility = Readonly<{
  analytics_environment: "production" | "staging";
  endpoint: TelemetryIngestEndpoint;
  event_family: TelemetryEventFamily;
  app_version: string;
  field_shape_fingerprint: string;
  field_shape_overflow: boolean;
  provider_classification: string;
  size_bucket: string;
}>;

type TelemetryQueueMessageBase = Readonly<{
  format_version: typeof TELEMETRY_QUEUE_FORMAT_VERSION;
  collision_visibility: TelemetryQueueCollisionVisibility;
}>;

export type TelemetryQueueMessage =
  | (TelemetryQueueMessageBase & Readonly<{
      kind: "telemetry_row";
      row: TelemetryRow;
    }>)
  | (TelemetryQueueMessageBase & Readonly<{
      kind: "blame_product_receipt";
      receipt: BlameProductReceipt;
    }>)
  | (TelemetryQueueMessageBase & Readonly<{
      kind: "install_stage_row";
      row: InstallStageRow;
    }>);

export class TelemetryQueueMessageTooLargeError extends Error {
  constructor() {
    super("queue_message_too_large");
  }
}

export function telemetryQueueCollisionIdentity(
  message: TelemetryQueueMessage,
): readonly [string, string] {
  if (message.kind === "telemetry_row") {
    return [message.row.event_id, message.row.payload_fingerprint];
  }
  if (message.kind === "blame_product_receipt") {
    return [message.receipt.event_id, message.receipt.replay_fingerprint];
  }
  return [
    message.row.event_id ?? message.row.install_attempt_id_hash,
    message.row.payload_fingerprint ?? "legacy_without_payload_fingerprint",
  ];
}

export function telemetryQueueRetryDelay(attempts: number): number {
  const attempt = Number.isSafeInteger(attempts) && attempts >= 1
    ? Math.min(attempts, 32)
    : 1;
  return Math.min(
    TELEMETRY_QUEUE_RETRY_BASE_SECONDS * (2 ** (attempt - 1)),
    TELEMETRY_QUEUE_RETRY_MAX_SECONDS,
  );
}

export function serializedTelemetryQueueMessageBytes(
  message: TelemetryQueueMessage,
): Uint8Array {
  return new TextEncoder().encode(JSON.stringify(message));
}

export async function encodeTelemetryQueueMessage(
  message: TelemetryQueueMessage,
): Promise<ArrayBuffer> {
  const serialized = serializedTelemetryQueueMessageBytes(message);
  if (serialized.byteLength > MAX_TELEMETRY_QUEUE_UNCOMPRESSED_BYTES) {
    throw new TelemetryQueueMessageTooLargeError();
  }
  const compressed = await transformBytes(
    serialized,
    new CompressionStream("gzip"),
  );
  if (compressed.byteLength > MAX_TELEMETRY_QUEUE_BODY_BYTES) {
    throw new TelemetryQueueMessageTooLargeError();
  }
  return compressed;
}

export async function decodeTelemetryQueueMessage(
  body: unknown,
): Promise<TelemetryQueueMessage | null> {
  const encoded = await queueBodyBytes(body);
  if (!encoded) return null;
  try {
    const decompressed = encoded.compression === "gzip"
      ? await decompressBytes(
          new Uint8Array(encoded.bytes),
          MAX_TELEMETRY_QUEUE_UNCOMPRESSED_BYTES,
        )
      : new Uint8Array(encoded.bytes);
    const text = new TextDecoder("utf-8", { fatal: true }).decode(decompressed);
    return parseTelemetryQueueMessage(JSON.parse(text) as unknown);
  } catch {
    return null;
  }
}

async function queueBodyBytes(body: unknown): Promise<Readonly<{
  bytes: ArrayBuffer;
  compression: "gzip" | "identity";
}> | null> {
  const direct = arrayBuffer(body);
  if (direct) {
    return direct.byteLength <= MAX_TELEMETRY_QUEUE_BODY_BYTES
      ? {bytes: direct, compression: "gzip"}
      : null;
  }
  if (
    !isRecord(body)
    || !hasExactKeys(body, [
      "body_base64",
      "body_sha256",
      "content_encoding",
      "content_type",
      "format_version",
      "kind",
    ])
    || body.format_version !== TELEMETRY_QUEUE_REDRIVE_FORMAT_VERSION
    || body.kind !== "telemetry_queue_redrive"
    || body.content_encoding !== "identity"
    || body.content_type !== "application/json"
    || !isSha256(body.body_sha256)
    || typeof body.body_base64 !== "string"
    || body.body_base64.length === 0
    || body.body_base64.length > MAX_TELEMETRY_QUEUE_REDRIVE_BASE64_BYTES
    || body.body_base64.length % 4 !== 0
    || !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/u
      .test(body.body_base64)
  ) return null;
  try {
    const binary = atob(body.body_base64);
    const bytes = new Uint8Array(binary.length);
    for (let index = 0; index < binary.length; index += 1) {
      bytes[index] = binary.charCodeAt(index);
    }
    if (bytes.byteLength > MAX_TELEMETRY_QUEUE_UNCOMPRESSED_BYTES) return null;
    const digest = await crypto.subtle.digest("SHA-256", bytes);
    const actual = Array.from(
      new Uint8Array(digest),
      (byte) => byte.toString(16).padStart(2, "0"),
    ).join("");
    return actual === body.body_sha256
      ? {bytes: bytes.buffer, compression: "identity"}
      : null;
  } catch {
    return null;
  }
}

async function decompressBytes(bytes: Uint8Array, maximumBytes: number): Promise<Uint8Array> {
  const owned = new ArrayBuffer(bytes.byteLength);
  new Uint8Array(owned).set(bytes);
  const reader = new Blob([owned])
    .stream()
    .pipeThrough(new DecompressionStream("gzip"))
    .getReader();
  const chunks: Uint8Array[] = [];
  let byteLength = 0;
  try {
    while (true) {
      const result = await reader.read();
      if (result.done) break;
      if (result.value.byteLength > maximumBytes - byteLength) {
        await reader.cancel();
        throw new TelemetryQueueMessageTooLargeError();
      }
      chunks.push(result.value);
      byteLength += result.value.byteLength;
    }
  } finally {
    reader.releaseLock();
  }
  const decompressed = new Uint8Array(byteLength);
  let offset = 0;
  for (const chunk of chunks) {
    decompressed.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return decompressed;
}

function parseTelemetryQueueMessage(value: unknown): TelemetryQueueMessage | null {
  if (!isRecord(value) || value.format_version !== TELEMETRY_QUEUE_FORMAT_VERSION) {
    return null;
  }
  if (!isCollisionVisibility(value.collision_visibility)) return null;
  if (value.kind === "telemetry_row") {
    if (!hasExactKeys(value, [
      "collision_visibility", "format_version", "kind", "row",
    ]) || !isTelemetryRow(value.row)
      || !collisionVisibilityMatchesTelemetryRow(value.collision_visibility, value.row)) return null;
    return value as TelemetryQueueMessage;
  }
  if (value.kind === "blame_product_receipt") {
    if (!hasExactKeys(value, [
      "collision_visibility", "format_version", "kind", "receipt",
    ]) || !isBlameProductReceipt(value.receipt)
      || !collisionVisibilityMatchesBlameProductReceipt(value.collision_visibility, value.receipt)) {
      return null;
    }
    return value as TelemetryQueueMessage;
  }
  if (value.kind === "install_stage_row") {
    if (!hasExactKeys(value, [
      "collision_visibility", "format_version", "kind", "row",
    ]) || !isInstallStageRow(value.row)
      || !collisionVisibilityMatchesInstallStageRow(value.collision_visibility, value.row)) return null;
    return value as TelemetryQueueMessage;
  }
  return null;
}

function isCollisionVisibility(value: unknown): value is TelemetryQueueCollisionVisibility {
  if (!isRecord(value) || !hasExactKeys(value, [
    "analytics_environment",
    "app_version",
    "endpoint",
    "event_family",
    "field_shape_fingerprint",
    "field_shape_overflow",
    "provider_classification",
    "size_bucket",
  ])) return false;
  return (value.analytics_environment === "production" || value.analytics_environment === "staging")
    && (value.app_version === "unknown" || isReleaseVersion(value.app_version))
    && TELEMETRY_ENDPOINTS.has(String(value.endpoint))
    && TELEMETRY_EVENT_FAMILIES.has(String(value.event_family))
    && (value.field_shape_fingerprint === "none" || isSha256(value.field_shape_fingerprint))
    && typeof value.field_shape_overflow === "boolean"
    && PROVIDER_CLASSIFICATIONS.has(String(value.provider_classification))
    && QUEUE_SIZE_BUCKETS.has(String(value.size_bucket));
}

const TELEMETRY_ROW_KEYS = [
  "activity_class", "analytics_environment", "app_version", "arch",
  "broker_device_id_hash", "broker_install_id_hash", "broker_runtime",
  "client_profile_id_hash", "data_root_id_hash", "device_id_hash", "duration_bucket",
  "duration_ms", "env_target", "event_id", "event_name", "event_version",
  "identity_key_version", "install_id_hash", "model_id", "occurred_at", "origin_device_id_hash",
  "origin_install_id_hash", "origin_runtime", "os", "payload_fingerprint", "plane",
  "properties", "provider_id", "received_at", "schema_version", "session_root_kind", "source",
  "status", "success", "surface", "traffic_class",
] as const;
const INSTALL_STAGE_ROW_KEYS = [
  "analytics_environment", "arch", "channel", "duration_bucket", "error_kind", "event_id",
  "event_name", "event_version", "install_attempt_id_hash", "occurred_at", "payload_fingerprint",
  "platform", "received_at", "schema_version", "script_family", "stage", "status",
  "traffic_class", "version",
] as const;
const BLAME_PRODUCT_RECEIPT_KEYS = [
  "activity_class", "analytics_environment", "app_version", "arch", "duration_bucket", "event_id",
  "identity_key_version", "occurred_at", "os", "outcome", "properties", "received_at",
  "replay_fingerprint", "subject_hash", "traffic_class",
] as const;
const ACTIVITY_CLASSES = new Set([
  "product_activity", "product_value", "setup", "status", "automatic", "liveness", "operational",
]);
const LEGACY_INSTALL_PLATFORMS = new Set([
  "linux-x64", "linux-aarch64", "macos-arm64", "macos-x64", "freebsd-x64", "windows-x64",
]);
const TELEMETRY_ENDPOINTS = new Set(["telemetry_batch", "install_stage"]);
const TELEMETRY_EVENT_FAMILIES = new Set([
  "batch", "analytics_delivery_observation", "cli_invocation", "operation_completed",
  "provider_refresh_completed", "runtime_observation", "install_stage", "unknown",
]);
const PROVIDER_CLASSIFICATIONS = new Set([
  "current", "historical", "unrecognized", "neutral", "invalid", "mixed", "unknown",
]);
const QUEUE_SIZE_BUCKETS = new Set([
  "lt_1kb", "1kb_8kb", "8kb_64kb", "64kb_256kb", "256kb_plus", "unknown",
]);

function isTelemetryRow(value: unknown): value is TelemetryRow {
  if (!isRecord(value) || !hasExactKeys(value, TELEMETRY_ROW_KEYS)) return false;
  if (
    !isUuid(value.event_id)
    || value.event_version !== 1
    || value.plane !== "product"
    || !isCanonicalTimestamp(value.occurred_at)
    || !isCanonicalTimestamp(value.received_at)
    || !isReleaseVersion(value.app_version)
    || !OPERATING_SYSTEMS.has(String(value.os))
    || !ARCHITECTURES.has(String(value.arch))
    || !SURFACES.has(String(value.surface))
    || value.broker_runtime !== value.surface
    || value.origin_runtime !== value.surface
    || value.source !== `ctx-${String(value.surface)}`
    || (value.analytics_environment !== "production" && value.analytics_environment !== "staging")
    || value.traffic_class !== trafficClass(value.analytics_environment)
    || !ACTIVITY_CLASSES.has(String(value.activity_class))
    || value.env_target !== null
    || value.model_id !== null
    || value.duration_ms !== null
    || !OUTCOMES.has(String(value.status))
    || value.success !== (value.status === "success")
    || value.session_root_kind !== null
    || !isSha256(value.payload_fingerprint)
    || (value.provider_id !== null && !PROVIDERS.has(String(value.provider_id)))
    || !isTelemetryScalarRecord(value.properties)
  ) return false;
  const isValid = value.event_name === "cli_invocation"
    ? isFrozenTelemetryRow(value)
    : isTypedTelemetryRow(value);
  if (!isValid) return false;
  const row = value as TelemetryRow;
  const operation = row.properties.operation;
  return typeof operation === "string"
    && row.activity_class === classifyActivity(
      row.event_name,
      row.surface,
      operation,
      row.status,
      row.properties,
    );
}

function collisionVisibilityMatchesTelemetryRow(
  visibility: TelemetryQueueCollisionVisibility,
  row: TelemetryRow,
): boolean {
  return visibility.endpoint === "telemetry_batch"
    && visibility.event_family === row.event_name
    && visibility.analytics_environment === row.analytics_environment;
}

function collisionVisibilityMatchesBlameProductReceipt(
  visibility: TelemetryQueueCollisionVisibility,
  receipt: BlameProductReceipt,
): boolean {
  return visibility.endpoint === "telemetry_batch"
    && visibility.event_family === "operation_completed"
    && visibility.analytics_environment === receipt.analytics_environment;
}

function collisionVisibilityMatchesInstallStageRow(
  visibility: TelemetryQueueCollisionVisibility,
  row: InstallStageRow,
): boolean {
  return visibility.endpoint === "install_stage"
    && visibility.event_family === "install_stage"
    && (row.analytics_environment === null
      || visibility.analytics_environment === row.analytics_environment);
}

function isTypedTelemetryRow(row: Record<string, unknown>): boolean {
  if (
    !EVENT_NAMES.has(String(row.event_name))
    || row.schema_version !== 1
    || (!DURATION_BUCKETS.has(String(row.duration_bucket)) && !isPreConvergenceDuration(row))
    || [
      row.install_id_hash, row.device_id_hash, row.broker_install_id_hash,
      row.broker_device_id_hash, row.origin_install_id_hash, row.origin_device_id_hash,
    ].some((value) => value !== null)
    || !isSha256(row.client_profile_id_hash)
    || !isSha256(row.data_root_id_hash)
    || !isIdentityKeyVersion(row.identity_key_version)
  ) return false;
  return validateTypedProperties(row as unknown as TelemetryRow);
}

function isPreConvergenceDuration(row: Record<string, unknown>): boolean {
  return row.app_version === "0.26.0"
    && row.event_name === "operation_completed"
    && row.surface === "pro_host"
    && row.duration_bucket === "gte_30s"
    && isRecord(row.properties)
    && row.properties.operation === "query";
}

function isFrozenTelemetryRow(row: Record<string, unknown>): boolean {
  if (
    row.schema_version !== null
    || row.surface !== "cli"
    || !LEGACY_DURATION_BUCKETS.has(String(row.duration_bucket))
    || !isSha256(row.install_id_hash)
    || row.install_id_hash !== row.data_root_id_hash
    || row.install_id_hash !== row.broker_install_id_hash
    || row.install_id_hash !== row.origin_install_id_hash
    || !isNullableSha256(row.device_id_hash)
    || row.device_id_hash !== row.client_profile_id_hash
    || row.device_id_hash !== row.broker_device_id_hash
    || row.device_id_hash !== row.origin_device_id_hash
    || !isIdentityKeyVersion(row.identity_key_version)
  ) return false;
  try {
    legacyCliRelease(String(row.app_version));
    return validateFrozenProperties(row as unknown as TelemetryRow);
  } catch {
    return false;
  }
}

function validateTypedProperties(row: TelemetryRow): boolean {
  const raw = { ...row.properties } as Record<string, unknown>;
  const operation = raw.operation;
  const outcome = raw.outcome;
  if (typeof operation !== "string" || outcome !== row.status) return false;
  delete raw.operation;
  delete raw.outcome;
  const installAttemptHash = removeDerivedHash(raw, "install_attempt_id_hash");
  const upgradeAttemptHash = removeDerivedHash(raw, "upgrade_attempt_id_hash");
  if (installAttemptHash === false || upgradeAttemptHash === false) return false;
  if (typeof upgradeAttemptHash === "string") raw.upgrade_attempt_id = "queue_validation";
  let parsed: Record<string, string | number | boolean | null>;
  try {
    if (row.event_name === "analytics_delivery_observation") {
      parsed = parseAnalyticsDeliveryProperties(raw, row.status, row.surface, operation);
    } else if (row.event_name === "operation_completed") {
      if (isCurrentBlameProductContract(raw)) return false;
      validateOperation(row.surface, operation);
      parsed = row.surface === "cli"
        ? parseOperationProperties(raw, operation, row.surface, row.status)
        : parseSurfaceOperationProperties(raw, operation, row.surface, row.status, row.app_version);
    } else if (row.event_name === "provider_refresh_completed") {
      validateProviderRefreshOperation(row.surface, operation);
      parsed = parseProviderRefreshProperties(raw, row.status, row.surface);
    } else if (row.event_name === "runtime_observation") {
      validateRuntimeOperation(row.surface, operation);
      parsed = parseRuntimeProperties(raw, row.surface, operation);
    } else {
      return false;
    }
    validateSharedPropertyConsistency(parsed, typeof installAttemptHash === "string");
  } catch {
    return false;
  }
  if (typeof upgradeAttemptHash === "string") {
    delete parsed.upgrade_attempt_id;
    parsed.upgrade_attempt_id_hash = upgradeAttemptHash;
  }
  if (typeof installAttemptHash === "string") {
    parsed.install_attempt_id_hash = installAttemptHash;
  }
  parsed.operation = operation;
  parsed.outcome = row.status;
  const provider = parsed.provider_filter ?? parsed.provider ?? null;
  return provider === row.provider_id && scalarRecordsEqual(parsed, row.properties);
}

function validateFrozenProperties(row: TelemetryRow): boolean {
  const raw = { ...row.properties } as Record<string, unknown>;
  const operation = raw.operation;
  const outcome = raw.outcome;
  delete raw.operation;
  delete raw.outcome;
  const installAttemptHash = removeDerivedHash(raw, "install_attempt_id_hash");
  if (installAttemptHash === false || Object.hasOwn(raw, "upgrade_attempt_id_hash")) return false;
  try {
    const parsed = parseLegacyCliProperties(
      legacyCliRelease(row.app_version),
      raw,
      row.success,
      typeof installAttemptHash === "string",
    );
    const action = parsed.action;
    const expectedOperation = action === "setup_started"
      ? "setup"
      : action === "integrations"
      ? "integration"
      : action;
    if (operation !== expectedOperation || outcome !== row.status) return false;
    if (typeof installAttemptHash === "string") {
      parsed.install_attempt_id_hash = installAttemptHash;
    }
    parsed.operation = String(operation);
    parsed.outcome = row.status;
    const provider = parsed.provider_filter ?? null;
    return provider === row.provider_id && scalarRecordsEqual(parsed, row.properties);
  } catch {
    return false;
  }
}

function isInstallStageRow(value: unknown): value is InstallStageRow {
  if (
    !isRecord(value)
    || !hasExactKeys(value, INSTALL_STAGE_ROW_KEYS)
    || !isSha256(value.install_attempt_id_hash)
    || !isSha256(value.payload_fingerprint)
    || !isUuid(value.event_id)
    || value.event_id !== uuidV4FromFingerprint(value.payload_fingerprint)
    || !isCanonicalTimestamp(value.occurred_at)
    || !INSTALL_STATUSES.has(String(value.status))
    || value.duration_bucket !== null
  ) return false;
  if (value.event_name === "install_stage") {
    if (
      value.event_version !== 1
      || value.schema_version !== 1
      || !INSTALL_STAGES.has(String(value.stage))
      || !INSTALL_PLATFORMS.has(String(value.platform))
      || !INSTALL_ARCHITECTURES.has(String(value.arch))
      || !INSTALL_SCRIPT_FAMILIES.has(String(value.script_family))
      || !isCanonicalTimestamp(value.received_at)
      || value.received_at !== value.occurred_at
      || (value.analytics_environment !== "production" && value.analytics_environment !== "staging")
      || value.traffic_class !== trafficClass(value.analytics_environment)
      || value.error_kind !== null
      || value.channel !== null
      || value.version !== null
    ) return false;
    try {
      validateInstallStatus(String(value.stage), String(value.status), false);
      return true;
    } catch {
      return false;
    }
  }
  if (
    value.event_name !== null
    || value.event_version !== null
    || value.schema_version !== null
    || !LEGACY_INSTALL_STAGES.has(String(value.stage))
    || (value.platform !== null && !LEGACY_INSTALL_PLATFORMS.has(String(value.platform)))
    || value.arch !== null
    || value.script_family !== null
    || value.received_at !== null
    || value.analytics_environment !== null
    || value.traffic_class !== null
    || (value.channel !== null && !LEGACY_INSTALL_CHANNELS.has(String(value.channel)))
    || (value.version !== null && !isReleaseVersion(value.version))
  ) return false;
  try {
    validateInstallStatus(String(value.stage), String(value.status), true);
    return value.error_kind === null
      || validateInstallError(value.error_kind) === value.error_kind;
  } catch {
    return false;
  }
}

function isBlameProductReceipt(value: unknown): value is BlameProductReceipt {
  if (
    !isRecord(value)
    || !hasExactKeys(value, BLAME_PRODUCT_RECEIPT_KEYS)
    || (value.activity_class !== "product_activity" && value.activity_class !== "product_value")
    || (value.analytics_environment !== "production" && value.analytics_environment !== "staging")
    || value.traffic_class !== trafficClass(value.analytics_environment)
    || typeof value.app_version !== "string"
    || !isBoundedExactBlameVersion(value.app_version)
    || !ARCHITECTURES.has(String(value.arch))
    || !OPERATING_SYSTEMS.has(String(value.os))
    || !DURATION_BUCKETS.has(String(value.duration_bucket))
    || !isUuid(value.event_id)
    || !isIdentityKeyVersion(value.identity_key_version)
    || !isCanonicalTimestamp(value.occurred_at)
    || !isCanonicalTimestamp(value.received_at)
    || !OUTCOMES.has(String(value.outcome))
    || !isSha256(value.replay_fingerprint)
    || !isSha256(value.subject_hash)
    || !isTelemetryScalarRecord(value.properties)
  ) return false;
  const raw = { ...value.properties };
  const operation = raw.operation;
  const outcome = raw.outcome;
  delete raw.operation;
  delete raw.outcome;
  if (operation !== "blame" || outcome !== value.outcome) return false;
  try {
    const parsed = parseCurrentBlameProductProperties(raw, String(value.outcome));
    parsed.operation = "blame";
    parsed.outcome = String(value.outcome);
    return value.activity_class === (value.outcome === "success" ? "product_value" : "product_activity")
      && scalarRecordsEqual(parsed, value.properties);
  } catch {
    return false;
  }
}

function removeDerivedHash(
  properties: Record<string, unknown>,
  key: string,
): string | false | undefined {
  if (!Object.hasOwn(properties, key)) return undefined;
  const value = properties[key];
  delete properties[key];
  return isSha256(value) ? value : false;
}

function scalarRecordsEqual(
  left: Record<string, string | number | boolean | null>,
  right: Record<string, string | number | boolean | null>,
): boolean {
  return hasExactKeys(left, Object.keys(right))
    && Object.entries(left).every(([key, value]) => right[key] === value);
}

function trafficClass(environment: unknown): string {
  return environment === "production" ? "unclassified_public" : "synthetic";
}

function isReleaseVersion(value: unknown): value is string {
  return typeof value === "string" && RELEASE_VERSION_PATTERN.test(value);
}

function isUuid(value: unknown): value is string {
  return typeof value === "string" && UUID_PATTERN.test(value);
}

function isSha256(value: unknown): value is string {
  return typeof value === "string" && /^[0-9a-f]{64}$/u.test(value);
}

function isNullableSha256(value: unknown): value is string | null {
  return value === null || isSha256(value);
}

function isIdentityKeyVersion(value: unknown): value is number {
  return Number.isSafeInteger(value) && (value as number) >= 1 && (value as number) <= 2_147_483_647;
}

function isCanonicalTimestamp(value: unknown): value is string {
  if (typeof value !== "string") return false;
  const milliseconds = Date.parse(value);
  return Number.isFinite(milliseconds) && new Date(milliseconds).toISOString() === value;
}

function isTelemetryScalar(value: unknown): value is string | number | boolean | null {
  return value === null
    || typeof value === "string"
    || typeof value === "boolean"
    || (typeof value === "number" && Number.isFinite(value));
}

function isTelemetryScalarRecord(
  value: unknown,
): value is Record<string, string | number | boolean | null> {
  return isRecord(value) && Object.values(value).every(isTelemetryScalar);
}

function uuidV4FromFingerprint(fingerprint: string): string {
  const bytes = fingerprint.slice(0, 32).split("");
  bytes[12] = "4";
  bytes[16] = ["8", "9", "a", "b"][Number.parseInt(bytes[16]!, 16) % 4]!;
  const hex = bytes.join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

function hasExactKeys(value: Record<string, unknown>, keys: readonly string[]): boolean {
  const actual = Object.keys(value).sort();
  const expected = [...keys].sort();
  return actual.length === expected.length
    && actual.every((key, index) => key === expected[index]);
}

async function transformBytes(
  bytes: Uint8Array,
  transform: CompressionStream | DecompressionStream,
): Promise<ArrayBuffer> {
  const owned = new ArrayBuffer(bytes.byteLength);
  new Uint8Array(owned).set(bytes);
  const stream = new Blob([owned]).stream().pipeThrough(transform);
  return new Response(stream).arrayBuffer();
}

function arrayBuffer(value: unknown): ArrayBuffer | null {
  if (value instanceof ArrayBuffer) return value;
  if (!ArrayBuffer.isView(value)) return null;
  return value.buffer.slice(value.byteOffset, value.byteOffset + value.byteLength) as ArrayBuffer;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
