import {
  createNeonTelemetryDatabase,
  createNeonTelemetryMaintenanceDatabase,
  TelemetryEventCollisionError,
  type TelemetryDatabase,
  type TelemetryDatabaseFactory,
  type TelemetryEventFamily,
  type TelemetryIngestEndpoint,
  type TelemetryIngestRejection,
  type PersistedTelemetryRejectionClass,
  type TelemetryRejectionClass,
  type TelemetryMaintenanceDatabaseFactory,
} from "./database";
import {
  buildInstallStageRow,
  buildTelemetryIngestPlan,
  hasProMaterializationRow,
  TelemetryIngestError,
  validateIdentityOptions,
  type TelemetryEnvironment,
} from "./telemetry-ingest";
import { MAX_BODY_BYTES } from "./telemetry-contract";
import {
  decodeTelemetryQueueMessage,
  telemetryQueueCollisionIdentity,
  telemetryQueueRetryDelay,
  TELEMETRY_QUEUE_FORMAT_VERSION,
  type TelemetryQueueCollisionVisibility,
  type TelemetryQueueMessage,
} from "./telemetry-queue";
import {
  admitQueueMessage,
  admitQueueMessages,
  type TelemetryQueueProducer,
} from "./telemetry-queue-producer";
import {
  buildRejectionDiagnostics,
  type TelemetryRejectionDiagnostics,
} from "./rejection-diagnostics";
import { verifiedBlameInstallationCoordinate } from "./blame-installation-proof";
import {
  buildBlameProductReceipt,
  hasCurrentBlameProductRow,
} from "./blame-product-receipt";
import { sha256Hex } from "./hash";
import {
  aggregateHealthStatus,
  HEALTH_SIGNALS,
  queueHealthFailureDiagnostic,
  readHealthSignal,
  readTelemetryQueueHealth,
  unavailableHealthStatus,
} from "./queue-health";

export type Env = {
  TELEMETRY_DATABASE_URL?: string;
  TELEMETRY_RETENTION_DATABASE_URL?: string;
  TELEMETRY_IDENTITY_HMAC_KEY?: string;
  TELEMETRY_IDENTITY_KEY_VERSION?: string;
  TELEMETRY_ANALYTICS_ENVIRONMENT?: string;
  TELEMETRY_CLOUDFLARE_ACCOUNT_ID?: string;
  TELEMETRY_QUEUE_HEALTH_API_TOKEN?: string;
  TELEMETRY_INGEST_QUEUE?: TelemetryQueueProducer;
  TELEMETRY_RATE_LIMITER?: RateLimitBinding;
};

export type { TelemetryQueueProducer } from "./telemetry-queue-producer";

export type TelemetryQueueConsumerMessage = Readonly<{
  body: unknown;
  attempts: number;
  ack(): void;
  retry(options?: { readonly delaySeconds?: number }): void;
}>;

export type TelemetryQueueConsumerBatch = Readonly<{
  queue: string;
  messages: readonly TelemetryQueueConsumerMessage[];
}>;
type TelemetryQueueRowMessage = Extract<TelemetryQueueMessage, { kind: "telemetry_row" }>;
export type RateLimitBinding = {
  limit(options: { readonly key: string }): Promise<{ readonly success: boolean }>;
};

type WorkerDeps = {
  createDatabaseClient: TelemetryDatabaseFactory;
  createMaintenanceDatabaseClient: TelemetryMaintenanceDatabaseFactory;
  now: () => Date;
  observeRejection: (observation: TelemetryRejectionObservation) => void;
  readQueueHealth: (env: Env) => Promise<boolean>;
};

type WorkerConfig = {
  analyticsEnvironment: TelemetryEnvironment;
  databaseUrl: string;
  identityHmacKey: string;
  identityKeyVersion: number;
  queue: TelemetryQueueProducer;
};

type RejectionStorageConfig = Pick<WorkerConfig, "analyticsEnvironment" | "databaseUrl">;
type RejectionTaskScheduler = (task: Promise<void>) => void;

type RejectionContext = Readonly<{
  diagnostics: TelemetryRejectionDiagnostics | null;
  endpoint: TelemetryIngestEndpoint;
  eventFamily: TelemetryEventFamily;
  schedule: RejectionTaskScheduler | null;
  storage: RejectionStorageConfig | null;
}>;

export type WorkerExecutionContext = Readonly<{
  waitUntil(task: Promise<void>): void;
}>;

export type TelemetryRejectionObservation = Readonly<{
  category: "pre_commit" | "rate_limit" | "schema";
  code: string;
  endpoint: TelemetryIngestEndpoint;
  event_family: TelemetryEventFamily;
  rejection_class: TelemetryRejectionClass;
  status: number;
}> & Partial<TelemetryRejectionDiagnostics>;

const REJECTION_CODE_PATTERN = /^[a-z][a-z0-9_]{0,63}$/u;
const JSON_CONTENT_TYPE = "application/json; charset=utf-8";
const INSTALL_ATTEMPT_PATH = "/functions/v1/install-attempt";
const HEALTH_PATH = "/functions/v1/analytics/health";
const TELEMETRY_COLLISION_RECEIPT_DOMAIN = "ctx.telemetry.queue-event-collision.v1";
const TELEMETRY_PATHS = new Set([
  "/functions/v1/analytics",
  "/functions/v1/telemetry",
]);
const ALL_POST_PATHS = new Set([...TELEMETRY_PATHS, INSTALL_ATTEMPT_PATH]);
const EVENT_FAMILIES = new Set<TelemetryEventFamily>([
  "analytics_delivery_observation",
  "cli_invocation",
  "operation_completed",
  "provider_refresh_completed",
  "runtime_observation",
]);
const IDENTITY_REJECTION_CODES = new Set([
  "invalid_broker_device_id",
  "invalid_broker_install_id",
  "invalid_client_profile_id",
  "invalid_data_root_id",
  "invalid_identity_hmac_key",
  "invalid_identity_key_version",
  "invalid_install_attempt_id",
  "invalid_upgrade_attempt_id",
  "invalid_origin_device_id",
  "invalid_origin_install_id",
  "origin_device_id_mismatch",
  "origin_install_id_mismatch",
]);
const ENVELOPE_REJECTION_CODES = new Set([
  "empty_events",
  "invalid_app_version",
  "invalid_arch",
  "invalid_batch",
  "invalid_broker_app_version",
  "invalid_broker_arch",
  "invalid_broker_os",
  "invalid_broker_runtime",
  "invalid_events",
  "invalid_legacy_event_count",
  "invalid_os",
  "unknown_batch_field",
]);
const EVENT_REJECTION_CODES = new Set([
  "event_too_large",
  "invalid_duration_bucket",
  "invalid_event",
  "invalid_event_id",
  "invalid_event_name",
  "invalid_event_version",
  "invalid_legacy_event_id",
  "invalid_occurred_at",
  "invalid_occurred_at_precision",
  "invalid_operation",
  "invalid_outcome",
  "invalid_provider_refresh_operation",
  "invalid_provider_refresh_surface",
  "invalid_runtime_operation",
  "invalid_runtime_surface",
  "invalid_surface",
  "occurred_at_out_of_range",
  "payload_too_deep",
  "unknown_event_field",
]);
const SECURITY_HEADERS = {
  "cache-control": "no-store",
  "content-security-policy": "default-src 'none'; frame-ancestors 'none'",
  "cross-origin-resource-policy": "same-origin",
  "permissions-policy": "camera=(), microphone=(), geolocation=()",
  "referrer-policy": "no-referrer",
  "x-content-type-options": "nosniff",
} as const;

const defaultDeps: WorkerDeps = {
  createDatabaseClient: createNeonTelemetryDatabase,
  createMaintenanceDatabaseClient: createNeonTelemetryMaintenanceDatabase,
  now: () => new Date(),
  observeRejection: (observation) => {
    console.warn("telemetry_ingest_rejected", observation);
  },
  readQueueHealth: (env) => readTelemetryQueueHealth(env),
};

const MATERIALIZATION_SUCCESS_CODE = "telemetry_history_materialization_succeeded";
const MATERIALIZATION_FAILURE_CODE = "telemetry_history_materialization_failed";

export function createTelemetryWorker(deps: Partial<WorkerDeps> = {}) {
  const resolvedDeps: WorkerDeps = { ...defaultDeps, ...deps };
  return {
    async fetch(
      request: Request,
      env: Env,
      executionContext?: WorkerExecutionContext,
    ): Promise<Response> {
      try {
        const schedule = executionContext
          ? (task: Promise<void>) => executionContext.waitUntil(task)
          : null;
        return await handleFetch(request, env, resolvedDeps, schedule);
      } catch {
        console.error("telemetry_worker_unhandled_error");
        return jsonResponse({ error: "internal_error" }, 500);
      }
    },
    async scheduled(_controller: ScheduledController, env: Env): Promise<void> {
      await handleScheduledMaterialization(env, resolvedDeps);
    },
    async queue(batch: TelemetryQueueConsumerBatch, env: Env): Promise<void> {
      await handleQueue(batch, env, resolvedDeps);
    },
  };
}

export default createTelemetryWorker();

async function handleFetch(
  request: Request,
  env: Env,
  deps: WorkerDeps,
  schedule: RejectionTaskScheduler | null,
): Promise<Response> {
  const url = new URL(request.url);
  if (url.pathname === HEALTH_PATH || url.pathname.startsWith(`${HEALTH_PATH}/`)) {
    return handleIngestHealth(request, env, deps, url);
  }
  if (!ALL_POST_PATHS.has(url.pathname)) return jsonResponse({ error: "not_found" }, 404);
  const context: RejectionContext = {
    diagnostics: null,
    endpoint: url.pathname === INSTALL_ATTEMPT_PATH ? "install_stage" : "telemetry_batch",
    eventFamily: url.pathname === INSTALL_ATTEMPT_PATH ? "install_stage" : "batch",
    schedule,
    storage: readRejectionStorageConfig(env),
  };
  const config = readConfig(env);
  if (!config) {
    return rejectionResponse(
      deps,
      { ...context, storage: null },
      "pre_commit",
      "telemetry_env_not_configured",
      500,
    );
  }
  const rateLimitFailure = await applyAnonymousRateLimit(
    env.TELEMETRY_RATE_LIMITER,
    url.pathname,
    request.headers.get("cf-connecting-ip") ?? "unknown",
    deps,
    context,
  );
  if (rateLimitFailure) return rateLimitFailure;
  if (url.search.length > 0) {
    return rejectionResponse(
      deps,
      context,
      "pre_commit",
      "query_parameters_not_allowed",
      400,
    );
  }
  if (request.headers.has("origin")) {
    return rejectionResponse(deps, context, "pre_commit", "browser_origin_not_allowed", 400);
  }
  if (request.method !== "POST") {
    return rejectionResponse(
      deps,
      context,
      "pre_commit",
      "method_not_allowed",
      405,
      { allow: "POST" },
    );
  }
  if (request.headers.has("content-encoding")) {
    return rejectionResponse(
      deps,
      context,
      "pre_commit",
      "content_encoding_not_supported",
      415,
    );
  }
  if (!isJsonContentType(request.headers.get("content-type"))) {
    return rejectionResponse(deps, context, "pre_commit", "unsupported_media_type", 415);
  }

  const declaredLength = parseContentLength(request.headers.get("content-length"));
  if (declaredLength === "invalid") {
    return rejectionResponse(deps, context, "pre_commit", "invalid_content_length", 400);
  }
  if (declaredLength !== null && declaredLength > MAX_BODY_BYTES) {
    return rejectionResponse(deps, context, "pre_commit", "body_too_large", 413);
  }
  const payloadResult = await readJsonBody(request, deps, context);
  if (payloadResult instanceof Response) return payloadResult;
  const payload = payloadResult.payload;
  const payloadContext = {
    ...context,
    diagnostics: buildRejectionDiagnostics(
      payload,
      payloadResult.byteLength,
      context.endpoint,
    ),
    eventFamily: eventFamilyForPayload(payload, context.endpoint),
  };

  const requestNow = deps.now();
  const options = {
    analyticsEnvironment: config.analyticsEnvironment,
    verifiedInstallationCoordinate: undefined as string | undefined,
    identityHmacKey: config.identityHmacKey,
    identityKeyVersion: config.identityKeyVersion,
    now: () => requestNow,
  };

  if (url.pathname === INSTALL_ATTEMPT_PATH) {
    try {
      const row = await buildInstallStageRow(payload, options);
      await admitQueueMessage(config.queue, {
        format_version: TELEMETRY_QUEUE_FORMAT_VERSION,
        kind: "install_stage_row",
        collision_visibility: await collisionVisibility(config, {
          ...payloadContext,
          endpoint: "install_stage",
          eventFamily: "install_stage",
        }),
        row,
      });
      return emptyResponse(204);
    } catch (error) {
      return ingestFailureResponse(
        error,
        "queue_admission_failed",
        deps,
        payloadContext,
      );
    }
  }

  try {
    const coordinate = await verifiedBlameInstallationCoordinate({
      body: payloadResult.body,
      headers: request.headers,
      method: request.method,
      now: requestNow,
      path: url.pathname,
      payload,
      query: url.search,
    });
    options.verifiedInstallationCoordinate = coordinate;
    const plan = await buildTelemetryIngestPlan(payload, options);
    const proofBackedMaterialization = hasProMaterializationRow(plan.rows);
    const receipt = proofBackedMaterialization
      ? undefined
      : await buildBlameProductReceipt(
        plan.rows,
        coordinate,
        config.identityHmacKey,
        config.identityKeyVersion,
      );
    if (receipt != null) {
      await admitQueueMessage(config.queue, {
        format_version: TELEMETRY_QUEUE_FORMAT_VERSION,
        kind: "blame_product_receipt",
        collision_visibility: await collisionVisibility(config, {
          ...payloadContext,
          endpoint: "telemetry_batch",
          eventFamily: "operation_completed",
        }),
        receipt,
      });
    } else {
      if (hasCurrentBlameProductRow(plan.rows)) {
        throw new TelemetryIngestError(422, "blame_installation_proof_required");
      }
      await admitQueueMessages(
        config.queue,
        await Promise.all(plan.rows.map(async (row) => ({
          format_version: TELEMETRY_QUEUE_FORMAT_VERSION,
          kind: "telemetry_row",
          collision_visibility: await collisionVisibility(config, {
            ...payloadContext,
            endpoint: "telemetry_batch",
            eventFamily: row.event_name as TelemetryEventFamily,
          }),
          row,
        }))),
      );
    }
    return emptyResponse(204);
  } catch (error) {
    return ingestFailureResponse(error, "queue_admission_failed", deps, payloadContext);
  }
}

async function handleQueue(
  batch: TelemetryQueueConsumerBatch,
  env: Env,
  deps: WorkerDeps,
): Promise<void> {
  const analyticsEnvironment = consumerAnalyticsEnvironment(env);
  if (analyticsEnvironment === null) {
    console.error("telemetry_queue_environment_mismatch");
    retryQueueBatch(batch);
    return;
  }
  const queueNames = telemetryQueueNames(analyticsEnvironment);
  if (batch.queue !== queueNames.primary) {
    console.error("telemetry_queue_binding_mismatch");
    retryQueueBatch(batch);
    return;
  }
  const databaseUrl = nonEmpty(env.TELEMETRY_DATABASE_URL);
  let databasePromise: Promise<TelemetryDatabase> | null = null;
  const getDatabase = async (): Promise<TelemetryDatabase> => {
    if (!databaseUrl) throw new Error("telemetry_database_not_configured");
    return databasePromise ??= Promise.resolve(deps.createDatabaseClient(databaseUrl));
  };
  const telemetryMessages: Array<readonly [TelemetryQueueConsumerMessage, TelemetryQueueRowMessage]> = [];
  const otherMessages: Array<readonly [TelemetryQueueConsumerMessage, TelemetryQueueMessage]> = [];
  for (const queueMessage of batch.messages) {
    const message = await decodeTelemetryQueueMessage(queueMessage.body);
    if (!message) {
      console.error("telemetry_queue_message_invalid");
      retryQueueMessage(queueMessage);
      continue;
    }
    if (
      !queueMessageEnvironmentMatches(message, analyticsEnvironment)
    ) {
      console.error("telemetry_queue_environment_mismatch");
      retryQueueMessage(queueMessage);
      continue;
    }
    if (message.kind === "telemetry_row") {
      telemetryMessages.push([queueMessage, message]);
    } else {
      otherMessages.push([queueMessage, message]);
    }
  }
  if (telemetryMessages.length > 0) {
    let database: TelemetryDatabase | null = null;
    try {
      database = await getDatabase();
      await database.insertTelemetryRows(telemetryMessages.map(([, message]) => message.row));
      for (const [queueMessage] of telemetryMessages) queueMessage.ack();
    } catch (error) {
      if (error instanceof TelemetryEventCollisionError && database !== null) {
        for (const [queueMessage, message] of telemetryMessages) {
          await persistQueueMessageIndividually(database, queueMessage, message);
        }
      } else {
        console.error("telemetry_queue_consumer_retry");
        for (const [queueMessage] of telemetryMessages) retryQueueMessage(queueMessage);
      }
    }
  }
  for (const [queueMessage, message] of otherMessages) {
    await persistQueueMessageIndividually(getDatabase(), queueMessage, message);
  }
}

function consumerAnalyticsEnvironment(env: Env): TelemetryEnvironment | null {
  const value = nonEmpty(env.TELEMETRY_ANALYTICS_ENVIRONMENT);
  return value === "production" || value === "staging" ? value : null;
}

function queueMessageEnvironmentMatches(
  message: TelemetryQueueMessage,
  expected: TelemetryEnvironment,
): boolean {
  if (message.collision_visibility.analytics_environment !== expected) return false;
  switch (message.kind) {
    case "telemetry_row":
      return message.row.analytics_environment === expected;
    case "blame_product_receipt":
      return message.receipt.analytics_environment === expected;
    case "install_stage_row":
      return message.row.analytics_environment === null
        || message.row.analytics_environment === expected;
  }
}

async function persistQueueMessage(
  database: TelemetryDatabase,
  message: TelemetryQueueMessage,
): Promise<void> {
  switch (message.kind) {
    case "telemetry_row":
      await database.insertTelemetryRows([message.row]);
      return;
    case "blame_product_receipt":
      await database.insertBlameProductReceipt(message.receipt);
      return;
    case "install_stage_row":
      await database.insertInstallStageRow(message.row);
  }
}

async function persistQueueMessageIndividually(
  databaseSource: TelemetryDatabase | Promise<TelemetryDatabase>,
  queueMessage: TelemetryQueueConsumerMessage,
  message: TelemetryQueueMessage,
): Promise<void> {
  let database: TelemetryDatabase | null = null;
  try {
    database = await databaseSource;
    await persistQueueMessage(database, message);
    queueMessage.ack();
  } catch (error) {
    if (!(error instanceof TelemetryEventCollisionError) || database === null) {
      console.error("telemetry_queue_consumer_retry");
      retryQueueMessage(queueMessage);
      return;
    }
    try {
      await persistCollisionVisibility(database, message);
    } catch {
      console.error("telemetry_queue_collision_visibility_retry");
      retryQueueMessage(queueMessage);
      return;
    }
    console.warn("telemetry_queue_event_id_collision");
    queueMessage.ack();
  }
}

async function persistCollisionVisibility(
  database: TelemetryDatabase,
  message: TelemetryQueueMessage,
): Promise<void> {
  if (!database.recordIngestRejection) throw new Error("collision_visibility_unavailable");
  const collisionReceiptId = await sha256Hex(JSON.stringify([
    TELEMETRY_COLLISION_RECEIPT_DOMAIN,
    message.format_version,
    message.collision_visibility.analytics_environment,
    message.kind,
    ...telemetryQueueCollisionIdentity(message),
  ]));
  await database.recordIngestRejection(
    {
      ...message.collision_visibility,
      rejection_class: "event_collision",
      rejection_code: "event_id_collision",
    },
    collisionReceiptId,
  );
}

function retryQueueMessage(message: TelemetryQueueConsumerMessage): void {
  message.retry({ delaySeconds: telemetryQueueRetryDelay(message.attempts) });
}

function retryQueueBatch(batch: TelemetryQueueConsumerBatch): void {
  for (const message of batch.messages) retryQueueMessage(message);
}
function telemetryQueueNames(environment: TelemetryEnvironment): Readonly<{
  primary: string;
}> {
  const suffix = environment === "production" ? "prod" : "staging";
  return {
    primary: `ctx-telemetry-ingest-${suffix}`,
  };
}

async function collisionVisibility(
  config: WorkerConfig,
  context: RejectionContext,
): Promise<TelemetryQueueCollisionVisibility> {
  const diagnostics = context.diagnostics;
  return {
    analytics_environment: config.analyticsEnvironment,
    endpoint: context.endpoint,
    event_family: context.eventFamily,
    app_version: diagnostics?.app_version ?? "unknown",
    field_shape_fingerprint: diagnostics
      ? await sha256Hex(JSON.stringify(diagnostics.field_shape))
      : "none",
    field_shape_overflow: diagnostics?.field_shape_overflow ?? false,
    provider_classification: diagnostics?.provider_classification ?? "unknown",
    size_bucket: diagnostics?.size_bucket ?? "unknown",
  };
}

async function handleIngestHealth(
  request: Request,
  env: Env,
  deps: WorkerDeps,
  url: URL,
): Promise<Response> {
  if (url.search.length > 0) return jsonResponse({ error: "not_found" }, 404);
  const suffix = url.pathname === HEALTH_PATH ? undefined : url.pathname.slice(HEALTH_PATH.length + 1);
  const signal = HEALTH_SIGNALS.find((value) => value === suffix);
  if (suffix !== undefined && signal === undefined) return jsonResponse({ error: "not_found" }, 404);
  if (request.method !== "GET") {
    return jsonResponse({ error: "method_not_allowed" }, 405, { allow: "GET" });
  }
  const config = readConfig(env);
  const configured = config !== null && rateLimiterConfigured(env.TELEMETRY_RATE_LIMITER);
  if (!signal && !configured) return jsonResponse(unavailableHealthStatus("producer_not_configured"), 503);
  const storage = readRejectionStorageConfig(env);
  const readSnapshot = async () => {
    if (!storage) return null;
    try {
      const database = await deps.createDatabaseClient(storage.databaseUrl);
      return await database.readIngestHealthSnapshot(storage.analyticsEnvironment);
    } catch {
      console.error("telemetry_ingest_health_unavailable");
      return null;
    }
  };
  const readQueue = async () => {
    try {
      return await deps.readQueueHealth(env) ? undefined : "telemetry_queue_health_signal";
    } catch (error) {
      console.error("telemetry_queue_health_unavailable", queueHealthFailureDiagnostic(error));
      return "queue_metrics_unavailable";
    }
  };
  // Each independently notified category reads only its owning dependency.
  if (signal) {
    const result = await readHealthSignal(signal, configured, storage !== null, readSnapshot, readQueue);
    return jsonResponse(result, result.alert ? 503 : 200);
  }
  // Independent reads share the response window; client failures must not hide Queue health.
  const [snapshot, queueReason] = await Promise.all([readSnapshot(), readQueue()]);
  if (!snapshot) return jsonResponse(unavailableHealthStatus("database_unavailable"), 503);
  const result = aggregateHealthStatus(snapshot, queueReason);
  return jsonResponse(result, result.status === "ok" ? 200 : 503);
}

async function applyAnonymousRateLimit(
  binding: RateLimitBinding | undefined,
  route: string,
  address: string,
  deps: WorkerDeps,
  context: RejectionContext,
): Promise<Response | null> {
  if (!rateLimiterConfigured(binding)) {
    return rejectionResponse(deps, context, "rate_limit", "rate_limit_unavailable", 503);
  }
  try {
    const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(address));
    const key = Array.from(
      new Uint8Array(digest).subarray(0, 16),
      (byte) => byte.toString(16).padStart(2, "0"),
    ).join("");
    const result = await binding.limit({ key: `${route}:${key}` });
    if (!result.success) {
      return rejectionResponse(
        deps,
        context,
        "rate_limit",
        "rate_limited",
        429,
        { "retry-after": "60" },
      );
    }
    return null;
  } catch {
    return rejectionResponse(deps, context, "rate_limit", "rate_limit_unavailable", 503);
  }
}

function rateLimiterConfigured(binding: RateLimitBinding | undefined): binding is RateLimitBinding {
  return typeof binding?.limit === "function";
}

async function handleScheduledMaterialization(env: Env, deps: WorkerDeps): Promise<void> {
  try {
    const databaseUrl = nonEmpty(env.TELEMETRY_RETENTION_DATABASE_URL);
    if (!databaseUrl) throw new Error(MATERIALIZATION_FAILURE_CODE);
    const database = await deps.createMaintenanceDatabaseClient(databaseUrl);
    const collisionReceiptsDeleted =
      await database.deleteExpiredTelemetryEventCollisionReceipts();
    const materialization = await database.materializeProductTelemetryHistory();
    console.info(MATERIALIZATION_SUCCESS_CODE, {
      collision_receipts_deleted: collisionReceiptsDeleted,
      ...materialization,
    });
  } catch {
    console.error(MATERIALIZATION_FAILURE_CODE);
    throw new Error(MATERIALIZATION_FAILURE_CODE);
  }
}

async function readJsonBody(
  request: Request,
  deps: WorkerDeps,
  context: RejectionContext,
): Promise<{ body: Uint8Array; byteLength: number; payload: unknown } | Response> {
  const reader = request.body?.getReader();
  if (!reader) return rejectionResponse(deps, context, "schema", "invalid_json", 400);
  const chunks: Uint8Array[] = [];
  let byteLength = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      byteLength += value.byteLength;
      if (byteLength > MAX_BODY_BYTES) {
        await reader.cancel();
        return rejectionResponse(deps, context, "pre_commit", "body_too_large", 413);
      }
      chunks.push(value);
    }
  } catch {
    return rejectionResponse(deps, context, "schema", "invalid_body", 400);
  }
  if (byteLength === 0) {
    return rejectionResponse(deps, context, "schema", "invalid_json", 400);
  }
  const bytes = new Uint8Array(byteLength);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  try {
    const text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
    return { body: bytes, byteLength, payload: JSON.parse(text) as unknown };
  } catch {
    return rejectionResponse(deps, context, "schema", "invalid_json", 400);
  }
}

function readRejectionStorageConfig(env: Env): RejectionStorageConfig | null {
  const databaseUrl = nonEmpty(env.TELEMETRY_DATABASE_URL);
  const environmentText = nonEmpty(env.TELEMETRY_ANALYTICS_ENVIRONMENT);
  if (
    !databaseUrl ||
    (environmentText !== "production" && environmentText !== "staging")
  ) return null;
  return { analyticsEnvironment: environmentText, databaseUrl };
}

function readConfig(env: Env): WorkerConfig | null {
  const databaseUrl = nonEmpty(env.TELEMETRY_DATABASE_URL);
  const identityHmacKey = nonEmpty(env.TELEMETRY_IDENTITY_HMAC_KEY);
  const keyVersionText = nonEmpty(env.TELEMETRY_IDENTITY_KEY_VERSION);
  const environmentText = nonEmpty(env.TELEMETRY_ANALYTICS_ENVIRONMENT);
  const queue = env.TELEMETRY_INGEST_QUEUE;
  if (
    !databaseUrl
    || !identityHmacKey
    || !keyVersionText
    || !environmentText
    || !queue
    || typeof queue.sendBatch !== "function"
  ) {
    return null;
  }
  const identityKeyVersion = Number(keyVersionText);
  try {
    validateIdentityOptions(identityHmacKey, identityKeyVersion);
  } catch {
    return null;
  }
  if (environmentText !== "production" && environmentText !== "staging") {
    return null;
  }
  return {
    analyticsEnvironment: environmentText,
    databaseUrl,
    identityHmacKey,
    identityKeyVersion,
    queue,
  };
}

async function ingestFailureResponse(
  error: unknown,
  databaseCode: string,
  deps: WorkerDeps,
  context: RejectionContext,
): Promise<Response> {
  if (error instanceof TelemetryIngestError) {
    return rejectionResponse(deps, context, "schema", error.code, error.status);
  }
  if (error instanceof TelemetryEventCollisionError) {
    return rejectionResponse(deps, context, "pre_commit", "event_id_collision", 409);
  }
  console.error(databaseCode);
  return rejectionResponse(
    deps,
    context,
    "pre_commit",
    databaseCode,
    databaseCode === "queue_admission_failed" ? 503 : 502,
  );
}

async function rejectionResponse(
  deps: WorkerDeps,
  context: RejectionContext,
  category: TelemetryRejectionObservation["category"],
  code: string,
  status: number,
  extraHeaders: Record<string, string> = {},
): Promise<Response> {
  const boundedCode = REJECTION_CODE_PATTERN.test(code) ? code : "unknown_rejection";
  const observation: TelemetryRejectionObservation = {
    category,
    code: boundedCode,
    endpoint: context.endpoint,
    event_family: context.eventFamily,
    rejection_class: rejectionClassFor(category, boundedCode),
    status,
    ...(context.diagnostics ?? {}),
  };
  observeRejection(deps, observation);
  // Rate-limit denials can be generated at attacker-controlled volume, so
  // Cloudflare request metrics and the bounded server log observation are the
  // authoritative counter for that class. Accepted requests are already
  // rate-bounded before any Neon-backed rejection counter is scheduled.
  if (
    category !== "rate_limit"
    && isPersistedRejectionClass(observation.rejection_class)
  ) {
    await recordRejection(
      deps,
      context,
      observation.event_family,
      observation.rejection_class,
      observation.code,
    );
  }
  return jsonResponse({ error: boundedCode }, status, extraHeaders);
}

function observeRejection(
  deps: WorkerDeps,
  observation: TelemetryRejectionObservation,
): void {
  try {
    deps.observeRejection(observation);
  } catch {
    console.error("telemetry_rejection_observer_failed");
  }
}

async function recordRejection(
  deps: WorkerDeps,
  context: RejectionContext,
  eventFamily: TelemetryEventFamily,
  rejectionClass: PersistedTelemetryRejectionClass,
  rejectionCode: string,
): Promise<void> {
  const storage = context.storage;
  if (!storage) return;
  const diagnostics = context.diagnostics;
  const task = (async () => {
    try {
      const fieldShapeFingerprint = diagnostics
        ? await sha256Hex(JSON.stringify(diagnostics.field_shape))
        : "none";
      const rejection: TelemetryIngestRejection = {
        analytics_environment: storage.analyticsEnvironment,
        endpoint: context.endpoint,
        event_family: eventFamily,
        rejection_class: rejectionClass,
        rejection_code: rejectionCode,
        app_version: diagnostics?.app_version ?? "unknown",
        field_shape_fingerprint: fieldShapeFingerprint,
        field_shape_overflow: diagnostics?.field_shape_overflow ?? false,
        provider_classification: diagnostics?.provider_classification ?? "unknown",
        size_bucket: diagnostics?.size_bucket ?? "unknown",
      };
      const database = await deps.createDatabaseClient(storage.databaseUrl);
      if (!database.recordIngestRejection) throw new Error("rejection_recorder_unavailable");
      await database.recordIngestRejection(rejection);
    } catch {
      console.error("telemetry_rejection_observer_failed");
    }
  })();
  if (context.schedule) {
    try {
      context.schedule(task);
    } catch {
      console.error("telemetry_rejection_observer_failed");
    }
  } else {
    await task;
  }
}

function isPersistedRejectionClass(
  value: TelemetryRejectionClass,
): value is PersistedTelemetryRejectionClass {
  return value !== "queue_admission" && value !== "rate_limited" && value !== "database";
}

function eventFamilyForPayload(
  payload: unknown,
  endpoint: TelemetryIngestEndpoint,
): TelemetryEventFamily {
  if (endpoint === "install_stage") return "install_stage";
  if (!isRecord(payload) || !Array.isArray(payload.events) || payload.events.length === 0) {
    return "batch";
  }
  const families = new Set<TelemetryEventFamily>();
  for (const event of payload.events) {
    if (!isRecord(event) || typeof event.event_name !== "string") return "batch";
    if (!EVENT_FAMILIES.has(event.event_name as TelemetryEventFamily)) return "batch";
    families.add(event.event_name as TelemetryEventFamily);
  }
  return families.size === 1 ? [...families][0] : "batch";
}

function rejectionClassFor(
  category: TelemetryRejectionObservation["category"],
  code: string,
): TelemetryRejectionClass {
  if (code === "body_too_large") return "body_too_large";
  if (code === "invalid_json" || code === "invalid_body") return "invalid_json";
  if (code === "too_many_events") return "too_many_events";
  if (code === "event_id_collision") return "event_collision";
  if (code === "queue_admission_failed") return "queue_admission";
  if (code === "telemetry_insert_failed" || code === "install_attempt_insert_failed") {
    return "database";
  }
  if (category === "rate_limit") return code === "rate_limited" ? "rate_limited" : "other";
  if (IDENTITY_REJECTION_CODES.has(code)) return "invalid_identity";
  if (ENVELOPE_REJECTION_CODES.has(code)) return "invalid_envelope";
  if (EVENT_REJECTION_CODES.has(code)) return "invalid_event";
  if (category === "schema") return "invalid_properties";
  return "other";
}

function parseContentLength(value: string | null): number | null | "invalid" {
  if (value === null) return null;
  if (!/^(?:0|[1-9]\d*)$/u.test(value)) return "invalid";
  const parsed = Number(value);
  return Number.isSafeInteger(parsed) ? parsed : "invalid";
}

function isJsonContentType(value: string | null): boolean {
  if (value === null) return false;
  const parts = value.toLowerCase().split(";").map((part) => part.trim());
  if (parts[0] !== "application/json") return false;
  return parts.length === 1 || (parts.length === 2 && parts[1] === "charset=utf-8");
}

function nonEmpty(value: string | undefined): string | null {
  if (typeof value !== "string" || value.trim().length === 0) return null;
  return value.trim();
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function emptyResponse(status: number): Response {
  return new Response(null, { status, headers: SECURITY_HEADERS });
}

function jsonResponse(
  body: Record<string, unknown>,
  status: number,
  extraHeaders: Record<string, string> = {},
): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      ...SECURITY_HEADERS,
      "content-type": JSON_CONTENT_TYPE,
      ...extraHeaders,
    },
  });
}

export type { TelemetryDatabase };
