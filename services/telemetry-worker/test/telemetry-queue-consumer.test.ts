import { afterEach, expect, test, vi } from "vitest";

import { buildBlameProductReceipt } from "../src/blame-product-receipt";
import {
  NeonTelemetryDatabase,
  TelemetryEventCollisionError,
  type NeonQueryClient,
  type TelemetryDatabase,
  type TelemetryIngestRejection,
} from "../src/database";
import {
  buildInstallStageRow,
  buildTelemetryIngestPlan,
  type TelemetryRow,
} from "../src/telemetry-ingest";
import {
  encodeTelemetryQueueMessage,
  TELEMETRY_QUEUE_FORMAT_VERSION,
  type TelemetryQueueMessage,
} from "../src/telemetry-queue";
import { createTelemetryWorker, type Env } from "../src/worker";
import {
  INGEST_OPTIONS,
  installStage,
  operationEvent,
  proOperationEvent,
  v1Batch,
} from "./worker-test-fixtures";

const SECOND_EVENT_ID = "44444444-4444-4444-8444-444444444444";
const PRIMARY_QUEUE = "ctx-telemetry-ingest-prod";
const CONSUMER_ENV: Env = {
  TELEMETRY_ANALYTICS_ENVIRONMENT: "production",
  TELEMETRY_DATABASE_URL: "postgresql://telemetry.example.test/db",
};
const COLLISION_VISIBILITY = {
  analytics_environment: "production" as const,
  endpoint: "telemetry_batch" as const,
  event_family: "operation_completed" as const,
  app_version: "0.26.0",
  field_shape_fingerprint: "f".repeat(64),
  field_shape_overflow: false,
  provider_classification: "neutral",
  size_bucket: "lt_1kb",
};

afterEach(() => {
  vi.restoreAllMocks();
});

test("persists a telemetry delivery with one database call and acks every row", async () => {
  const firstRow = await telemetryRow();
  const secondRow = await telemetryRow(SECOND_EVENT_ID);
  const database = createQueueDatabase();
  const worker = consumerFor(database.database);
  const first = queueDelivery(await queueBody(firstRow));
  const second = queueDelivery(await queueBody(secondRow));

  await worker.queue({ queue: PRIMARY_QUEUE, messages: [first, second] }, CONSUMER_ENV);

  expect(database.insertTelemetryRows).toHaveBeenCalledTimes(1);
  expect(database.insertTelemetryRows).toHaveBeenCalledWith([firstRow, secondRow]);
  expect(first.ack).toHaveBeenCalledOnce();
  expect(second.ack).toHaveBeenCalledOnce();
  expect(first.retry).not.toHaveBeenCalled();
  expect(second.retry).not.toHaveBeenCalled();
});

test("does not ack grouped telemetry rows before the bulk insert resolves", async () => {
  const firstRow = await telemetryRow();
  const secondRow = await telemetryRow(SECOND_EVENT_ID);
  const allowInsert = deferred();
  const insertStarted = deferred();
  const database = createQueueDatabase(async () => {
    insertStarted.resolve();
    await allowInsert.promise;
  });
  const worker = consumerFor(database.database);
  const first = queueDelivery(await queueBody(firstRow));
  const second = queueDelivery(await queueBody(secondRow));

  const consuming = worker.queue({ queue: PRIMARY_QUEUE, messages: [first, second] }, CONSUMER_ENV);
  await insertStarted.promise;
  expect(first.ack).not.toHaveBeenCalled();
  expect(second.ack).not.toHaveBeenCalled();

  allowInsert.resolve();
  await consuming;
  expect(first.ack).toHaveBeenCalledOnce();
  expect(second.ack).toHaveBeenCalledOnce();
});

test("retries every telemetry row when the grouped database write fails", async () => {
  const firstRow = await telemetryRow();
  const secondRow = await telemetryRow(SECOND_EVENT_ID);
  const database = createQueueDatabase(async () => {
    throw new Error("transient_database_failure");
  });
  const worker = consumerFor(database.database);
  const first = queueDelivery(await queueBody(firstRow), () => {}, 1);
  const second = queueDelivery(await queueBody(secondRow), () => {}, 2);
  const retryLog = vi.spyOn(console, "error").mockImplementation(() => {});

  await worker.queue({ queue: PRIMARY_QUEUE, messages: [first, second] }, CONSUMER_ENV);

  expect(database.insertTelemetryRows).toHaveBeenCalledTimes(1);
  expect(database.insertTelemetryRows).toHaveBeenCalledWith([firstRow, secondRow]);
  expect(first.ack).not.toHaveBeenCalled();
  expect(second.ack).not.toHaveBeenCalled();
  expect(first.retry).toHaveBeenCalledWith({ delaySeconds: 300 });
  expect(second.retry).toHaveBeenCalledWith({ delaySeconds: 600 });
  expect(retryLog).toHaveBeenCalledTimes(1);
  expect(retryLog).toHaveBeenCalledWith("telemetry_queue_consumer_retry");
});

test("keeps telemetry, install, and Blame persistence isolated in a mixed delivery", async () => {
  const row = await telemetryRow();
  const installRow = await buildInstallStageRow(installStage(), INGEST_OPTIONS);
  const receipt = await blameReceipt();
  const database = createQueueDatabase(
    async () => { throw new Error("telemetry_write_failed"); },
    async () => {},
    async () => { throw new Error("install_write_failed"); },
  );
  const worker = consumerFor(database.database);
  const telemetry = queueDelivery(await queueBody(row));
  const install = queueDelivery(await queueMessageBody({
    format_version: TELEMETRY_QUEUE_FORMAT_VERSION,
    kind: "install_stage_row",
    collision_visibility: {
      ...COLLISION_VISIBILITY,
      endpoint: "install_stage",
      event_family: "install_stage",
    },
    row: installRow,
  }));
  const blame = queueDelivery(await queueMessageBody({
    format_version: TELEMETRY_QUEUE_FORMAT_VERSION,
    kind: "blame_product_receipt",
    collision_visibility: COLLISION_VISIBILITY,
    receipt,
  }));
  const retryLog = vi.spyOn(console, "error").mockImplementation(() => {});

  await worker.queue({
    queue: PRIMARY_QUEUE,
    messages: [telemetry, install, blame],
  }, CONSUMER_ENV);

  expect(database.insertTelemetryRows).toHaveBeenCalledWith([row]);
  expect(database.insertInstallStageRow).toHaveBeenCalledWith(installRow);
  expect(database.insertBlameProductReceipt).toHaveBeenCalledWith(receipt);
  expect(telemetry.ack).not.toHaveBeenCalled();
  expect(install.ack).not.toHaveBeenCalled();
  expect(blame.ack).toHaveBeenCalledOnce();
  expect(telemetry.retry).toHaveBeenCalledWith({ delaySeconds: 300 });
  expect(install.retry).toHaveBeenCalledWith({ delaySeconds: 300 });
  expect(blame.retry).not.toHaveBeenCalled();
  expect(retryLog).toHaveBeenCalledTimes(2);
});

test("isolates telemetry collisions after a grouped write while preserving valid siblings", async () => {
  const collidingRow = await telemetryRow();
  const validRow = await telemetryRow(SECOND_EVENT_ID);
  const accountedReceipts = new Set<string>();
  const database = createQueueDatabase(
    async (rows) => {
      if (rows.length > 1 || rows[0]?.event_id === collidingRow.event_id) {
        throw new TelemetryEventCollisionError();
      }
    },
    async (_rejection, receiptId) => {
      if (!receiptId) throw new Error("missing_collision_receipt");
      accountedReceipts.add(receiptId);
    },
  );
  const worker = consumerFor(database.database);
  const colliding = queueDelivery(await queueBody(collidingRow));
  const valid = queueDelivery(await queueBody(validRow));
  const warning = vi.spyOn(console, "warn").mockImplementation(() => {});

  await worker.queue({ queue: PRIMARY_QUEUE, messages: [colliding, valid] }, CONSUMER_ENV);
  await worker.queue({
    queue: PRIMARY_QUEUE,
    messages: [queueDelivery(await queueBody(collidingRow))],
  }, CONSUMER_ENV);

  expect(database.insertTelemetryRows.mock.calls).toEqual([
    [[collidingRow, validRow]],
    [[collidingRow]],
    [[validRow]],
    [[collidingRow]],
    [[collidingRow]],
  ]);
  expect(database.recordIngestRejection).toHaveBeenCalledTimes(2);
  expect(database.recordIngestRejection.mock.calls[1]?.[1])
    .toBe(database.recordIngestRejection.mock.calls[0]?.[1]);
  expect(accountedReceipts.size).toBe(1);
  expect(colliding.ack).toHaveBeenCalledOnce();
  expect(valid.ack).toHaveBeenCalledOnce();
  expect(colliding.retry).not.toHaveBeenCalled();
  expect(valid.retry).not.toHaveBeenCalled();
  expect(warning).toHaveBeenCalledTimes(2);
});

test("keeps malformed siblings isolated from a valid telemetry delivery", async () => {
  const row = await telemetryRow();
  const database = createQueueDatabase();
  const worker = consumerFor(database.database);
  const valid = queueDelivery(await queueBody(row));
  const malformed = queueDelivery(new TextEncoder().encode("not-a-queue-message").buffer);
  const error = vi.spyOn(console, "error").mockImplementation(() => {});

  await worker.queue({ queue: PRIMARY_QUEUE, messages: [valid, malformed] }, CONSUMER_ENV);

  expect(database.insertTelemetryRows).toHaveBeenCalledTimes(1);
  expect(database.insertTelemetryRows).toHaveBeenCalledWith([row]);
  expect(valid.ack).toHaveBeenCalledOnce();
  expect(valid.retry).not.toHaveBeenCalled();
  expect(malformed.ack).not.toHaveBeenCalled();
  expect(malformed.retry).toHaveBeenCalledWith({ delaySeconds: 300 });
  expect(error).toHaveBeenCalledWith("telemetry_queue_message_invalid");
});

test("uses one stable private receipt across ack-lost redelivery", async () => {
  const row = await telemetryRow();
  const accountedReceipts = new Set<string>();
  let aggregateCollisionCount = 0;
  const database = createCollisionDatabase(async (_rejection, receiptId) => {
    if (!receiptId) throw new Error("missing_collision_receipt");
    if (!accountedReceipts.has(receiptId)) {
      accountedReceipts.add(receiptId);
      aggregateCollisionCount += 1;
    }
  });
  const worker = consumerFor(database.database);
  const body = await queueBody(row);
  const firstDelivery = queueDelivery(body);
  const ackLostReplay = queueDelivery(body);
  const warning = vi.spyOn(console, "warn").mockImplementation(() => {});

  await worker.queue({ queue: PRIMARY_QUEUE, messages: [firstDelivery] }, CONSUMER_ENV);
  await worker.queue({ queue: PRIMARY_QUEUE, messages: [ackLostReplay] }, CONSUMER_ENV);

  expect(database.insertTelemetryRows.mock.calls).toEqual([[[row]], [[row]], [[row]], [[row]]]);
  expect(database.recordIngestRejection).toHaveBeenCalledTimes(2);
  const firstReceipt = database.recordIngestRejection.mock.calls[0]?.[1];
  const replayReceipt = database.recordIngestRejection.mock.calls[1]?.[1];
  expect(firstReceipt).toMatch(/^[0-9a-f]{64}$/u);
  expect(replayReceipt).toBe(firstReceipt);
  expect(firstReceipt).not.toContain(row.event_id);
  expect(aggregateCollisionCount).toBe(1);
  expect(firstDelivery.ack).toHaveBeenCalledOnce();
  expect(ackLostReplay.ack).toHaveBeenCalledOnce();
  expect(firstDelivery.retry).not.toHaveBeenCalled();
  expect(ackLostReplay.retry).not.toHaveBeenCalled();
  expect(warning.mock.calls).toEqual([
    ["telemetry_queue_event_id_collision"],
    ["telemetry_queue_event_id_collision"],
  ]);
});

test("keeps a collision receipt stable when only receipt time changes", async () => {
  const firstRow = await telemetryRow();
  const replayRow = { ...firstRow, received_at: "2026-07-22T18:35:00.000Z" };
  const database = createCollisionDatabase();
  const worker = consumerFor(database.database);
  vi.spyOn(console, "warn").mockImplementation(() => {});

  await worker.queue({
    queue: PRIMARY_QUEUE,
    messages: [queueDelivery(await queueBody(firstRow))],
  }, CONSUMER_ENV);
  await worker.queue({
    queue: PRIMARY_QUEUE,
    messages: [queueDelivery(await queueBody(replayRow))],
  }, CONSUMER_ENV);

  const receipts = database.recordIngestRejection.mock.calls.map((call) => call[1]);
  expect(receipts).toHaveLength(2);
  expect(receipts[1]).toBe(receipts[0]);
});

test("derives distinct receipts for distinct one-row messages", async () => {
  const firstRow = await telemetryRow();
  const secondRow = await telemetryRow(SECOND_EVENT_ID);
  const database = createCollisionDatabase();
  const worker = consumerFor(database.database);
  vi.spyOn(console, "warn").mockImplementation(() => {});

  await worker.queue({
    queue: PRIMARY_QUEUE,
    messages: [queueDelivery(await queueBody(firstRow))],
  }, CONSUMER_ENV);
  await worker.queue({
    queue: PRIMARY_QUEUE,
    messages: [queueDelivery(await queueBody(secondRow))],
  }, CONSUMER_ENV);

  const receipts = database.recordIngestRejection.mock.calls.map((call) => call[1]);
  expect(receipts).toHaveLength(2);
  expect(receipts[0]).toMatch(/^[0-9a-f]{64}$/u);
  expect(receipts[1]).toMatch(/^[0-9a-f]{64}$/u);
  expect(receipts[1]).not.toBe(receipts[0]);
  expect(database.insertTelemetryRows.mock.calls.every((call) => call[0].length === 1))
    .toBe(true);
});

test("retries the same receipt after transient accounting failure", async () => {
  const row = await telemetryRow();
  const receiptAttempts: Array<string | undefined> = [];
  let transientPending = true;
  const database = createCollisionDatabase(async (_rejection, receiptId) => {
    receiptAttempts.push(receiptId);
    if (transientPending) {
      transientPending = false;
      throw new Error("transient_collision_accounting");
    }
  });
  const worker = consumerFor(database.database);
  const body = await queueBody(row);
  const firstDelivery = queueDelivery(body);
  const retryLog = vi.spyOn(console, "error").mockImplementation(() => {});
  vi.spyOn(console, "warn").mockImplementation(() => {});

  await worker.queue({ queue: PRIMARY_QUEUE, messages: [firstDelivery] }, CONSUMER_ENV);

  expect(firstDelivery.ack).not.toHaveBeenCalled();
  expect(firstDelivery.retry).toHaveBeenCalledWith({ delaySeconds: 300 });
  expect(retryLog).toHaveBeenCalledWith("telemetry_queue_collision_visibility_retry");

  const replay = queueDelivery(body);
  await worker.queue({ queue: PRIMARY_QUEUE, messages: [replay] }, CONSUMER_ENV);

  expect(receiptAttempts).toHaveLength(2);
  expect(receiptAttempts[0]).toMatch(/^[0-9a-f]{64}$/u);
  expect(receiptAttempts[1]).toBe(receiptAttempts[0]);
  expect(replay.retry).not.toHaveBeenCalled();
  expect(replay.ack).toHaveBeenCalledOnce();
});

test("does not ack before the collision receipt write completes", async () => {
  const row = await telemetryRow();
  const allowReceiptWrite = deferred();
  const receiptWriteStarted = deferred();
  let receiptPersisted = false;
  const database = createCollisionDatabase(async () => {
    receiptWriteStarted.resolve();
    await allowReceiptWrite.promise;
    receiptPersisted = true;
  });
  const worker = consumerFor(database.database);
  vi.spyOn(console, "warn").mockImplementation(() => {});
  const delivery = queueDelivery(await queueBody(row), () => {
    if (!receiptPersisted) throw new Error("ack_preceded_collision_receipt");
  });

  const consuming = worker.queue({ queue: PRIMARY_QUEUE, messages: [delivery] }, CONSUMER_ENV);
  await receiptWriteStarted.promise;
  expect(delivery.ack).not.toHaveBeenCalled();

  allowReceiptWrite.resolve();
  await consuming;
  expect(receiptPersisted).toBe(true);
  expect(delivery.ack).toHaveBeenCalledOnce();
  expect(delivery.retry).not.toHaveBeenCalled();
});

test("routes only valid collision receipts to the idempotent Neon function", async () => {
  const calls: Array<{ params: readonly unknown[]; sql: string }> = [];
  const client: NeonQueryClient = {
    async query<T extends Record<string, unknown> = Record<string, unknown>>(
      sql: string,
      params: readonly (string | number | boolean | null)[] = [],
    ): Promise<T[]> {
      calls.push({ params, sql });
      return [];
    },
    async transaction() {
      throw new Error("unexpected_transaction");
    },
  };
  const database = new NeonTelemetryDatabase(client);
  const rejection = collisionRejection();
  const receiptId = "a".repeat(64);

  await database.recordIngestRejection(rejection, receiptId);

  expect(calls).toHaveLength(1);
  expect(calls[0]?.sql).toContain("ctx.record_telemetry_event_collision");
  expect(calls[0]?.params).toEqual([
    receiptId,
    "production",
    "telemetry_batch",
    "operation_completed",
    "0.26.0",
    "f".repeat(64),
    false,
    "neutral",
    "lt_1kb",
  ]);
  await expect(database.recordIngestRejection(rejection))
    .rejects.toThrow("invalid_telemetry_collision_receipt");
  await expect(database.recordIngestRejection(
    { ...rejection, rejection_class: "invalid_event" },
    receiptId,
  )).rejects.toThrow("unexpected_telemetry_collision_receipt");
  expect(calls).toHaveLength(1);
});

async function telemetryRow(eventId?: string): Promise<TelemetryRow> {
  const plan = await buildTelemetryIngestPlan(
    v1Batch([operationEvent(eventId ? { event_id: eventId } : {})]),
    INGEST_OPTIONS,
  );
  return plan.rows[0]!;
}

async function queueBody(row: TelemetryRow): Promise<ArrayBuffer> {
  return queueMessageBody({
    format_version: TELEMETRY_QUEUE_FORMAT_VERSION,
    kind: "telemetry_row",
    collision_visibility: COLLISION_VISIBILITY,
    row,
  });
}

async function queueMessageBody(message: TelemetryQueueMessage): Promise<ArrayBuffer> {
  return encodeTelemetryQueueMessage(message);
}

async function blameReceipt() {
  const coordinate = "install:7Qz9mK4xN2pR8vW5";
  const plan = await buildTelemetryIngestPlan({
    app_version: "0.26.0",
    os: "linux",
    arch: "x86_64",
    events: [proOperationEvent("blame", {
      blame_schema_version: 2,
      blame_surface: "mcp",
      blame_target_kind: "pull_request",
      blame_request_kind: "continuation",
      blame_query_duration_bucket: "lt_1s",
      blame_result_state: "none",
      blame_result_count_bucket: "0",
      blame_freshness: "stale_committed",
      blame_has_more: false,
    })],
  }, { ...INGEST_OPTIONS, verifiedInstallationCoordinate: coordinate });
  const receipt = await buildBlameProductReceipt(
    plan.rows, coordinate, INGEST_OPTIONS.identityHmacKey, INGEST_OPTIONS.identityKeyVersion,
  );
  if (!receipt) throw new Error("expected_blame_receipt");
  return receipt;
}

function consumerFor(database: TelemetryDatabase) {
  return createTelemetryWorker({ createDatabaseClient: () => database });
}

function queueDelivery(body: ArrayBuffer, onAck: () => void = () => {}, attempts = 1) {
  return {
    body,
    attempts,
    ack: vi.fn(onAck),
    retry: vi.fn(),
  };
}

function createCollisionDatabase(
  onRecord: (
    rejection: TelemetryIngestRejection,
    collisionReceiptId?: string,
  ) => Promise<void> = async () => {},
) {
  const insertTelemetryRows = vi.fn(async (_rows: readonly TelemetryRow[]) => {
    throw new TelemetryEventCollisionError();
  });
  const recordIngestRejection = vi.fn(onRecord);
  const database: TelemetryDatabase = {
    insertTelemetryRows,
    recordIngestRejection,
    async insertBlameProductReceipt() {},
    async insertInstallStageRow() {},
    async readIngestHealthSnapshot() {
      return {
        compatibilityRejectionMax: 0n,
        deliveryDegradedCount: 0n,
        deliveryDroppedCount: 0n,
        eventCollisionCount: 0n,
        otherRejectionCount: 0n,
        providerRefreshFailureCount: 0n,
      };
    },
  };
  return { database, insertTelemetryRows, recordIngestRejection };
}

function createQueueDatabase(
  onInsert: (rows: readonly TelemetryRow[]) => Promise<void> = async () => {},
  onRecord: (
    rejection: TelemetryIngestRejection,
    collisionReceiptId?: string,
  ) => Promise<void> = async () => {},
  onInstall: TelemetryDatabase["insertInstallStageRow"] = async () => {},
  onBlame: TelemetryDatabase["insertBlameProductReceipt"] = async () => {},
) {
  const insertTelemetryRows = vi.fn(onInsert);
  const recordIngestRejection = vi.fn(onRecord);
  const insertInstallStageRow = vi.fn(onInstall);
  const insertBlameProductReceipt = vi.fn(onBlame);
  const database: TelemetryDatabase = {
    insertTelemetryRows,
    recordIngestRejection,
    insertBlameProductReceipt,
    insertInstallStageRow,
    async readIngestHealthSnapshot() {
      return {
        compatibilityRejectionMax: 0n,
        deliveryDegradedCount: 0n,
        deliveryDroppedCount: 0n,
        eventCollisionCount: 0n,
        otherRejectionCount: 0n,
        providerRefreshFailureCount: 0n,
      };
    },
  };
  return {
    database,
    insertBlameProductReceipt,
    insertInstallStageRow,
    insertTelemetryRows,
    recordIngestRejection,
  };
}

function collisionRejection(): TelemetryIngestRejection {
  return {
    ...COLLISION_VISIBILITY,
    rejection_class: "event_collision",
    rejection_code: "event_id_collision",
  };
}

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}
