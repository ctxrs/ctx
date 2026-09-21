import { expect, test, vi } from "vitest";

import {
  clientHealthIsHealthy,
  ingestHealthIsHealthy,
  queueHealthFailureDiagnostic,
  queueHealthIsHealthy,
  readTelemetryQueueHealth,
  type TelemetryQueueHealthFailureDiagnostic,
  type TelemetryQueueHealthSnapshot,
} from "../src/queue-health";

const NOW = Date.parse("2026-09-01T16:00:00.000Z");
const ENV = {
  TELEMETRY_ANALYTICS_ENVIRONMENT: "production",
  TELEMETRY_CLOUDFLARE_ACCOUNT_ID: "a".repeat(32),
  TELEMETRY_QUEUE_HEALTH_API_TOKEN: "sensitive-token",
};
const HEALTHY: TelemetryQueueHealthSnapshot = {
  primaryBacklog: 0,
  primaryOldestAgeSeconds: 0,
  dlqBacklog: 0,
  lagMillisecondsMax: 0,
  retryCountMax: 0,
  failedOperations: 0,
  dlqOutcomes: 0,
  consumerConcurrencyMax: 0,
};
const SENSITIVE_VALUES = [
  "Authorization: Bearer test-token",
  "test-token",
  "raw-response-body-canary",
  "event-payload-canary",
  "203.0.113.42",
] as const;
const SENSITIVE_RESPONSE = SENSITIVE_VALUES.join(" ");

type DiagnosticCase = Readonly<{
  name: string;
  expected: TelemetryQueueHealthFailureDiagnostic;
  intercept: DiagnosticInterceptor;
}>;

type DiagnosticInterceptor = (
  url: URL,
  init?: RequestInit,
) => Response | Error | undefined;

const BELOW_SNAPSHOT_THRESHOLDS = {
  compatibilityRejectionMax: 99n,
  eventCollisionCount: 0n,
  otherRejectionCount: 4n,
  providerRefreshFailureCount: 4n,
  deliveryDegradedCount: 4n,
  deliveryDroppedCount: 0n,
};

test("keeps both snapshot components healthy immediately below the original thresholds", () => {
  expect(ingestHealthIsHealthy(BELOW_SNAPSHOT_THRESHOLDS)).toBe(true);
  expect(clientHealthIsHealthy(BELOW_SNAPSHOT_THRESHOLDS)).toBe(true);
});

test.each([
  ["compatibility", { compatibilityRejectionMax: 100n }, false, true],
  ["collision", { eventCollisionCount: 1n }, false, true],
  ["other rejection", { otherRejectionCount: 5n }, false, true],
  ["refresh", { providerRefreshFailureCount: 5n }, true, false],
  ["delivery backlog", { deliveryDegradedCount: 5n }, true, false],
  ["delivery drop", { deliveryDroppedCount: 1n }, true, false],
] as const)("preserves the %s threshold in its independent component", (
  _name, change, ingestionHealthy, clientHealthy,
) => {
  const snapshot = { ...BELOW_SNAPSHOT_THRESHOLDS, ...change };
  expect(ingestHealthIsHealthy(snapshot)).toBe(ingestionHealthy);
  expect(clientHealthIsHealthy(snapshot)).toBe(clientHealthy);
});

test.each([
  ["realtime backlog", {primaryBacklog: 1_000, consumerConcurrencyMax: 1}],
  ["oldest primary age", {primaryOldestAgeSeconds: 300}],
  ["DLQ backlog", {dlqBacklog: 1}],
  ["consumer lag", {lagMillisecondsMax: 300_000}],
  ["retry count", {retryCountMax: 1}],
  ["failed operations", {failedOperations: 1}],
  ["DLQ outcomes", {dlqOutcomes: 1}],
] as const)("matches the operator gate's exact unhealthy threshold for %s", (_name, change) => {
  expect(queueHealthIsHealthy({ ...HEALTHY, ...change })).toBe(false);
});

test("treats concurrency at the cap with a fresh primary queue as healthy", () => {
  expect(queueHealthIsHealthy({
    ...HEALTHY,
    primaryBacklog: 1,
    consumerConcurrencyMax: 4,
    primaryOldestAgeSeconds: 59,
  })).toBe(true);
});

test("alerts on sustained consumer-concurrency saturation", () => {
  expect(queueHealthIsHealthy({
    ...HEALTHY,
    primaryBacklog: 1,
    consumerConcurrencyMax: 4,
    primaryOldestAgeSeconds: 60,
  })).toBe(false);
});

test("allows realtime backlog below 1000 without another signal", () => {
  expect(queueHealthIsHealthy({
    ...HEALTHY,
    primaryBacklog: 999,
    consumerConcurrencyMax: 1,
  })).toBe(true);
});

test.each([
  ["positive backlog", {primaryBacklog: 1}, false],
  ["empty backlog", {primaryBacklog: 0}, true],
] as const)("handles zero consumer concurrency with %s", (_name, change, expected) => {
  expect(queueHealthIsHealthy({
    ...HEALTHY,
    ...change,
    consumerConcurrencyMax: 0,
  })).toBe(expected);
});

test("reads the exact live Queue metrics used by the health alert", async () => {
  const timeout = vi.spyOn(AbortSignal, "timeout");
  const signals: AbortSignal[] = [];
  const fetchImpl = vi.fn(async function (
    this: unknown,
    input: string | URL | Request,
    init?: RequestInit,
  ) {
    expect(this).toBeUndefined();
    const url = new URL(typeof input === "string" ? input : input.toString());
    expect(init?.headers).toMatchObject({ Authorization: `Bearer ${ENV.TELEMETRY_QUEUE_HEALTH_API_TOKEN}` });
    if (init?.signal) signals.push(init.signal);
    if (url.pathname.endsWith("/queues")) {
      return cloudflareResponse([
        {queue_id: "primary-id", queue_name: "ctx-telemetry-ingest-prod"},
        {queue_id: "dlq-id", queue_name: "ctx-telemetry-ingest-prod-dlq"},
      ], {total_pages: 1});
    }
    if (url.pathname.endsWith("/queues/primary-id/metrics")) {
      return cloudflareResponse({
        backlog_count: 0,
        oldest_message_timestamp_ms: 0,
      });
    }
    if (url.pathname.endsWith("/queues/dlq-id/metrics")) {
      return cloudflareResponse({
        backlog_count: 0,
        oldest_message_timestamp_ms: 0,
      });
    }
    if (url.pathname.endsWith("/graphql")) {
      const body = JSON.parse(String(init?.body)) as {query: string};
      const dataset = body.query.includes("queueConsumerMetricsAdaptiveGroups")
        ? "queueConsumerMetricsAdaptiveGroups"
        : "queueMessageOperationsAdaptiveGroups";
      const rows = dataset === "queueConsumerMetricsAdaptiveGroups"
        ? [{avg: {concurrency: 1}}]
        : [
            {
              count: 1,
              avg: {lagTime: 2, retryCount: 0},
              dimensions: {actionType: "WriteMessage", outcome: ""},
            },
            {
              count: 1,
              avg: {lagTime: 2, retryCount: 0},
              dimensions: {actionType: "ReadMessage", outcome: ""},
            },
            {
              count: 1,
              avg: {lagTime: 2, retryCount: 0},
              dimensions: {actionType: "WriteMessage", outcome: null},
            },
            {
              count: 1,
              avg: {lagTime: 2, retryCount: 0},
              dimensions: {actionType: "ReadMessage", outcome: null},
            },
            {
              count: 1,
              avg: {lagTime: 2, retryCount: 0},
              dimensions: {actionType: "DeleteMessage", outcome: "success"},
            },
          ];
      return new Response(JSON.stringify({
        data: {viewer: {accounts: [{[dataset]: rows}]}},
      }), {status: 200});
    }
    throw new Error(`unexpected ${url.pathname}`);
  });

  try {
    await expect(readTelemetryQueueHealth(ENV, fetchImpl as typeof fetch, NOW)).resolves.toBe(true);
    expect(timeout).toHaveBeenCalledWith(4_500);
    expect(fetchImpl).toHaveBeenCalledTimes(5);
    expect(signals).toHaveLength(5);
    expect(signals.every((signal) => signal === signals[0] && !signal.aborted)).toBe(true);
  } finally {
    timeout.mockRestore();
  }
});

test.each([10, 11])("bounds external reads for an inventory reporting %s pages", async (totalPages) => {
  const metricFetch = diagnosticFetch(() => undefined);
  const fetchImpl = vi.fn(async (input: string | URL | Request, init?: RequestInit) => {
    const url = new URL(String(input));
    if (!url.pathname.endsWith("/queues")) return metricFetch(input, init);
    const page = Number(url.searchParams.get("page"));
    return cloudflareResponse(page === totalPages ? [
      { queue_id: "primary-id", queue_name: "ctx-telemetry-ingest-prod" },
      { queue_id: "dlq-id", queue_name: "ctx-telemetry-ingest-prod-dlq" },
    ] : [{ queue_id: `unrelated-${page}`, queue_name: `unrelated-${page}` }], { total_pages: totalPages });
  });
  const pending = readTelemetryQueueHealth(ENV, fetchImpl as typeof fetch, NOW);
  if (totalPages === 10) {
    await expect(pending).resolves.toBe(true);
    expect(fetchImpl).toHaveBeenCalledTimes(14);
    expect(metricFetch).toHaveBeenCalledTimes(4);
  } else {
    await expect(pending).rejects.toThrow("telemetry_queue_health_inventory_too_large");
    expect(fetchImpl).toHaveBeenCalledTimes(10);
    expect(metricFetch).not.toHaveBeenCalled();
  }
});

test.each([
  {
    name: "inventory upstream",
    expected: {
      stage: "inventory",
      kind: "upstream",
      http_status: 403,
      cloudflare_error_code: 10_000,
    },
    intercept: (url: URL) => url.pathname.endsWith("/queues")
      ? cloudflareFailureResponse(403, 10_000)
      : undefined,
  },
  {
    name: "primary metrics parse",
    expected: {stage: "primary_metrics", kind: "parse"},
    intercept: (url: URL) => url.pathname.endsWith("/queues/primary-id/metrics")
      ? cloudflareResponse({backlog_count: SENSITIVE_RESPONSE, oldest_message_timestamp_ms: 0})
      : undefined,
  },
  {
    name: "DLQ metrics upstream",
    expected: {
      stage: "dlq_metrics",
      kind: "upstream",
      http_status: 429,
      cloudflare_error_code: 1_015,
    },
    intercept: (url: URL) => url.pathname.endsWith("/queues/dlq-id/metrics")
      ? cloudflareFailureResponse(429, 1_015)
      : undefined,
  },
  {
    name: "concurrency GraphQL parse",
    expected: {stage: "concurrency_graphql", kind: "parse"},
    intercept: (_url: URL, init?: RequestInit) => graphqlDataset(init)
      === "queueConsumerMetricsAdaptiveGroups"
      ? new Response(SENSITIVE_RESPONSE, {status: 200})
      : undefined,
  },
  {
    name: "operations GraphQL upstream",
    expected: {
      stage: "operations_graphql",
      kind: "upstream",
      http_status: 200,
      cloudflare_error_code: 10_001,
    },
    intercept: (_url: URL, init?: RequestInit) => graphqlDataset(init)
      === "queueMessageOperationsAdaptiveGroups"
      ? new Response(JSON.stringify({
          errors: [{
            extensions: {code: "10001"},
            message: SENSITIVE_RESPONSE,
          }],
        }), {status: 200})
      : undefined,
  },
] satisfies readonly DiagnosticCase[])("classifies $name without retaining sensitive data", async ({
  expected,
  intercept,
}) => {
  const failure = await readTelemetryQueueHealth(
    ENV,
    diagnosticFetch(intercept),
    NOW,
  ).then(() => null, (error: unknown) => error);

  expect(failure).toBeInstanceOf(Error);
  const diagnostic = queueHealthFailureDiagnostic(failure);
  expect(diagnostic).toEqual(expected);
  for (const sensitive of [...SENSITIVE_VALUES, ENV.TELEMETRY_QUEUE_HEALTH_API_TOKEN]) {
    expect(JSON.stringify(diagnostic)).not.toContain(sensitive);
  }
});

test("classifies the shared deadline without retaining the thrown error", async () => {
  const timeout = Object.assign(new Error(SENSITIVE_RESPONSE), {name: "TimeoutError"});
  const failure = await readTelemetryQueueHealth(
    ENV,
    diagnosticFetch((url) => {
      if (url.pathname.endsWith("/queues")) return timeout;
      return undefined;
    }),
    NOW,
  ).then(() => null, (error: unknown) => error);

  expect(queueHealthFailureDiagnostic(failure)).toEqual({
    stage: "inventory",
    kind: "timeout",
  });
  for (const sensitive of SENSITIVE_VALUES) {
    expect(JSON.stringify(queueHealthFailureDiagnostic(failure))).not.toContain(sensitive);
  }
});

test("sanitizes unexpected Queue health failures", () => {
  const diagnostic = queueHealthFailureDiagnostic(new Error(SENSITIVE_RESPONSE));
  expect(diagnostic).toEqual({stage: "unknown", kind: "unknown"});
  for (const sensitive of SENSITIVE_VALUES) {
    expect(JSON.stringify(diagnostic)).not.toContain(sensitive);
  }
});

test.each([
  ["empty concurrency", [], [{
    count: 1,
    avg: {lagTime: 2, retryCount: 0},
    dimensions: {actionType: "DeleteMessage", outcome: "success"},
  }]],
  ["empty operations", [{avg: {concurrency: 1}}], []],
  ["malformed operations", [{avg: {concurrency: 1}}], [{
    count: "one",
    avg: {lagTime: 2, retryCount: 0},
    dimensions: {actionType: "DeleteMessage", outcome: "success"},
  }]],
  ["empty delete outcome", [{avg: {concurrency: 1}}], [{
    count: 1,
    avg: {lagTime: 2, retryCount: 0},
    dimensions: {actionType: "DeleteMessage", outcome: ""},
  }]],
  ["null delete outcome", [{avg: {concurrency: 1}}], [{
    count: 1,
    avg: {lagTime: 2, retryCount: 0},
    dimensions: {actionType: "DeleteMessage", outcome: null},
  }]],
  ["missing delete outcome", [{avg: {concurrency: 1}}], [{
    count: 1,
    avg: {lagTime: 2, retryCount: 0},
    dimensions: {actionType: "DeleteMessage"},
  }]],
])("fails closed for %s metric authority", async (_name, concurrencyRows, operationRows) => {
  const fetchImpl = vi.fn(async (input: string | URL | Request, init?: RequestInit) => {
    const url = new URL(typeof input === "string" ? input : input.toString());
    if (url.pathname.endsWith("/queues")) {
      return cloudflareResponse([
        {queue_id: "primary-id", queue_name: "ctx-telemetry-ingest-prod"},
        {queue_id: "dlq-id", queue_name: "ctx-telemetry-ingest-prod-dlq"},
      ], {total_pages: 1});
    }
    if (url.pathname.endsWith("/metrics")) {
      return cloudflareResponse({backlog_count: 0, oldest_message_timestamp_ms: 0});
    }
    const body = JSON.parse(String(init?.body)) as {query: string};
    const dataset = body.query.includes("queueConsumerMetricsAdaptiveGroups")
      ? "queueConsumerMetricsAdaptiveGroups"
      : "queueMessageOperationsAdaptiveGroups";
    return new Response(JSON.stringify({
      data: {viewer: {accounts: [{
        [dataset]: dataset === "queueConsumerMetricsAdaptiveGroups"
          ? concurrencyRows
          : operationRows,
      }]}},
    }), {status: 200});
  });

  await expect(readTelemetryQueueHealth(ENV, fetchImpl as typeof fetch, NOW)).rejects.toThrow(
    /telemetry_queue_health_/u,
  );
});

test("fails closed for missing authority and malformed live inventory", async () => {
  await expect(readTelemetryQueueHealth({
    TELEMETRY_ANALYTICS_ENVIRONMENT: "production",
  })).rejects.toThrow("telemetry_queue_health_not_configured");
  await expect(readTelemetryQueueHealth(
    ENV,
    vi.fn(async () => cloudflareResponse([])) as typeof fetch,
    NOW,
  )).rejects.toThrow("telemetry_queue_health_topology_mismatch");
});

function cloudflareResponse(result: unknown, resultInfo?: unknown): Response {
  return new Response(JSON.stringify({success: true, result, result_info: resultInfo}), {
    headers: {"content-type": "application/json"},
    status: 200,
  });
}

function diagnosticFetch(intercept: DiagnosticInterceptor): typeof fetch {
  return vi.fn(async (input: string | URL | Request, init?: RequestInit) => {
    const url = new URL(typeof input === "string" ? input : input.toString());
    const intercepted = intercept(url, init);
    if (intercepted instanceof Error) throw intercepted;
    if (intercepted) return intercepted;
    if (url.pathname.endsWith("/queues")) {
      return cloudflareResponse([
        {queue_id: "primary-id", queue_name: "ctx-telemetry-ingest-prod"},
        {queue_id: "dlq-id", queue_name: "ctx-telemetry-ingest-prod-dlq"},
      ], {total_pages: 1});
    }
    if (url.pathname.endsWith("/metrics")) {
      return cloudflareResponse({backlog_count: 0, oldest_message_timestamp_ms: 0});
    }
    const dataset = graphqlDataset(init);
    const rows = dataset === "queueConsumerMetricsAdaptiveGroups"
      ? [{avg: {concurrency: 1}}]
      : [{
          count: 1,
          avg: {lagTime: 2, retryCount: 0},
          dimensions: {actionType: "DeleteMessage", outcome: "success"},
        }];
    return new Response(JSON.stringify({
      data: {viewer: {accounts: [{[dataset]: rows}]}},
    }), {status: 200});
  }) as unknown as typeof fetch;
}

function graphqlDataset(init?: RequestInit): string {
  if (!init?.body) return "";
  const body = JSON.parse(String(init.body)) as {query?: unknown};
  if (typeof body.query !== "string") return "";
  return body.query.includes("queueConsumerMetricsAdaptiveGroups")
    ? "queueConsumerMetricsAdaptiveGroups"
    : "queueMessageOperationsAdaptiveGroups";
}

function cloudflareFailureResponse(status: number, code: number): Response {
  return new Response(JSON.stringify({
    success: false,
    errors: [{code, message: SENSITIVE_RESPONSE}],
    raw: SENSITIVE_RESPONSE,
  }), {status});
}
