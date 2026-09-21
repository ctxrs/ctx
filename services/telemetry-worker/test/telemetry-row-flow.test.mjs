import { readFileSync } from "node:fs";

import { describe, expect, test } from "vitest";

import { NeonTelemetryDatabase } from "../src/database";
import { createTelemetryWorker } from "../src/worker";

const FIXTURE = JSON.parse(readFileSync(
  new URL("./fixtures/public-telemetry-v1/row-flow-candidate.valid.json", import.meta.url),
  "utf8",
));
const PUBLIC_PROVIDER_FIXTURE = JSON.parse(readFileSync(
  new URL(
    "./fixtures/public-telemetry-v1/provider_refresh_completed.privacy-observability.valid.json",
    import.meta.url,
  ),
  "utf8",
));
const NOW = new Date("2026-07-22T12:35:00.000Z");
const ENV = {
  TELEMETRY_ANALYTICS_ENVIRONMENT: "staging",
  TELEMETRY_DATABASE_URL: "postgresql://telemetry.example.test/db",
  TELEMETRY_IDENTITY_HMAC_KEY: "row-flow-hmac-key-with-32-bytes-minimum",
  TELEMETRY_IDENTITY_KEY_VERSION: "1",
  TELEMETRY_RATE_LIMITER: { async limit() { return { success: true }; } },
};

describe("candidate to readonly telemetry row flow", () => {
  test("preserves candidate dimensions through Worker parsing and Neon insert parameters", async () => {
    const neon = neonRecorder();
    const worker = createTelemetryWorker({
      createDatabaseClient: () => new NeonTelemetryDatabase(neon.client),
      now: () => NOW,
    });
    const queueBodies = [];

    const response = await worker.fetch(jsonRequest(FIXTURE.batch), {
      ...ENV,
      TELEMETRY_INGEST_QUEUE: {
        async sendBatch(entries) {
          queueBodies.push(...Array.from(entries, ({ body }) => body));
        },
      },
    });

    expect(response.status, await response.text()).toBe(204);
    expect(queueBodies).toHaveLength(FIXTURE.batch.events.length);
    expect(neon.transactions).toHaveLength(0);
    await worker.queue({
      queue: "ctx-telemetry-ingest-staging",
      messages: queueBodies.map((body) => ({ body, attempts: 1, ack() {}, retry() {} })),
    }, ENV);
    expect(FIXTURE.batch.events[0]).toEqual(PUBLIC_PROVIDER_FIXTURE);
    expect(neon.transactions).toEqual([{ isolationLevel: "ReadCommitted" }]);
    const sourceOrder = new Map(FIXTURE.batch.events.map((event, index) => [event.event_id, index]));
    const insertedRows = telemetryInsertRows(neon.queries).sort((left, right) => (
      sourceOrder.get(left.event_id) - sourceOrder.get(right.event_id)
    ));
    expect(insertedRows).toHaveLength(FIXTURE.batch.events.length);
    expect(insertedRows.map(({ event_name }) => event_name)).toEqual([
      "provider_refresh_completed",
      "operation_completed",
      "operation_completed",
      "provider_refresh_completed",
    ]);
    expect(insertedRows[0]).toMatchObject({
      analytics_environment: "staging",
      traffic_class: "synthetic",
      schema_version: 1,
      surface: "cli",
      provider_id: "codex",
      status: "success",
      success: true,
    });
    expect(insertedRows[0].properties).toEqual({
      ...FIXTURE.batch.events[0].properties,
      operation: "refresh",
      outcome: "success",
    });
    expect(insertedRows[1]).toMatchObject({
      analytics_environment: "staging",
      traffic_class: "synthetic",
      activity_class: "product_value",
      schema_version: 1,
      surface: "pro_host",
      status: "success",
      success: true,
    });
    expect(insertedRows[1].properties).toEqual({
      ...FIXTURE.batch.events[1].properties,
      operation: "blame",
      outcome: "success",
    });
    expect(insertedRows[2]).toMatchObject({
      analytics_environment: "staging",
      traffic_class: "synthetic",
      activity_class: "product_value",
      surface: "cli",
      status: "success",
      success: true,
    });
    expect(insertedRows[2].properties).toEqual({
      ...FIXTURE.batch.events[2].properties,
      operation: "search",
      outcome: "success",
    });
    expect(insertedRows[3]).toMatchObject({
      analytics_environment: "staging",
      traffic_class: "synthetic",
      activity_class: "automatic",
      surface: "daemon",
      status: "success",
      success: true,
    });
    expect(insertedRows[3].properties).toEqual({
      ...FIXTURE.batch.events[3].properties,
      operation: "refresh",
      outcome: "success",
    });
  });
});

function neonRecorder() {
  const queries = [];
  const transactions = [];
  const query = async (sql, params = []) => {
    queries.push({ params, sql });
    return [];
  };
  return {
    client: {
      query,
      async transaction(build, options) {
        transactions.push(options);
        return Promise.all(build({ query }));
      },
    },
    queries,
    transactions,
  };
}

function telemetryInsertRows(queries) {
  const inserts = queries.filter(({ sql }) => (
    /\bINSERT INTO ctx\.telemetry_event\b/u.test(sql)
  ));
  const rows = [];
  for (const insert of inserts) {
    const columnsMatch = insert.sql.match(
      /\bINSERT INTO ctx\.telemetry_event\s*\(([^)]+)\)\s*VALUES/su,
    );
    expect(columnsMatch, "missing telemetry insert column list").not.toBeNull();
    const columns = columnsMatch[1].split(",").map((column) => column.trim());
    expect(insert.params.length % columns.length).toBe(0);
    for (let offset = 0; offset < insert.params.length; offset += columns.length) {
      const row = Object.fromEntries(columns.map((column, index) => (
        [column, insert.params[offset + index]]
      )));
      row.properties = JSON.parse(row.properties);
      rows.push(row);
    }
  }
  return rows;
}

function jsonRequest(body) {
  return new Request("https://api.example.test/functions/v1/telemetry", {
    method: "POST",
    body: JSON.stringify(body),
    headers: { "content-type": "application/json; charset=utf-8" },
  });
}
