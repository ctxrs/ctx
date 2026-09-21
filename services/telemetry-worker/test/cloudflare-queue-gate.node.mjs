import assert from "node:assert/strict";
import test from "node:test";

import {
  QUEUE_OPERATIONAL_CONTRACT,
  readDesiredQueueInventory,
  runQueueGate,
  summarizeGraphqlMetrics,
} from "../scripts/cloudflare-queue-gate.mjs";

const NOW = Date.parse("2026-09-01T16:00:00.000Z");

function config(environment, overrides = {}) {
  const suffix = environment === "prod" ? "prod" : "staging";
  return {
    name: environment === "prod" ? "ctx-telemetry" : "ctx-telemetry-staging",
    queues: {
      producers: [{binding: "TELEMETRY_INGEST_QUEUE", queue: `ctx-telemetry-ingest-${suffix}`}],
      consumers: [{
        queue: `ctx-telemetry-ingest-${suffix}`,
        dead_letter_queue: `ctx-telemetry-ingest-${suffix}-dlq`,
        max_batch_size: 10,
        max_batch_timeout: 5,
        max_retries: 10,
        max_concurrency: 4,
        ...overrides,
      }],
    },
  };
}

function response(result, resultInfo = undefined) {
  return {
    ok: true,
    status: 200,
    async json() {
      return {success: true, result, ...(resultInfo ? {result_info: resultInfo} : {})};
    },
  };
}

function graphqlResponse(dataset, rows) {
  return {
    ok: true,
    status: 200,
    async json() {
      return {data: {viewer: {accounts: [{[dataset]: rows}]}}};
    },
  };
}

function liveFetch({
  backlog = 0,
  dlqBacklog = 0,
  operations = [{
    count: 1,
    avg: {lagTime: 0, retryCount: 0},
    dimensions: {actionType: "DeleteMessage", outcome: "success"},
  }],
  concurrency = 1,
  primaryOldestAgeSeconds = backlog > 0 ? 600 : 0,
} = {}) {
  const calls = [];
  const queues = [
    {
      queue_id: "primary-id",
      queue_name: "ctx-telemetry-ingest-staging",
      settings: {message_retention_period: 345600, delivery_paused: false},
      producers: [{script_name: "ctx-telemetry-staging"}],
    },
    {
      queue_id: "dlq-id",
      queue_name: "ctx-telemetry-ingest-staging-dlq",
      settings: {message_retention_period: 345600, delivery_paused: false},
      producers: [],
    },
  ];
  const fetchImpl = async (url, init = {}) => {
    calls.push({url, init});
    const parsed = new URL(url);
    if (parsed.pathname.endsWith("/graphql")) {
      const body = JSON.parse(init.body);
      if (body.query.includes("queueBacklogAdaptiveGroups")) {
        return graphqlResponse("queueBacklogAdaptiveGroups", [{avg: {messages: backlog, bytes: 20}}]);
      }
      if (body.query.includes("queueConsumerMetricsAdaptiveGroups")) {
        return graphqlResponse("queueConsumerMetricsAdaptiveGroups", [{avg: {concurrency}}]);
      }
      return graphqlResponse("queueMessageOperationsAdaptiveGroups", operations);
    }
    if (parsed.pathname.endsWith("/queues")) return response(queues, {total_pages: 1});
    if (parsed.pathname.endsWith("/workers/scripts")) {
      return response([{id: "ctx-telemetry-staging", handlers: ["fetch", "scheduled", "queue"]}], {total_pages: 1});
    }
    if (parsed.pathname.endsWith("/workers/scripts/ctx-telemetry-staging/settings")) {
      return response({bindings: [{
        type: "queue",
        name: "TELEMETRY_INGEST_QUEUE",
        queue_name: "ctx-telemetry-ingest-staging",
      }]});
    }
    if (parsed.pathname.endsWith("/queues/primary-id/consumers")) {
      return response([{
        script_name: "ctx-telemetry-staging",
        dead_letter_queue: "ctx-telemetry-ingest-staging-dlq",
        settings: {batch_size: 10, max_wait_time_ms: 5000, max_retries: 10, max_concurrency: 4},
      }]);
    }
    if (parsed.pathname.endsWith("/queues/dlq-id/consumers")) return response([]);
    if (parsed.pathname.endsWith("/queues/primary-id/metrics")) {
      return response({
        backlog_count: backlog,
        backlog_bytes: backlog * 20,
        oldest_message_timestamp_ms: primaryOldestAgeSeconds > 0
          ? NOW - primaryOldestAgeSeconds * 1_000
          : 0,
      });
    }
    if (parsed.pathname.endsWith("/queues/dlq-id/metrics")) {
      return response({
        backlog_count: dlqBacklog,
        backlog_bytes: dlqBacklog * 20,
        oldest_message_timestamp_ms: dlqBacklog > 0 ? NOW - 60_000 : 0,
      });
    }
    throw new Error(`unexpected request: ${url}`);
  };
  return {calls, fetchImpl};
}

test("uses Wrangler's normalized config contract and requires bounded concurrency", async () => {
  const desired = await readDesiredQueueInventory({
    environments: ["staging"],
    wranglerConfig: "candidate.toml",
    readConfig: async ({config: path, env}) => {
      assert.equal(path, "candidate.toml");
      return config(env, {max_concurrency: undefined});
    },
  });

  assert.deepEqual(desired[0].config_errors, [
    "config:staging:max_concurrency:expected-4:observed-unset",
  ]);
});

test("keeps exact operator thresholds aligned with Worker health boundaries", () => {
  assert.equal(QUEUE_OPERATIONAL_CONTRACT.max_concurrency, 4);
  assert.deepEqual(QUEUE_OPERATIONAL_CONTRACT.alert_thresholds, {
    backlog_count: 1000,
    oldest_age_seconds: 300,
    lag_milliseconds: 300_000,
    concurrency_saturation_oldest_age_seconds: 60,
  });
});

test("passes publication only with exact topology, four-day retention, handlers, and empty metrics", async () => {
  const live = liveFetch();
  const result = await runQueueGate({
    argv: ["--environment", "staging", "--phase", "publication"],
    env: {CLOUDFLARE_ACCOUNT_ID: "account", CLOUDFLARE_API_TOKEN: "token"},
    fetchImpl: live.fetchImpl,
    readConfig: async ({env}) => config(env),
    now: () => NOW,
  });

  assert.equal(result.exitCode, 0);
  assert.equal(result.receipt.ok, true);
  assert.deepEqual(result.receipt.failures, []);
  assert.deepEqual(result.receipt.alerts, []);
  assert.equal(result.receipt.samples[0].environments[0].primary.retention_seconds, 345600);
  assert(live.calls.some((call) => call.url.endsWith("/queues/primary-id/metrics")));
  assert(live.calls.filter((call) => call.url.endsWith("/graphql")).length === 3);
  assert(!JSON.stringify(result.receipt).includes("account"));
  assert(!JSON.stringify(result.receipt).includes("token"));
});

test("fails ongoing health for stalled backlog, retries, failures, saturation inputs, and DLQ", async () => {
  const live = liveFetch({
    backlog: 1_000,
    dlqBacklog: 2,
    concurrency: 0,
    operations: [
      {
        count: 3,
        avg: {lagTime: 400_000, retryCount: 2},
        dimensions: {actionType: "DeleteMessage", outcome: "fail"},
      },
      {
        count: 1,
        avg: {lagTime: 1, retryCount: 0},
        dimensions: {actionType: "DeleteMessage", outcome: "dlq"},
      },
    ],
  });
  const result = await runQueueGate({
    argv: ["--environment", "staging", "--phase", "ongoing"],
    env: {CLOUDFLARE_ACCOUNT_ID: "account", CLOUDFLARE_API_TOKEN: "token"},
    fetchImpl: live.fetchImpl,
    readConfig: async ({env}) => config(env),
    now: () => NOW,
  });

  assert.equal(result.exitCode, 1);
  assert(result.receipt.alerts.some((alert) => alert.includes("primary-backlog")));
  assert(result.receipt.alerts.some((alert) => alert.includes("primary-oldest-age")));
  assert(result.receipt.alerts.some((alert) => alert.includes("consumer-lag")));
  assert(result.receipt.alerts.some((alert) => alert.includes("retries")));
  assert(result.receipt.alerts.some((alert) => alert.includes("failed-operations")));
  assert(result.receipt.alerts.some((alert) => alert.includes("dlq-outcomes")));
  assert(result.receipt.alerts.some((alert) => alert.includes("stalled-consumer")));
});

test("keeps cap concurrency visible without alerting while the primary queue is fresh", async () => {
  const live = liveFetch({backlog: 1, concurrency: 4, primaryOldestAgeSeconds: 59});
  const result = await runQueueGate({
    argv: ["--environment", "staging", "--phase", "ongoing"],
    env: {CLOUDFLARE_ACCOUNT_ID: "account", CLOUDFLARE_API_TOKEN: "token"},
    fetchImpl: live.fetchImpl,
    readConfig: async ({env}) => config(env),
    now: () => NOW,
  });

  assert.equal(result.exitCode, 0);
  assert.equal(result.receipt.alerts.some((alert) => alert.includes("consumer-concurrency:4")), false);
  assert.equal(result.receipt.samples[0].environments[0].metrics.graphql.consumer_concurrency_max, 4);
});

test("alerts on cap concurrency only after the primary queue is at least 60 seconds old", async () => {
  const live = liveFetch({backlog: 1, concurrency: 4, primaryOldestAgeSeconds: 60});
  const result = await runQueueGate({
    argv: ["--environment", "staging", "--phase", "ongoing"],
    env: {CLOUDFLARE_ACCOUNT_ID: "account", CLOUDFLARE_API_TOKEN: "token"},
    fetchImpl: live.fetchImpl,
    readConfig: async ({env}) => config(env),
    now: () => NOW,
  });

  assert.equal(result.exitCode, 1);
  assert(result.receipt.alerts.some((alert) => alert.includes("consumer-concurrency:4")));
});

test("allows realtime backlog below 1000 without another alert signal", async () => {
  const live = liveFetch({backlog: 999, primaryOldestAgeSeconds: 20});
  const result = await runQueueGate({
    argv: ["--environment", "staging", "--phase", "ongoing"],
    env: {CLOUDFLARE_ACCOUNT_ID: "account", CLOUDFLARE_API_TOKEN: "token"},
    fetchImpl: live.fetchImpl,
    readConfig: async ({env}) => config(env),
    now: () => NOW,
  });

  assert.equal(result.exitCode, 0);
  assert.deepEqual(result.receipt.alerts, []);
});

test("alerts at realtime backlog 1000", async () => {
  const live = liveFetch({backlog: 1_000, primaryOldestAgeSeconds: 20});
  const result = await runQueueGate({
    argv: ["--environment", "staging", "--phase", "ongoing"],
    env: {CLOUDFLARE_ACCOUNT_ID: "account", CLOUDFLARE_API_TOKEN: "token"},
    fetchImpl: live.fetchImpl,
    readConfig: async ({env}) => config(env),
    now: () => NOW,
  });

  assert.equal(result.exitCode, 1);
  assert(result.receipt.alerts.some((alert) => alert.includes("primary-backlog:1000")));
});

test("never overclaims sampled drain as authorization for a Queue-less rollback", async () => {
  const live = liveFetch();
  const result = await runQueueGate({
    argv: [
      "--environment", "staging", "--phase", "rollback",
      "--rollback-samples", "3", "--sample-interval-seconds", "0",
    ],
    env: {CLOUDFLARE_ACCOUNT_ID: "account", CLOUDFLARE_API_TOKEN: "token"},
    fetchImpl: live.fetchImpl,
    readConfig: async ({env}) => config(env),
    now: () => NOW,
    sleep: async () => {},
  });

  assert.equal(result.exitCode, 1);
  assert.equal(result.receipt.samples.length, 3);
  assert(result.receipt.failures.includes(
    "queue-less-rollback-requires-a-separately-reviewed-live-producer-fence",
  ));
  assert.equal(live.calls.filter((call) => call.url.endsWith("/queues/primary-id/metrics")).length, 3);
});

test("summarizes the exact GraphQL Queue fields used for alerting", () => {
  assert.deepEqual(summarizeGraphqlMetrics({
    backlogRows: [{avg: {messages: 2, bytes: 30}}],
    concurrencyRows: [{avg: {concurrency: 4}}],
    operationRows: [
      {
        count: 2,
        avg: {lagTime: 7, retryCount: 3},
        dimensions: {actionType: "DeleteMessage", outcome: "fail"},
      },
      {
        count: 1,
        avg: {lagTime: 5, retryCount: 1},
        dimensions: {actionType: "DeleteMessage", outcome: "dlq"},
      },
    ],
  }), {
    backlog_average_messages: 2,
    backlog_average_bytes: 30,
    consumer_concurrency_max: 4,
    lag_milliseconds_max: 7,
    retry_count_max: 3,
    failed_operations: 2,
    dlq_outcomes: 1,
  });
});

test("accepts live-shaped empty write/read outcomes and their null equivalent", () => {
  const summary = summarizeGraphqlMetrics({
    backlogRows: [{avg: {messages: 0, bytes: 0}}],
    concurrencyRows: [{avg: {concurrency: 1}}],
    operationRows: [
      {
        count: 1,
        avg: {lagTime: 0, retryCount: 0},
        dimensions: {actionType: "WriteMessage", outcome: ""},
      },
      {
        count: 1,
        avg: {lagTime: 0, retryCount: 0},
        dimensions: {actionType: "ReadMessage", outcome: ""},
      },
      {
        count: 1,
        avg: {lagTime: 0, retryCount: 0},
        dimensions: {actionType: "WriteMessage", outcome: null},
      },
      {
        count: 1,
        avg: {lagTime: 0, retryCount: 0},
        dimensions: {actionType: "ReadMessage", outcome: null},
      },
      {
        count: 1,
        avg: {lagTime: 0, retryCount: 0},
        dimensions: {actionType: "DeleteMessage", outcome: "success"},
      },
    ],
  });

  assert.equal(summary.failed_operations, 0);
  assert.equal(summary.dlq_outcomes, 0);
});

test("rejects invalid or missing delete outcomes", () => {
  for (const dimensions of [
    {actionType: "DeleteMessage", outcome: ""},
    {actionType: "DeleteMessage", outcome: null},
    {actionType: "DeleteMessage"},
  ]) {
    assert.throws(
      () => summarizeGraphqlMetrics({
        backlogRows: [{avg: {messages: 0, bytes: 0}}],
        concurrencyRows: [{avg: {concurrency: 1}}],
        operationRows: [{
          count: 1,
          avg: {lagTime: 0, retryCount: 0},
          dimensions,
        }],
      }),
      /operation dimensions are invalid/u,
    );
  }
});

test("fails closed for missing metric samples", () => {
  assert.throws(
    () => summarizeGraphqlMetrics({
      backlogRows: [],
      concurrencyRows: [{avg: {concurrency: 1}}],
      operationRows: [{
        count: 1,
        avg: {lagTime: 0, retryCount: 0},
        dimensions: {actionType: "DeleteMessage", outcome: "success"},
      }],
    }),
    /no samples/u,
  );
});
