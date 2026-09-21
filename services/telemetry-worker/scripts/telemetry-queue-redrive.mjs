#!/usr/bin/env node

import {createHash} from "node:crypto";
import fs from "node:fs";
import {gunzipSync} from "node:zlib";

const API = "https://api.cloudflare.com/client/v4";
const RECEIPT_MAX_AGE_MS = 10 * 60 * 1000;
const PULL_VISIBILITY_TIMEOUT_MS = 10 * 60 * 1000;
const MAX_QUEUE_MESSAGE_BYTES = 127_800;
const MAX_UNCOMPRESSED_BYTES = 64 * 1024;
const DLQ_RETENTION_MS = 4 * 24 * 60 * 60 * 1000;
const MINIMUM_REMAINING_RETENTION_MS = 30 * 60 * 1000;
const MAX_RECEIPT_ATTEMPTS = 97;
const MAX_PULLED_ATTEMPTS = 98;
const REDRIVE_WRAPPER_KEYS = [
  "body_base64",
  "body_sha256",
  "content_encoding",
  "content_type",
  "format_version",
  "kind",
];
const PULL_SETTINGS = Object.freeze({
  batch_size: 100,
  max_retries: 100,
  retry_delay: 0,
  visibility_timeout_ms: PULL_VISIBILITY_TIMEOUT_MS,
});

function usage() {
  return `Usage: node scripts/telemetry-queue-redrive.mjs [options]

Redrive one exact offline-inspected DLQ snapshot through a temporary HTTP pull
consumer. Each original body is durably admitted to the primary Queue before
its DLQ lease is acknowledged. Mutations require --apply.

Options:
  --environment staging|prod
  --action start|stop
  --receipt FILE             Required for start
  --apply                    Perform and verify the requested mutation
  --help

Required environment:
  CLOUDFLARE_ACCOUNT_ID
  CLOUDFLARE_API_TOKEN
`;
}

export function parseRedriveArgs(argv) {
  const options = {action: "", apply: false, environment: "", help: false, receipt: ""};
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    const value = () => {
      index += 1;
      if (index >= argv.length || argv[index].startsWith("--")) {
        throw new Error(`${argument} requires a value`);
      }
      return argv[index];
    };
    if (argument === "--action") options.action = value();
    else if (argument === "--apply") options.apply = true;
    else if (argument === "--environment") options.environment = value();
    else if (argument === "--receipt") options.receipt = value();
    else if (argument === "--help" || argument === "-h") options.help = true;
    else throw new Error(`unknown argument: ${argument}`);
  }
  if (options.help) return options;
  if (!new Set(["start", "stop"]).has(options.action)) {
    throw new Error("--action must be start or stop");
  }
  if (!new Set(["staging", "prod"]).has(options.environment)) {
    throw new Error("--environment must be staging or prod");
  }
  if (options.action === "start" && !options.receipt) {
    throw new Error("--receipt is required for start");
  }
  if (options.action === "stop" && options.receipt) {
    throw new Error("--receipt is not accepted for stop");
  }
  return options;
}

function desired(environment) {
  const suffix = environment === "prod" ? "prod" : "staging";
  return {
    analyticsEnvironment: environment === "prod" ? "production" : "staging",
    dlq: `ctx-telemetry-ingest-${suffix}-dlq`,
    primary: `ctx-telemetry-ingest-${suffix}`,
    worker: environment === "prod" ? "ctx-telemetry" : "ctx-telemetry-staging",
  };
}

function isRecord(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function decodeBase64(value, maximumBytes = MAX_QUEUE_MESSAGE_BYTES) {
  if (
    typeof value !== "string"
    || value.length === 0
    || value.length % 4 !== 0
  ) throw new Error("Queue message body is not padded RFC 4648 base64");
  if (value.length > Math.ceil(maximumBytes / 3) * 4) {
    throw new Error("Queue message body exceeds its size limit");
  }
  if (!/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/u.test(value)) {
    throw new Error("Queue message body is not padded RFC 4648 base64");
  }
  const decoded = Buffer.from(value, "base64");
  if (decoded.byteLength > maximumBytes) throw new Error("Queue message body exceeds its size limit");
  return decoded;
}

function hasExactKeys(value, expected) {
  return JSON.stringify(Object.keys(value).sort()) === JSON.stringify([...expected].sort());
}

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

function decodeRedriveWrapper(encoded) {
  let text;
  try {
    text = new TextDecoder("utf-8", {fatal: true}).decode(encoded);
  } catch {
    throw new Error("live DLQ JSON message is not UTF-8");
  }
  let wrapper;
  try {
    wrapper = JSON.parse(text);
  } catch {
    throw new Error("live DLQ JSON message is invalid");
  }
  if (
    !isRecord(wrapper)
    || !hasExactKeys(wrapper, REDRIVE_WRAPPER_KEYS)
    || wrapper.format_version !== 1
    || wrapper.kind !== "telemetry_queue_redrive"
    || wrapper.content_encoding !== "identity"
    || wrapper.content_type !== "application/json"
    || typeof wrapper.body_sha256 !== "string"
    || !/^[0-9a-f]{64}$/u.test(wrapper.body_sha256)
  ) throw new Error("live DLQ redrive wrapper is invalid");
  const normalized = decodeBase64(wrapper.body_base64, MAX_UNCOMPRESSED_BYTES);
  if (
    normalized.byteLength > MAX_UNCOMPRESSED_BYTES
    || sha256(normalized) !== wrapper.body_sha256
  ) throw new Error("live DLQ redrive wrapper is invalid");
  return normalized;
}

function validateReceipt(receipt, expected, nowMs) {
  if (
    !isRecord(receipt)
    || receipt.schema_version !== 2
    || receipt.offline !== true
    || receipt.environment !== expected.analyticsEnvironment
    || receipt.source_queue !== expected.dlq
    || !Array.isArray(receipt.messages)
    || !Number.isInteger(receipt.message_count)
    || receipt.message_count !== receipt.messages.length
  ) throw new Error("offline decoder receipt does not describe a valid bounded DLQ snapshot");
  const checkedAt = Date.parse(String(receipt.checked_at ?? ""));
  if (!Number.isFinite(checkedAt) || checkedAt > nowMs || nowMs - checkedAt > RECEIPT_MAX_AGE_MS) {
    throw new Error("offline decoder receipt is stale");
  }
  if (receipt.message_count < 1 || receipt.message_count > 100) {
    throw new Error("DLQ redrive requires a bounded snapshot between 1 and 100 messages");
  }
  if (receipt.messages.some((message) => (
    !isRecord(message)
    || message.analytics_environment !== expected.analyticsEnvironment
    || message.format_version !== 1
    || typeof message.body_sha256 !== "string"
    || !/^[0-9a-f]{64}$/u.test(message.body_sha256)
    || !new Set(["bytes", "json"]).has(message.queue_content_type)
    || !Number.isSafeInteger(message.attempts)
    || message.attempts < 0
    || message.attempts > MAX_RECEIPT_ATTEMPTS
    || !safeTimestamp(message.timestamp_ms, nowMs)
    || !new Set(["telemetry_row", "blame_product_receipt", "install_stage_row"])
      .has(message.kind)
  ))) throw new Error("offline decoder receipt contains an unsupported message");
}

function safeTimestamp(value, nowMs) {
  return Number.isSafeInteger(value)
    && value > 0
    && value <= nowMs
    && nowMs - value <= DLQ_RETENTION_MS - MINIMUM_REMAINING_RETENTION_MS;
}

function readReceiptFile(file) {
  try {
    return JSON.parse(fs.readFileSync(file, "utf8"));
  } catch {
    throw new Error("offline decoder receipt is unreadable or invalid JSON");
  }
}

function apiError(payload, status) {
  const codes = Array.isArray(payload?.errors)
    ? payload.errors.map((item) => Number(item?.code)).filter(Number.isFinite).slice(0, 4)
    : [];
  return `Cloudflare API failed with HTTP ${status}${codes.length ? ` codes ${codes.join(",")}` : ""}`;
}

async function request(context, path, init = {}) {
  const response = await context.fetchImpl(`${API}${path}`, {
    ...init,
    headers: {
      Authorization: `Bearer ${context.token}`,
      "Content-Type": "application/json",
    },
    signal: AbortSignal.timeout(15_000),
  });
  let payload;
  try {
    payload = await response.json();
  } catch {
    throw new Error(`Cloudflare API returned non-JSON HTTP ${response.status}`);
  }
  if (!response.ok || payload?.success !== true) throw new Error(apiError(payload, response.status));
  return payload;
}

async function listQueues(context) {
  const queues = [];
  for (let page = 1; page <= 100; page += 1) {
    const payload = await request(
      context,
      `/accounts/${context.accountId}/queues?per_page=100&page=${page}`,
    );
    if (!Array.isArray(payload.result)) throw new Error("Cloudflare Queue list is invalid");
    queues.push(...payload.result);
    const totalPages = Number(payload.result_info?.total_pages ?? 1);
    if (page >= totalPages || payload.result.length === 0) return queues;
  }
  throw new Error("Cloudflare Queue pagination exceeded 100 pages");
}

async function listConsumers(context, queueId) {
  const payload = await request(
    context,
    `/accounts/${context.accountId}/queues/${queueId}/consumers?per_page=100&page=1`,
  );
  if (!Array.isArray(payload.result)) throw new Error("Cloudflare consumer list is invalid");
  return payload.result;
}

async function queueMetrics(context, queueId) {
  const payload = await request(
    context,
    `/accounts/${context.accountId}/queues/${queueId}/metrics`,
  );
  const backlogCount = Number(payload.result?.backlog_count);
  if (!Number.isSafeInteger(backlogCount) || backlogCount < 0) {
    throw new Error("Cloudflare Queue backlog metric is invalid");
  }
  return backlogCount;
}

function exactQueue(queues, name) {
  const matches = queues.filter((queue) => queue?.queue_name === name);
  if (matches.length !== 1 || typeof matches[0]?.queue_id !== "string") {
    throw new Error(`expected exactly one Queue named ${name}`);
  }
  return matches[0];
}

function primaryConsumerIdentity(consumer) {
  if (!isRecord(consumer)) return null;
  const identities = ["script", "script_name"]
    .filter((field) => Object.hasOwn(consumer, field))
    .map((field) => consumer[field]);
  if (
    identities.length === 0
    || identities.some((identity) => typeof identity !== "string" || identity.length === 0)
  ) return null;
  return identities.every((identity) => identity === identities[0]) ? identities[0] : null;
}

function isPrimaryConsumer(consumer, expected) {
  const settings = consumer?.settings ?? {};
  return consumer?.type === "worker"
    && primaryConsumerIdentity(consumer) === expected.worker
    && consumer.dead_letter_queue === expected.dlq
    && settings.batch_size === 10
    && settings.max_wait_time_ms === 5_000
    && settings.max_retries === 10
    && settings.max_concurrency === 4;
}

function isManagedPullConsumer(consumer) {
  const settings = consumer?.settings ?? {};
  return consumer?.type === "http_pull"
    && !consumer.dead_letter_queue
    && Object.entries(PULL_SETTINGS).every(([key, value]) => settings[key] === value);
}

async function inspect(context, expected) {
  const queues = await listQueues(context);
  const primary = exactQueue(queues, expected.primary);
  const dlq = exactQueue(queues, expected.dlq);
  const [primaryConsumers, dlqConsumers, primaryBacklog, dlqBacklog] = await Promise.all([
    listConsumers(context, primary.queue_id),
    listConsumers(context, dlq.queue_id),
    queueMetrics(context, primary.queue_id),
    queueMetrics(context, dlq.queue_id),
  ]);
  if (primaryConsumers.length !== 1 || !isPrimaryConsumer(primaryConsumers[0], expected)) {
    throw new Error("primary Queue consumer does not match the release contract");
  }
  return {dlq, dlqBacklog, dlqConsumers, primary, primaryBacklog};
}

async function peekBodyHashes(context, queueId, batchSize) {
  const payload = await request(
    context,
    `/accounts/${context.accountId}/queues/${queueId}/messages/peek`,
    {method: "POST", body: JSON.stringify({batch_size: batchSize})},
  );
  if (!isRecord(payload.result) || !Array.isArray(payload.result.messages)) {
    throw new Error("Cloudflare DLQ peek response is invalid");
  }
  return payload.result.messages.map((message) => queueBody(message).fingerprint).sort();
}

function queueBody(message) {
  if (!isRecord(message)) throw new Error("live DLQ contains an invalid message");
  const contentType = message.metadata?.["CF-Content-Type"];
  if (contentType === "bytes") {
    const encoded = decodeBase64(message.body);
    let normalized;
    try {
      normalized = gunzipSync(encoded, {maxOutputLength: MAX_UNCOMPRESSED_BYTES});
    } catch {
      throw new Error("live DLQ bytes message is not bounded gzip");
    }
    return {
      fingerprint: `bytes:${sha256(encoded)}`,
      normalized,
    };
  }
  if (contentType === "json") {
    const encoded = decodeBase64(message.body);
    const normalized = decodeRedriveWrapper(encoded);
    return {
      fingerprint: `json:${sha256(encoded)}`,
      normalized,
    };
  }
  throw new Error("live DLQ message content type is unsupported");
}

async function createPullConsumer(context, queueId) {
  const payload = await request(
    context,
    `/accounts/${context.accountId}/queues/${queueId}/consumers`,
    {
      method: "POST",
      body: JSON.stringify({type: "http_pull", settings: PULL_SETTINGS}),
    },
  );
  if (!isManagedPullConsumer(payload.result)) {
    throw new Error("created HTTP pull consumer does not match the bounded contract");
  }
  return payload.result;
}

async function removePullConsumer(context, queueId, consumer) {
  const consumerId = consumer?.consumer_id;
  if (typeof consumerId !== "string" || consumerId.length === 0) {
    throw new Error("managed HTTP pull consumer has no ID");
  }
  await request(
    context,
    `/accounts/${context.accountId}/queues/${queueId}/consumers/${consumerId}`,
    {method: "DELETE"},
  );
  if ((await listConsumers(context, queueId)).length !== 0) {
    throw new Error("HTTP pull consumer still exists after deletion");
  }
}

async function removeAnyManagedPullConsumer(context, queueId) {
  const consumers = await listConsumers(context, queueId);
  if (consumers.length === 0) return;
  if (consumers.length !== 1 || !isManagedPullConsumer(consumers[0])) {
    throw new Error("ambiguous HTTP pull consumer cleanup state");
  }
  await removePullConsumer(context, queueId, consumers[0]);
}

async function pullExactMessages(context, queueId, count) {
  const payload = await request(
    context,
    `/accounts/${context.accountId}/queues/${queueId}/messages/pull`,
    {
      method: "POST",
      body: JSON.stringify({
        batch_size: count,
        visibility_timeout_ms: PULL_VISIBILITY_TIMEOUT_MS,
      }),
    },
  );
  if (!isRecord(payload.result) || !Array.isArray(payload.result.messages)) {
    throw new Error("Cloudflare DLQ pull response is invalid");
  }
  return payload.result.messages.map((message) => {
    if (
      !isRecord(message)
      || typeof message.lease_id !== "string"
      || message.lease_id.length === 0
      || !Number.isSafeInteger(message.attempts)
      || message.attempts < 0
      || message.attempts > MAX_PULLED_ATTEMPTS
      || !safeTimestamp(message.timestamp_ms, context.now())
    ) throw new Error("Cloudflare DLQ pull message is invalid");
    const body = queueBody(message);
    return {fingerprint: body.fingerprint, leaseId: message.lease_id, normalized: body.normalized};
  });
}

async function pushToPrimary(context, queueId, message) {
  const {normalized} = message;
  const wrapper = {
    body_base64: normalized.toString("base64"),
    body_sha256: sha256(normalized),
    content_encoding: "identity",
    content_type: "application/json",
    format_version: 1,
    kind: "telemetry_queue_redrive",
  };
  if (Buffer.byteLength(JSON.stringify(wrapper), "utf8") > MAX_QUEUE_MESSAGE_BYTES) {
    throw new Error("redrive wrapper exceeds the Queue message size contract");
  }
  await request(
    context,
    `/accounts/${context.accountId}/queues/${queueId}/messages`,
    {
      method: "POST",
      body: JSON.stringify({
        body: wrapper,
        content_type: "json",
      }),
    },
  );
}

async function settleLeases(context, queueId, field, leaseIds) {
  const payload = await request(
    context,
    `/accounts/${context.accountId}/queues/${queueId}/messages/ack`,
    {
      method: "POST",
      body: JSON.stringify({
        acks: field === "acks" ? leaseIds.map((lease_id) => ({lease_id})) : [],
        retries: field === "retries"
          ? leaseIds.map((lease_id) => ({delay_seconds: 0, lease_id}))
          : [],
      }),
    },
  );
  const expected = field === "acks" ? "ackCount" : "retryCount";
  if (!isRecord(payload.result) || payload.result[expected] !== leaseIds.length) {
    throw new Error(`Cloudflare DLQ ${field} count is invalid`);
  }
}

async function start(context, expected, state, receipt, apply) {
  if (state.dlqConsumers.length !== 0) throw new Error("DLQ already has a consumer");
  validateReceipt(receipt, expected, context.now());
  const liveHashes = await peekBodyHashes(
    context,
    state.dlq.queue_id,
    receipt.message_count,
  );
  const receiptHashes = receipt.messages
    .map((message) => `${message.queue_content_type}:${message.body_sha256}`)
    .sort();
  if (JSON.stringify(liveHashes) !== JSON.stringify(receiptHashes)) {
    throw new Error("offline decoder receipt does not match the exact live DLQ messages");
  }
  if (!apply) return {action: "start", applied: false, redriven_count: 0};

  let creationAttempted = false;
  let pulled = [];
  let redrivenCount = 0;
  try {
    creationAttempted = true;
    await createPullConsumer(context, state.dlq.queue_id);
    const consumers = await listConsumers(context, state.dlq.queue_id);
    if (consumers.length !== 1 || !isManagedPullConsumer(consumers[0])) {
      throw new Error("HTTP pull consumer failed verification");
    }
    pulled = await pullExactMessages(context, state.dlq.queue_id, receipt.message_count);
    const pulledHashes = pulled.map((message) => message.fingerprint).sort();
    if (JSON.stringify(pulledHashes) !== JSON.stringify(receiptHashes)) {
      await settleLeases(
        context,
        state.dlq.queue_id,
        "retries",
        pulled.map((message) => message.leaseId),
      );
      pulled = [];
      throw new Error("pulled DLQ messages do not match the exact offline receipt");
    }
    const admissions = await Promise.allSettled(
      pulled.map((message) => pushToPrimary(context, state.primary.queue_id, message)),
    );
    if (admissions.some((result) => result.status === "rejected")) {
      await settleLeases(
        context,
        state.dlq.queue_id,
        "retries",
        pulled.map((message) => message.leaseId),
      );
      pulled = [];
      throw new Error("primary Queue admission failed during redrive");
    }
    await settleLeases(
      context,
      state.dlq.queue_id,
      "acks",
      pulled.map((message) => message.leaseId),
    );
    redrivenCount = pulled.length;
    pulled = [];
  } catch (error) {
    if (pulled.length > 0) {
      try {
        await settleLeases(
          context,
          state.dlq.queue_id,
          "retries",
          pulled.map((message) => message.leaseId),
        );
      } catch {
        // The bounded visibility timeout preserves unacknowledged messages.
      }
    }
    throw error;
  } finally {
    if (creationAttempted) await removeAnyManagedPullConsumer(context, state.dlq.queue_id);
  }
  const residualMessageObserved = (
    await peekBodyHashes(context, state.dlq.queue_id, 1)
  ).length > 0;
  return {
    action: "start",
    applied: true,
    redriven_count: redrivenCount,
    residual_message_observed: residualMessageObserved,
  };
}

async function stop(context, state, apply) {
  if (state.dlqConsumers.length !== 1 || !isManagedPullConsumer(state.dlqConsumers[0])) {
    throw new Error("DLQ does not have exactly one managed HTTP pull consumer");
  }
  if (!apply) return {action: "stop", applied: false, redriven_count: 0};
  await removePullConsumer(context, state.dlq.queue_id, state.dlqConsumers[0]);
  return {action: "stop", applied: true, redriven_count: 0};
}

export async function runRedrive({
  argv = process.argv.slice(2),
  env = process.env,
  fetchImpl = globalThis.fetch,
  now = () => Date.now(),
  readReceipt = readReceiptFile,
} = {}) {
  const options = parseRedriveArgs(argv);
  if (options.help) return {help: true, usage: usage()};
  const accountId = String(env.CLOUDFLARE_ACCOUNT_ID ?? "").trim();
  const token = String(env.CLOUDFLARE_API_TOKEN ?? "").trim();
  if (!accountId || !token) {
    throw new Error("CLOUDFLARE_ACCOUNT_ID and CLOUDFLARE_API_TOKEN are required");
  }
  const context = {accountId, fetchImpl, now, token};
  const expected = desired(options.environment);
  const state = await inspect(context, expected);
  const outcome = options.action === "start"
    ? await start(context, expected, state, readReceipt(options.receipt), options.apply)
    : await stop(context, state, options.apply);
  return {
    help: false,
    receipt: {
      schema_version: 2,
      checked_at: new Date(now()).toISOString(),
      environment: expected.analyticsEnvironment,
      primary_queue: expected.primary,
      dlq: expected.dlq,
      observed_primary_backlog_approximate: state.primaryBacklog,
      observed_dlq_backlog_approximate: state.dlqBacklog,
      transport: "http_pull_exact_hashes",
      ...outcome,
    },
  };
}

async function main() {
  try {
    const result = await runRedrive();
    process.stdout.write(result.help ? result.usage : `${JSON.stringify(result.receipt, null, 2)}\n`);
  } catch (error) {
    process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
    process.exitCode = 1;
  }
}

if (import.meta.url === `file://${process.argv[1]}`) await main();
