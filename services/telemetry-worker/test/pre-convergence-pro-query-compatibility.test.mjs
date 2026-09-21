import { readFileSync } from "node:fs";

import { describe, expect, test } from "vitest";

import { buildTelemetryIngestPlan } from "../src/telemetry-ingest";
import { createTelemetryWorker } from "../src/worker";
import { withCapturedQueue } from "./queue-capture.mjs";

const FIXTURE_ROOT = new URL("./fixtures/public-telemetry-v1/", import.meta.url);
const VALID_FIXTURE_URL = new URL(
  "pro-query-v026-pre-convergence.valid.json",
  FIXTURE_ROOT,
);
const INVALID_FIXTURE_URL = new URL(
  "pro-query-v026-pre-convergence.invalid.json",
  FIXTURE_ROOT,
);
const VALID = JSON.parse(readFileSync(VALID_FIXTURE_URL, "utf8"));
const INVALID = JSON.parse(readFileSync(INVALID_FIXTURE_URL, "utf8"));
const NOW = new Date("2026-07-25T23:35:00.000Z");
const INGEST_OPTIONS = {
  analyticsEnvironment: "staging",
  identityHmacKey: "v026-query-hmac-key-with-32-bytes-minimum",
  identityKeyVersion: 1,
  now: () => NOW,
};
const ENV = {
  TELEMETRY_ANALYTICS_ENVIRONMENT: "staging",
  TELEMETRY_DATABASE_URL: "postgresql://telemetry.example.test/db",
  TELEMETRY_IDENTITY_HMAC_KEY: INGEST_OPTIONS.identityHmacKey,
  TELEMETRY_IDENTITY_KEY_VERSION: "1",
  TELEMETRY_RATE_LIMITER: { async limit() { return { success: true }; } },
};
describe("pinned pre-convergence v0.26 Pro query compatibility", () => {
  test("retains the released closed property vocabulary", () => {
    expect(VALID.schema_version).toBe(1);
    expect(VALID.operation).toBe("query");
    expect(VALID.required_properties).toEqual([
      "query_kind",
      "query_surface",
      "helper_connection_outcome",
      "query_auto_materialization",
    ]);
    expect(VALID.optional_properties).toEqual([
      "access_state",
      "query_result_count_bucket",
      "query_empty",
      "query_truncated",
      "query_freshness",
      "query_failure_bucket",
      "materialization_mode",
      "materialization_commit",
      "materialization_freshness",
      "materialization_result",
      "materialization_batch_count_bucket",
      "materialization_input_count_bucket",
      "materialization_output_count_bucket",
      "materialization_lag_bucket",
      "materialization_failure_bucket",
    ]);
    expect(Object.keys(VALID.dimensions)).toEqual([
      "query_kind",
      "query_surface",
      "access_state",
      "helper_connection_outcome",
      "query_result_count_bucket",
      "query_empty",
      "query_truncated",
      "query_freshness",
      "query_auto_materialization",
      "query_failure_bucket",
      "materialization_mode",
      "materialization_commit",
      "materialization_freshness",
      "materialization_result",
      "materialization_batch_count_bucket",
      "materialization_input_count_bucket",
      "materialization_output_count_bucket",
      "materialization_lag_bucket",
      "materialization_failure_bucket",
    ]);
    for (const values of Object.values(VALID.dimensions)) {
      expect(new Set(values).size).toBe(values.length);
    }
  });

  test("accepts representative success and failure producer events through the Worker", async () => {
    const harness = workerHarness();
    const response = await harness.worker.fetch(
      jsonRequest(VALID.batch),
      ENV,
    );

    expect(response.status, await response.text()).toBe(204);
    expect(harness.telemetryWrites).toHaveLength(1);
    expect(harness.telemetryWrites[0]).toHaveLength(2);
    expect(harness.telemetryWrites[0].map(({ activity_class }) => activity_class)).toEqual([
      "product_value",
      "product_activity",
    ]);
    expect(harness.telemetryWrites[0].map(({ properties }) => properties.operation))
      .toEqual(["query", "query"]);
  });

  test("accepts the producer's required-only query shape", async () => {
    const event = queryEvent(minimalQueryProperties());
    const plan = await buildTelemetryIngestPlan(batch([event]), INGEST_OPTIONS);

    expect(plan.rows).toHaveLength(1);
    expect(plan.rows[0]).toMatchObject({
      activity_class: "product_activity",
      app_version: "0.26.0",
      surface: "pro_host",
      status: "success",
      properties: {
        operation: "query",
        outcome: "success",
        ...minimalQueryProperties(),
      },
    });
  });

  test.each(Object.entries(VALID.dimensions))(
    "accepts every closed %s value",
    async (property, values) => {
      const events = values.map((value, index) => queryEvent(
        queryPropertiesForVariant(property, value),
        {
          event_id: eventId(index + 1),
          outcome: property.endsWith("failure_bucket") ? "failure" : "success",
        },
      ));

      const plan = await buildTelemetryIngestPlan(batch(events), INGEST_OPTIONS);

      expect(plan.rows).toHaveLength(values.length);
      expect(new Set(plan.rows.map(({ properties }) => properties[property])))
        .toEqual(new Set(values));
    },
  );

  test.each(INVALID.cases)(
    "rejects $name",
    async ({ operation, field, value, remove, error }) => {
      const properties = baseProperties(operation);
      if (remove) delete properties[remove];
      if (field) properties[field] = value;
      const event = operationEvent(operation, properties);

      await expect(buildTelemetryIngestPlan(batch([event]), INGEST_OPTIONS))
        .rejects.toMatchObject({ code: error });
    },
  );

  test("keeps final NativePath status and blame acceptance/classification unchanged", async () => {
    const events = [
      operationEvent("status", baseProperties("status"), {
        event_id: eventId(1),
      }),
      operationEvent("blame", baseProperties("blame"), {
        event_id: eventId(2),
      }),
    ];

    const plan = await buildTelemetryIngestPlan(batch(events), INGEST_OPTIONS);

    expect(plan.rows.map(({ activity_class }) => activity_class)).toEqual([
      "operational",
      "product_value",
    ]);
    expect(plan.rows.map(({ properties }) => properties)).toEqual([
      {
        status_surface: "mcp",
        access_state: "active",
        helper_connection_outcome: "connected",
        operation: "status",
        outcome: "success",
      },
      {
        blame_target_kind: "pull_request",
        blame_surface: "mcp",
        blame_result_count_bucket: "2-5",
        blame_has_more: true,
        operation: "blame",
        outcome: "success",
      },
    ]);
  });

});

function queryPropertiesForVariant(property, value) {
  const properties = minimalQueryProperties();
  if (property.startsWith("materialization_")) {
    Object.assign(properties, materializationProperties());
    properties.query_auto_materialization = "completed";
  }
  if (property === "query_auto_materialization" && value !== "not_needed") {
    Object.assign(properties, materializationProperties());
  }
  properties[property] = value;
  return properties;
}

function minimalQueryProperties() {
  return {
    query_kind: "show",
    query_surface: "cli",
    helper_connection_outcome: "connected",
    query_auto_materialization: "not_needed",
  };
}

function materializationProperties() {
  return {
    materialization_mode: "incremental",
    materialization_commit: "committed",
    materialization_freshness: "current",
    materialization_result: "completed",
    materialization_batch_count_bucket: "1",
    materialization_input_count_bucket: "2-5",
    materialization_output_count_bucket: "2-5",
    materialization_lag_bucket: "0",
  };
}

function baseProperties(operation) {
  if (operation === "query") {
    return {
      ...minimalQueryProperties(),
      query_kind: "facts",
      query_surface: "mcp",
      query_result_count_bucket: "2-5",
      query_empty: false,
      query_truncated: true,
      query_freshness: "current",
      query_auto_materialization: "completed",
      ...materializationProperties(),
    };
  }
  if (operation === "status") {
    return {
      status_surface: "mcp",
      access_state: "active",
      helper_connection_outcome: "connected",
    };
  }
  if (operation === "blame") {
    return {
      blame_target_kind: "pull_request",
      blame_surface: "mcp",
      blame_result_count_bucket: "2-5",
      blame_has_more: true,
    };
  }
  if (operation === "lifecycle") {
    return {
      lifecycle_operation: "status",
      access_state: "active",
      helper_connection_outcome: "connected",
      reconcile_outcome: "current",
    };
  }
  if (operation === "materialize") {
    return {
      ...materializationProperties(),
      helper_connection_outcome: "connected",
    };
  }
  throw new Error(`unsupported Pro operation fixture: ${operation}`);
}

function queryEvent(properties, overrides = {}) {
  return operationEvent("query", properties, overrides);
}

function operationEvent(operation, properties, overrides = {}) {
  return {
    event_id: eventId(1),
    event_name: "operation_completed",
    event_version: 1,
    occurred_at: "2026-07-25T23:34:00Z",
    surface: "pro_host",
    operation,
    outcome: "success",
    duration_bucket: "lt_1s",
    properties,
    ...overrides,
  };
}

function batch(events) {
  return {
    client_profile_id: "11111111-1111-4111-8111-111111111111",
    data_root_id: "22222222-2222-4222-8222-222222222222",
    app_version: "0.26.0",
    os: "linux",
    arch: "x86_64",
    events,
  };
}

function eventId(sequence) {
  return `10000000-0000-4000-8000-${String(sequence).padStart(12, "0")}`;
}

function jsonRequest(body) {
  return new Request("https://api.example.test/functions/v1/telemetry", {
    method: "POST",
    body: JSON.stringify(body),
    headers: { "content-type": "application/json; charset=utf-8" },
  });
}

function workerHarness() {
  const telemetryWrites = [];
  const database = {
    async insertTelemetryRows(rows) {
      telemetryWrites.push(rows);
    },
    async insertInstallStageRow() {},
  };
  const worker = withCapturedQueue(createTelemetryWorker({
    createDatabaseClient: () => database,
    now: () => NOW,
  }), { telemetryWrites });
  return { telemetryWrites, worker };
}
