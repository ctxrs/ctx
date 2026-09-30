import { postgresQueryClient } from "./support/postgres-query-client.mjs";
import { expect, test } from "vitest";

import { NeonTelemetryDatabase } from "../src/database";
import { decodeTelemetryQueueMessage } from "../src/telemetry-queue";
import { createTelemetryWorker } from "../src/worker";
import { BASE_SCHEMA, scalar, sql, sqlFileAs, startPostgres } from "./support/postgres.mjs";
import {
  BLAME_ENV, BLAME_EVENTS, BLAME_NOW, ordinaryBlameBatch, ordinaryBlameRequest,
} from "./ordinary-blame-fixtures.mjs";

test("ordinary Rust-shaped Blame traverses HTTP, Queue, real SQL and typed constraints", async () => {
  const postgres = startPostgres();
  try {
    sql(postgres, BASE_SCHEMA);
    sqlFileAs(postgres, "0044_privacy_safe_telemetry_history.sql", "ctx_migration");
    sqlFileAs(postgres, "0044a_privacy_safe_telemetry_event_contract.sql", "neondb_owner");
    // Synthetic fixture grants only what the actual ordinary adapter uses.
    sql(postgres, "grant select, insert on ctx.telemetry_event to ctx_telemetry_ingest");
    const events = Object.values(BLAME_EVENTS).map((event, index) => ({
      ...event, event_id: `15000000-0000-4000-8000-${String(index + 1).padStart(12, "0")}`,
    }));
    const inserted = new NeonTelemetryDatabase(postgresQueryClient(postgres));
    let databaseCalls = 0;
    const worker = createTelemetryWorker({
      now: () => BLAME_NOW,
      createDatabaseClient() { databaseCalls += 1; return inserted; },
    });
    const bodies = [];
    const response = await worker.fetch(ordinaryBlameRequest(ordinaryBlameBatch(events)), {
      ...BLAME_ENV,
      TELEMETRY_INGEST_QUEUE: {
        async sendBatch(entries) { bodies.push(...Array.from(entries, ({ body }) => body)); },
      },
    });
    expect(response.status, await response.text()).toBe(204);
    expect(databaseCalls).toBe(0);
    expect(bodies).toHaveLength(events.length);
    for (const body of bodies) expect((await decodeTelemetryQueueMessage(body))?.kind).toBe("telemetry_row");
    for (let replay = 0; replay < 2; replay += 1) {
      let acknowledgements = 0;
      let retries = 0;
      await worker.queue({
        queue: "ctx-telemetry-ingest-staging",
        messages: bodies.map((body) => ({
          body, attempts: 1,
          ack() { acknowledgements += 1; },
          retry() { retries += 1; },
        })),
      }, BLAME_ENV);
      expect(acknowledgements).toBe(events.length);
      expect(retries).toBe(0);
      expect(scalar(postgres, "select count(*) from ctx.telemetry_event")).toBe(String(events.length));
    }
    const persisted = JSON.parse(scalar(postgres, `
      select json_agg(row_to_json(result) order by event_id) from (
        select event_id, event_name, schema_version, surface, status,
          client_profile_id_hash, data_root_id_hash, activity_class, properties
        from ctx.telemetry_event
      ) as result
    `));
    for (const [index, row] of persisted.entries()) {
      const event = events[index];
      expect(row).toMatchObject({
        event_id: event.event_id, event_name: "operation_completed", schema_version: 1,
        surface: event.surface, status: event.outcome,
        client_profile_id_hash: expect.stringMatching(/^[0-9a-f]{64}$/u),
        data_root_id_hash: expect.stringMatching(/^[0-9a-f]{64}$/u),
        activity_class: event.outcome === "success" ? "product_value" : "product_activity",
        properties: { ...event.properties, operation: "blame", outcome: event.outcome },
      });
    }
    expect(scalar(postgres, "select count(*) from ctx.blame_product_event")).toBe("0");
    expect(() => sql(postgres, "update ctx.telemetry_event set data_root_id_hash = null"))
      .toThrow(/telemetry_event_typed_v1_contract_chk/u);
    expect(scalar(postgres, "select ctx.delete_expired_raw_product_telemetry(clock_timestamp())")).toBe("0");
    expect(scalar(postgres, "select count(*) from ctx.telemetry_event")).toBe(String(events.length));
  } finally {
    postgres.cleanup();
  }
});
