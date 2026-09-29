import { readFileSync } from "node:fs";
import { expect, test } from "vitest";

import { buildRejectionDiagnostics } from "../src/rejection-diagnostics";
import { buildTelemetryIngestPlan } from "../src/telemetry-ingest";
import { decodeTelemetryQueueMessage, encodeTelemetryQueueMessage } from "../src/telemetry-queue";
import {
  ENV, INGEST_OPTIONS, jsonRequest, operationEvent, v1Batch, workerHarness,
} from "./worker-test-fixtures";

const fixtures = [
  "hosted_operation_completed.valid.json",
  "hosted_operation_failure.valid.json",
  "hosted_output_failure.valid.json",
].map((name) => JSON.parse(readFileSync(new URL(
  `../../../contracts/telemetry-v1/fixtures/${name}`, import.meta.url,
), "utf8")));

const operations = [
  "archive_export", "archive_restore", "remote_connect", "remote_share", "remote_sync",
  "server_init", "server_invite", "server_grant", "server_revoke", "server_withdraw",
  "server_backup", "server_restore",
];
const failureTypes = [
  "invalid_request", "unauthorized", "forbidden", "not_found", "conflict", "credentials",
  "policy_denied", "unavailable", "capacity", "io", "invalid_archive", "other",
];

function batch(event) {
  return { ...v1Batch([event]), app_version: "2.1.3" };
}

test.each(fixtures)("admits the shared Rust hosted fixture through HTTP and Queue: $operation/$outcome", async (event) => {
  const harness = workerHarness();
  const response = await harness.worker.fetch(
    jsonRequest("/functions/v1/analytics", batch(event)), ENV,
  );
  expect(response.status, await response.text()).toBe(204);
  const [message] = await harness.queueMessages();
  expect(harness.queueBodies).toHaveLength(1);
  expect(message.kind).toBe("telemetry_row");
  expect(message.row).toMatchObject({
    event_name: "operation_completed", event_version: 1, surface: "cli",
    status: event.outcome, duration_bucket: event.duration_bucket,
    activity_class: "product_activity", provider_id: null,
    properties: { ...event.properties, operation: event.operation, outcome: event.outcome },
  });
  expect(message.row.properties).toEqual({
    ...event.properties, operation: event.operation, outcome: event.outcome,
  });
  expect(message.row.client_profile_id_hash).toMatch(/^[0-9a-f]{64}$/u);
  expect(message.row.data_root_id_hash).toMatch(/^[0-9a-f]{64}$/u);
  await expect(decodeTelemetryQueueMessage(await encodeTelemetryQueueMessage(message)))
    .resolves.toEqual(message);
});

test("accepts every finite operation and closed failure type without arbitrary fields", async () => {
  for (const operation of operations) {
    for (const failureType of [null, ...failureTypes]) {
      const event = {
        ...fixtures[0], operation,
        outcome: failureType === null ? "success" : "failure",
        properties: failureType === null ? { output: "human" } : {
          output: "json", hosted_failure_stage: "operation", failure_type: failureType,
        },
      };
      const { rows } = await buildTelemetryIngestPlan(batch(event), INGEST_OPTIONS);
      expect(rows).toHaveLength(1);
      expect(rows[0].properties).toEqual({
        ...event.properties, operation, outcome: event.outcome,
      });
    }
  }
});

test("keeps existing shared envelope metadata compatible", async () => {
  const event = {
    ...fixtures[0], install_attempt_id: "ia_synthetic_hosted_install",
    properties: {
      ...fixtures[0].properties, install_manager: "ctx-hosted-installer",
      capability_snapshot_schema: 1, available_parallelism_bucket: "3-4",
      host_memory_bucket: "8-16gb", cpu_vector_tier: "avx2",
      acceleration_candidate: "not_detected",
    },
  };
  const harness = workerHarness();
  const response = await harness.worker.fetch(
    jsonRequest("/functions/v1/telemetry", batch(event)), ENV,
  );
  expect(response.status, await response.text()).toBe(204);
  const [message] = await harness.queueMessages();
  expect(message.row.properties.install_attempt_id_hash).toMatch(/^[0-9a-f]{64}$/u);
  expect(JSON.stringify(message)).not.toContain(event.install_attempt_id);
  expect(message.row.properties.capability_snapshot_schema).toBe(1);
});

test("rejects contradictory or unsupported terminals, including Queue replay", async () => {
  const invalid = [
    { ...fixtures[0], operation: "server_run" },
    { ...fixtures[0], operation: "remote_search" },
    { ...fixtures[0], operation: "hosted" },
    { ...fixtures[0], surface: "daemon" },
    { ...fixtures[0], surface: "mcp" },
    { ...fixtures[0], properties: {} },
    { ...fixtures[0], properties: { output: "xml" } },
    { ...fixtures[1], outcome: "success" },
    { ...fixtures[0], outcome: "failure" },
    { ...fixtures[1], properties: { output: "json", failure_type: "io" } },
    { ...fixtures[1], properties: { output: "json", hosted_failure_stage: "operation" } },
    { ...fixtures[1], properties: { ...fixtures[1].properties, failure_type: "recovery_closed" } },
    { ...fixtures[1], properties: { ...fixtures[1].properties, failure_type: "unknown" } },
    { ...fixtures[1], properties: { ...fixtures[1].properties, hosted_failure_stage: "upload" } },
    { ...fixtures[1], properties: { ...fixtures[1].properties, hosted_failure_stage: "output" } },
    { ...fixtures[0], properties: { output: "json", auto_upgrade_probe: true } },
    { ...fixtures[0], properties: { output: "json", deprecated_daemon_control: true } },
  ];
  for (const event of invalid) {
    await expect(buildTelemetryIngestPlan(batch(event), INGEST_OPTIONS)).rejects.toThrow();
  }
  const harness = workerHarness();
  await harness.worker.fetch(jsonRequest("/functions/v1/analytics", batch(fixtures[0])), ENV);
  const [message] = await harness.queueMessages();
  for (const event of invalid) {
    const tampered = structuredClone(message);
    tampered.row.surface = event.surface;
    tampered.row.status = event.outcome;
    tampered.row.success = event.outcome === "success";
    tampered.row.properties = {
      ...event.properties, operation: event.operation, outcome: event.outcome,
    };
    await expect(decodeTelemetryQueueMessage(await encodeTelemetryQueueMessage(tampered)))
      .resolves.toBeNull();
  }
});

test("rejects content and topology without putting values into rejection diagnostics", async () => {
  const canary = "private-canary-history-token-path-query";
  for (const key of [
    "url", "hostname", "token", "user", "collection", "source_path", "query", "history",
    "error_message", "connection", "topology", "count", "source_count_bucket", canary,
  ]) {
    const payload = batch({ ...fixtures[0], properties: { output: "json", [key]: canary } });
    const harness = workerHarness();
    const response = await harness.worker.fetch(jsonRequest("/functions/v1/analytics", payload), ENV);
    expect(response.status).toBe(422);
    expect(harness.queueBodies).toHaveLength(0);
    expect(await response.text()).not.toContain(canary);
    const diagnostics = buildRejectionDiagnostics(payload, 1024, "telemetry_batch");
    expect(JSON.stringify(diagnostics)).not.toContain(canary);
  }
  for (const key of ["failure_type", "hosted_failure_stage"]) {
    const payload = batch({ ...fixtures[1], properties: { ...fixtures[1].properties, [key]: canary } });
    await expect(buildTelemetryIngestPlan(payload, INGEST_OPTIONS)).rejects.toThrow();
    const diagnostics = buildRejectionDiagnostics(payload, 1024, "telemetry_batch");
    expect(diagnostics.field_shape).toContain(`properties.${key}:string`);
    expect(JSON.stringify(diagnostics)).not.toContain(canary);
  }
});

test("does not widen ordinary operation properties", async () => {
  const ordinary = operationEvent();
  await expect(buildTelemetryIngestPlan(batch(ordinary), INGEST_OPTIONS)).resolves.toBeDefined();
  for (const key of ["hosted_failure_stage", "failure_type"]) {
    const event = {
      ...ordinary, properties: { ...ordinary.properties, [key]: key === "failure_type" ? "io" : "operation" },
    };
    await expect(buildTelemetryIngestPlan(batch(event), INGEST_OPTIONS)).rejects.toThrow();
  }
});
