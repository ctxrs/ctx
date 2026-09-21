import type { TelemetryIngestHealthSnapshot } from "./database";

const API = "https://api.cloudflare.com/client/v4";
const GRAPHQL = `${API}/graphql`;
const BACKLOG_ALERT_COUNT = 1_000;
const OLDEST_ALERT_SECONDS = 300;
const LAG_ALERT_MILLISECONDS = 300_000;
const MAX_CONCURRENCY = 4;
// Twelve max-batch waits: distinguish normal cap usage from sustained saturation.
const CONCURRENCY_SATURATION_OLDEST_AGE_SECONDS = 60;
const LOOKBACK_MILLISECONDS = 15 * 60 * 1000;
// Cover the measured inventory-plus-metrics path while staying below the native monitor timeout.
const QUEUE_HEALTH_TIMEOUT_MILLISECONDS = 4_500;

const CONCURRENCY_QUERY = `query QueueConcurrency($accountTag: String!, $queueId: String!, $start: Time!, $end: Time!) {
  viewer { accounts(filter: {accountTag: $accountTag}) {
    queueConsumerMetricsAdaptiveGroups(
      limit: 1000
      filter: {queueId: $queueId, datetime_geq: $start, datetime_leq: $end}
    ) { avg { concurrency } }
  } }
}`;

const OPERATIONS_QUERY = `query QueueOperations($accountTag: String!, $queueId: String!, $start: Date!, $end: Date!) {
  viewer { accounts(filter: {accountTag: $accountTag}) {
    queueMessageOperationsAdaptiveGroups(
      limit: 1000
      filter: {queueId: $queueId, datetime_geq: $start, datetime_leq: $end}
    ) { count avg { lagTime retryCount } dimensions { actionType outcome } }
  } }
}`;

export type TelemetryQueueHealthEnv = Readonly<{
  TELEMETRY_ANALYTICS_ENVIRONMENT?: string;
  TELEMETRY_CLOUDFLARE_ACCOUNT_ID?: string;
  TELEMETRY_QUEUE_HEALTH_API_TOKEN?: string;
}>;

export type TelemetryQueueHealthSnapshot = Readonly<{
  primaryBacklog: number;
  primaryOldestAgeSeconds: number;
  dlqBacklog: number;
  lagMillisecondsMax: number;
  retryCountMax: number;
  failedOperations: number;
  dlqOutcomes: number;
  consumerConcurrencyMax: number;
}>;

export type TelemetryQueueHealthFailureDiagnostic = Readonly<{
  stage: "inventory"
    | "primary_metrics"
    | "dlq_metrics"
    | "concurrency_graphql"
    | "operations_graphql"
    | "unknown";
  kind: "timeout" | "parse" | "upstream" | "unknown";
  http_status?: number;
  cloudflare_error_code?: number;
}>;

class TelemetryQueueHealthReadError extends Error {
  constructor(
    message: string,
    readonly diagnostic: TelemetryQueueHealthFailureDiagnostic,
  ) {
    super(message);
    this.name = "TelemetryQueueHealthReadError";
  }
}

export function queueHealthFailureDiagnostic(
  error: unknown,
): TelemetryQueueHealthFailureDiagnostic {
  return error instanceof TelemetryQueueHealthReadError
    ? error.diagnostic
    : {stage: "unknown", kind: "unknown"};
}

export function queueHealthIsHealthy(snapshot: TelemetryQueueHealthSnapshot): boolean {
  return snapshot.primaryBacklog < BACKLOG_ALERT_COUNT
    && snapshot.primaryOldestAgeSeconds < OLDEST_ALERT_SECONDS
    && snapshot.dlqBacklog === 0
    && snapshot.lagMillisecondsMax < LAG_ALERT_MILLISECONDS
    && snapshot.retryCountMax === 0
    && snapshot.failedOperations === 0
    && snapshot.dlqOutcomes === 0
    && !(snapshot.primaryBacklog > 0 && snapshot.consumerConcurrencyMax === 0)
    && !(
      snapshot.consumerConcurrencyMax >= MAX_CONCURRENCY
      && snapshot.primaryOldestAgeSeconds >= CONCURRENCY_SATURATION_OLDEST_AGE_SECONDS
    );
}

export const HEALTH_SIGNALS = [
  "producer_configuration", "database_availability", "compatibility_rejections",
  "event_collisions", "other_rejections", "queue_health", "queue_metrics",
] as const;
export type HealthSignal = typeof HEALTH_SIGNALS[number];

export function ingestHealthSignals(snapshot: TelemetryIngestHealthSnapshot) {
  return {
    compatibility_rejections: snapshot.compatibilityRejectionMax < 100n,
    event_collisions: snapshot.eventCollisionCount < 1n,
    other_rejections: snapshot.otherRejectionCount < 5n,
  };
}

export function ingestHealthIsHealthy(snapshot: TelemetryIngestHealthSnapshot): boolean {
  return Object.values(ingestHealthSignals(snapshot)).every(Boolean);
}

export async function readHealthSignal(signal: HealthSignal, configured: boolean, storageConfigured: boolean,
  readSnapshot: () => Promise<TelemetryIngestHealthSnapshot | null>, readQueue: () => Promise<string | undefined>) {
  let status: "ok" | "degraded" | "unavailable";
  if (signal === "producer_configuration") {
    status = configured ? "ok" : "degraded";
  } else if (signal === "queue_health" || signal === "queue_metrics") {
    const reason = await readQueue();
    status = signal === "queue_metrics"
      ? reason === "queue_metrics_unavailable" ? "degraded" : "ok"
      : reason === "queue_metrics_unavailable" ? "unavailable" : reason ? "degraded" : "ok";
  } else if (!storageConfigured) {
    status = "unavailable";
  } else {
    const snapshot = await readSnapshot();
    status = signal === "database_availability" ? snapshot ? "ok" : "degraded"
      : snapshot ? ingestHealthSignals(snapshot)[signal] ? "ok" : "degraded" : "unavailable";
  }
  // Unavailable is not healthy: its prerequisite's monitor owns that incident.
  return { signal, status, alert: status === "degraded" };
}

export function clientHealthIsHealthy(snapshot: TelemetryIngestHealthSnapshot): boolean {
  return snapshot.providerRefreshFailureCount < 5n
    && snapshot.deliveryDegradedCount < 5n
    && snapshot.deliveryDroppedCount < 1n;
}

export function unavailableHealthStatus(reason: "producer_not_configured" | "database_unavailable") {
  return {
    status: "degraded", reason,
    ingestion: { status: "degraded", reason },
    client: { status: "unavailable", reason },
  };
}

export function aggregateHealthStatus(snapshot: TelemetryIngestHealthSnapshot, queueReason?: string) {
  const ingestionReason = ingestHealthIsHealthy(snapshot) ? queueReason : "telemetry_health_signal";
  const clientReason = clientHealthIsHealthy(snapshot) ? undefined : "telemetry_health_signal";
  // Service availability does not depend on a client's local import or outbox state.
  return {
    status: ingestionReason ? "degraded" : "ok",
    ...(ingestionReason ? { reason: ingestionReason } : {}),
    ingestion: ingestionReason ? { status: "degraded", reason: ingestionReason } : { status: "ok" },
    client: clientReason ? { status: "degraded", reason: clientReason } : { status: "ok" },
  };
}

export async function readTelemetryQueueHealth(
  env: TelemetryQueueHealthEnv,
  fetchImpl: typeof fetch = fetch,
  nowMs = Date.now(),
): Promise<boolean> {
  const environment = env.TELEMETRY_ANALYTICS_ENVIRONMENT;
  const accountId = env.TELEMETRY_CLOUDFLARE_ACCOUNT_ID;
  const token = env.TELEMETRY_QUEUE_HEALTH_API_TOKEN;
  if (
    (environment !== "production" && environment !== "staging")
    || !accountId
    || !/^[0-9a-f]{32}$/u.test(accountId)
    || !token
  ) throw new Error("telemetry_queue_health_not_configured");
  const suffix = environment === "production" ? "prod" : "staging";
  const expected = {
    primary: `ctx-telemetry-ingest-${suffix}`,
    dlq: `ctx-telemetry-ingest-${suffix}-dlq`,
  };
  const context = {
    accountId,
    fetchImpl,
    signal: AbortSignal.timeout(QUEUE_HEALTH_TIMEOUT_MILLISECONDS),
    token,
  };
  const {primary, dlq} = await queueHealthStage("inventory", context.signal, async () => {
    const queues = await listQueues(context);
    return {
      primary: exactQueue(queues, expected.primary),
      dlq: exactQueue(queues, expected.dlq),
    };
  });
  const time = {
    start: new Date(nowMs - LOOKBACK_MILLISECONDS).toISOString(),
    end: new Date(nowMs).toISOString(),
  };
  const [primaryMetrics, dlqMetrics, concurrency, operations] = await Promise.all([
    queueHealthStage("primary_metrics", context.signal,
      () => queueMetrics(context, primary.queue_id, "primary_metrics")),
    queueHealthStage("dlq_metrics", context.signal,
      () => queueMetrics(context, dlq.queue_id, "dlq_metrics")),
    queueHealthStage("concurrency_graphql", context.signal, async () => parseConcurrencyRows(
      await graphqlRows(context, primary.queue_id, time, CONCURRENCY_QUERY,
        "queueConsumerMetricsAdaptiveGroups", "concurrency_graphql"),
    )),
    queueHealthStage("operations_graphql", context.signal, async () => parseOperationRows(
      await graphqlRows(context, primary.queue_id, time, OPERATIONS_QUERY,
        "queueMessageOperationsAdaptiveGroups", "operations_graphql"),
    )),
  ]);
  return queueHealthIsHealthy({
    primaryBacklog: primaryMetrics.backlog,
    primaryOldestAgeSeconds: oldestAgeSeconds(primaryMetrics.oldest, nowMs),
    dlqBacklog: dlqMetrics.backlog,
    lagMillisecondsMax: operations.lagMillisecondsMax,
    retryCountMax: operations.retryCountMax,
    failedOperations: operations.failedOperations,
    dlqOutcomes: operations.dlqOutcomes,
    consumerConcurrencyMax: concurrency,
  });
}

type Context = Readonly<{
  accountId: string;
  fetchImpl: typeof fetch;
  signal: AbortSignal;
  token: string;
}>;

type QueueHealthStage = Exclude<TelemetryQueueHealthFailureDiagnostic["stage"], "unknown">;

async function cloudflare(
  context: Context,
  stage: QueueHealthStage,
  url: string,
  init: RequestInit = {},
): Promise<unknown> {
  const {payload, status} = await cloudflareJson(context, stage, url, init);
  if (!isRecord(payload) || typeof payload.success !== "boolean") {
    throw new Error("telemetry_queue_health_invalid_response");
  }
  if (payload.success !== true) {
    throw queueHealthError("telemetry_queue_health_api_failed", stage, "upstream", {
      http_status: status,
      cloudflare_error_code: cloudflareErrorCode(payload),
    });
  }
  return payload;
}

async function cloudflareJson(
  context: Context,
  stage: QueueHealthStage,
  url: string,
  init: RequestInit = {},
): Promise<{payload: unknown; status: number}> {
  const fetchImpl = context.fetchImpl;
  let response: Response;
  try {
    response = await fetchImpl(url, {
      ...init,
      headers: {
        Authorization: `Bearer ${context.token}`,
        "Content-Type": "application/json",
      },
      signal: context.signal,
    });
  } catch (error) {
    throw queueHealthError(
      "telemetry_queue_health_request_failed",
      stage,
      isTimeout(context.signal, error) ? "timeout" : "upstream",
    );
  }
  let payload: unknown;
  try {
    payload = await response.json();
  } catch (error) {
    if (!response.ok) {
      throw queueHealthError("telemetry_queue_health_api_failed", stage, "upstream", {
        http_status: response.status,
      });
    }
    if (isTimeout(context.signal, error)) {
      throw queueHealthError("telemetry_queue_health_request_failed", stage, "timeout");
    }
    throw new Error("telemetry_queue_health_invalid_response");
  }
  if (!response.ok) {
    throw queueHealthError("telemetry_queue_health_api_failed", stage, "upstream", {
      http_status: response.status,
      cloudflare_error_code: cloudflareErrorCode(payload),
    });
  }
  return {payload, status: response.status};
}

async function listQueues(context: Context): Promise<Record<string, unknown>[]> {
  const queues: Record<string, unknown>[] = [];
  for (let page = 1; page <= 10; page += 1) {
    const payload = await cloudflare(
      context,
      "inventory",
      `${API}/accounts/${context.accountId}/queues?per_page=100&page=${page}`,
    );
    if (!isRecord(payload) || !Array.isArray(payload.result)) {
      throw new Error("telemetry_queue_health_invalid_inventory");
    }
    const rows = payload.result.filter(isRecord);
    if (rows.length !== payload.result.length) {
      throw new Error("telemetry_queue_health_invalid_inventory");
    }
    queues.push(...rows);
    const totalPages = isRecord(payload.result_info)
      ? Number(payload.result_info.total_pages ?? 1)
      : 1;
    if (page >= totalPages || rows.length === 0) return queues;
  }
  throw new Error("telemetry_queue_health_inventory_too_large");
}

function exactQueue(queues: Record<string, unknown>[], name: string): {queue_id: string} {
  const matches = queues.filter((queue) => queue.queue_name === name);
  if (matches.length !== 1 || typeof matches[0]?.queue_id !== "string") {
    throw new Error("telemetry_queue_health_topology_mismatch");
  }
  return {queue_id: matches[0].queue_id};
}

async function queueMetrics(
  context: Context,
  queueId: string,
  stage: "primary_metrics" | "dlq_metrics",
): Promise<{backlog: number; oldest: number}> {
  const payload = await cloudflare(
    context,
    stage,
    `${API}/accounts/${context.accountId}/queues/${queueId}/metrics`,
  );
  if (!isRecord(payload) || !isRecord(payload.result)) {
    throw new Error("telemetry_queue_health_invalid_metrics");
  }
  const backlog = Number(payload.result.backlog_count);
  const oldest = Number(payload.result.oldest_message_timestamp_ms);
  if (!Number.isSafeInteger(backlog) || backlog < 0 || !Number.isFinite(oldest)) {
    throw new Error("telemetry_queue_health_invalid_metrics");
  }
  return {backlog, oldest};
}

async function graphqlRows(
  context: Context,
  queueId: string,
  time: {start: string; end: string},
  query: string,
  dataset: string,
  stage: "concurrency_graphql" | "operations_graphql",
): Promise<Record<string, unknown>[]> {
  const {payload, status} = await cloudflareJson(context, stage, GRAPHQL, {
    method: "POST",
    body: JSON.stringify({
      query,
      variables: {accountTag: context.accountId, queueId, ...time},
    }),
  });
  if (!isRecord(payload)) {
    throw new Error("telemetry_queue_health_invalid_graphql");
  }
  if (Array.isArray(payload.errors) && payload.errors.length) {
    throw queueHealthError("telemetry_queue_health_graphql_failed", stage, "upstream", {
      http_status: status,
      cloudflare_error_code: cloudflareErrorCode(payload),
    });
  }
  const accounts = isRecord(payload.data) && isRecord(payload.data.viewer)
    ? payload.data.viewer.accounts
    : null;
  const rows = Array.isArray(accounts) && accounts.length === 1 && isRecord(accounts[0])
    ? accounts[0][dataset]
    : null;
  if (!Array.isArray(rows) || !rows.every(isRecord)) {
    throw new Error("telemetry_queue_health_invalid_graphql");
  }
  return rows;
}

function parseConcurrencyRows(rows: Record<string, unknown>[]): number {
  if (rows.length === 0) throw new Error("telemetry_queue_health_concurrency_missing");
  const values = rows.map((row) => {
    const value = isRecord(row.avg) ? row.avg.concurrency : null;
    if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
      throw new Error("telemetry_queue_health_concurrency_invalid");
    }
    return value;
  });
  return Math.max(...values);
}

function parseOperationRows(rows: Record<string, unknown>[]): Readonly<{
  lagMillisecondsMax: number;
  retryCountMax: number;
  failedOperations: number;
  dlqOutcomes: number;
}> {
  if (rows.length === 0) throw new Error("telemetry_queue_health_operations_missing");
  const lagValues: number[] = [];
  const retryValues: number[] = [];
  let failedOperations = 0;
  let dlqOutcomes = 0;
  for (const row of rows) {
    if (!Number.isSafeInteger(row.count) || (row.count as number) < 0) {
      throw new Error("telemetry_queue_health_operation_count_invalid");
    }
    if (!isRecord(row.avg) || !isRecord(row.dimensions)) {
      throw new Error("telemetry_queue_health_operation_row_invalid");
    }
    for (const [field, target] of [
      ["lagTime", lagValues],
      ["retryCount", retryValues],
    ] as const) {
      const value = row.avg[field];
      if (value === null) continue;
      if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
        throw new Error("telemetry_queue_health_operation_average_invalid");
      }
      target.push(value);
    }
    const actionType = row.dimensions.actionType;
    const outcome = row.dimensions.outcome;
    const validOutcome = actionType === "DeleteMessage"
      ? typeof outcome === "string" && ["success", "dlq", "fail"].includes(outcome)
      : (actionType === "WriteMessage" || actionType === "ReadMessage")
        && (outcome === null || outcome === "");
    if (
      typeof actionType !== "string"
      || !new Set(["WriteMessage", "ReadMessage", "DeleteMessage"]).has(actionType)
      || !validOutcome
    ) throw new Error("telemetry_queue_health_operation_dimensions_invalid");
    if (actionType === "DeleteMessage" && outcome === "fail") {
      failedOperations += row.count as number;
    }
    if (actionType === "DeleteMessage" && outcome === "dlq") {
      dlqOutcomes += row.count as number;
    }
  }
  if (lagValues.length === 0 || retryValues.length === 0) {
    throw new Error("telemetry_queue_health_operation_averages_missing");
  }
  return {
    lagMillisecondsMax: Math.max(...lagValues),
    retryCountMax: Math.max(...retryValues),
    failedOperations,
    dlqOutcomes,
  };
}

function oldestAgeSeconds(timestamp: number, nowMs: number): number {
  return timestamp > 0 ? Math.max(0, Math.round((nowMs - timestamp) / 1000)) : 0;
}

async function queueHealthStage<T>(
  stage: QueueHealthStage,
  signal: AbortSignal,
  read: () => Promise<T>,
): Promise<T> {
  try {
    return await read();
  } catch (error) {
    if (error instanceof TelemetryQueueHealthReadError) throw error;
    throw queueHealthError(
      error instanceof Error ? error.message : "telemetry_queue_health_read_failed",
      stage,
      isTimeout(signal, error) ? "timeout" : "parse",
    );
  }
}

function queueHealthError(
  message: string,
  stage: QueueHealthStage,
  kind: Exclude<TelemetryQueueHealthFailureDiagnostic["kind"], "unknown">,
  upstream: Readonly<{
    http_status?: number;
    cloudflare_error_code?: number;
  }> = {},
): TelemetryQueueHealthReadError {
  return new TelemetryQueueHealthReadError(message, {
    stage,
    kind,
    ...(upstream.http_status === undefined ? {} : {http_status: upstream.http_status}),
    ...(upstream.cloudflare_error_code === undefined
      ? {}
      : {cloudflare_error_code: upstream.cloudflare_error_code}),
  });
}

function isTimeout(signal: AbortSignal, error: unknown): boolean {
  if (signal.aborted) return true;
  return error instanceof Error && (error.name === "AbortError" || error.name === "TimeoutError");
}

function cloudflareErrorCode(payload: unknown): number | undefined {
  if (!isRecord(payload) || !Array.isArray(payload.errors)) return undefined;
  for (const error of payload.errors) {
    if (!isRecord(error)) continue;
    const extensionCode = isRecord(error.extensions) ? error.extensions.code : undefined;
    for (const value of [error.code, extensionCode]) {
      if (typeof value === "number" && Number.isSafeInteger(value) && value >= 0) return value;
      if (typeof value === "string" && /^\d{1,15}$/u.test(value)) return Number(value);
    }
  }
  return undefined;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
