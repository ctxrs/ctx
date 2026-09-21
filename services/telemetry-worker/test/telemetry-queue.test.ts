import { expect, test, vi } from "vitest";

import { buildBlameProductReceipt } from "../src/blame-product-receipt";
import { TelemetryEventCollisionError } from "../src/database";
import {
  buildInstallStageRow,
  buildTelemetryIngestPlan,
  type TelemetryRow,
} from "../src/telemetry-ingest";
import { EVENT_NAMES, MAX_EVENT_BYTES } from "../src/telemetry-contract";
import {
  CLOUDFLARE_QUEUE_MESSAGE_LIMIT_BYTES,
  decodeTelemetryQueueMessage,
  encodeTelemetryQueueMessage,
  MAX_TELEMETRY_QUEUE_BODY_BYTES,
  MAX_TELEMETRY_QUEUE_UNCOMPRESSED_BYTES,
  serializedTelemetryQueueMessageBytes,
  TELEMETRY_QUEUE_FORMAT_VERSION,
  TelemetryQueueMessageTooLargeError,
  type TelemetryQueueCollisionVisibility,
  type TelemetryQueueMessage,
} from "../src/telemetry-queue";
import {
  INGEST_OPTIONS,
  ENV,
  CLIENT_PROFILE_ID,
  DATA_ROOT_ID,
  jsonRequest,
  INSTALL_ATTEMPT_ID,
  maximalSearchEvent,
  operationEvent,
  installStage,
  v1Batch,
  workerHarness,
} from "./worker-test-fixtures";
import {
  createTelemetryWorker,
  type TelemetryQueueProducer,
} from "../src/worker";
import {
  confirmTelemetryQueueBatchAdmission,
  telemetryQueueBatchChunks,
  TELEMETRY_QUEUE_BATCH_MAX_ENCODED_BYTES,
  TELEMETRY_QUEUE_BATCH_MAX_MESSAGES,
  type TelemetryQueueBatchEntry,
} from "../src/telemetry-queue-producer";

const COLLISION_VISIBILITY = {
  analytics_environment: "production" as const,
  endpoint: "telemetry_batch" as const,
  event_family: "operation_completed" as const,
  app_version: "0.26.0",
  field_shape_fingerprint: "f".repeat(64),
  field_shape_overflow: false,
  provider_classification: "neutral",
  size_bucket: "64kb_256kb",
};
const PRIMARY_QUEUE = "ctx-telemetry-ingest-prod";

test("proves every accepted queue family fits with high-entropy legal maxima", async () => {
  const cases = await legalMaximumCases();
  const receipts = await Promise.all(cases.map(async (item) => {
    const uncompressedBytes = serializedTelemetryQueueMessageBytes(item.message).byteLength;
    const encoded = await encodeTelemetryQueueMessage(item.message);
    await expect(decodeTelemetryQueueMessage(encoded)).resolves.toEqual(item.message);
    return {
      case: item.name,
      count: item.count,
      uncompressed_bytes: uncompressedBytes,
      compressed_bytes: encoded.byteLength,
      encoded_margin_bytes: MAX_TELEMETRY_QUEUE_BODY_BYTES - encoded.byteLength,
    };
  }));
  const telemetryFamilies = cases
    .filter((item) => item.eventFamily != null)
    .map((item) => item.eventFamily);
  const largest = receipts.reduce((left, right) => (
    right.compressed_bytes > left.compressed_bytes ? right : left
  ));

  expect(telemetryFamilies).toEqual([...EVENT_NAMES, "cli_invocation"]);
  expect(cases.every((item) => item.count === 1)).toBe(true);
  expect(largest.case).toBe("typed/operation_completed");
  expect(largest.uncompressed_bytes).toBeLessThan(MAX_TELEMETRY_QUEUE_UNCOMPRESSED_BYTES);
  expect(MAX_TELEMETRY_QUEUE_UNCOMPRESSED_BYTES - largest.uncompressed_bytes)
    .toBeGreaterThanOrEqual(48 * 1024);
  expect(largest.encoded_margin_bytes).toBeGreaterThanOrEqual(120 * 1024);
  expect(receipts).toEqual(EXPECTED_SIZE_RECEIPTS);
});

test("rejects a deterministic gzip expansion bomb past the uncompressed ceiling", async () => {
  const expanded = new Uint8Array(MAX_TELEMETRY_QUEUE_UNCOMPRESSED_BYTES + 1);
  expanded.fill(0x41);
  const compressed = await gzipBytes(expanded);

  expect(compressed.byteLength).toBeLessThan(2 * 1024);
  expect(compressed.byteLength).toBeLessThan(MAX_TELEMETRY_QUEUE_BODY_BYTES);
  await expect(decodeTelemetryQueueMessage(compressed)).resolves.toBeNull();
});

test("cause diagnostics fit existing wire and Queue limits on fully populated refreshes", async () => {
  const coverage = maximalProviderRefreshEvent();
  const coverageProperties = coverage.properties as Record<string, unknown>;
  coverageProperties.refresh_coverage_reason = "missing_terminal_authority";
  coverageProperties.refresh_failure_reason = "io_read_only_filesystem";
  const partial = {
    ...coverage,
    outcome: "success",
    properties: {
      ...coverageProperties,
      refresh_result: "partial",
      core_result: "partial",
      failure_scope: "mixed",
      failure_type: "mixed",
      failure_code: "none",
      refresh_source_failure_class: "source_changed",
    } as Record<string, unknown>,
  };
  for (const key of ["refresh_coverage_reason", "refresh_failure_reason", "refresh_failure_stage", "refresh_failure_kind",
    "refresh_retained_previous_generation"]) delete partial.properties[key];
  for (const event of [coverage, partial]) {
    expect(new TextEncoder().encode(JSON.stringify(event)).byteLength).toBeLessThan(MAX_EVENT_BYTES);
    const row = (await buildTelemetryIngestPlan(v1Batch([event]), INGEST_OPTIONS)).rows[0];
    const message = queueRow(row);
    expect(serializedTelemetryQueueMessageBytes(message).byteLength)
      .toBeLessThan(MAX_TELEMETRY_QUEUE_UNCOMPRESSED_BYTES);
    const encoded = await encodeTelemetryQueueMessage(message);
    expect(encoded.byteLength).toBeLessThan(MAX_TELEMETRY_QUEUE_BODY_BYTES);
    await expect(decodeTelemetryQueueMessage(encoded)).resolves.toEqual(message);
  }
});

test("rejects an over-ceiling producer message before compressible admission", async () => {
  const message = (await legalMaximumCases())[0]!.message;
  const oversized = {
    ...message,
    collision_visibility: {
      ...message.collision_visibility,
      app_version: "A".repeat(MAX_TELEMETRY_QUEUE_UNCOMPRESSED_BYTES),
    },
  } as TelemetryQueueMessage;

  expect(serializedTelemetryQueueMessageBytes(oversized).byteLength)
    .toBeGreaterThan(MAX_TELEMETRY_QUEUE_UNCOMPRESSED_BYTES);
  await expect(encodeTelemetryQueueMessage(oversized))
    .rejects.toBeInstanceOf(TelemetryQueueMessageTooLargeError);
});

test("admits only normalized content-free bytes and commits one message before ack", async () => {
  const harness = workerHarness();
  const response = await harness.worker.fetch(
    jsonRequest("/functions/v1/telemetry", v1Batch([operationEvent()])),
    ENV,
  );

  expect(response.status).toBe(204);
  expect(harness.createDatabaseClient).not.toHaveBeenCalled();
  expect(harness.queueBodies).toHaveLength(1);
  const serializedQueueBody = JSON.stringify(await harness.queueMessages());
  expect(serializedQueueBody).not.toContain(CLIENT_PROFILE_ID);
  expect(serializedQueueBody).not.toContain(DATA_ROOT_ID);

  const delivery = queueDelivery(harness.queueBodies[0]!);
  await harness.worker.queue({ queue: PRIMARY_QUEUE, messages: [delivery] }, ENV);

  expect(harness.insertTelemetryRows).toHaveBeenCalledOnce();
  expect(delivery.ack).toHaveBeenCalledOnce();
  expect(delivery.retry).not.toHaveBeenCalled();
});

test("sends one exact Uint8Array batch entry and awaits Queue admission", async () => {
  let sentBody: Uint8Array<ArrayBuffer> | undefined;
  let sentContentType: "bytes" | undefined;
  let release!: () => void;
  const gate = new Promise<void>((resolve) => { release = resolve; });
  const queue: TelemetryQueueProducer = {
    async sendBatch(entries: Iterable<TelemetryQueueBatchEntry>) {
      const [entry, ...rest] = Array.from(entries);
      const body = entry?.body;
      if (body instanceof ArrayBuffer || !ArrayBuffer.isView(body)) {
        throw new TypeError("queue bytes require an ArrayBufferView");
      }
      if (!(body instanceof Uint8Array) || !(body.buffer instanceof ArrayBuffer)) {
        throw new TypeError("queue bytes require an owned Uint8Array");
      }
      sentBody = body as Uint8Array<ArrayBuffer>;
      sentContentType = entry.contentType;
      if (rest.length > 0) throw new Error("unexpected additional queue entry");
      await gate;
    },
  };
  const worker = createTelemetryWorker({ now: () => INGEST_OPTIONS.now() });
  let settled = false;
  const responsePromise = worker.fetch(
    jsonRequest("/functions/v1/telemetry", v1Batch([operationEvent()])),
    { ...ENV, TELEMETRY_INGEST_QUEUE: queue },
  ).then((response) => {
    settled = true;
    return response;
  });

  await vi.waitFor(() => expect(sentBody).toBeDefined());
  await Promise.resolve();
  expect(settled).toBe(false);
  expect(sentContentType).toBe("bytes");
  expect(ArrayBuffer.isView(sentBody)).toBe(true);
  expect(sentBody).toBeInstanceOf(Uint8Array);
  expect(sentBody!.byteOffset).toBe(0);
  expect(sentBody!.byteLength).toBe(sentBody!.buffer.byteLength);
  const decoded = await decodeTelemetryQueueMessage(sentBody);
  expect(decoded).not.toBeNull();
  expect(new Uint8Array(await encodeTelemetryQueueMessage(decoded!))).toEqual(sentBody);

  release();
  expect((await responsePromise).status).toBe(204);
});

test("chunks encoded Queue entries deterministically at byte and message boundaries", () => {
  const entry = (size: number, fill: number): TelemetryQueueBatchEntry => ({
    body: new Uint8Array(size).fill(fill),
    contentType: "bytes",
  });
  const byteBounded = [entry(120_000, 1), entry(120_000, 2), entry(1, 3)];
  const chunks = telemetryQueueBatchChunks(byteBounded);

  expect(chunks.map((chunk) => chunk.length)).toEqual([2, 1]);
  expect(chunks.flat()).toEqual(byteBounded);
  expect(chunks.map((chunk) => chunk.reduce((total, item) => total + item.body.byteLength, 0)))
    .toEqual([TELEMETRY_QUEUE_BATCH_MAX_ENCODED_BYTES, 1]);

  const countBounded = telemetryQueueBatchChunks(
    Array.from({ length: TELEMETRY_QUEUE_BATCH_MAX_MESSAGES + 1 }, () => entry(1, 4)),
  );
  expect(countBounded.map((chunk) => chunk.length)).toEqual([
    TELEMETRY_QUEUE_BATCH_MAX_MESSAGES,
    1,
  ]);
});

test("waits for concurrent chunks and reissues stable entries after partial admission failure", async () => {
  const entries = ["event-a", "event-b", "event-c"].map((eventId) => ({
    body: new TextEncoder().encode(eventId),
    contentType: "bytes" as const,
  }));
  const chunks = [entries.slice(0, 2), entries.slice(2)];
  const sent: string[][] = [];
  let releaseFirst!: () => void;
  let rejectSecond!: (reason: Error) => void;
  const first = new Promise<void>((resolve) => { releaseFirst = resolve; });
  const second = new Promise<void>((_resolve, reject) => { rejectSecond = reject; });
  let calls = 0;
  const queue: TelemetryQueueProducer = {
    sendBatch(batch) {
      sent.push(Array.from(batch, ({ body }) => new TextDecoder().decode(body)));
      calls += 1;
      if (calls === 1) return first;
      if (calls === 2) return second;
      return Promise.resolve();
    },
  };
  let settled = false;
  const admission = confirmTelemetryQueueBatchAdmission(queue, chunks).finally(() => {
    settled = true;
  });

  await vi.waitFor(() => expect(sent).toHaveLength(2));
  rejectSecond(new Error("second_chunk_unavailable"));
  await Promise.resolve();
  expect(settled).toBe(false);
  releaseFirst();
  await expect(admission).rejects.toThrow("second_chunk_unavailable");

  await confirmTelemetryQueueBatchAdmission(queue, chunks);
  expect(sent).toEqual([
    ["event-a", "event-b"],
    ["event-c"],
    ["event-a", "event-b"],
    ["event-c"],
  ]);
});

test("commits an exact hash-bound HTTP redrive wrapper through the primary consumer", async () => {
  const source = workerHarness();
  await source.worker.fetch(
    jsonRequest("/functions/v1/telemetry", v1Batch([operationEvent()])),
    ENV,
  );
  const body = source.queueBodies[0]!;
  const consumer = workerHarness();
  const delivery = queueDelivery(await redriveWrapper(body));

  await consumer.worker.queue({
    queue: PRIMARY_QUEUE,
    messages: [delivery],
  }, ENV);

  expect(consumer.insertTelemetryRows).toHaveBeenCalledOnce();
  expect(delivery.ack).toHaveBeenCalledOnce();
  expect(delivery.retry).not.toHaveBeenCalled();
});

test("rejects a redrive wrapper whose body hash does not match", async () => {
  const source = workerHarness();
  await source.worker.fetch(
    jsonRequest("/functions/v1/telemetry", v1Batch([operationEvent()])),
    ENV,
  );
  const wrapper = await redriveWrapper(source.queueBodies[0]!);
  const retryDelivery = queueDelivery({...wrapper, body_sha256: "f".repeat(64)});
  const consumer = workerHarness();
  const retryLog = vi.spyOn(console, "error").mockImplementation(() => {});
  await consumer.worker.queue({
    queue: PRIMARY_QUEUE,
    messages: [retryDelivery],
  }, ENV);
  expect(retryDelivery.ack).not.toHaveBeenCalled();
  expect(retryDelivery.retry).toHaveBeenCalledWith({ delaySeconds: 300 });
  expect(consumer.createDatabaseClient).not.toHaveBeenCalled();
  expect(retryLog).toHaveBeenCalledWith("telemetry_queue_message_invalid");
  retryLog.mockRestore();
});

test.each([
  ["oversized normalized body", {
    body_base64: "A".repeat(Math.ceil(MAX_TELEMETRY_QUEUE_UNCOMPRESSED_BYTES / 3) * 4 + 4),
    body_sha256: "0".repeat(64),
    content_encoding: "identity",
    content_type: "application/json",
    format_version: 1,
    kind: "telemetry_queue_redrive",
  }],
  ["invalid nested message", await redriveIdentityWrapper(
    new TextEncoder().encode('{"format_version":1}'),
  )],
])("rejects a hash-bound redrive wrapper with %s", async (_name, wrapper) => {
  const consumer = workerHarness();
  const delivery = queueDelivery(wrapper);
  const retryLog = vi.spyOn(console, "error").mockImplementation(() => {});

  await consumer.worker.queue({queue: PRIMARY_QUEUE, messages: [delivery]}, ENV);

  expect(delivery.ack).not.toHaveBeenCalled();
  expect(delivery.retry).toHaveBeenCalledWith({delaySeconds: 300});
  expect(consumer.createDatabaseClient).not.toHaveBeenCalled();
  expect(retryLog).toHaveBeenCalledWith("telemetry_queue_message_invalid");
  retryLog.mockRestore();
});

test.each(["unexpected-queue", "ctx-telemetry-ingest-prod-dlq"])(
  "fences Queue binding %s before opening Neon",
  async (queue) => {
  const harness = workerHarness();
  await harness.worker.fetch(
    jsonRequest("/functions/v1/telemetry", v1Batch([operationEvent()])),
    ENV,
  );
  const delivery = queueDelivery(harness.queueBodies[0]!);
  const error = vi.spyOn(console, "error").mockImplementation(() => {});

  await harness.worker.queue({ queue, messages: [delivery] }, ENV);

  expect(delivery.ack).not.toHaveBeenCalled();
  expect(delivery.retry).toHaveBeenCalledWith({ delaySeconds: 300 });
  expect(harness.createDatabaseClient).not.toHaveBeenCalled();
  expect(error).toHaveBeenCalledWith("telemetry_queue_binding_mismatch");
  error.mockRestore();
  },
);

test("replays an identical delivery through the idempotent transaction", async () => {
  const harness = workerHarness();
  await harness.worker.fetch(
    jsonRequest("/functions/v1/telemetry", v1Batch([operationEvent()])),
    ENV,
  );
  const first = queueDelivery(harness.queueBodies[0]!);
  const replay = queueDelivery(harness.queueBodies[0]!);

  await harness.worker.queue({ queue: PRIMARY_QUEUE, messages: [first] }, ENV);
  await harness.worker.queue({ queue: PRIMARY_QUEUE, messages: [replay] }, ENV);

  expect(harness.insertTelemetryRows).toHaveBeenCalledTimes(2);
  expect(harness.insertTelemetryRows.mock.calls[1]?.[0])
    .toEqual(harness.insertTelemetryRows.mock.calls[0]?.[0]);
  expect(first.ack).toHaveBeenCalledOnce();
  expect(replay.ack).toHaveBeenCalledOnce();
});

test("gives identical legacy install retries one stable event id", async () => {
  const harness = workerHarness();
  const requestBody = {
    install_attempt_id: INSTALL_ATTEMPT_ID,
    stage: "artifact_download_completed",
    status: "completed",
    error_kind: "",
    platform: "linux-x64",
    channel: "stable",
    version: "0.26.0",
  };
  await harness.worker.fetch(jsonRequest("/functions/v1/install-attempt", requestBody), ENV);
  await harness.worker.fetch(jsonRequest("/functions/v1/install-attempt", requestBody), ENV);
  const messages = await harness.queueMessages();
  if (
    messages[0]?.kind !== "install_stage_row"
    || messages[1]?.kind !== "install_stage_row"
  ) throw new Error("expected_install_rows");

  expect(messages[1].row.event_id).toBe(messages[0].row.event_id);
  expect(messages[1].row.payload_fingerprint).toBe(messages[0].row.payload_fingerprint);
  const first = queueDelivery(harness.queueBodies[0]!);
  const replay = queueDelivery(harness.queueBodies[0]!);
  await harness.worker.queue({ queue: PRIMARY_QUEUE, messages: [first, replay] }, ENV);

  expect(harness.insertInstallStageRow).toHaveBeenCalledTimes(2);
  expect(harness.insertInstallStageRow.mock.calls[1]?.[0])
    .toEqual(harness.insertInstallStageRow.mock.calls[0]?.[0]);
  expect(first.ack).toHaveBeenCalledOnce();
  expect(replay.ack).toHaveBeenCalledOnce();
});

test("retries transient consumer failures without logging raw errors or message values", async () => {
  const sensitive = `database failed for ${CLIENT_PROFILE_ID}`;
  const harness = workerHarness({ telemetryError: new Error(sensitive) });
  await harness.worker.fetch(
    jsonRequest("/functions/v1/telemetry", v1Batch([operationEvent()])),
    ENV,
  );
  const delivery = queueDelivery(harness.queueBodies[0]!);
  const error = vi.spyOn(console, "error").mockImplementation(() => {});

  await harness.worker.queue({ queue: PRIMARY_QUEUE, messages: [delivery] }, ENV);

  expect(delivery.ack).not.toHaveBeenCalled();
  expect(delivery.retry).toHaveBeenCalledWith({ delaySeconds: 300 });
  expect(error).toHaveBeenCalledWith("telemetry_queue_consumer_retry");
  expect(JSON.stringify(error.mock.calls)).not.toContain(sensitive);
  expect(JSON.stringify(error.mock.calls)).not.toContain(CLIENT_PROFILE_ID);
  error.mockRestore();
});

test("persists bounded collision visibility before ack and retries visibility failures", async () => {
  const collision = vi.spyOn(console, "warn").mockImplementation(() => {});
  const harness = workerHarness({ telemetryError: new TelemetryEventCollisionError() });
  await harness.worker.fetch(
    jsonRequest("/functions/v1/telemetry", v1Batch([operationEvent()])),
    ENV,
  );
  const delivery = queueDelivery(harness.queueBodies[0]!);

  await harness.worker.queue({ queue: PRIMARY_QUEUE, messages: [delivery] }, ENV);

  expect(harness.recordIngestRejection).toHaveBeenCalledWith(
    {
      analytics_environment: "production",
      endpoint: "telemetry_batch",
      event_family: "operation_completed",
      rejection_class: "event_collision",
      rejection_code: "event_id_collision",
      app_version: "0.26.0",
      field_shape_fingerprint: expect.stringMatching(/^[0-9a-f]{64}$/u),
      field_shape_overflow: false,
      provider_classification: "neutral",
      size_bucket: expect.stringMatching(/^(?:lt_1kb|1kb_8kb)$/u),
    },
    expect.stringMatching(/^[0-9a-f]{64}$/u),
  );
  expect(delivery.ack).toHaveBeenCalledOnce();
  expect(delivery.retry).not.toHaveBeenCalled();
  expect(collision).toHaveBeenCalledWith("telemetry_queue_event_id_collision");
  expect(JSON.stringify(collision.mock.calls)).not.toContain(CLIENT_PROFILE_ID);
  collision.mockRestore();

  const retryLog = vi.spyOn(console, "error").mockImplementation(() => {});
  const failedVisibility = workerHarness({
    telemetryError: new TelemetryEventCollisionError(),
    rejectionError: new Error("visibility unavailable"),
  });
  await failedVisibility.worker.fetch(
    jsonRequest("/functions/v1/telemetry", v1Batch([operationEvent()])),
    ENV,
  );
  const retryDelivery = queueDelivery(failedVisibility.queueBodies[0]!);
  await failedVisibility.worker.queue({ queue: PRIMARY_QUEUE, messages: [retryDelivery] }, ENV);
  expect(retryDelivery.ack).not.toHaveBeenCalled();
  expect(retryDelivery.retry).toHaveBeenCalledWith({ delaySeconds: 300 });
  expect(retryLog).toHaveBeenCalledWith("telemetry_queue_collision_visibility_retry");
  retryLog.mockRestore();
});

test("retries an invalid durable message to its DLQ without opening Neon", async () => {
  const harness = workerHarness();
  const delivery = queueDelivery(asArrayBuffer(new TextEncoder().encode("raw request body")));
  const error = vi.spyOn(console, "error").mockImplementation(() => {});

  await harness.worker.queue({ queue: PRIMARY_QUEUE, messages: [delivery] }, ENV);

  expect(delivery.ack).not.toHaveBeenCalled();
  expect(delivery.retry).toHaveBeenCalledWith({ delaySeconds: 300 });
  expect(harness.createDatabaseClient).not.toHaveBeenCalled();
  expect(error).toHaveBeenCalledWith("telemetry_queue_message_invalid");
  expect(JSON.stringify(error.mock.calls)).not.toContain("raw request body");
  error.mockRestore();
});

test("retries an unknown queue version to its DLQ without opening Neon", async () => {
  const harness = workerHarness();
  const body = await gzipJson({ format_version: 2, kind: "future_message" });
  const delivery = queueDelivery(body);
  const error = vi.spyOn(console, "error").mockImplementation(() => {});

  await harness.worker.queue({ queue: PRIMARY_QUEUE, messages: [delivery] }, ENV);

  expect(delivery.ack).not.toHaveBeenCalled();
  expect(delivery.retry).toHaveBeenCalledWith({ delaySeconds: 300 });
  expect(harness.createDatabaseClient).not.toHaveBeenCalled();
  expect(error).toHaveBeenCalledWith("telemetry_queue_message_invalid");
  error.mockRestore();
});

test("backs off by delivery attempt and caps the retry delay", async () => {
  const harness = workerHarness();
  const body = asArrayBuffer(new TextEncoder().encode("invalid"));
  const cases = [
    { attempts: 1, delaySeconds: 300 },
    { attempts: 2, delaySeconds: 600 },
    { attempts: 5, delaySeconds: 4_800 },
    { attempts: 9, delaySeconds: 43_200 },
    { attempts: 20, delaySeconds: 43_200 },
  ];
  const error = vi.spyOn(console, "error").mockImplementation(() => {});

  for (const item of cases) {
    const delivery = queueDelivery(body, item.attempts);
    await harness.worker.queue({ queue: PRIMARY_QUEUE, messages: [delivery] }, ENV);
    expect(delivery.retry).toHaveBeenCalledWith({ delaySeconds: item.delaySeconds });
  }

  expect(harness.createDatabaseClient).not.toHaveBeenCalled();
  error.mockRestore();
});

test("fences cross-environment and invalid-environment deliveries before Neon", async () => {
  const harness = workerHarness();
  await harness.worker.fetch(
    jsonRequest("/functions/v1/telemetry", v1Batch([operationEvent()])),
    ENV,
  );
  const productionBody = harness.queueBodies[0]!;
  const productionMessage = (await harness.queueMessages())[0]!;
  if (productionMessage.kind !== "telemetry_row") throw new Error("expected_row");
  const mismatchedRowBody = await encodeTelemetryQueueMessage({
    ...productionMessage,
    row: {
      ...productionMessage.row,
      analytics_environment: "staging" as const,
    },
  });
  const cases = [
    {
      body: productionBody,
      env: { ...ENV, TELEMETRY_ANALYTICS_ENVIRONMENT: "staging" },
    },
    { body: mismatchedRowBody, env: ENV },
    {
      body: productionBody,
      env: { ...ENV, TELEMETRY_ANALYTICS_ENVIRONMENT: "preview" },
    },
  ];
  const error = vi.spyOn(console, "error").mockImplementation(() => {});

  for (const item of cases) {
    const delivery = queueDelivery(item.body);
    await harness.worker.queue({ queue: PRIMARY_QUEUE, messages: [delivery] }, item.env);
    expect(delivery.ack).not.toHaveBeenCalled();
    expect(delivery.retry).toHaveBeenCalledWith({ delaySeconds: 300 });
  }

  expect(harness.createDatabaseClient).not.toHaveBeenCalled();
  expect(error).toHaveBeenCalledTimes(cases.length);
  expect(error).toHaveBeenCalledWith("telemetry_queue_environment_mismatch");
  error.mockRestore();
});

test("makes whole-request retry stable after an ambiguous batch admission failure", async () => {
  const attempts: Uint8Array<ArrayBuffer>[][] = [];
  const queue: TelemetryQueueProducer = {
    async sendBatch(entries) {
      attempts.push(Array.from(entries, ({ body }) => body));
      if (attempts.length === 1) throw new Error("deterministic_partial_failure");
    },
  };
  const worker = createTelemetryWorker({ now: () => INGEST_OPTIONS.now() });
  const events = [0, 1, 2].map((index) => operationEvent({
    event_id: `00000000-0000-4000-8000-${index.toString(16).padStart(12, "0")}`,
  }));
  const request = () => jsonRequest("/functions/v1/telemetry", v1Batch(events));
  const environment = { ...ENV, TELEMETRY_INGEST_QUEUE: queue };
  expect((await worker.fetch(request(), environment)).status).toBe(503);
  expect((await worker.fetch(request(), environment)).status).toBe(204);
  expect(attempts).toHaveLength(2);

  const decoded = await Promise.all(attempts.flat().map(decodeTelemetryQueueMessage));
  expect(decoded.every((message) => message?.kind === "telemetry_row")).toBe(true);
  const firstAttempt = decoded.slice(0, 3).map(eventIdentity)
    .sort(([left], [right]) => left.localeCompare(right));
  const retryAttempt = decoded.slice(3).map(eventIdentity)
    .sort(([left], [right]) => left.localeCompare(right));
  expect(firstAttempt).toEqual(retryAttempt);
});

test("rejects invalid normalized data at every consumer message family", async () => {
  for (const item of await legalMaximumCases()) {
    const invalid = structuredClone(item.message) as TelemetryQueueMessage;
    if (invalid.kind === "telemetry_row") {
      (invalid.row as { success: boolean }).success = !invalid.row.success;
    } else if (invalid.kind === "install_stage_row") {
      (invalid.row as { event_id: string | null }).event_id = "00000000-0000-4000-8000-000000000000";
    } else {
      (invalid.receipt as { activity_class: string }).activity_class = "setup";
    }
    await expect(decodeTelemetryQueueMessage(await gzipJson(invalid))).resolves.toBeNull();
  }
});

test("rejects activity and kind metadata mismatches before opening Neon", async () => {
  const cases = await legalMaximumCases();
  const telemetry = structuredClone(cases.find((item) => item.name === "typed/operation_completed")?.message);
  const install = structuredClone(cases.find((item) => item.name === "install/current")?.message);
  const receipt = structuredClone(cases.find((item) => item.name === "blame/v1")?.message);
  if (
    telemetry?.kind !== "telemetry_row"
    || install?.kind !== "install_stage_row"
    || receipt?.kind !== "blame_product_receipt"
  ) throw new Error("expected_queue_message_families");
  const activityMismatch: TelemetryQueueMessage = {
    ...telemetry,
    row: { ...telemetry.row, activity_class: "automatic" },
  };
  const telemetryMetadataMismatch: TelemetryQueueMessage = {
    ...telemetry,
    collision_visibility: {
      ...telemetry.collision_visibility,
      event_family: "runtime_observation",
    },
  };
  const installMetadataMismatch: TelemetryQueueMessage = {
    ...install,
    collision_visibility: {
      ...install.collision_visibility,
      endpoint: "telemetry_batch",
      event_family: "operation_completed",
    },
  };
  const receiptMetadataMismatch: TelemetryQueueMessage = {
    ...receipt,
    collision_visibility: {
      ...receipt.collision_visibility,
      endpoint: "install_stage",
      event_family: "install_stage",
    },
  };
  const harness = workerHarness();
  const error = vi.spyOn(console, "error").mockImplementation(() => {});

  for (const message of [
    activityMismatch,
    telemetryMetadataMismatch,
    installMetadataMismatch,
    receiptMetadataMismatch,
  ]) {
    const delivery = queueDelivery(await encodeTelemetryQueueMessage(message));
    await harness.worker.queue({ queue: PRIMARY_QUEUE, messages: [delivery] }, ENV);
    expect(delivery.ack).not.toHaveBeenCalled();
    expect(delivery.retry).toHaveBeenCalledWith({ delaySeconds: 300 });
  }

  expect(harness.createDatabaseClient).not.toHaveBeenCalled();
  expect(error).toHaveBeenCalledTimes(4);
  expect(error).toHaveBeenCalledWith("telemetry_queue_message_invalid");
  error.mockRestore();
});

const EXPECTED_SIZE_RECEIPTS: readonly {
  case: string;
  count: number;
  uncompressed_bytes: number;
  compressed_bytes: number;
  encoded_margin_bytes: number;
}[] = [
  {
    case: "typed/analytics_delivery_observation",
    count: 1,
    uncompressed_bytes: 1701,
    compressed_bytes: 770,
    encoded_margin_bytes: 127030,
  },
  {
    case: "typed/operation_completed",
    count: 1,
    uncompressed_bytes: 4284,
    compressed_bytes: 1540,
    encoded_margin_bytes: 126260,
  },
  {
    case: "typed/provider_refresh_completed",
    count: 1,
    uncompressed_bytes: 3601,
    compressed_bytes: 1379,
    encoded_margin_bytes: 126421,
  },
  {
    case: "typed/runtime_observation",
    count: 1,
    uncompressed_bytes: 2349,
    compressed_bytes: 1051,
    encoded_margin_bytes: 126749,
  },
  {
    case: "frozen/cli_invocation",
    count: 1,
    uncompressed_bytes: 3507,
    compressed_bytes: 1286,
    encoded_margin_bytes: 126514,
  },
  {
    case: "install/current",
    count: 1,
    uncompressed_bytes: 1018,
    compressed_bytes: 519,
    encoded_margin_bytes: 127281,
  },
  {
    case: "install/frozen",
    count: 1,
    uncompressed_bytes: 1033,
    compressed_bytes: 564,
    encoded_margin_bytes: 127236,
  },
  {
    case: "blame/v1",
    count: 1,
    uncompressed_bytes: 1437,
    compressed_bytes: 698,
    encoded_margin_bytes: 127102,
  },
  {
    case: "blame/v2",
    count: 1,
    uncompressed_bytes: 1314,
    compressed_bytes: 663,
    encoded_margin_bytes: 127137,
  },
];

async function legalMaximumCases(): Promise<readonly {
  name: string;
  count: 1;
  eventFamily?: string;
  message: TelemetryQueueMessage;
}[]> {
  const typedEvents = [
    {
      name: "typed/analytics_delivery_observation",
      event: {
        event_id: "11111111-1111-4111-8111-111111111111",
        event_name: "analytics_delivery_observation",
        event_version: 1,
        occurred_at: "2026-07-22T18:34:00Z",
        surface: "cli",
        operation: "outbox",
        outcome: "failure",
        duration_bucket: "gte_1h",
        properties: {
          queued_count_bucket: "1m+",
          retry_attempt_count_bucket: "1m+",
          dropped_count_bucket: "1m+",
          oldest_queued_age_bucket: "gte_1h",
          failure_class: "configuration",
        },
      },
    },
    { name: "typed/operation_completed", event: maximalSearchEvent(2) },
    {
      name: "typed/provider_refresh_completed",
      event: maximalProviderRefreshEvent(),
    },
    { name: "typed/runtime_observation", event: maximalRuntimeEvent() },
  ] as const;
  const cases: Array<{
    name: string;
    count: 1;
    eventFamily?: string;
    message: TelemetryQueueMessage;
  }> = [];
  for (const item of typedEvents) {
    const row = (await buildTelemetryIngestPlan(v1Batch([item.event]), INGEST_OPTIONS)).rows[0]!;
    cases.push({
      name: item.name,
      count: 1,
      eventFamily: row.event_name,
      message: queueRow(row),
    });
  }
  const frozenRow = (await buildTelemetryIngestPlan(maximalFrozenBatch(), INGEST_OPTIONS)).rows[0]!;
  cases.push({
    name: "frozen/cli_invocation",
    count: 1,
    eventFamily: frozenRow.event_name,
    message: queueRow(frozenRow),
  });
  const currentInstall = await buildInstallStageRow(installStage({
    install_attempt_id: `ia_${"Q7z_".repeat(32)}`,
    stage: "binary_install",
    status: "completed",
    platform: "windows",
    arch: "x64",
    script_family: "powershell",
  }), INGEST_OPTIONS);
  const frozenInstall = await buildInstallStageRow({
    install_attempt_id: `ia_${"M9x-".repeat(32)}`,
    stage: "artifact_download_completed",
    status: "failed",
    error_kind: "checksum_mismatch",
    platform: "windows-x64",
    channel: "canary",
    version: "9.8.7-rc.1234567890+high_entropy_build_abcdef",
  }, INGEST_OPTIONS);
  cases.push(
    { name: "install/current", count: 1, message: queueInstall(currentInstall) },
    { name: "install/frozen", count: 1, message: queueInstall(frozenInstall) },
  );
  for (const version of [1, 2] as const) {
    const payload = blameBatch(version);
    const plan = await buildTelemetryIngestPlan(payload, {
      ...INGEST_OPTIONS,
      verifiedInstallationCoordinate: "install:7Qz9mK4xN2pR8vW5",
    });
    const receipt = await buildBlameProductReceipt(
      plan.rows,
      "install:7Qz9mK4xN2pR8vW5",
      INGEST_OPTIONS.identityHmacKey,
      INGEST_OPTIONS.identityKeyVersion,
    );
    if (!receipt) throw new Error("expected_blame_receipt");
    cases.push({
      name: `blame/v${version}`,
      count: 1,
      message: {
        format_version: TELEMETRY_QUEUE_FORMAT_VERSION,
        kind: "blame_product_receipt",
        collision_visibility: COLLISION_VISIBILITY,
        receipt,
      },
    });
  }
  return cases;
}

function queueRow(row: TelemetryRow): TelemetryQueueMessage {
  return {
    format_version: TELEMETRY_QUEUE_FORMAT_VERSION,
    kind: "telemetry_row",
    collision_visibility: {
      ...COLLISION_VISIBILITY,
      event_family: row.event_name as TelemetryQueueCollisionVisibility["event_family"],
    },
    row,
  };
}

function queueInstall(row: Awaited<ReturnType<typeof buildInstallStageRow>>): TelemetryQueueMessage {
  return {
    format_version: TELEMETRY_QUEUE_FORMAT_VERSION,
    kind: "install_stage_row",
    collision_visibility: {
      ...COLLISION_VISIBILITY,
      endpoint: "install_stage",
      event_family: "install_stage",
    },
    row,
  };
}

function eventIdentity(message: TelemetryQueueMessage | null): readonly [string, string] {
  if (message?.kind !== "telemetry_row") throw new Error("expected_row");
  return [message.row.event_id, message.row.payload_fingerprint];
}

function maximalProviderRefreshEvent(): Record<string, unknown> {
  return {
    event_id: "44444444-4444-4444-8444-444444444444",
    event_name: "provider_refresh_completed",
    event_version: 1,
    occurred_at: "2026-07-22T18:34:00Z",
    surface: "daemon",
    operation: "refresh",
    outcome: "failure",
    duration_bucket: "gte_1h",
    install_attempt_id: `ia_${"R5n_".repeat(32)}`,
    properties: {
      install_manager: "ctx-hosted-installer",
      capability_snapshot_schema: 1,
      available_parallelism_bucket: "65+",
      host_memory_bucket: "64gb+",
      cpu_vector_tier: "x86_baseline",
      acceleration_candidate: "nvidia_cuda",
      provider: "factory_ai_droid",
      trigger: "daemon",
      source_mode: "history_source_plugin",
      change: "changed",
      work_remaining: true,
      sources_bucket: "1m+",
      source_files_bucket: "1m+",
      sessions_bucket: "1m+",
      events_bucket: "1m+",
      edges_bucket: "1m+",
      skips_bucket: "1m+",
      rejections_bucket: "1m+",
      failures_bucket: "1m+",
      records_bucket: "1m+",
      bytes_bucket: "100gb+",
      logical_bytes_bucket: "100gb+",
      refresh_queue_wait_duration_bucket: "gte_1h",
      refresh_discovery_duration_bucket: "gte_1h",
      refresh_scan_stage_duration_bucket: "gte_1h",
      refresh_commit_duration_bucket: "gte_1h",
      refresh_coalesced_request_count_bucket: "1m+",
      refresh_successor_pending: true,
      refresh_configured_indexing_mode: "automatic",
      refresh_daemon_trigger_kind: "periodic_reconciliation",
      refresh_reconciliation_demand: "exhaustive",
      refresh_retained_previous_generation: true,
      refresh_processed_sessions_bucket: "1m+",
      refresh_processed_messages_bucket: "1m+",
      refresh_processed_tool_calls_bucket: "1m+",
      refresh_processed_bytes_bucket: "100gb+",
      corpus_stock_indexed_documents_bucket: "1m+",
      corpus_stock_retained_records_bucket: "1m+",
      corpus_stock_rejected_records_bucket: "1m+",
      corpus_transition_removed_sources_bucket: "1m+",
      corpus_stock_certified_source_bytes_bucket: "100gb+",
      content_evidence: "unknown",
      work_kind: "replace",
      refresh_result: "failure",
      core_result: "failure",
      canonical_pro_result: "failure",
      output_pro_result: "failure",
      failure_scope: "system",
      failure_type: "unsupported_schema",
      failure_code: "all_provider_terminal_coverage_unavailable",
      retryable: true,
      refresh_failure_stage: "verification",
      refresh_failure_kind: "provider",
      retired_records_bucket: "1m+",
      cpu_duration_bucket: "gte_1h",
      observed_process_peak_rss_bucket: "100gb+",
    },
  };
}

function maximalRuntimeEvent(): Record<string, unknown> {
  return {
    event_id: "55555555-5555-4555-8555-555555555555",
    event_name: "runtime_observation",
    event_version: 1,
    occurred_at: "2026-07-22T18:34:00Z",
    surface: "daemon",
    operation: "liveness",
    outcome: "success",
    duration_bucket: "gte_1h",
    install_attempt_id: `ia_${"T3v-".repeat(32)}`,
    properties: {
      install_manager: "ctx-hosted-installer",
      capability_snapshot_schema: 1,
      available_parallelism_bucket: "65+",
      host_memory_bucket: "64gb+",
      cpu_vector_tier: "x86_baseline",
      acceleration_candidate: "nvidia_cuda",
      start_mode: "manual",
      supervisor: "cli_autostart",
      trigger_command: "semantic",
      history_freshness: "unknown",
      semantic_backlog_bucket: "1m+",
      semantic_coverage: "incomplete",
      retry_backoff: "semantic",
      filesystem_total_bytes_bucket: "5tb+",
      filesystem_available_bytes_bucket: "5tb+",
      filesystem_available_fraction_bucket: "60pct+",
      core_active_logical_bytes_bucket: "5tb+",
      core_certified_source_bytes_bucket: "5tb+",
      core_logical_amplification_bucket: "2x+",
      filesystem_available_to_active_core_ratio_bucket: "4x+",
    },
  };
}

function maximalFrozenBatch(): Record<string, unknown> {
  const dataRoot = "8d5c3a1f-7b29-4e60-9c42-f1a8d6e30b75";
  const profile = "b741e29c-65d8-4f03-a917-2c86e50d9ab4";
  return {
    broker_install_id: dataRoot,
    broker_device_id: profile,
    broker_runtime: "cli",
    broker_app_version: "0.25.0",
    broker_os: "freebsd",
    broker_arch: "aarch64",
    events: [{
      event_id: "018f1f2e-7b3c-7abc-8def-0123456789ab",
      event_name: "cli_invocation",
      event_version: 1,
      occurred_at: "2026-07-22T18:34:00.000Z",
      plane: "product",
      delivery: "remote",
      origin_runtime: "cli",
      origin_install_id: dataRoot,
      origin_device_id: profile,
      app_version: "0.25.0",
      os: "freebsd",
      arch: "aarch64",
      surface: "cli",
      source: "ctx-cli",
      duration_ms: 30_000,
      duration_bucket: "gte_30s",
      status: "ok",
      success: true,
      install_attempt_id: "V8p_".repeat(32),
      properties: {
        action: "search",
        json_output: true,
        analytics_client: "ctx-cli",
        install_manager: "ctx-hosted-installer",
        capability_snapshot_schema: 1,
        available_parallelism_bucket: "65+",
        host_memory_bucket: "64gb+",
        cpu_vector_tier: "x86_baseline",
        acceleration_candidate: "nvidia_cuda",
        auto_upgrade_probe: true,
        auto_upgrade_due: true,
        auto_upgrade_spawned: false,
        auto_upgrade_spawn_status: "current_exe_error",
        upgrade_channel: "canary",
        has_query: true,
        has_provider_filter: true,
        has_workspace_filter: true,
        has_since_filter: true,
        has_event_type_filter: true,
        has_file_filter: true,
        has_session_filter: true,
        event_results: true,
        primary_only: true,
        include_subagents: true,
        include_current_session: true,
        limit_bucket: "1k+",
        provider_filter: "factory_ai_droid",
        had_existing_store_before_search: true,
        indexed_content_before_search_known: true,
        had_indexed_content_before_search: true,
        refresh_duration_bucket: "gte_30s",
        search_refresh_mode: "background",
        search_refresh_status: "daemon_background",
        search_refresh_source_count_bucket: "1k+",
        db_size_bucket: "1gb+",
        store_created_by_search: true,
        indexed_sessions_bucket: "1k+",
        indexed_events_bucket: "1k+",
        indexed_items_bucket: "1k+",
        has_indexed_content_after_search: true,
        query_length_bucket: "500+",
        query_term_count_bucket: "1k+",
        query_duration_bucket: "gte_30s",
        search_backend_requested: "semantic",
        search_backend_effective: "semantic",
        result_count_bucket: "1k+",
        citation_count_bucket: "1k+",
        zero_result: false,
        render_duration_bucket: "gte_30s",
      },
    }],
  };
}

function blameBatch(version: 1 | 2): Record<string, unknown> {
  const properties = version === 1 ? {
    blame_schema_version: 1,
    blame_semantics_version: 1,
    blame_surface: "cli",
    blame_target_kind: "pull_request",
    blame_request_kind: "continuation",
    blame_access_state: "canceling_paid",
    blame_result_state: "conflicting",
    blame_freshness: "stale_committed",
    blame_has_more: true,
    blame_output_served: true,
    blame_pro_version: "9.8.7-rc.1234567890+high-entropy-build-abcdef",
    blame_pro_protocol_version: 65_535,
  } : {
    blame_schema_version: 2,
    blame_surface: "mcp",
    blame_target_kind: "pull_request",
    blame_request_kind: "continuation",
    blame_query_duration_bucket: "gte_1h",
    blame_result_state: "conflicting",
    blame_result_count_bucket: "1m+",
    blame_freshness: "stale_committed",
    blame_has_more: true,
  };
  return {
    app_version: "9.8.7-rc.1234567890+high-entropy-build-abcdef",
    os: "freebsd",
    arch: "aarch64",
    events: [{
      event_id: version === 1
        ? "66666666-6666-4666-8666-666666666666"
        : "77777777-7777-4777-8777-777777777777",
      event_name: "operation_completed",
      event_version: 1,
      occurred_at: "2026-07-22T18:34:00Z",
      surface: "pro_host",
      operation: "blame",
      outcome: "success",
      duration_bucket: "gte_1h",
      properties,
    }],
  };
}

function queueDelivery(body: unknown, attempts = 1) {
  return {
    body,
    attempts,
    ack: vi.fn(),
    retry: vi.fn(),
  };
}

async function gzipJson(value: unknown): Promise<ArrayBuffer> {
  const bytes = new TextEncoder().encode(JSON.stringify(value));
  return gzipBytes(bytes);
}

async function gzipBytes(bytes: Uint8Array): Promise<ArrayBuffer> {
  const stream = new Blob([asArrayBuffer(bytes)]).stream()
    .pipeThrough(new CompressionStream("gzip"));
  return new Response(stream).arrayBuffer();
}

function asArrayBuffer(bytes: Uint8Array): ArrayBuffer {
  return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer;
}

async function redriveWrapper(body: Uint8Array<ArrayBuffer>): Promise<Record<string, unknown>> {
  return redriveIdentityWrapper(await gunzipForTest(body));
}

async function redriveIdentityWrapper(bytes: Uint8Array): Promise<Record<string, unknown>> {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  const digest = await crypto.subtle.digest("SHA-256", asArrayBuffer(bytes));
  return {
    body_base64: btoa(binary),
    body_sha256: Array.from(
      new Uint8Array(digest),
      (byte) => byte.toString(16).padStart(2, "0"),
    ).join(""),
    content_encoding: "identity",
    content_type: "application/json",
    format_version: 1,
    kind: "telemetry_queue_redrive",
  };
}

async function gunzipForTest(body: Uint8Array<ArrayBuffer>): Promise<Uint8Array> {
  const reader = new Blob([body])
    .stream()
    .pipeThrough(new DecompressionStream("gzip"))
    .getReader();
  const chunks: Uint8Array[] = [];
  let byteLength = 0;
  while (true) {
    const result = await reader.read();
    if (result.done) break;
    chunks.push(result.value);
    byteLength += result.value.byteLength;
  }
  const output = new Uint8Array(byteLength);
  let offset = 0;
  for (const chunk of chunks) {
    output.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return output;
}
