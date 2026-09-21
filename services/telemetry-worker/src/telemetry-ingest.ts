import { classifyActivity } from "./telemetry-activity";
export { classifyActivity } from "./telemetry-activity";
import { hmacSha256Hex } from "./hash";
import {
  LEGACY_DURATION_BUCKETS,
  legacyCliRelease,
  parseLegacyCliProperties,
} from "./legacy-cli-contract";
import {
  ARCHITECTURES,
  DURATION_BUCKETS,
  EVENT_NAMES,
  INSTALL_ARCHITECTURES,
  INSTALL_ATTEMPT_ID_PATTERN,
  INSTALL_PLATFORMS,
  INSTALL_SCRIPT_FAMILIES,
  INSTALL_STAGE_V1_KEYS,
  INSTALL_STAGES,
  INSTALL_STATUSES,
  LEGACY_INSTALL_CHANNELS,
  LEGACY_INSTALL_STAGE_KEYS,
  LEGACY_INSTALL_STAGES,
  MAX_DEPTH,
  MAX_EVENT_BYTES,
  MAX_EVENTS,
  OPERATING_SYSTEMS,
  OUTCOMES,
  SURFACES,
  TelemetryIngestError,
  type TelemetryScalar,
  UUID_V7_PATTERN,
  V1_BATCH_KEYS,
  V1_EVENT_KEYS,
  optionalInstallAttemptId,
  optionalLegacyInstallAttemptId,
  optionalLegacyEnum,
  optionalLegacyVersion,
  parseOperationProperties,
  rejectUnknownKeys,
  requireBoolean,
  requireEnum,
  requireExact,
  requireInteger,
  requireRecord,
  requireString,
  requireUuid,
  requireUuidV4,
  requireVersion,
  schemaError,
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
import { parseAnalyticsDeliveryProperties } from "./analytics-delivery-contract";
import { isBoundedExactBlameVersion } from "./blame-product-contract";

export { TelemetryIngestError } from "./telemetry-contract";

export type TelemetryEnvironment = "production" | "staging";
export type TelemetryTrafficClass = "unclassified_public" | "synthetic";
export type TelemetryActivityClass =
  | "product_activity"
  | "product_value"
  | "setup"
  | "status"
  | "automatic"
  | "liveness"
  | "operational";

export type TelemetryRow = {
  event_id: string;
  install_id_hash: string | null;
  device_id_hash: string | null;
  broker_install_id_hash: string | null;
  broker_device_id_hash: string | null;
  origin_install_id_hash: string | null;
  origin_device_id_hash: string | null;
  occurred_at: string;
  received_at: string;
  event_name: string;
  event_version: 1;
  schema_version: 1 | null;
  plane: "product";
  broker_runtime: string;
  origin_runtime: string;
  source: string;
  analytics_environment: TelemetryEnvironment;
  traffic_class: TelemetryTrafficClass;
  activity_class: TelemetryActivityClass;
  app_version: string;
  os: string;
  arch: string;
  surface: string;
  env_target: null;
  provider_id: string | null;
  model_id: null;
  duration_ms: null;
  duration_bucket: string;
  status: string;
  success: boolean;
  session_root_kind: null;
  client_profile_id_hash: string | null;
  data_root_id_hash: string | null;
  identity_key_version: number | null;
  payload_fingerprint: string;
  properties: Record<string, TelemetryScalar>;
};

export type InstallStageRow = {
  event_id: string | null;
  event_name: "install_stage" | null;
  event_version: 1 | null;
  schema_version: 1 | null;
  install_attempt_id_hash: string;
  payload_fingerprint: string | null;
  occurred_at: string;
  received_at: string | null;
  analytics_environment: TelemetryEnvironment | null;
  traffic_class: TelemetryTrafficClass | null;
  stage: string;
  status: string;
  error_kind: string | null;
  platform: string | null;
  arch: string | null;
  script_family: string | null;
  channel: string | null;
  version: string | null;
  duration_bucket: string | null;
};

export type TelemetryIngestPlan = { rows: TelemetryRow[] };

type IngestOptions = {
  analyticsEnvironment: TelemetryEnvironment;
  verifiedInstallationCoordinate?: string;
  identityHmacKey: string;
  identityKeyVersion: number;
  now?: () => Date;
};

const MAX_PAST_AGE_MS = 48 * 60 * 60 * 1000;
const MAX_FUTURE_SKEW_MS = 5 * 60 * 1000;
const PRE_CONVERGENCE_V026_DURATION_BUCKETS = new Set(["gte_30s"]);
type PayloadFingerprintSchema =
  | "install-stage.v1"
  | "legacy-install-stage.v1"
  | "legacy-cli-invocation.v1"
  | "telemetry-event.v1";

export async function buildTelemetryIngestPlan(
  payload: unknown,
  opts: IngestOptions,
): Promise<TelemetryIngestPlan> {
  validateIdentityOptions(opts.identityHmacKey, opts.identityKeyVersion);
  const batch = requireRecord(payload, "invalid_batch");
  assertDepth(batch, MAX_DEPTH);
  const events = batch.events;
  if (!Array.isArray(events)) throw schemaError("invalid_events");
  if (events.length === 0) throw schemaError("empty_events");
  if (events.length > MAX_EVENTS) throw new TelemetryIngestError(413, "too_many_events");
  for (const event of events) assertEventSize(event);

  const now = (opts.now ?? (() => new Date()))();
  const rows = Object.hasOwn(batch, "broker_install_id")
    ? await parseLegacyBatch(batch, events, opts, now)
    : await parseV1Batch(batch, events, opts, now);
  return { rows: deduplicateRows(rows) };
}

export async function buildInstallStageRow(
  payload: unknown,
  opts: IngestOptions,
): Promise<InstallStageRow> {
  validateIdentityOptions(opts.identityHmacKey, opts.identityKeyVersion);
  const input = requireRecord(payload, "invalid_install_stage");
  assertDepth(input, 2);
  assertEventSize(input);
  const now = (opts.now ?? (() => new Date()))().toISOString();
  const isV1 = Object.hasOwn(input, "event_name");
  rejectUnknownKeys(
    input,
    isV1 ? INSTALL_STAGE_V1_KEYS : LEGACY_INSTALL_STAGE_KEYS,
    "unknown_install_stage_field",
  );

  const attemptId = requireString(input.install_attempt_id, "invalid_install_attempt_id");
  if (!INSTALL_ATTEMPT_ID_PATTERN.test(attemptId)) throw schemaError("invalid_install_attempt_id");
  const stage = requireEnum(
    input.stage,
    isV1 ? INSTALL_STAGES : LEGACY_INSTALL_STAGES,
    "invalid_install_stage",
  );
  const status = requireEnum(input.status, INSTALL_STATUSES, "invalid_install_status");
  validateInstallStatus(stage, status, !isV1);
  const attemptHash = await opaqueIdentity(
    opts.identityHmacKey,
    "ctx.telemetry.install-attempt.v1",
    attemptId,
  );

  if (!isV1) {
    const errorKind = validateInstallError(input.error_kind);
    const platform = optionalLegacyEnum(
      input.platform,
      legacyInstallPlatforms(),
      "invalid_install_platform",
    );
    const channel = optionalLegacyEnum(
      input.channel,
      LEGACY_INSTALL_CHANNELS,
      "invalid_install_channel",
    );
    const version = optionalLegacyVersion(input.version);
    const fingerprint = await payloadFingerprint(
      opts.identityHmacKey,
      opts.analyticsEnvironment,
      "legacy-install-stage.v1",
      {
        install_attempt_id: attemptId,
        stage,
        status,
        error_kind: errorKind,
        platform,
        channel,
        version,
      },
    );
    return {
      event_id: uuidV4FromFingerprint(fingerprint),
      event_name: null,
      event_version: null,
      schema_version: null,
      install_attempt_id_hash: attemptHash,
      payload_fingerprint: fingerprint,
      occurred_at: now,
      received_at: null,
      analytics_environment: null,
      traffic_class: null,
      stage,
      status,
      error_kind: errorKind,
      platform,
      arch: null,
      script_family: null,
      channel,
      version,
      duration_bucket: null,
    };
  }

  requireExact(input.event_name, "install_stage", "invalid_event_name");
  requireExact(input.event_version, 1, "invalid_event_version");
  const platform = requireEnum(input.platform, INSTALL_PLATFORMS, "invalid_install_platform");
  const arch = requireEnum(input.arch, INSTALL_ARCHITECTURES, "invalid_install_arch");
  const scriptFamily = requireEnum(
    input.script_family,
    INSTALL_SCRIPT_FAMILIES,
    "invalid_install_script_family",
  );
  const normalized = {
    event_name: "install_stage",
    event_version: 1,
    install_attempt_id: attemptId,
    stage,
    status,
    platform,
    arch,
    script_family: scriptFamily,
  };
  const fingerprint = await payloadFingerprint(
    opts.identityHmacKey,
    opts.analyticsEnvironment,
    "install-stage.v1",
    normalized,
  );
  return {
    event_id: uuidV4FromFingerprint(fingerprint),
    event_name: "install_stage",
    event_version: 1,
    schema_version: 1,
    install_attempt_id_hash: attemptHash,
    payload_fingerprint: fingerprint,
    occurred_at: now,
    received_at: now,
    analytics_environment: opts.analyticsEnvironment,
    traffic_class: trafficClassFor(opts.analyticsEnvironment),
    stage,
    status,
    error_kind: null,
    platform,
    arch,
    script_family: scriptFamily,
    channel: null,
    version: null,
    duration_bucket: null,
  };
}

async function parseV1Batch(
  batch: Record<string, unknown>,
  events: unknown[],
  opts: IngestOptions,
  now: Date,
): Promise<TelemetryRow[]> {
  rejectUnknownKeys(batch, V1_BATCH_KEYS, "unknown_batch_field");
  const hasClientProfileId = Object.hasOwn(batch, "client_profile_id");
  const hasDataRootId = Object.hasOwn(batch, "data_root_id");
  const identitylessEnvelope = !hasClientProfileId && !hasDataRootId;
  const hasProMaterialization = events.some(isProMaterializationEvent);
  const identitylessMaterialization = identitylessEnvelope && hasProMaterialization;
  const verifiedMaterialization = opts.verifiedInstallationCoordinate != null
    && hasProMaterialization;
  if ((identitylessMaterialization || verifiedMaterialization) && events.length !== 1) {
    throw schemaError("invalid_materialization_proof_event_count");
  }
  if (verifiedMaterialization && !identitylessEnvelope) {
    throw schemaError("materialization_installation_proof_requires_identityless");
  }
  if (identitylessMaterialization && opts.verifiedInstallationCoordinate == null) {
    throw schemaError("materialization_installation_proof_required");
  }
  const identitylessBlame = opts.verifiedInstallationCoordinate != null
    && isSoleCurrentBlameEvent(events)
    && !hasClientProfileId
    && !hasDataRootId;
  const proofBackedMaterialization = identitylessMaterialization
    && opts.verifiedInstallationCoordinate != null;
  const proofBackedIdentityless = identitylessBlame || proofBackedMaterialization;
  const clientProfileId = proofBackedIdentityless
    ? null
    : requireUuid(batch.client_profile_id, "invalid_client_profile_id");
  const dataRootId = proofBackedIdentityless
    ? null
    : requireUuid(batch.data_root_id, "invalid_data_root_id");
  const appVersion = requireVersion(batch.app_version, "invalid_app_version");
  const os = requireEnum(batch.os, OPERATING_SYSTEMS, "invalid_os");
  const arch = requireEnum(batch.arch, ARCHITECTURES, "invalid_arch");
  const [profileHash, dataRootHash] = proofBackedMaterialization
    ? await proofDerivedMaterializationIdentities(
      opts.identityHmacKey,
      opts.identityKeyVersion,
      opts.verifiedInstallationCoordinate!,
    )
    : await Promise.all([
      clientProfileId == null
        ? Promise.resolve(null)
        : opaqueIdentity(opts.identityHmacKey, "ctx.telemetry.client-profile.v1", clientProfileId),
      dataRootId == null
        ? Promise.resolve(null)
        : opaqueIdentity(opts.identityHmacKey, "ctx.telemetry.data-root.v1", dataRootId),
    ]);
  const envelope = compactObject({
    client_profile_id: clientProfileId,
    data_root_id: dataRootId,
    app_version: appVersion,
    os,
    arch,
  });
  return Promise.all(events.map((event) => parseV1Event(
    event,
    { appVersion, arch, dataRootHash, envelope, os, profileHash },
    opts,
    now,
  )));
}

async function parseV1Event(
  raw: unknown,
  batch: {
    appVersion: string;
    arch: string;
    dataRootHash: string | null;
    envelope: Record<string, unknown>;
    os: string;
    profileHash: string | null;
  },
  opts: IngestOptions,
  now: Date,
): Promise<TelemetryRow> {
  const event = requireRecord(raw, "invalid_event");
  rejectUnknownKeys(event, V1_EVENT_KEYS, "unknown_event_field");
  const eventId = requireUuidV4(event.event_id, "invalid_event_id");
  const eventName = requireEnum(event.event_name, EVENT_NAMES, "invalid_event_name");
  requireExact(event.event_version, 1, "invalid_event_version");
  const occurredAt = requireOccurredAt(event.occurred_at, now, true);
  const surface = requireEnum(event.surface, SURFACES, "invalid_surface");
  const operation = requireString(event.operation, "invalid_operation");
  const outcome = requireEnum(event.outcome, OUTCOMES, "invalid_outcome");
  const durationBucket = requireV1DurationBucket(
    event.duration_bucket,
    batch.appVersion,
    eventName,
    surface,
    operation,
  );
  const installAttemptId = optionalInstallAttemptId(event.install_attempt_id);
  const installAttemptHash = installAttemptId
    ? await opaqueIdentity(opts.identityHmacKey, "ctx.telemetry.install-attempt.v1", installAttemptId)
    : null;

  let properties: Record<string, TelemetryScalar>;
  let providerId: string | null = null;
  if (eventName === "analytics_delivery_observation") {
    properties = parseAnalyticsDeliveryProperties(
      event.properties,
      outcome,
      surface,
      operation,
    );
  } else if (eventName === "operation_completed") {
    validateOperation(surface, operation);
    properties = surface === "cli"
      ? parseOperationProperties(event.properties, operation, surface, outcome)
      : parseSurfaceOperationProperties(
        event.properties,
        operation,
        surface,
        outcome,
        batch.appVersion,
      );
    providerId = typeof properties.provider_filter === "string"
      ? properties.provider_filter
      : null;
  } else if (eventName === "provider_refresh_completed") {
    validateProviderRefreshOperation(surface, operation);
    properties = parseProviderRefreshProperties(event.properties, outcome, surface);
    providerId = typeof properties.provider === "string" ? properties.provider : null;
  } else {
    validateRuntimeOperation(surface, operation);
    properties = parseRuntimeProperties(event.properties, surface, operation);
  }
  validateSharedPropertyConsistency(properties, installAttemptId !== null);
  if (typeof properties.upgrade_attempt_id === "string") {
    properties.upgrade_attempt_id_hash = await opaqueIdentity(
      opts.identityHmacKey,
      "ctx.telemetry.upgrade-attempt.v1",
      properties.upgrade_attempt_id,
    );
    delete properties.upgrade_attempt_id;
  }
  if (installAttemptHash) properties.install_attempt_id_hash = installAttemptHash;
  if (
    (properties.blame_schema_version === 1 || properties.blame_schema_version === 2)
    && !isBoundedExactBlameVersion(batch.appVersion)
  ) throw schemaError("invalid_app_version");
  properties.operation = operation;
  properties.outcome = outcome;

  const normalizedEvent = compactObject({
    event_id: eventId,
    event_name: eventName,
    event_version: 1,
    occurred_at: occurredAt,
    surface,
    operation,
    outcome,
    duration_bucket: durationBucket,
    install_attempt_id: installAttemptId,
    properties: event.properties,
  });
  return rowFromNormalized({
    analyticsEnvironment: opts.analyticsEnvironment,
    appVersion: batch.appVersion,
    arch: batch.arch,
    dataRootHash: batch.dataRootHash,
    durationBucket,
    eventId,
    eventName,
    identityKeyVersion: opts.identityKeyVersion,
    identityHmacKey: opts.identityHmacKey,
    normalizedForFingerprint: { envelope: batch.envelope, event: normalizedEvent },
    occurredAt,
    operation,
    os: batch.os,
    outcome,
    profileHash: batch.profileHash,
    properties,
    providerId,
    receivedAt: now.toISOString(),
    schemaVersion: 1,
    surface,
  });
}

async function parseLegacyBatch(
  batch: Record<string, unknown>,
  events: unknown[],
  opts: IngestOptions,
  now: Date,
): Promise<TelemetryRow[]> {
  if (events.length !== 1) throw schemaError("invalid_legacy_event_count");
  const appVersion = requireVersion(batch.broker_app_version, "invalid_broker_app_version");
  const release = legacyCliRelease(appVersion);
  rejectUnknownKeys(batch, release.batchKeys, "unknown_batch_field");
  const brokerDataRootId = requireUuidV4(batch.broker_install_id, "invalid_broker_install_id");
  const brokerProfileId = release.hasDeviceIdentity
    ? requireUuidV4(batch.broker_device_id, "invalid_broker_device_id")
    : null;
  requireExact(batch.broker_runtime, "cli", "invalid_broker_runtime");
  const os = requireEnum(batch.broker_os, OPERATING_SYSTEMS, "invalid_broker_os");
  const arch = requireEnum(batch.broker_arch, ARCHITECTURES, "invalid_broker_arch");
  const [brokerDataRootHash, brokerProfileHash] = await Promise.all([
    opaqueIdentity(opts.identityHmacKey, "ctx.telemetry.data-root.v1", brokerDataRootId),
    brokerProfileId
      ? opaqueIdentity(opts.identityHmacKey, "ctx.telemetry.client-profile.v1", brokerProfileId)
      : Promise.resolve(null),
  ]);
  const envelope: Record<string, unknown> = {
    broker_install_id: brokerDataRootId,
    broker_runtime: "cli",
    broker_app_version: appVersion,
    broker_os: os,
    broker_arch: arch,
  };
  if (brokerProfileId) envelope.broker_device_id = brokerProfileId;

  return Promise.all(events.map(async (raw) => {
    const event = requireRecord(raw, "invalid_event");
    rejectUnknownKeys(event, release.eventKeys, "unknown_event_field");
    const eventId = requireUuid(event.event_id, "invalid_event_id");
    if (!UUID_V7_PATTERN.test(eventId)) throw schemaError("invalid_legacy_event_id");
    requireExact(event.event_name, "cli_invocation", "invalid_event_name");
    requireExact(event.event_version, 1, "invalid_event_version");
    const occurredAt = requireOccurredAt(event.occurred_at, now, false);
    requireExact(event.plane, "product", "invalid_plane");
    requireExact(event.delivery, "remote", "invalid_delivery");
    requireExact(event.origin_runtime, "cli", "invalid_origin_runtime");
    requireExact(event.surface, "cli", "invalid_surface");
    requireExact(event.source, "ctx-cli", "invalid_source");
    requireExact(event.app_version, appVersion, "app_version_mismatch");
    requireExact(event.os, os, "os_mismatch");
    requireExact(event.arch, arch, "arch_mismatch");
    const dataRootId = requireUuidV4(event.origin_install_id, "invalid_origin_install_id");
    const profileId = release.hasDeviceIdentity
      ? requireUuidV4(event.origin_device_id, "invalid_origin_device_id")
      : null;
    if (dataRootId !== brokerDataRootId) throw schemaError("origin_install_id_mismatch");
    if (profileId !== brokerProfileId) throw schemaError("origin_device_id_mismatch");
    const durationMs = requireLegacyDurationMs(event.duration_ms);
    const durationBucket = requireEnum(
      event.duration_bucket,
      LEGACY_DURATION_BUCKETS,
      "invalid_duration_bucket",
    );
    if (durationBucket !== legacyDurationBucket(durationMs)) throw schemaError("inconsistent_duration_bucket");
    const status = requireEnum(event.status, new Set(["ok", "error"]), "invalid_status");
    const success = requireBoolean(event.success, "invalid_success");
    if ((status === "ok") !== success) throw schemaError("inconsistent_status");
    const installAttemptId = optionalLegacyInstallAttemptId(event.install_attempt_id);
    const properties = parseLegacyCliProperties(
      release,
      event.properties,
      success,
      installAttemptId !== null,
    );
    const action = properties.action as string;
    const operation = action === "setup_started"
      ? "setup"
      : action === "integrations"
      ? "integration"
      : action;
    const [dataRootHash, profileHash] = await Promise.all([
      opaqueIdentity(opts.identityHmacKey, "ctx.telemetry.data-root.v1", dataRootId),
      profileId
        ? opaqueIdentity(opts.identityHmacKey, "ctx.telemetry.client-profile.v1", profileId)
        : Promise.resolve(null),
    ]);
    if (installAttemptId) {
      properties.install_attempt_id_hash = await opaqueIdentity(
        opts.identityHmacKey,
        "ctx.telemetry.install-attempt.v1",
        installAttemptId,
      );
    }
    properties.operation = operation;
    properties.outcome = success ? "success" : "failure";
    const row = await rowFromNormalized({
      analyticsEnvironment: opts.analyticsEnvironment,
      appVersion,
      arch,
      dataRootHash,
      durationBucket,
      eventId,
      eventName: "cli_invocation",
      identityKeyVersion: opts.identityKeyVersion,
      identityHmacKey: opts.identityHmacKey,
      normalizedForFingerprint: { envelope, event },
      occurredAt,
      operation,
      os,
      outcome: success ? "success" : "failure",
      profileHash,
      properties,
      providerId: typeof properties.provider_filter === "string" ? properties.provider_filter : null,
      receivedAt: now.toISOString(),
      schemaVersion: null,
      surface: "cli",
    });
    row.install_id_hash = brokerDataRootHash;
    row.broker_install_id_hash = brokerDataRootHash;
    row.broker_device_id_hash = brokerProfileHash;
    return row;
  }));
}

async function rowFromNormalized(input: {
  analyticsEnvironment: TelemetryEnvironment;
  appVersion: string;
  arch: string;
  dataRootHash: string | null;
  durationBucket: string;
  eventId: string;
  eventName: string;
  identityHmacKey: string;
  identityKeyVersion: number;
  normalizedForFingerprint: unknown;
  occurredAt: string;
  operation: string;
  os: string;
  outcome: string;
  profileHash: string | null;
  properties: Record<string, TelemetryScalar>;
  providerId: string | null;
  receivedAt: string;
  schemaVersion: 1 | null;
  surface: string;
}): Promise<TelemetryRow> {
  const isTyped = input.schemaVersion === 1;
  return {
    event_id: input.eventId,
    install_id_hash: isTyped ? null : input.dataRootHash,
    device_id_hash: isTyped ? null : input.profileHash,
    broker_install_id_hash: isTyped ? null : input.dataRootHash,
    broker_device_id_hash: isTyped ? null : input.profileHash,
    origin_install_id_hash: isTyped ? null : input.dataRootHash,
    origin_device_id_hash: isTyped ? null : input.profileHash,
    occurred_at: input.occurredAt,
    received_at: input.receivedAt,
    event_name: input.eventName,
    event_version: 1,
    schema_version: input.schemaVersion,
    plane: "product",
    broker_runtime: input.surface,
    origin_runtime: input.surface,
    source: `ctx-${input.surface}`,
    analytics_environment: input.analyticsEnvironment,
    traffic_class: trafficClassFor(input.analyticsEnvironment),
    activity_class: classifyActivity(
      input.eventName,
      input.surface,
      input.operation,
      input.outcome,
      input.properties,
    ),
    app_version: input.appVersion,
    os: input.os,
    arch: input.arch,
    surface: input.surface,
    env_target: null,
    provider_id: input.providerId,
    model_id: null,
    duration_ms: null,
    duration_bucket: input.durationBucket,
    status: input.outcome,
    success: input.outcome === "success",
    session_root_kind: null,
    client_profile_id_hash: input.profileHash,
    data_root_id_hash: input.dataRootHash,
    identity_key_version: input.profileHash == null && input.dataRootHash == null
      ? null
      : input.identityKeyVersion,
    payload_fingerprint: await payloadFingerprint(
      input.identityHmacKey,
      input.analyticsEnvironment,
      input.schemaVersion === 1 ? "telemetry-event.v1" : "legacy-cli-invocation.v1",
      input.normalizedForFingerprint,
    ),
    properties: input.properties,
  };
}

function isSoleCurrentBlameEvent(events: readonly unknown[]): boolean {
  if (events.length !== 1) return false;
  const event = events[0];
  if (!isPlainRecord(event)) return false;
  const properties = event.properties;
  return event.event_name === "operation_completed"
    && event.surface === "pro_host"
    && event.operation === "blame"
    && isPlainRecord(properties)
    && (
      (
        properties.blame_schema_version === 1
        && properties.blame_semantics_version === 1
      )
      || properties.blame_schema_version === 2
    );
}

function isProMaterializationEvent(event: unknown): boolean {
  return isPlainRecord(event)
    && event.event_name === "operation_completed"
    && event.surface === "pro_host"
    && event.operation === "materialize";
}

export function hasProMaterializationRow(
  rows: readonly TelemetryRow[],
): boolean {
  if (rows.length !== 1) return false;
  const row = rows[0]!;
  return row.event_name === "operation_completed"
    && row.surface === "pro_host"
    && row.properties.operation === "materialize";
}

function proofDerivedMaterializationIdentities(
  key: string,
  keyVersion: number,
  coordinate: string,
): Promise<[string, string]> {
  return Promise.all([
    opaqueIdentity(
      key,
      `ctx.telemetry.pro-materialization.client-profile.v1.key-${keyVersion}`,
      coordinate,
    ),
    opaqueIdentity(
      key,
      `ctx.telemetry.pro-materialization.data-root.v1.key-${keyVersion}`,
      coordinate,
    ),
  ]);
}

function isPlainRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}


function deduplicateRows(rows: readonly TelemetryRow[]): TelemetryRow[] {
  const unique = new Map<string, TelemetryRow>();
  for (const row of rows) {
    const previous = unique.get(row.event_id);
    if (previous && previous.payload_fingerprint !== row.payload_fingerprint) {
      throw new TelemetryIngestError(409, "event_id_collision");
    }
    if (!previous) unique.set(row.event_id, row);
  }
  return [...unique.values()];
}

function trafficClassFor(environment: TelemetryEnvironment): TelemetryTrafficClass {
  return environment === "production" ? "unclassified_public" : "synthetic";
}

function legacyDurationBucket(durationMs: number): string {
  if (durationMs < 100) return "lt_100ms";
  if (durationMs < 1_000) return "lt_1s";
  if (durationMs < 5_000) return "lt_5s";
  if (durationMs < 30_000) return "lt_30s";
  return "gte_30s";
}

function requireV1DurationBucket(
  value: unknown,
  appVersion: string,
  eventName: string,
  surface: string,
  operation: string,
): string {
  if (typeof value === "string" && DURATION_BUCKETS.has(value)) return value;
  if (
    appVersion === "0.26.0" &&
    eventName === "operation_completed" &&
    surface === "pro_host" &&
    operation === "query" &&
    typeof value === "string" &&
    PRE_CONVERGENCE_V026_DURATION_BUCKETS.has(value)
  ) {
    return value;
  }
  throw schemaError("invalid_duration_bucket");
}

function requireLegacyDurationMs(value: unknown): number {
  // Released senders clamp Duration::as_millis() to i64::MAX. JSON numbers
  // above the safe-integer ceiling are already rounded by the Worker runtime,
  // but the value is used only to verify the coarse duration bucket.
  if (
    typeof value !== "number" ||
    !Number.isInteger(value) ||
    value < 0 ||
    value > 9_223_372_036_854_776_000
  ) {
    throw schemaError("invalid_duration_ms");
  }
  return value;
}

function requireOccurredAt(value: unknown, now: Date, minuteRounded: boolean): string {
  if (typeof value !== "string") throw schemaError("invalid_occurred_at");
  if (minuteRounded && !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:00(?:\.000)?Z$/u.test(value)) {
    throw schemaError("invalid_occurred_at_precision");
  }
  const parsed = Date.parse(value);
  if (!Number.isFinite(parsed)) throw schemaError("invalid_occurred_at");
  const canonical = new Date(parsed).toISOString();
  if (minuteRounded) {
    const expectedCanonical = value.endsWith(":00Z")
      ? `${value.slice(0, -1)}.000Z`
      : value;
    if (canonical !== expectedCanonical) throw schemaError("invalid_occurred_at");
  }
  const age = now.getTime() - parsed;
  if (age > MAX_PAST_AGE_MS || age < -MAX_FUTURE_SKEW_MS) {
    throw schemaError("occurred_at_out_of_range");
  }
  return canonical;
}

export function validateIdentityOptions(key: string, keyVersion: number): void {
  if (key.length < 32) throw new TelemetryIngestError(500, "invalid_identity_hmac_key");
  if (!Number.isSafeInteger(keyVersion) || keyVersion < 1 || keyVersion > 2_147_483_647) {
    throw new TelemetryIngestError(500, "invalid_identity_key_version");
  }
}

function opaqueIdentity(key: string, domain: string, value: string): Promise<string> {
  return hmacSha256Hex(key, domain, value);
}

function payloadFingerprint(
  key: string,
  environment: TelemetryEnvironment,
  schema: PayloadFingerprintSchema,
  payload: unknown,
): Promise<string> {
  return hmacSha256Hex(
    key,
    `ctx.telemetry.payload-fingerprint.${environment}.${schema}`,
    canonicalJson(payload),
  );
}

function assertEventSize(value: unknown): void {
  const encoded = new TextEncoder().encode(JSON.stringify(value));
  if (encoded.byteLength > MAX_EVENT_BYTES) throw new TelemetryIngestError(413, "event_too_large");
}

function assertDepth(value: unknown, maximum: number): void {
  const visit = (entry: unknown, depth: number): void => {
    if (depth > maximum) throw schemaError("payload_too_deep");
    if (Array.isArray(entry)) {
      for (const child of entry) visit(child, depth + 1);
    } else if (typeof entry === "object" && entry !== null) {
      for (const child of Object.values(entry as Record<string, unknown>)) visit(child, depth + 1);
    }
  };
  visit(value, 0);
}

function canonicalJson(value: unknown): string {
  return JSON.stringify(canonicalValue(value));
}

function canonicalValue(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(canonicalValue);
  if (typeof value === "object" && value !== null) {
    return Object.fromEntries(
      Object.entries(value as Record<string, unknown>)
        .sort(([left], [right]) => left.localeCompare(right))
        .map(([key, entry]) => [key, canonicalValue(entry)]),
    );
  }
  return value;
}

function compactObject(value: Record<string, unknown>): Record<string, unknown> {
  return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== null));
}

function uuidV4FromFingerprint(fingerprint: string): string {
  const bytes = fingerprint.slice(0, 32).split("");
  bytes[12] = "4";
  bytes[16] = ["8", "9", "a", "b"][Number.parseInt(bytes[16], 16) % 4];
  const hex = bytes.join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

function legacyInstallPlatforms(): ReadonlySet<string> {
  return new Set([
    "linux-x64", "linux-aarch64", "macos-arm64", "macos-x64", "freebsd-x64", "windows-x64",
  ]);
}
