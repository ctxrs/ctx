import assert from "node:assert/strict";
import {createHash} from "node:crypto";
import test from "node:test";
import {gzipSync} from "node:zlib";

import {parseRedriveArgs, runRedrive} from "../scripts/telemetry-queue-redrive.mjs";

const NOW = Date.parse("2026-09-01T16:00:00.000Z");
const MESSAGE_TIMESTAMP = NOW - 60 * 60 * 1000;
const ENV = {CLOUDFLARE_ACCOUNT_ID: "account", CLOUDFLARE_API_TOKEN: "secret"};
const LIVE_NORMALIZED = Buffer.from('{"format_version":1}');
const OTHER_NORMALIZED = Buffer.from('{"format_version":2}');
const LIVE_BODY = gzipSync(LIVE_NORMALIZED);
const OTHER_BODY = gzipSync(OTHER_NORMALIZED);
const LIVE_BODY_HASH = createHash("sha256").update(LIVE_BODY).digest("hex");
const PULL_SETTINGS = {
  batch_size: 100,
  max_retries: 100,
  retry_delay: 0,
  visibility_timeout_ms: 600_000,
};

function primaryConsumer(identity = {script: "ctx-telemetry-staging"}) {
  return {
    consumer_id: "primary-consumer",
    type: "worker",
    dead_letter_queue: "ctx-telemetry-ingest-staging-dlq",
    settings: {batch_size: 10, max_concurrency: 4, max_retries: 10, max_wait_time_ms: 5000},
    ...identity,
  };
}

function pullConsumer() {
  return {
    consumer_id: "pull-consumer",
    type: "http_pull",
    dead_letter_queue: "",
    settings: PULL_SETTINGS,
  };
}

function offlineReceipt(overrides = {}) {
  return {
    schema_version: 2,
    offline: true,
    checked_at: new Date(NOW).toISOString(),
    environment: "staging",
    source_queue: "ctx-telemetry-ingest-staging-dlq",
    message_count: 1,
    messages: [{
      analytics_environment: "staging",
      attempts: 11,
      body_sha256: LIVE_BODY_HASH,
      format_version: 1,
      kind: "telemetry_row",
      queue_content_type: "bytes",
      timestamp_ms: MESSAGE_TIMESTAMP,
    }],
    ...overrides,
  };
}

function response(result, {ok = true, status = 200} = {}) {
  return new Response(JSON.stringify({
    success: ok,
    result,
    ...(ok ? {result_info: {total_pages: 1}} : {errors: [{code: 1000}]}),
  }), {
    headers: {"content-type": "application/json"},
    status,
  });
}

function cloudflare({
  dlqBacklog = 1,
  initialPull = false,
  loseCreateResponse = false,
  primaryBacklog = 0,
  primaryConsumers = [primaryConsumer()],
  pullBodies = [LIVE_BODY],
  rejectPrimaryAt = -1,
  visibleBodies,
} = {}) {
  let consumers = initialPull ? [pullConsumer()] : [];
  let availableBodies = visibleBodies
    ? [...visibleBodies]
    : Array.from({length: dlqBacklog}, () => LIVE_BODY);
  let leasedBodies = [];
  const calls = [];
  const mutations = [];
  let primaryPushes = 0;
  const fetchImpl = async (url, init = {}) => {
    const parsed = new URL(url);
    const method = init.method ?? "GET";
    const body = init.body ? JSON.parse(init.body) : null;
    calls.push({body, method, path: parsed.pathname});
    if (parsed.pathname.endsWith("/queues") && method === "GET") {
      return response([
        {queue_id: "primary-id", queue_name: "ctx-telemetry-ingest-staging"},
        {queue_id: "dlq-id", queue_name: "ctx-telemetry-ingest-staging-dlq"},
      ]);
    }
    if (parsed.pathname.endsWith("/queues/primary-id/consumers")) {
      return response(primaryConsumers);
    }
    if (parsed.pathname.endsWith("/queues/primary-id/metrics")) {
      return response({backlog_count: primaryBacklog});
    }
    if (parsed.pathname.endsWith("/queues/dlq-id/metrics")) {
      return response({backlog_count: dlqBacklog});
    }
    if (parsed.pathname.endsWith("/queues/dlq-id/messages/peek") && method === "POST") {
      assert(Number.isInteger(body.batch_size));
      return response({
        messages: availableBodies.slice(0, body.batch_size)
          .map((value, index) => queueBody(value, index)),
      });
    }
    if (parsed.pathname.endsWith("/queues/dlq-id/messages/pull") && method === "POST") {
      assert.deepEqual(body, {batch_size: pullBodies.length, visibility_timeout_ms: 600_000});
      availableBodies = availableBodies.slice(pullBodies.length);
      leasedBodies = [...pullBodies];
      return response({
        messages: leasedBodies.map((value, index) => ({
          ...queueBody(value, index, 12),
          lease_id: `lease-${index}`,
        })),
      });
    }
    if (parsed.pathname.endsWith("/queues/dlq-id/messages/ack") && method === "POST") {
      const field = body.acks.length > 0 ? "acks" : "retries";
      if (field === "retries") {
        assert(body.retries.every((retry) => retry.delay_seconds === 0));
        availableBodies = leasedBodies;
      }
      leasedBodies = [];
      mutations.push(field);
      return response({
        ackCount: body.acks.length,
        retryCount: body.retries.length,
        warnings: [],
      });
    }
    if (parsed.pathname.endsWith("/queues/dlq-id/consumers") && method === "GET") {
      return response(consumers);
    }
    if (parsed.pathname.endsWith("/queues/dlq-id/consumers") && method === "POST") {
      assert.deepEqual(body, {type: "http_pull", settings: PULL_SETTINGS});
      consumers = [pullConsumer()];
      mutations.push("created");
      if (loseCreateResponse) throw new Error("ambiguous create response");
      return response(consumers[0]);
    }
    if (
      parsed.pathname.endsWith("/queues/dlq-id/consumers/pull-consumer")
      && method === "DELETE"
    ) {
      consumers = [];
      mutations.push("deleted");
      return response(null);
    }
    if (parsed.pathname.endsWith("/queues/primary-id/messages") && method === "POST") {
      const index = primaryPushes;
      primaryPushes += 1;
      if (index === rejectPrimaryAt) return response(null, {ok: false, status: 503});
      mutations.push("primary-push");
      return response({metadata: {metrics: {}}});
    }
    throw new Error(`unexpected request ${method} ${parsed.pathname}`);
  };
  return {calls, fetchImpl, mutations};
}

function queueBody(value, index = 0, attempts = 11) {
  const body = Buffer.isBuffer(value) ? value : Buffer.from(value, "utf8");
  return {
    attempts,
    body: body.toString("base64"),
    id: `message-${index}`,
    metadata: {"CF-Content-Type": typeof value === "string" ? "json" : "bytes"},
    timestamp_ms: MESSAGE_TIMESTAMP,
  };
}

test("requires explicit bounded action arguments", () => {
  assert.deepEqual(parseRedriveArgs([
    "--environment", "staging", "--action", "start", "--receipt", "receipt.json",
  ]), {
    action: "start",
    apply: false,
    environment: "staging",
    help: false,
    receipt: "receipt.json",
  });
  assert.throws(
    () => parseRedriveArgs(["--environment", "prod", "--action", "start"]),
    /--receipt is required/u,
  );
});

test("dry-run accepts the live script identity and binds a fresh peek without mutation", async () => {
  const mock = cloudflare();
  const result = await runRedrive({
    argv: ["--environment", "staging", "--action", "start", "--receipt", "receipt.json"],
    env: ENV,
    fetchImpl: mock.fetchImpl,
    now: () => NOW,
    readReceipt: () => offlineReceipt(),
  });

  assert.equal(result.receipt.schema_version, 2);
  assert.equal(result.receipt.transport, "http_pull_exact_hashes");
  assert.equal(result.receipt.applied, false);
  assert.deepEqual(mock.mutations, []);
  assert(!JSON.stringify(result).includes(ENV.CLOUDFLARE_API_TOKEN));
});

test("keeps script_name compatibility and accepts matching dual identities", async () => {
  for (const identity of [
    {script_name: "ctx-telemetry-staging"},
    {script: "ctx-telemetry-staging", script_name: "ctx-telemetry-staging"},
  ]) {
    const mock = cloudflare({primaryConsumers: [primaryConsumer(identity)]});
    const result = await runRedrive({
      argv: ["--environment", "staging", "--action", "start", "--receipt", "receipt.json"],
      env: ENV,
      fetchImpl: mock.fetchImpl,
      now: () => NOW,
      readReceipt: () => offlineReceipt(),
    });

    assert.equal(result.receipt.applied, false);
    assert.deepEqual(mock.mutations, []);
  }
});

test("rejects absent, empty, non-string, or conflicting identities before mutation", async () => {
  for (const identity of [
    {},
    {script: ""},
    {script: 7},
    {script: "ctx-telemetry-staging", script_name: "other-worker"},
  ]) {
    const mock = cloudflare({primaryConsumers: [primaryConsumer(identity)]});
    await assert.rejects(
      runRedrive({
        argv: [
          "--environment", "staging", "--action", "start",
          "--receipt", "receipt.json", "--apply",
        ],
        env: ENV,
        fetchImpl: mock.fetchImpl,
        now: () => NOW,
        readReceipt: () => offlineReceipt(),
      }),
      /primary Queue consumer does not match/u,
    );
    assert.deepEqual(mock.mutations, []);
  }
});

test("pulls only the inspected messages, durably requeues, then acknowledges and cleans up", async () => {
  const mock = cloudflare();
  const result = await runRedrive({
    argv: [
      "--environment", "staging", "--action", "start",
      "--receipt", "receipt.json", "--apply",
    ],
    env: ENV,
    fetchImpl: mock.fetchImpl,
    now: () => NOW,
    readReceipt: () => offlineReceipt(),
  });

  assert.equal(result.receipt.applied, true);
  assert.equal(result.receipt.redriven_count, 1);
  assert.equal(result.receipt.residual_message_observed, false);
  assert.deepEqual(mock.mutations, ["created", "primary-push", "acks", "deleted"]);
  const push = mock.calls.find((call) => call.path.endsWith("/queues/primary-id/messages"));
  assert.deepEqual(push.body, {
    body: {
      body_base64: LIVE_NORMALIZED.toString("base64"),
      body_sha256: createHash("sha256").update(LIVE_NORMALIZED).digest("hex"),
      content_encoding: "identity",
      content_type: "application/json",
      format_version: 1,
      kind: "telemetry_queue_redrive",
    },
    content_type: "json",
  });
});

test("a pull/receipt race retries every lease without admitting or deleting a message", async () => {
  const mock = cloudflare({pullBodies: [OTHER_BODY]});
  await assert.rejects(
    runRedrive({
      argv: [
        "--environment", "staging", "--action", "start",
        "--receipt", "receipt.json", "--apply",
      ],
      env: ENV,
      fetchImpl: mock.fetchImpl,
      now: () => NOW,
      readReceipt: () => offlineReceipt(),
    }),
    /pulled DLQ messages do not match/u,
  );
  assert.deepEqual(mock.mutations, ["created", "retries", "deleted"]);
});

test("partial primary admission retries the complete lease set and remains idempotent", async () => {
  const messages = Array.from({length: 2}, () => offlineReceipt().messages[0]);
  const mock = cloudflare({
    dlqBacklog: 2,
    pullBodies: [LIVE_BODY, LIVE_BODY],
    rejectPrimaryAt: 1,
  });
  await assert.rejects(
    runRedrive({
      argv: [
        "--environment", "staging", "--action", "start",
        "--receipt", "receipt.json", "--apply",
      ],
      env: ENV,
      fetchImpl: mock.fetchImpl,
      now: () => NOW,
      readReceipt: () => offlineReceipt({message_count: 2, messages}),
    }),
    /primary Queue admission failed/u,
  );
  assert.deepEqual(mock.mutations, ["created", "primary-push", "retries", "deleted"]);
});

test("the largest bounded normalized wrapper remains below the Queue body limit", () => {
  const normalized = Buffer.alloc(64 * 1024, 0x7f);
  const wrapper = {
    body_base64: normalized.toString("base64"),
    body_sha256: createHash("sha256").update(normalized).digest("hex"),
    content_encoding: "identity",
    content_type: "application/json",
    format_version: 1,
    kind: "telemetry_queue_redrive",
  };
  assert(Buffer.byteLength(JSON.stringify(wrapper), "utf8") < 127_800);
});

test("treats the receipt as a bounded snapshot instead of exact metrics backlog", async () => {
  const mock = cloudflare({dlqBacklog: 2, pullBodies: [LIVE_BODY]});
  const result = await runRedrive({
    argv: [
      "--environment", "staging", "--action", "start",
      "--receipt", "receipt.json", "--apply",
    ],
    env: ENV,
    fetchImpl: mock.fetchImpl,
    now: () => NOW,
    readReceipt: () => offlineReceipt(),
  });

  assert.equal(result.receipt.observed_dlq_backlog_approximate, 2);
  assert.equal(result.receipt.redriven_count, 1);
  assert.equal(result.receipt.residual_message_observed, true);
  assert.deepEqual(mock.mutations, ["created", "primary-push", "acks", "deleted"]);
});

test("re-redrives a retained JSON wrapper through the same bounded normal form", async () => {
  const wrapper = {
    body_base64: LIVE_NORMALIZED.toString("base64"),
    body_sha256: createHash("sha256").update(LIVE_NORMALIZED).digest("hex"),
    content_encoding: "identity",
    content_type: "application/json",
    format_version: 1,
    kind: "telemetry_queue_redrive",
  };
  const wrapperText = JSON.stringify(wrapper);
  const mock = cloudflare({pullBodies: [wrapperText], visibleBodies: [wrapperText]});
  const result = await runRedrive({
    argv: [
      "--environment", "staging", "--action", "start",
      "--receipt", "receipt.json", "--apply",
    ],
    env: ENV,
    fetchImpl: mock.fetchImpl,
    now: () => NOW,
    readReceipt: () => offlineReceipt({messages: [{
      ...offlineReceipt().messages[0],
      body_sha256: createHash("sha256").update(wrapperText).digest("hex"),
      queue_content_type: "json",
    }]}),
  });

  assert.equal(result.receipt.redriven_count, 1);
  const push = mock.calls.find((call) => call.path.endsWith("/queues/primary-id/messages"));
  assert.deepEqual(push.body, {body: wrapper, content_type: "json"});
});

test("stop removes only an exact stranded HTTP pull consumer without deleting messages", async () => {
  const mock = cloudflare({initialPull: true, primaryBacklog: 3});
  const result = await runRedrive({
    argv: ["--environment", "staging", "--action", "stop", "--apply"],
    env: ENV,
    fetchImpl: mock.fetchImpl,
    now: () => NOW,
  });
  assert.equal(result.receipt.applied, true);
  assert.deepEqual(mock.mutations, ["deleted"]);
  assert(mock.calls.every((call) => !call.path.endsWith("/messages/purge")));
});

test("an accepted create with a lost response is reconciled in finally", async () => {
  const mock = cloudflare({loseCreateResponse: true});
  await assert.rejects(
    runRedrive({
      argv: [
        "--environment", "staging", "--action", "start",
        "--receipt", "receipt.json", "--apply",
      ],
      env: ENV,
      fetchImpl: mock.fetchImpl,
      now: () => NOW,
      readReceipt: () => offlineReceipt(),
    }),
    /ambiguous create response/u,
  );
  assert.deepEqual(mock.mutations, ["created", "deleted"]);
});

test("rejects a snapshot without attempt and retention headroom", async () => {
  for (const message of [
    {...offlineReceipt().messages[0], attempts: 98},
    {...offlineReceipt().messages[0], timestamp_ms: NOW - 4 * 24 * 60 * 60 * 1000},
  ]) {
    const mock = cloudflare();
    await assert.rejects(
      runRedrive({
        argv: ["--environment", "staging", "--action", "start", "--receipt", "receipt.json"],
        env: ENV,
        fetchImpl: mock.fetchImpl,
        now: () => NOW,
        readReceipt: () => offlineReceipt({messages: [message]}),
      }),
      /unsupported message/u,
    );
    assert.deepEqual(mock.mutations, []);
  }
});

test("start rejects stale, partial, or oversized offline receipts", async () => {
  for (const receipt of [
    offlineReceipt({checked_at: "2026-09-01T15:00:00.000Z"}),
    offlineReceipt({message_count: 0, messages: []}),
  ]) {
    const mock = cloudflare();
    await assert.rejects(
      runRedrive({
        argv: ["--environment", "staging", "--action", "start", "--receipt", "receipt.json"],
        env: ENV,
        fetchImpl: mock.fetchImpl,
        now: () => NOW,
        readReceipt: () => receipt,
      }),
      /receipt|bounded snapshot/u,
    );
    assert.deepEqual(mock.mutations, []);
  }

  const oversized = cloudflare({dlqBacklog: 101});
  await assert.rejects(
    runRedrive({
      argv: ["--environment", "staging", "--action", "start", "--receipt", "receipt.json"],
      env: ENV,
      fetchImpl: oversized.fetchImpl,
      now: () => NOW,
      readReceipt: () => offlineReceipt({
        message_count: 101,
        messages: Array(101).fill({
          analytics_environment: "staging",
          body_sha256: LIVE_BODY_HASH,
          format_version: 1,
          kind: "telemetry_row",
        }),
      }),
    }),
    /bounded snapshot/u,
  );
});
