#!/usr/bin/env node

const CLOUDFLARE_API = "https://api.cloudflare.com/client/v4";
const CLOUDFLARE_GRAPHQL = `${CLOUDFLARE_API}/graphql`;

export const QUEUE_OPERATIONAL_CONTRACT = Object.freeze({
  message_retention_seconds: 4 * 24 * 60 * 60,
  max_batch_size: 10,
  max_batch_timeout_seconds: 5,
  max_retries: 10,
  max_concurrency: 4,
  alert_thresholds: Object.freeze({
    backlog_count: 1000,
    oldest_age_seconds: 300,
    lag_milliseconds: 300_000,
    concurrency_saturation_oldest_age_seconds: 60,
  }),
});

const GRAPHQL_QUERIES = Object.freeze({
  backlog: `query QueueBacklog($accountTag: String!, $queueId: String!, $start: Time!, $end: Time!) {
    viewer {
      accounts(filter: {accountTag: $accountTag}) {
        queueBacklogAdaptiveGroups(
          limit: 1000
          filter: {queueId: $queueId, datetime_geq: $start, datetime_leq: $end}
        ) {
          avg { bytes messages }
          dimensions { datetimeMinute queueId }
        }
      }
    }
  }`,
  concurrency: `query QueueConcurrency($accountTag: String!, $queueId: String!, $start: Time!, $end: Time!) {
    viewer {
      accounts(filter: {accountTag: $accountTag}) {
        queueConsumerMetricsAdaptiveGroups(
          limit: 1000
          filter: {queueId: $queueId, datetime_geq: $start, datetime_leq: $end}
        ) {
          avg { concurrency }
          dimensions { datetimeMinute queueId }
        }
      }
    }
  }`,
  operations: `query QueueOperations($accountTag: String!, $queueId: String!, $start: Date!, $end: Date!) {
    viewer {
      accounts(filter: {accountTag: $accountTag}) {
        queueMessageOperationsAdaptiveGroups(
          limit: 1000
          filter: {queueId: $queueId, datetime_geq: $start, datetime_leq: $end}
        ) {
          count
          avg { lagTime retryCount }
          dimensions { actionType consumerType datetimeMinute outcome queueId }
        }
      }
    }
  }`,
});

function usage() {
  return `Usage: node scripts/cloudflare-queue-gate.mjs [options]

Read-only Cloudflare Queue publication, ongoing-health, and rollback gate.

Options:
  --environment staging|prod  Environment to check; repeatable (default: both)
  --phase publication|ongoing|rollback
                               Gate mode (default: publication)
  --wrangler-config PATH       Wrangler configuration (default: wrangler.toml)
  --window-minutes N           GraphQL lookback (default: 15)
  --rollback-samples N         Zero-backlog samples in rollback mode (default: 3)
  --sample-interval-seconds N  Delay between rollback samples (default: 10)
  --json                       Emit one redacted JSON receipt (default)
  --help                       Show this help

Required environment:
  CLOUDFLARE_ACCOUNT_ID
  CLOUDFLARE_API_TOKEN
`;
}

export function parseQueueGateArgs(argv) {
  const options = {
    environments: [],
    phase: "publication",
    wranglerConfig: "wrangler.toml",
    windowMinutes: 15,
    rollbackSamples: 3,
    sampleIntervalSeconds: 10,
    help: false,
  };

  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    const value = () => {
      index += 1;
      if (index >= argv.length) throw new Error(`${argument} requires a value`);
      return argv[index];
    };
    if (argument === "--environment") options.environments.push(value());
    else if (argument === "--phase") options.phase = value();
    else if (argument === "--wrangler-config") options.wranglerConfig = value();
    else if (argument === "--window-minutes") options.windowMinutes = Number(value());
    else if (argument === "--rollback-samples") options.rollbackSamples = Number(value());
    else if (argument === "--sample-interval-seconds") options.sampleIntervalSeconds = Number(value());
    else if (argument === "--json") continue;
    else if (argument === "--help" || argument === "-h") options.help = true;
    else throw new Error(`unknown argument: ${argument}`);
  }

  if (options.environments.length === 0) options.environments = ["staging", "prod"];
  const uniqueEnvironments = [...new Set(options.environments)];
  if (uniqueEnvironments.some((environment) => !["staging", "prod"].includes(environment))) {
    throw new Error("--environment must be staging or prod");
  }
  options.environments = uniqueEnvironments;
  if (!["publication", "ongoing", "rollback"].includes(options.phase)) {
    throw new Error("--phase must be publication, ongoing, or rollback");
  }
  for (const [name, number] of [
    ["--window-minutes", options.windowMinutes],
    ["--rollback-samples", options.rollbackSamples],
    ["--sample-interval-seconds", options.sampleIntervalSeconds],
  ]) {
    if (!Number.isInteger(number) || number < (name === "--sample-interval-seconds" ? 0 : 1)) {
      throw new Error(`${name} must be an integer in range`);
    }
  }
  return options;
}

async function defaultReadConfig({config, env}) {
  const {unstable_readConfig: readConfig} = await import("wrangler");
  return readConfig({config, env});
}

function desiredFromConfig(environment, config) {
  const producers = config.queues?.producers ?? [];
  const consumers = config.queues?.consumers ?? [];
  const errors = [];
  if (producers.length !== 1) errors.push(`config:${environment}:expected-one-producer`);
  if (consumers.length !== 1) errors.push(`config:${environment}:expected-one-consumer`);
  const producer = producers[0] ?? {};
  const consumer = consumers[0] ?? {};
  if (producer.queue && consumer.queue && producer.queue !== consumer.queue) {
    errors.push(`config:${environment}:producer-consumer-queue-mismatch`);
  }
  if (producer.binding !== "TELEMETRY_INGEST_QUEUE") {
    errors.push(`config:${environment}:producer-binding-mismatch`);
  }
  for (const [field, expected] of [
    ["max_batch_size", QUEUE_OPERATIONAL_CONTRACT.max_batch_size],
    ["max_batch_timeout", QUEUE_OPERATIONAL_CONTRACT.max_batch_timeout_seconds],
    ["max_retries", QUEUE_OPERATIONAL_CONTRACT.max_retries],
    ["max_concurrency", QUEUE_OPERATIONAL_CONTRACT.max_concurrency],
  ]) {
    if (consumer[field] !== expected) {
      errors.push(`config:${environment}:${field}:expected-${expected}:observed-${consumer[field] ?? "unset"}`);
    }
  }
  if (!config.name) errors.push(`config:${environment}:worker-name-missing`);
  if (!producer.queue) errors.push(`config:${environment}:primary-queue-missing`);
  if (!consumer.dead_letter_queue) errors.push(`config:${environment}:dlq-missing`);

  return {
    environment,
    worker_name: config.name ?? null,
    primary_queue: producer.queue ?? consumer.queue ?? null,
    dlq: consumer.dead_letter_queue ?? null,
    producer_binding: producer.binding ?? null,
    consumer: {
      max_batch_size: consumer.max_batch_size ?? null,
      max_batch_timeout_seconds: consumer.max_batch_timeout ?? null,
      max_retries: consumer.max_retries ?? null,
      max_concurrency: consumer.max_concurrency ?? null,
      retry_delay_seconds: consumer.retry_delay ?? null,
    },
    config_errors: errors,
  };
}

export async function readDesiredQueueInventory({
  environments,
  wranglerConfig,
  readConfig = defaultReadConfig,
}) {
  return Promise.all(environments.map(async (environment) => desiredFromConfig(
    environment,
    await readConfig({config: wranglerConfig, env: environment}),
  )));
}

function cloudflareEnvelope(payload, endpoint) {
  if (!payload || payload.success !== true || payload.result === undefined) {
    const detail = payload?.errors?.map((error) => error.message).filter(Boolean).join("; ");
    throw new Error(`Cloudflare rejected ${endpoint}${detail ? `: ${detail}` : ""}`);
  }
  return payload;
}

async function cloudflareRequest({fetchImpl, token, endpoint, method = "GET", body}) {
  const response = await fetchImpl(`${CLOUDFLARE_API}${endpoint}`, {
    method,
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
    },
    ...(body === undefined ? {} : {body: JSON.stringify(body)}),
  });
  let payload;
  try {
    payload = await response.json();
  } catch {
    throw new Error(`Cloudflare ${endpoint} returned non-JSON HTTP ${response.status}`);
  }
  if (!response.ok) {
    const detail = payload?.errors?.map((error) => error.message).filter(Boolean).join("; ");
    throw new Error(`Cloudflare ${endpoint} returned HTTP ${response.status}${detail ? `: ${detail}` : ""}`);
  }
  return cloudflareEnvelope(payload, endpoint);
}

async function collectPages(context, endpoint) {
  const collected = [];
  for (let page = 1; page <= 1000; page += 1) {
    const separator = endpoint.includes("?") ? "&" : "?";
    const payload = await cloudflareRequest({
      ...context,
      endpoint: `${endpoint}${separator}per_page=100&page=${page}`,
    });
    if (!Array.isArray(payload.result)) throw new Error(`Cloudflare ${endpoint} did not return an array`);
    collected.push(...payload.result);
    const totalPages = Number(payload.result_info?.total_pages ?? 1);
    if (page >= totalPages || payload.result.length === 0) return collected;
  }
  throw new Error(`Cloudflare pagination exceeded 1000 pages for ${endpoint}`);
}

async function graphqlRows({fetchImpl, token, accountId, query, dataset, queueId, start, end}) {
  const response = await fetchImpl(CLOUDFLARE_GRAPHQL, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({
      query,
      variables: {accountTag: accountId, queueId, start, end},
    }),
  });
  const payload = await response.json();
  if (!response.ok || payload.errors?.length) {
    const detail = payload.errors?.map((error) => error.message).join("; ");
    throw new Error(`Cloudflare GraphQL ${dataset} failed${detail ? `: ${detail}` : ""}`);
  }
  const accounts = payload.data?.viewer?.accounts;
  if (!Array.isArray(accounts) || accounts.length !== 1) {
    throw new Error(`Cloudflare GraphQL ${dataset} returned an unexpected account result`);
  }
  const rows = accounts[0]?.[dataset];
  if (
    !Array.isArray(rows)
    || rows.length === 0
    || rows.some((row) => !row || typeof row !== "object" || Array.isArray(row))
  ) throw new Error(`Cloudflare GraphQL ${dataset} did not return complete rows`);
  return rows;
}

function queueByName(queues, name) {
  return queues.filter((queue) => queue.queue_name === name);
}

function oldestAgeSeconds(metrics, nowMs) {
  const timestamp = Number(metrics?.oldest_message_timestamp_ms ?? 0);
  return timestamp > 0 ? Math.max(0, Math.round((nowMs - timestamp) / 1000)) : 0;
}

function finiteNonnegative(value, label) {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
    throw new Error(`Cloudflare GraphQL metric is invalid: ${label}`);
  }
  return value;
}

export function summarizeGraphqlMetrics({backlogRows, concurrencyRows, operationRows}) {
  if (backlogRows.length === 0 || concurrencyRows.length === 0 || operationRows.length === 0) {
    throw new Error("Cloudflare GraphQL metric authority returned no samples");
  }
  const backlogMessages = backlogRows.map((row) => finiteNonnegative(
    row?.avg?.messages,
    "backlog.messages",
  ));
  const backlogBytes = backlogRows.map((row) => finiteNonnegative(
    row?.avg?.bytes,
    "backlog.bytes",
  ));
  const concurrency = concurrencyRows.map((row) => finiteNonnegative(
    row?.avg?.concurrency,
    "consumer.concurrency",
  ));
  const lag = [];
  const retries = [];
  let failedOperations = 0;
  let dlqOutcomes = 0;
  for (const row of operationRows) {
    if (!Number.isSafeInteger(row?.count) || row.count < 0) {
      throw new Error("Cloudflare GraphQL operation count is invalid");
    }
    if (!row.avg || typeof row.avg !== "object" || !row.dimensions || typeof row.dimensions !== "object") {
      throw new Error("Cloudflare GraphQL operation row is invalid");
    }
    if (row.avg.lagTime !== null) {
      lag.push(finiteNonnegative(row.avg.lagTime, "operation.lagTime"));
    }
    if (row.avg.retryCount !== null) {
      retries.push(finiteNonnegative(row.avg.retryCount, "operation.retryCount"));
    }
    const {actionType, outcome} = row.dimensions;
    const validOutcome = actionType === "DeleteMessage"
      ? ["success", "dlq", "fail"].includes(outcome)
      : (actionType === "WriteMessage" || actionType === "ReadMessage")
        && (outcome === null || outcome === "");
    if (
      !["WriteMessage", "ReadMessage", "DeleteMessage"].includes(actionType)
      || !validOutcome
    ) throw new Error("Cloudflare GraphQL operation dimensions are invalid");
    if (actionType === "DeleteMessage" && outcome === "fail") failedOperations += row.count;
    if (actionType === "DeleteMessage" && outcome === "dlq") dlqOutcomes += row.count;
  }
  if (lag.length === 0 || retries.length === 0) {
    throw new Error("Cloudflare GraphQL operation averages are missing");
  }
  return {
    backlog_average_messages: Math.max(...backlogMessages),
    backlog_average_bytes: Math.max(...backlogBytes),
    consumer_concurrency_max: Math.max(...concurrency),
    lag_milliseconds_max: Math.max(...lag),
    retry_count_max: Math.max(...retries),
    failed_operations: failedOperations,
    dlq_outcomes: dlqOutcomes,
  };
}

function bindingMatches(binding, desired) {
  return binding.type === "queue"
    && binding.name === desired.producer_binding
    && binding.queue_name === desired.primary_queue;
}

function consumerScriptName(consumer) {
  return consumer.script_name ?? consumer.script ?? consumer.service ?? null;
}

function queueProducerNames(queue) {
  return (queue.producers ?? [])
    .map((producer) => producer.script_name ?? producer.script ?? producer.service ?? null)
    .filter(Boolean);
}

function compareConsumer(desired, consumers, failures) {
  const attached = consumers.filter((consumer) => consumerScriptName(consumer) === desired.worker_name);
  if (attached.length !== 1) {
    failures.push(`${desired.environment}:consumer-attachment-count:${attached.length}`);
    return null;
  }
  const consumer = attached[0];
  const settings = consumer.settings ?? {};
  const checks = [
    ["dead-letter-queue", consumer.dead_letter_queue, desired.dlq],
    ["batch-size", settings.batch_size, QUEUE_OPERATIONAL_CONTRACT.max_batch_size],
    ["batch-wait-ms", settings.max_wait_time_ms, QUEUE_OPERATIONAL_CONTRACT.max_batch_timeout_seconds * 1000],
    ["retries", settings.max_retries, QUEUE_OPERATIONAL_CONTRACT.max_retries],
    ["concurrency", settings.max_concurrency, QUEUE_OPERATIONAL_CONTRACT.max_concurrency],
  ];
  for (const [label, observed, expected] of checks) {
    if (observed !== expected) failures.push(`${desired.environment}:consumer-${label}:expected-${expected}:observed-${observed ?? "unset"}`);
  }
  return {
    script_name: consumerScriptName(consumer),
    dead_letter_queue: consumer.dead_letter_queue ?? null,
    settings: {
      batch_size: settings.batch_size ?? null,
      max_wait_time_ms: settings.max_wait_time_ms ?? null,
      max_retries: settings.max_retries ?? null,
      max_concurrency: settings.max_concurrency ?? null,
      retry_delay_seconds: settings.retry_delay ?? null,
    },
  };
}

function addMetricAlerts({desired, primaryMetrics, dlqMetrics, graphql, alerts}) {
  const thresholds = QUEUE_OPERATIONAL_CONTRACT.alert_thresholds;
  if (primaryMetrics.backlog_count >= thresholds.backlog_count) {
    alerts.push(`${desired.environment}:primary-backlog:${primaryMetrics.backlog_count}`);
  }
  if (primaryMetrics.oldest_age_seconds >= thresholds.oldest_age_seconds) {
    alerts.push(`${desired.environment}:primary-oldest-age-seconds:${primaryMetrics.oldest_age_seconds}`);
  }
  if (dlqMetrics.backlog_count > 0) alerts.push(`${desired.environment}:dlq-backlog:${dlqMetrics.backlog_count}`);
  if (graphql.lag_milliseconds_max >= thresholds.lag_milliseconds) {
    alerts.push(`${desired.environment}:consumer-lag-milliseconds:${graphql.lag_milliseconds_max}`);
  }
  if (graphql.retry_count_max > 0) alerts.push(`${desired.environment}:retries:${graphql.retry_count_max}`);
  if (graphql.failed_operations > 0) alerts.push(`${desired.environment}:failed-operations:${graphql.failed_operations}`);
  if (graphql.dlq_outcomes > 0) alerts.push(`${desired.environment}:dlq-outcomes:${graphql.dlq_outcomes}`);
  if (
    graphql.consumer_concurrency_max >= QUEUE_OPERATIONAL_CONTRACT.max_concurrency
    && primaryMetrics.oldest_age_seconds
      >= thresholds.concurrency_saturation_oldest_age_seconds
  ) {
    alerts.push(`${desired.environment}:consumer-concurrency:${graphql.consumer_concurrency_max}`);
  }
  if (primaryMetrics.backlog_count > 0 && graphql.consumer_concurrency_max === 0) {
    alerts.push(`${desired.environment}:stalled-consumer:backlog-with-zero-concurrency`);
  }
}

async function getQueueMetrics(context, queue, nowMs) {
  const payload = await cloudflareRequest({
    ...context,
    endpoint: `/accounts/${context.accountId}/queues/${queue.queue_id}/metrics`,
  });
  const metrics = {
    backlog_count: Number(payload.result.backlog_count),
    backlog_bytes: Number(payload.result.backlog_bytes),
    oldest_message_timestamp_ms: Number(payload.result.oldest_message_timestamp_ms),
    oldest_age_seconds: oldestAgeSeconds(payload.result, nowMs),
  };
  if (![metrics.backlog_count, metrics.backlog_bytes, metrics.oldest_message_timestamp_ms]
    .every(Number.isFinite)) {
    throw new Error(`Cloudflare Queue metrics are incomplete for ${queue.queue_name}`);
  }
  return metrics;
}

async function inspectEnvironment({context, desired, queues, scripts, nowMs, windowMinutes}) {
  const failures = [...desired.config_errors];
  const alerts = [];
  const primaryMatches = queueByName(queues, desired.primary_queue);
  const dlqMatches = queueByName(queues, desired.dlq);
  if (primaryMatches.length !== 1) failures.push(`${desired.environment}:primary-queue-count:${primaryMatches.length}`);
  if (dlqMatches.length !== 1) failures.push(`${desired.environment}:dlq-count:${dlqMatches.length}`);
  const scriptMatches = scripts.filter((script) => script.id === desired.worker_name);
  if (scriptMatches.length !== 1) failures.push(`${desired.environment}:worker-count:${scriptMatches.length}`);
  const handlers = scriptMatches[0]?.handlers ?? [];
  if (!handlers.includes("queue")) failures.push(`${desired.environment}:worker-queue-handler-missing`);

  const result = {
    environment: desired.environment,
    worker: {name: desired.worker_name, handlers},
    desired: {
      primary_queue: desired.primary_queue,
      dlq: desired.dlq,
      producer_binding: desired.producer_binding,
      retention_seconds: QUEUE_OPERATIONAL_CONTRACT.message_retention_seconds,
      consumer: {
        ...desired.consumer,
        max_concurrency_required: QUEUE_OPERATIONAL_CONTRACT.max_concurrency,
      },
    },
    primary: null,
    dlq: null,
    metrics: null,
    failures,
    alerts,
  };

  if (scriptMatches.length === 1) {
    const settings = await cloudflareRequest({
      ...context,
      endpoint: `/accounts/${context.accountId}/workers/scripts/${encodeURIComponent(desired.worker_name)}/settings`,
    });
    const queueBindings = (settings.result.bindings ?? []).filter((binding) => binding.type === "queue");
    const matchingBindings = queueBindings.filter((binding) => bindingMatches(binding, desired));
    if (matchingBindings.length !== 1) failures.push(`${desired.environment}:producer-binding-count:${matchingBindings.length}`);
    if (queueBindings.length !== 1) failures.push(`${desired.environment}:worker-queue-binding-total:${queueBindings.length}`);
    result.worker.queue_bindings = queueBindings.map((binding) => ({
      name: binding.name ?? null,
      queue_name: binding.queue_name ?? null,
      type: binding.type,
    }));
  }

  if (primaryMatches.length !== 1 || dlqMatches.length !== 1) return result;
  const primary = primaryMatches[0];
  const dlq = dlqMatches[0];
  const [primaryConsumers, dlqConsumers] = await Promise.all([
    collectPages(context, `/accounts/${context.accountId}/queues/${primary.queue_id}/consumers`),
    collectPages(context, `/accounts/${context.accountId}/queues/${dlq.queue_id}/consumers`),
  ]);
  const attachedConsumer = compareConsumer(desired, primaryConsumers, failures);
  if (dlqConsumers.length !== 0) failures.push(`${desired.environment}:dlq-consumer-count:${dlqConsumers.length}`);
  for (const [kind, queue] of [["primary", primary], ["dlq", dlq]]) {
    const retention = queue.settings?.message_retention_period ?? null;
    if (retention !== QUEUE_OPERATIONAL_CONTRACT.message_retention_seconds) {
      failures.push(`${desired.environment}:${kind}-retention:expected-${QUEUE_OPERATIONAL_CONTRACT.message_retention_seconds}:observed-${retention ?? "unset"}`);
    }
  }
  const producerNames = queueProducerNames(primary);
  if (
    !Array.isArray(primary.producers)
    || producerNames.length !== 1
    || producerNames[0] !== desired.worker_name
  ) {
    failures.push(`${desired.environment}:primary-producer-inventory-mismatch`);
  }
  if (!Array.isArray(dlq.producers)) {
    failures.push(`${desired.environment}:dlq-producer-inventory-missing`);
  } else if (dlq.producers.length !== 0) {
    failures.push(`${desired.environment}:dlq-direct-producers-present`);
  }
  result.primary = {
    name: primary.queue_name,
    retention_seconds: primary.settings?.message_retention_period ?? null,
    delivery_paused: primary.settings?.delivery_paused ?? null,
    producers: producerNames,
    consumer: attachedConsumer,
  };
  result.dlq = {
    name: dlq.queue_name,
    retention_seconds: dlq.settings?.message_retention_period ?? null,
    delivery_paused: dlq.settings?.delivery_paused ?? null,
    direct_producer_count: (dlq.producers ?? []).length,
    consumer_count: dlqConsumers.length,
  };

  const startMs = nowMs - windowMinutes * 60_000;
  const timeVariables = {start: new Date(startMs).toISOString(), end: new Date(nowMs).toISOString()};
  const [primaryMetrics, dlqMetrics, backlogRows, concurrencyRows, operationRows] = await Promise.all([
    getQueueMetrics(context, primary, nowMs),
    getQueueMetrics(context, dlq, nowMs),
    graphqlRows({
      ...context,
      query: GRAPHQL_QUERIES.backlog,
      dataset: "queueBacklogAdaptiveGroups",
      queueId: primary.queue_id,
      ...timeVariables,
    }),
    graphqlRows({
      ...context,
      query: GRAPHQL_QUERIES.concurrency,
      dataset: "queueConsumerMetricsAdaptiveGroups",
      queueId: primary.queue_id,
      ...timeVariables,
    }),
    graphqlRows({
      ...context,
      query: GRAPHQL_QUERIES.operations,
      dataset: "queueMessageOperationsAdaptiveGroups",
      queueId: primary.queue_id,
      ...timeVariables,
    }),
  ]);
  const graphql = summarizeGraphqlMetrics({backlogRows, concurrencyRows, operationRows});
  result.metrics = {
    primary_realtime: primaryMetrics,
    dlq_realtime: dlqMetrics,
    graphql_window_minutes: windowMinutes,
    graphql,
  };
  addMetricAlerts({desired, primaryMetrics, dlqMetrics, graphql, alerts});
  return result;
}

function allQueuesEmpty(result) {
  return result.metrics?.primary_realtime?.backlog_count === 0
    && result.metrics?.dlq_realtime?.backlog_count === 0;
}

export async function runQueueGate({
  argv = process.argv.slice(2),
  env = process.env,
  fetchImpl = globalThis.fetch,
  readConfig = defaultReadConfig,
  now = () => Date.now(),
  sleep = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds)),
} = {}) {
  const options = parseQueueGateArgs(argv);
  if (options.help) return {help: true, usage: usage(), exitCode: 0};
  const accountId = env.CLOUDFLARE_ACCOUNT_ID;
  const token = env.CLOUDFLARE_API_TOKEN;
  if (!accountId || !token) throw new Error("CLOUDFLARE_ACCOUNT_ID and CLOUDFLARE_API_TOKEN are required");
  if (typeof fetchImpl !== "function") throw new Error("global fetch is unavailable");

  const desired = await readDesiredQueueInventory({
    environments: options.environments,
    wranglerConfig: options.wranglerConfig,
    readConfig,
  });
  const context = {fetchImpl, token, accountId};
  const [queues, scripts] = await Promise.all([
    collectPages(context, `/accounts/${accountId}/queues`),
    collectPages(context, `/accounts/${accountId}/workers/scripts`),
  ]);
  const sampleCount = options.phase === "rollback" ? options.rollbackSamples : 1;
  const samples = [];
  for (let sample = 0; sample < sampleCount; sample += 1) {
    const checkedAtMs = now();
    const environments = [];
    for (const expected of desired) {
      environments.push(await inspectEnvironment({
        context,
        desired: expected,
        queues,
        scripts,
        nowMs: checkedAtMs,
        windowMinutes: options.windowMinutes,
      }));
    }
    samples.push({checked_at: new Date(checkedAtMs).toISOString(), environments});
    if (sample + 1 < sampleCount) await sleep(options.sampleIntervalSeconds * 1000);
  }

  const latest = samples.at(-1).environments;
  const failures = [...new Set(latest.flatMap((result) => result.failures))];
  const alerts = [...new Set(latest.flatMap((result) => result.alerts))];
  if (options.phase === "publication") {
    for (const result of latest) {
      if (!allQueuesEmpty(result)) failures.push(`${result.environment}:publication-requires-empty-primary-and-dlq`);
    }
  }
  if (options.phase === "rollback") {
    for (const sample of samples) {
      for (const result of sample.environments) {
        if (!allQueuesEmpty(result)) failures.push(`${result.environment}:rollback-sample-not-empty:${sample.checked_at}`);
      }
    }
    if (alerts.length > 0) failures.push("rollback-requires-no-recent-queue-alert-signals");
    failures.push("queue-less-rollback-requires-a-separately-reviewed-live-producer-fence");
  }
  const uniqueFailures = [...new Set(failures)];
  const receipt = {
    schema_version: 1,
    checked_at: new Date(now()).toISOString(),
    phase: options.phase,
    ok: uniqueFailures.length === 0 && alerts.length === 0,
    operational_contract: QUEUE_OPERATIONAL_CONTRACT,
    failures: uniqueFailures,
    alerts,
    samples,
    limitations: [
      "Queue REST metrics are best-effort and expose combined unacknowledged backlog, not ready and delayed sub-counts.",
      "Cloudflare Notifications exposes no native Queue alert type; a scheduler must alert on this command's non-zero exit.",
      "Rollback samples are drain observations only. They never authorize a Queue-less Worker while ingress can still enqueue; that transition requires a separately reviewed live producer fence retaining the compatible consumer.",
    ],
  };
  return {help: false, receipt, exitCode: receipt.ok ? 0 : 1};
}

async function main() {
  try {
    const result = await runQueueGate();
    if (result.help) process.stdout.write(result.usage);
    else process.stdout.write(`${JSON.stringify(result.receipt, null, 2)}\n`);
    process.exitCode = result.exitCode;
  } catch (error) {
    process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
    process.exitCode = 2;
  }
}

if (import.meta.url === `file://${process.argv[1]}`) await main();
