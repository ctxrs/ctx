import { describe, expect, test, vi } from "vitest";

import { buildTelemetryIngestPlan } from "../src/telemetry-ingest";
import { workerHarness } from "./worker-test-fixtures";
import {
  BLAME_DOCS_EVENT, BLAME_ENV, BLAME_EVENTS, BLAME_NOW, ordinaryBlameBatch, ordinaryBlameRequest,
} from "./ordinary-blame-fixtures.mjs";

describe.each(["/functions/v1/analytics", "/functions/v1/telemetry"])(
  "ordinary Blame on %s", (route) => {
    test.each(Object.entries(BLAME_EVENTS))("one terminal %s uses the ordinary queue", async (_name, event) => {
      const h = workerHarness({ now: BLAME_NOW });
      const response = await h.worker.fetch(ordinaryBlameRequest(ordinaryBlameBatch([event]), route), BLAME_ENV);
      expect(response.status, await response.text()).toBe(204);
      expect(h.createDatabaseClient).not.toHaveBeenCalled();
      expect(h.queueBodies).toHaveLength(1);
      const [message] = await h.queueMessages();
      expect(message).toMatchObject({
        kind: "telemetry_row",
        row: {
          event_name: "operation_completed", schema_version: 1,
          surface: event.surface, status: event.outcome,
          analytics_environment: "staging", traffic_class: "synthetic",
          client_profile_id_hash: expect.stringMatching(/^[0-9a-f]{64}$/u),
          data_root_id_hash: expect.stringMatching(/^[0-9a-f]{64}$/u),
          properties: { ...event.properties, operation: "blame", outcome: event.outcome },
          activity_class: event.outcome === "success" ? "product_value" : "product_activity",
        },
      });
      const delivery = { body: h.queueBodies[0], attempts: 1, ack: vi.fn(), retry: vi.fn() };
      await h.worker.queue({ queue: "ctx-telemetry-ingest-staging", messages: [delivery] }, BLAME_ENV);
      expect(h.insertTelemetryRows).toHaveBeenCalledExactlyOnceWith([message.row]);
      expect(h.insertBlameProductReceipt).not.toHaveBeenCalled();
      expect(delivery.ack).toHaveBeenCalledOnce();
      expect(delivery.retry).not.toHaveBeenCalled();
    });
  },
);

test("Blame shares Core profile/root pseudonyms with an ordinary operation", async () => {
  const blame = BLAME_EVENTS.cli_proven;
  const status = {
    ...blame, event_id: "15000000-0000-4000-8000-000000000002", operation: "status",
    properties: { output: "json", initialized: true, indexed_items_bucket: "21-100" },
  };
  const plan = await buildTelemetryIngestPlan(ordinaryBlameBatch([blame, status]), {
    analyticsEnvironment: "staging", now: () => BLAME_NOW,
    identityHmacKey: BLAME_ENV.TELEMETRY_IDENTITY_HMAC_KEY, identityKeyVersion: 1,
  });
  for (const key of ["client_profile_id_hash", "data_root_id_hash", "identity_key_version"]) {
    expect(plan.rows[0][key]).toEqual(plan.rows[1][key]);
  }
});

test("the new Blame docs topic is accepted without opening arbitrary topics", async () => {
  const h = workerHarness({ now: BLAME_NOW });
  const response = await h.worker.fetch(ordinaryBlameRequest(ordinaryBlameBatch([BLAME_DOCS_EVENT])), BLAME_ENV);
  expect(response.status).toBe(204);
  expect((await h.queueMessages())[0].row.properties.topic).toBe("blame");
  const invalid = structuredClone(BLAME_DOCS_EVENT);
  invalid.properties.topic = "blame/private-path";
  const rejected = workerHarness({ now: BLAME_NOW });
  expect((await rejected.worker.fetch(ordinaryBlameRequest(ordinaryBlameBatch([invalid])), BLAME_ENV)).status).toBe(422);
  expect(rejected.queueBodies).toHaveLength(0);
});

test.each(["client_profile_id", "data_root_id", "both"])("ordinary Blame requires existing identity: %s", async (missing) => {
  const batch = ordinaryBlameBatch([BLAME_EVENTS.cli_proven]);
  if (missing !== "data_root_id") delete batch.client_profile_id;
  if (missing !== "client_profile_id") delete batch.data_root_id;
  const h = workerHarness({ now: BLAME_NOW });
  const response = await h.worker.fetch(ordinaryBlameRequest(batch), BLAME_ENV);
  expect(response.status).toBe(422);
  expect(h.queueBodies).toHaveLength(0);
});

test.each([
  ["blame_target_kind", "src/private.rs"], ["blame_request_kind", "page_42"],
  ["blame_query_duration_bucket", 42], ["blame_query_duration_bucket", "unknown"],
  ["blame_result_state", "invented"], ["blame_result_count_bucket", 27],
  ["blame_freshness", "uncommitted"], ["blame_has_more", "false"],
  ["blame_output_served", "true"], ["blame_machine_hash", "forbidden"],
  ["blame_target", "/private/file"], ["blame_cursor", "private-cursor"],
  ["blame_schema_version", 2], ["blame_access_state", "active"],
])("rejects raw, legacy, or invalid ordinary %s=%j", async (key, value) => {
  for (const surface of ["cli", "mcp"]) {
    const event = structuredClone(BLAME_EVENTS[`${surface}_proven`]);
    event.properties[key] = value;
    const h = workerHarness({ now: BLAME_NOW });
    const response = await h.worker.fetch(ordinaryBlameRequest(ordinaryBlameBatch([event])), BLAME_ENV);
    expect(response.status).toBe(422);
    expect(h.queueBodies).toHaveLength(0);
  }
});

test("rejects incomplete facts and facts on another operation", async () => {
  for (const mutate of [
    (e) => { delete e.properties.blame_target_kind; },
    (e) => { delete e.properties.blame_freshness; },
    (e) => { e.properties.blame_failure_class = "output"; },
    (e) => { e.outcome = "failure"; },
    (e) => { e.operation = "status"; },
  ]) {
    const event = structuredClone(BLAME_EVENTS.cli_proven);
    mutate(event);
    const h = workerHarness({ now: BLAME_NOW });
    const response = await h.worker.fetch(ordinaryBlameRequest(ordinaryBlameBatch([event])), BLAME_ENV);
    expect(response.status).toBe(422);
    expect(h.queueBodies).toHaveLength(0);
  }
});

test("retains pre-1.5 MCP Blame without new measurements", async () => {
  const event = { ...BLAME_EVENTS.mcp_proven, properties: { method: "tools_call", tool: "blame" } };
  const batch = { ...ordinaryBlameBatch([event]), app_version: "1.4.12" };
  const h = workerHarness({ now: BLAME_NOW });
  expect((await h.worker.fetch(ordinaryBlameRequest(batch), BLAME_ENV)).status).toBe(204);
  expect((await h.queueMessages())[0].row.properties).toEqual({
    method: "tools_call", tool: "blame", operation: "blame", outcome: "success",
  });
});

test("HTTP acknowledgement waits for ordinary Queue admission", async () => {
  let release;
  let started;
  const admitting = new Promise((resolve) => { started = resolve; });
  const queuePromise = new Promise((resolve) => { release = resolve; });
  const h = workerHarness({ now: BLAME_NOW, queuePromise });
  h.queueSendBatch.mockImplementationOnce(async () => { started(); await queuePromise; });
  let finished = false;
  const response = h.worker.fetch(ordinaryBlameRequest(ordinaryBlameBatch([BLAME_EVENTS.cli_none])), BLAME_ENV)
    .then((value) => { finished = true; return value; });
  await admitting;
  expect(finished).toBe(false);
  release();
  expect((await response).status).toBe(204);
});
