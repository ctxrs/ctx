import { readFileSync } from "node:fs";

import { describe, expect, test } from "vitest";

import { createTelemetryWorker } from "../src/worker";
import { withCapturedQueue } from "./queue-capture.mjs";

const NOW = new Date("2026-09-13T12:00:00Z");
const ENV = {
  TELEMETRY_ANALYTICS_ENVIRONMENT: "production",
  TELEMETRY_DATABASE_URL: "postgresql://telemetry.example.test/db",
  TELEMETRY_IDENTITY_HMAC_KEY: "fixture-test-hmac-key-with-32-bytes-minimum",
  TELEMETRY_IDENTITY_KEY_VERSION: "1",
  TELEMETRY_RATE_LIMITER: { async limit() { return { success: true }; } },
};
const ROUTES = ["/functions/v1/analytics", "/functions/v1/telemetry"];
// Released producer values, independent of the receiver's allowlists.
const ACTIONS = ["install", "remove", "status"];
const TARGETS = ["mcp", "skills", "slash_commands", "plugin"];
// The event envelope is also checked against the Rust serializer.
const OPERATION_EVENT = JSON.parse(readFileSync(new URL(
  "../../../contracts/telemetry-v1/fixtures/operation_completed.valid.json", import.meta.url,
), "utf8"));

function batch(properties, outcome = "success", appVersion = "1.4.2") {
  return {
    client_profile_id: "11111111-1111-4111-8111-111111111111",
    data_root_id: "22222222-2222-4222-8222-222222222222",
    app_version: appVersion,
    os: "macos",
    arch: "aarch64",
    events: [{
      ...OPERATION_EVENT,
      occurred_at: NOW.toISOString(),
      operation: "integration",
      outcome,
      properties: { output: OPERATION_EVENT.properties.output, ...properties },
    }],
  };
}

function harness() {
  const telemetryWrites = [];
  const rejections = [];
  const worker = withCapturedQueue(createTelemetryWorker({
    now: () => NOW,
    observeRejection: () => {},
    createDatabaseClient: () => ({
      async insertTelemetryRows() { throw new Error("ingest_must_use_queue"); },
      async insertInstallStageRow() { throw new Error("unexpected_install_write"); },
      async recordIngestRejection(rejection) { rejections.push(rejection); },
    }),
  }), { telemetryWrites });
  return { telemetryWrites, rejections, worker };
}

function request(route, payload) {
  return new Request(`https://telemetry.example.test${route}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(payload),
  });
}

describe.each(ROUTES)("released integration telemetry on %s", (route) => {
  test.each(ACTIONS.flatMap((action) => TARGETS.flatMap((target) =>
    ["success", "failure"].map((outcome) => ({ action, target, outcome })),
  )))("queues and decodes $action/$target/$outcome", async ({ action, target, outcome }) => {
    const h = harness();
    const properties = {
      integration_action: action,
      integration_target: target,
      integration_scope: "global",
      target_agent_group: "explicit",
      force: false,
      resolved_agents_count_bucket: "1",
      integration_result: outcome === "success" ? "ok" : "partial_error",
      modified_targets_bucket: outcome === "success" ? "1" : "0",
    };
    const response = await h.worker.fetch(request(route, batch(properties, outcome)), ENV);

    expect(response.status, await response.text()).toBe(204);
    expect(h.rejections).toHaveLength(0);
    expect(h.telemetryWrites).toHaveLength(1);
    expect(h.telemetryWrites[0]).toHaveLength(1);
    expect(h.telemetryWrites[0][0]).toMatchObject({
      app_version: "1.4.2",
      event_name: "operation_completed",
      status: outcome,
      properties: { ...properties, operation: "integration", outcome },
    });
  });

  test.each(ACTIONS.flatMap((action) => ["global", "project"].flatMap((scope) =>
    ["success", "failure"].map((outcome) => ({ action, scope, outcome })),
  )))("queues Sift output-hook $action/$scope/$outcome", async ({ action, scope, outcome }) => {
    const h = harness();
    // The output-hook path emits these fields without integration_result or force.
    const properties = {
      integration_action: action,
      integration_target: "output_hook",
      integration_scope: scope,
      target_agent_group: "explicit",
      target_agents_count_bucket: "1",
    };
    const payload = batch(properties, outcome, "2.2.6");
    const response = await h.worker.fetch(request(route, payload), ENV);

    expect(response.status, await response.text()).toBe(204);
    expect(h.rejections).toHaveLength(0);
    expect(h.telemetryWrites).toHaveLength(1);
    expect(h.telemetryWrites[0]).toHaveLength(1);
    const row = h.telemetryWrites[0][0];
    expect(row).toMatchObject({
      app_version: "2.2.6",
      event_name: "operation_completed",
      status: outcome,
      activity_class: action === "status" ? "status" : "setup",
      client_profile_id_hash: expect.stringMatching(/^[0-9a-f]{64}$/u),
      data_root_id_hash: expect.stringMatching(/^[0-9a-f]{64}$/u),
    });
    expect(row.properties).toEqual({ ...payload.events[0].properties, operation: "integration", outcome });
    expect(row).not.toHaveProperty("client_profile_id");
    expect(row).not.toHaveProperty("data_root_id");
  });

  test.each([
    ["integration_action", "delete", "invalid_integration_action"],
    ["integration_action", true, "invalid_integration_action"],
    ["integration_action", " remove ", "invalid_integration_action"],
    ["integration_target", "arbitrary_plugin_name", "invalid_integration_target"],
    ["integration_target", "output-hook", "invalid_integration_target"],
    ["integration_target", "sift", "invalid_integration_target"],
    ["integration_target", " output_hook ", "invalid_integration_target"],
    ["integration_target", null, "invalid_integration_target"],
    ["integration_path", "/private/example", "unknown_operation_property"],
  ])("rejects invalid %s=%j before Queue admission", async (key, value, code) => {
    const h = harness();
    const payload = batch({ integration_action: "install", integration_target: "output_hook", [key]: value });
    const response = await h.worker.fetch(request(route, payload), ENV);

    expect(response.status).toBe(422);
    expect(await response.json()).toEqual({ error: code });
    expect(h.telemetryWrites).toHaveLength(0);
    expect(h.rejections).toHaveLength(1);
    expect(h.rejections[0].rejection_code).toBe(code);
  });
});
