import { expect, test, vi } from "vitest";

import { NeonTelemetryDatabase } from "../src/database";
import { buildTelemetryIngestPlan } from "../src/telemetry-ingest";
import {
  decodeTelemetryQueueMessage,
  encodeTelemetryQueueMessage,
  type TelemetryQueueMessage,
} from "../src/telemetry-queue";
import {
  INGEST_OPTIONS,
  ENV,
  jsonRequest,
  neonHarness,
  operationEvent,
  providerRefreshEvent,
  providerRefreshProperties,
  runtimeEvent,
  v1Batch,
  workerHarness,
} from "./worker-test-fixtures";

function failedRefresh(properties: Record<string, unknown> = {}) {
  return providerRefreshEvent({
    surface: "daemon",
    outcome: "failure",
    properties: providerRefreshProperties({
      trigger: "daemon",
      refresh_result: "failure",
      core_result: "failure",
      failure_scope: "system",
      failure_type: "system",
      failure_code: "source_refresh_failed",
      retryable: true,
      refresh_failure_stage: "execution",
      refresh_failure_kind: "unknown",
      ...properties,
    }),
  });
}

test("preserves closed refresh diagnostics across ingress and Queue validation", async () => {
  for (const stage of ["admission", "execution", "verification", "finalization"]) {
    for (const kind of ["io", "index", "provider", "unknown"]) {
      const { rows } = await buildTelemetryIngestPlan(v1Batch([failedRefresh({
        refresh_failure_stage: stage,
        refresh_failure_kind: kind,
      })]), INGEST_OPTIONS);
      expect(rows[0].properties).toMatchObject({
        refresh_failure_stage: stage,
        refresh_failure_kind: kind,
        failure_code: "source_refresh_failed",
        retryable: true,
      });
      const message: TelemetryQueueMessage = {
        format_version: 1,
        kind: "telemetry_row",
        row: rows[0],
        collision_visibility: {
          analytics_environment: "production",
          endpoint: "telemetry_batch",
          event_family: "provider_refresh_completed",
          app_version: rows[0].app_version,
          field_shape_fingerprint: "f".repeat(64),
          field_shape_overflow: false,
          provider_classification: "neutral",
          size_bucket: "64kb_256kb",
        },
      };
      await expect(decodeTelemetryQueueMessage(await encodeTelemetryQueueMessage(message)))
        .resolves.toEqual(message);
    }
  }
});

test("keeps old refresh events compatible without either diagnostic field", async () => {
  const event = failedRefresh();
  const properties = event.properties as Record<string, unknown>;
  delete properties.refresh_failure_stage;
  delete properties.refresh_failure_kind;
  const { rows } = await buildTelemetryIngestPlan(v1Batch([event]), INGEST_OPTIONS);
  expect(rows[0].properties).not.toHaveProperty("refresh_failure_stage");
  expect(rows[0].properties).not.toHaveProperty("refresh_failure_kind");
});

test.each([
  ["refresh_failure_stage", "raw local error", "invalid_refresh_failure_stage"],
  ["refresh_failure_kind", "/private/source/path", "invalid_refresh_failure_kind"],
  ["refresh_failure_stage", null, "invalid_refresh_failure_stage"],
  ["refresh_failure_kind", 1, "invalid_refresh_failure_kind"],
])("rejects open or malformed diagnostic %s", async (field, value, code) => {
  await expect(buildTelemetryIngestPlan(v1Batch([failedRefresh({ [field]: value })]), INGEST_OPTIONS))
    .rejects.toMatchObject({ status: 422, code });
});

test("rejects incomplete diagnostics and diagnostics outside failed daemon refresh", async () => {
  for (const field of ["refresh_failure_stage", "refresh_failure_kind"]) {
    const event = failedRefresh();
    delete (event.properties as Record<string, unknown>)[field];
    await expect(buildTelemetryIngestPlan(v1Batch([event]), INGEST_OPTIONS))
      .rejects.toMatchObject({ code: "incomplete_refresh_failure_diagnostic" });
  }
  const cli = { ...failedRefresh(), surface: "cli" };
  const success = { ...failedRefresh({ failure_code: "none" }), outcome: "success" };
  const noFailureCode = failedRefresh();
  delete (noFailureCode.properties as Record<string, unknown>).failure_code;
  delete (noFailureCode.properties as Record<string, unknown>).retryable;
  for (const event of [cli, success, noFailureCode]) {
    await expect(buildTelemetryIngestPlan(v1Batch([event]), INGEST_OPTIONS))
      .rejects.toMatchObject({ code: "inconsistent_refresh_failure_diagnostic" });
  }
});

function coverageRefresh(properties: Record<string, unknown> = {}) {
  return failedRefresh({
    failure_code: "all_provider_terminal_coverage_unavailable",
    refresh_failure_kind: "provider",
    refresh_coverage_reason: "missing_terminal_authority",
    ...properties,
  });
}

function partialRefresh(properties: Record<string, unknown> = {}) {
  return providerRefreshEvent({
    surface: "daemon",
    properties: providerRefreshProperties({
      trigger: "daemon",
      refresh_result: "partial",
      core_result: "partial",
      failure_scope: "source",
      failure_type: "unknown",
      work_remaining: true,
      failure_code: "none",
      retryable: true,
      refresh_source_failure_class: "unreadable",
      ...properties,
    }),
  });
}

test("preserves every closed cause through admission, persistence and Queue replay", async () => {
  const events = [
    ...["catalog_unavailable", "unsafe_root", "missing_terminal_authority", "route_failed",
      "invalid_route_identity", "missing_empty_authority"].flatMap((reason) => (
      ["admission", "execution", "verification", "finalization"].map((stage) => coverageRefresh({
        refresh_coverage_reason: reason, refresh_failure_stage: stage,
      }))
    )),
    ...["unavailable", "source_changed", "unreadable", "incompatible", "mixed"].flatMap((kind) => (
      ["source", "mixed"].map((scope) => partialRefresh({
        refresh_source_failure_class: kind, failure_scope: scope,
      }))
    )),
  ];
  for (const event of events) {
    const properties = event.properties as Record<string, unknown>;
    delete properties.provider;
    const harness = workerHarness();
    const response = await harness.worker.fetch(
      jsonRequest("/functions/v1/telemetry", v1Batch([event])), ENV,
    );
    expect(response.status, await response.text()).toBe(204);
    const message = (await harness.queueMessages())[0];
    if (message.kind !== "telemetry_row") throw new Error("expected_row");
    expect(message.row.properties).toEqual({ ...properties, operation: "refresh", outcome: event.outcome });
    expect(message.row.provider_id).toBeNull();
    expect(message.row.status).toBe(event.outcome);
    expect(message.row.success).toBe(event.outcome === "success");
    for (let attempt = 0; attempt < 2; attempt++) {
      const delivery = { body: harness.queueBodies[0], attempts: attempt + 1, ack: vi.fn(), retry: vi.fn() };
      await harness.worker.queue({ queue: "ctx-telemetry-ingest-prod", messages: [delivery] }, ENV);
      expect(delivery.ack).toHaveBeenCalledOnce();
      expect(delivery.retry).not.toHaveBeenCalled();
    }
    expect(harness.insertTelemetryRows.mock.calls).toEqual([[[message.row]], [[message.row]]]);
    const neon = neonHarness();
    const database = new NeonTelemetryDatabase(neon.client);
    await database.insertTelemetryRows([message.row]);
    await database.insertTelemetryRows([message.row]);
    expect(neon.calls[2]).toEqual(neon.calls[0]);
    expect(neon.calls[3]).toEqual(neon.calls[1]);
    expect(neon.calls[0][0]).toContain("ON CONFLICT (event_id) DO NOTHING");
    expect(neon.calls[0][1]).toContain(JSON.stringify(message.row.properties));
    expect(neon.calls[1][1]).toEqual([message.row.event_id, message.row.payload_fingerprint]);
  }
});

test("keeps cause-free structured and legacy refreshes accepted", async () => {
  for (const event of [coverageRefresh(), partialRefresh()]) {
    const properties = event.properties as Record<string, unknown>;
    delete properties.refresh_coverage_reason;
    delete properties.refresh_source_failure_class;
    const { rows } = await buildTelemetryIngestPlan(v1Batch([event]), INGEST_OPTIONS);
    expect(rows[0].properties).not.toHaveProperty("refresh_coverage_reason");
    expect(rows[0].properties).not.toHaveProperty("refresh_source_failure_class");
    for (const key of ["refresh_result", "core_result", "failure_scope", "failure_type",
      "content_evidence", "work_kind", "canonical_pro_result", "output_pro_result",
      "retired_records_bucket", "refresh_failure_stage", "refresh_failure_kind"]) delete properties[key];
    await expect(buildTelemetryIngestPlan(v1Batch([event]), INGEST_OPTIONS)).resolves.toBeDefined();
  }
});

test("source classification needs only the existing required structured tuple", async () => {
  const event = partialRefresh();
  const properties = event.properties as Record<string, unknown>;
  for (const key of ["failure_code", "retryable", "canonical_pro_result", "output_pro_result"]) {
    delete properties[key];
  }
  const { rows } = await buildTelemetryIngestPlan(v1Batch([event]), INGEST_OPTIONS);
  expect(rows[0].properties.refresh_source_failure_class).toBe("unreadable");
});

test("accepts source classes alongside record rejection in mixed-scope partial receipts", async () => {
  for (const kind of ["unavailable", "source_changed", "unreadable", "incompatible", "mixed"]) {
    for (const retryable of [true, false]) {
      const event = partialRefresh({
        refresh_source_failure_class: kind,
        failure_scope: "mixed",
        failure_type: "mixed",
        rejections_bucket: "2-5",
        failures_bucket: "1",
        retryable,
      });
      const { rows } = await buildTelemetryIngestPlan(v1Batch([event]), INGEST_OPTIONS);
      expect(rows[0].properties).toEqual({
        ...event.properties as Record<string, unknown>, operation: "refresh", outcome: "success",
      });
      expect(rows[0].success).toBe(true);
    }
  }
});

test("retains existing failure-code and partial-tuple rejection rules", async () => {
  for (const [event, code] of [
    [coverageRefresh({ failure_code: "none" }), "inconsistent_provider_failure_code"],
    [partialRefresh({ failure_code: "source_refresh_failed" }), "inconsistent_provider_failure_code"],
    [{ ...partialRefresh(), outcome: "failure" }, "inconsistent_provider_failure_code"],
    [partialRefresh({ failure_type: "none" }), "inconsistent_provider_failure"],
  ] as const) {
    await expect(buildTelemetryIngestPlan(v1Batch([event]), INGEST_OPTIONS))
      .rejects.toMatchObject({ status: 422, code });
  }
});

test("causes participate in the existing fingerprint and collision contract", async () => {
  for (const [first, changed] of [
    [coverageRefresh(), coverageRefresh({ refresh_coverage_reason: "unsafe_root" })],
    [partialRefresh(), partialRefresh({ refresh_source_failure_class: "mixed" })],
  ]) {
    const { rows } = await buildTelemetryIngestPlan(v1Batch([first, first]), INGEST_OPTIONS);
    expect(rows).toHaveLength(1);
    const different = await buildTelemetryIngestPlan(v1Batch([changed]), INGEST_OPTIONS);
    expect(different.rows[0].payload_fingerprint).not.toBe(rows[0].payload_fingerprint);
    await expect(buildTelemetryIngestPlan(v1Batch([first, changed]), INGEST_OPTIONS))
      .rejects.toMatchObject({ code: "event_id_collision" });
  }
});

test("rejects unknown, raw and non-string causes without echoing them", async () => {
  for (const [field, makeEvent] of [
    ["refresh_coverage_reason", coverageRefresh],
    ["refresh_source_failure_class", partialRefresh],
  ] as const) {
    for (const value of ["unknown", "", "raw_error /private/source token=secret-canary",
      "https://private.example/source", null, 1, true, [], {}]) {
      const harness = workerHarness();
      const response = await harness.worker.fetch(
        jsonRequest("/functions/v1/telemetry", v1Batch([makeEvent({ [field]: value })])), ENV,
      );
      expect(response.status).toBe(422);
      expect(await response.json()).toEqual({ error: `invalid_${field}` });
      expect(harness.queueBodies).toHaveLength(0);
      expect(harness.insertTelemetryRows).not.toHaveBeenCalled();
      expect(JSON.stringify(harness.observeRejection.mock.calls)).not.toContain("secret-canary");
      const { rows } = await buildTelemetryIngestPlan(v1Batch([makeEvent()]), INGEST_OPTIONS);
      const message: TelemetryQueueMessage = {
        format_version: 1, kind: "telemetry_row", row: rows[0],
        collision_visibility: {
          analytics_environment: "production", endpoint: "telemetry_batch",
          event_family: "provider_refresh_completed", app_version: rows[0].app_version,
          field_shape_fingerprint: "f".repeat(64), field_shape_overflow: false,
          provider_classification: "neutral", size_bucket: "64kb_256kb",
        },
      };
      Object.assign(message.row.properties, { [field]: value });
      await expect(decodeTelemetryQueueMessage(await encodeTelemetryQueueMessage(message)))
        .resolves.toBeNull();
    }
  }
});

test("rejects causes outside their frozen contexts and malformed existing tuples", async () => {
  const events = [
    { ...coverageRefresh(), surface: "cli" },
    { ...partialRefresh(), surface: "cli" },
    { ...partialRefresh(), outcome: "failure" },
    coverageRefresh({ failure_code: "source_refresh_failed" }),
    coverageRefresh({ refresh_failure_kind: "io" }),
    coverageRefresh({ refresh_failure_stage: "unknown" }),
    coverageRefresh({ refresh_result: "complete" }),
    coverageRefresh({ refresh_source_failure_class: "mixed" }),
    partialRefresh({ refresh_coverage_reason: "unsafe_root" }),
    partialRefresh({ refresh_result: "complete" }),
    partialRefresh({ refresh_result: "failure" }),
    partialRefresh({ core_result: "invalid" }),
    partialRefresh({ failure_code: "source_refresh_failed" }),
    partialRefresh({ retryable: null }),
    partialRefresh({ failure_type: "none" }),
    partialRefresh({ refresh_failure_stage: "execution", refresh_failure_kind: "provider" }),
    ...["none", "record", "system", "unknown"].map((scope) => partialRefresh({ failure_scope: scope })),
  ];
  for (const makeEvent of [coverageRefresh, partialRefresh]) {
    for (const keys of [["refresh_result"], ["core_result"], ["failure_scope"], ["failure_type"],
      ["canonical_pro_result"], ["retryable"],
      ["refresh_result", "core_result", "failure_scope", "failure_type"]]) {
      const event = makeEvent();
      for (const key of keys) delete (event.properties as Record<string, unknown>)[key];
      events.push(event);
    }
  }
  for (const keys of [["refresh_failure_stage"], ["refresh_failure_kind"],
    ["refresh_failure_stage", "refresh_failure_kind"], ["failure_code", "retryable"]]) {
    const event = coverageRefresh();
    for (const key of keys) delete (event.properties as Record<string, unknown>)[key];
    events.push(event);
  }
  for (const field of ["refresh_coverage_reason", "refresh_source_failure_class"]) {
    for (const event of [operationEvent(), runtimeEvent()]) {
      (event.properties as Record<string, unknown>)[field] = "mixed";
      events.push(event);
    }
  }
  for (const event of events) {
    await expect(buildTelemetryIngestPlan(v1Batch([event]), INGEST_OPTIONS))
      .rejects.toMatchObject({ status: 422 });
  }
});
