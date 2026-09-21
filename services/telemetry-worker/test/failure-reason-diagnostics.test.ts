import { expect, test, vi } from "vitest";

import { NeonTelemetryDatabase } from "../src/database";
import { buildRejectionDiagnostics } from "../src/rejection-diagnostics";
import { buildTelemetryIngestPlan } from "../src/telemetry-ingest";
import { decodeTelemetryQueueMessage, encodeTelemetryQueueMessage } from "../src/telemetry-queue";
import {
  ENV, EVENT_ID, INGEST_OPTIONS, OCCURRED_AT, jsonRequest, neonHarness,
  operationEvent, providerRefreshEvent, providerRefreshProperties, v1Batch, workerHarness,
} from "./worker-test-fixtures";

// Independent wire expectations; do not derive these from receiver allowlists.
const IO_REASONS = ["io_not_found", "io_permission_denied", "io_storage_full",
  "io_read_only_filesystem", "io_out_of_memory", "io_timed_out"];
const REFRESH_CASES = [
  ...IO_REASONS.flatMap((reason) => ["io", "index", "provider"].map((kind) => ({ reason, kind }))),
  ...["route_output_limit", "route_scratch_limit"].map((reason) => ({ reason, kind: "provider" })),
  ...["index_memory_limit", "index_scratch_limit", "index_writer_invariant"]
    .map((reason) => ({ reason, kind: "index" })),
];
const DELIVERY_CASES = [
  ...["request_dns", "request_connect", "request_timeout", "request_io", "response_status_408",
    "response_body_timeout", "response_body_io"].map((reason) => ({ reason, kind: "transport" })),
  ...["file_open", "file_write", "file_flush", "outbox_corrupt", "outbox_expired",
    "outbox_capacity", "outbox_clock", "outbox_oversized"].map((reason) => ({ reason, kind: "local_io" })),
];

function refresh(reason: unknown = "io_permission_denied", kind = "io", stage = "execution") {
  return providerRefreshEvent({
    surface: "daemon", outcome: "failure",
    properties: providerRefreshProperties({
      trigger: "daemon", refresh_result: "failure", core_result: "failure",
      failure_scope: "system", failure_type: "system", failure_code: "source_refresh_failed",
      retryable: true, refresh_failure_stage: stage, refresh_failure_kind: kind,
      refresh_failure_reason: reason,
    }),
  });
}

function delivery(reason: unknown = "request_timeout", kind = "transport") {
  return {
    event_id: EVENT_ID, event_name: "analytics_delivery_observation", event_version: 1,
    occurred_at: OCCURRED_AT, surface: "cli", operation: "outbox", outcome: "failure",
    duration_bucket: "unknown",
    properties: {
      queued_count_bucket: "1", retry_attempt_count_bucket: "1", dropped_count_bucket: "0",
      oldest_queued_age_bucket: "lt_10m", failure_class: kind, delivery_failure_reason: reason,
    } as Record<string, unknown>,
  };
}

test("retains every allowed reason through ingress, Queue replay and Neon parameters", async () => {
  const events = [
    ...REFRESH_CASES.flatMap(({ reason, kind }) =>
      ["admission", "execution", "verification", "finalization"].map((stage) => refresh(reason, kind, stage))),
    ...DELIVERY_CASES.map(({ reason, kind }) => delivery(reason, kind)),
  ];
  for (const event of events) {
    const harness = workerHarness();
    const response = await harness.worker.fetch(jsonRequest("/functions/v1/telemetry", v1Batch([event])), ENV);
    expect(response.status, await response.text()).toBe(204);
    const message = (await harness.queueMessages())[0];
    if (message.kind !== "telemetry_row") throw new Error("expected_telemetry_row");
    expect(message.row.properties).toEqual({
      ...event.properties as Record<string, unknown>, operation: event.operation, outcome: event.outcome,
    });
    for (let attempt = 1; attempt <= 2; attempt++) {
      const item = { body: harness.queueBodies[0], attempts: attempt, ack: vi.fn(), retry: vi.fn() };
      await harness.worker.queue({ queue: "ctx-telemetry-ingest-prod", messages: [item] }, ENV);
      expect(item.ack).toHaveBeenCalledOnce();
      expect(item.retry).not.toHaveBeenCalled();
    }
    expect(harness.insertTelemetryRows.mock.calls).toEqual([[[message.row]], [[message.row]]]);
    const neon = neonHarness();
    const database = new NeonTelemetryDatabase(neon.client);
    await database.insertTelemetryRows([message.row]);
    await database.insertTelemetryRows([message.row]);
    expect(neon.calls[0]).toEqual(neon.calls[2]);
    expect(neon.calls[1]).toEqual(neon.calls[3]);
    expect(neon.calls[0][0]).toContain("ON CONFLICT (event_id) DO NOTHING");
    expect(neon.calls[0][1]).toContain(JSON.stringify(message.row.properties));
  }
});

test("keeps cause-free older events and final zero-queue recovery compatible", async () => {
  const oldRefresh = refresh();
  delete (oldRefresh.properties as Record<string, unknown>).refresh_failure_reason;
  const oldDelivery = delivery();
  delete oldDelivery.properties.delivery_failure_reason;
  const recovered = structuredClone(oldDelivery);
  Object.assign(recovered, { outcome: "success" });
  Object.assign(recovered.properties, {
    queued_count_bucket: "0", retry_attempt_count_bucket: "0", oldest_queued_age_bucket: "unknown",
    failure_class: "none",
  });
  for (const event of [oldRefresh, oldDelivery, recovered]) {
    const plan = await buildTelemetryIngestPlan(v1Batch([event]), INGEST_OPTIONS);
    expect(plan.rows[0].properties).not.toHaveProperty("refresh_failure_reason");
    expect(plan.rows[0].properties).not.toHaveProperty("delivery_failure_reason");
  }
});

test("diagnostic changes retain ordinary fingerprint and collision semantics", async () => {
  for (const [first, changed] of [[refresh(), refresh("io_not_found")],
    [delivery(), delivery("request_connect")]]) {
    const plan = await buildTelemetryIngestPlan(v1Batch([first, first]), INGEST_OPTIONS);
    expect(plan.rows).toHaveLength(1);
    const other = await buildTelemetryIngestPlan(v1Batch([changed]), INGEST_OPTIONS);
    expect(other.rows[0].payload_fingerprint).not.toBe(plan.rows[0].payload_fingerprint);
    await expect(buildTelemetryIngestPlan(v1Batch([first, changed]), INGEST_OPTIONS))
      .rejects.toMatchObject({ code: "event_id_collision" });
  }
});

test("rejects raw, unknown and malformed causes in both admission and Queue decoding", async () => {
  for (const [field, makeEvent] of [["refresh_failure_reason", refresh], ["delivery_failure_reason", delivery]] as const) {
    for (const value of ["unknown", "", "raw_error /private/source secret-canary", "https://private.test/source",
      null, 1, true, [], {}]) {
      const harness = workerHarness();
      const response = await harness.worker.fetch(jsonRequest("/functions/v1/analytics", v1Batch([makeEvent(value)])), ENV);
      expect(response.status).toBe(422);
      expect(await response.json()).toEqual({ error: `invalid_${field}` });
      expect(harness.queueBodies).toHaveLength(0);
      expect(JSON.stringify(harness.observeRejection.mock.calls)).not.toContain("secret-canary");
      const accepted = workerHarness();
      expect((await accepted.worker.fetch(jsonRequest("/functions/v1/analytics", v1Batch([makeEvent()])), ENV)).status).toBe(204);
      const message = (await accepted.queueMessages())[0];
      if (message.kind !== "telemetry_row") throw new Error("expected_telemetry_row");
      message.row.properties[field] = value as string;
      await expect(decodeTelemetryQueueMessage(await encodeTelemetryQueueMessage(message))).resolves.toBeNull();
    }
  }
});

test("rejects incompatible kinds, incomplete tuples and wrong event contexts", async () => {
  const events = [
    refresh("io_timed_out", "unknown"), refresh("route_output_limit", "io"),
    refresh("route_scratch_limit", "index"), refresh("index_memory_limit", "provider"),
    refresh("index_scratch_limit", "io"), refresh("index_writer_invariant", "unknown"),
    { ...refresh(), surface: "cli" }, { ...refresh(), outcome: "success" },
    { ...delivery(), surface: "daemon" }, { ...delivery(), operation: "search" },
    { ...delivery(), outcome: "success" },
    ...["none", "rate_limited", "client_rejection", "server", "configuration", "unknown", "local_io"]
      .map((kind) => delivery("request_timeout", kind)),
    delivery("file_open", "transport"),
  ];
  for (const key of ["refresh_failure_stage", "refresh_failure_kind", "failure_code", "retryable",
    "refresh_result", "core_result", "failure_scope", "failure_type"]) {
    const event = refresh();
    delete (event.properties as Record<string, unknown>)[key];
    events.push(event);
  }
  for (const result of ["complete", "partial"]) {
    const event = refresh();
    (event.properties as Record<string, unknown>).refresh_result = result;
    events.push(event);
  }
  for (const field of ["refresh_failure_reason", "delivery_failure_reason"]) {
    const event = operationEvent();
    (event.properties as Record<string, unknown>)[field] = "io_not_found";
    events.push(event);
  }
  for (const event of events) {
    await expect(buildTelemetryIngestPlan(v1Batch([event]), INGEST_OPTIONS)).rejects.toMatchObject({ status: 422 });
  }
});

test("rejection shapes retain known diagnostic presence and type, never values or unknown names", () => {
  const event = delivery("secret-canary");
  event.properties["private-field-canary"] = "private-value-canary";
  const diagnostics = buildRejectionDiagnostics(v1Batch([refresh(null), event]), 1000, "telemetry_batch");
  expect(diagnostics.field_shape).toContain("properties.refresh_failure_reason:null");
  expect(diagnostics.field_shape).toContain("properties.delivery_failure_reason:string");
  expect(diagnostics.field_shape).toContain("properties.failure_class:string");
  expect(JSON.stringify(diagnostics)).not.toContain("canary");
});
