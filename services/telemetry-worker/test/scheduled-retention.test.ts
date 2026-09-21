import { afterEach, describe, expect, test, vi } from "vitest";

import {
  NeonTelemetryMaintenanceDatabase,
  type NeonQueryClient,
  type TelemetryDatabase,
  type TelemetryMaintenanceDatabase,
  type TelemetryHistoryMaterialization,
} from "../src/database";
import { createTelemetryWorker, type Env } from "../src/worker";

const INGESTION_DATABASE_URL = "postgresql://ctx_telemetry_ingest@db.example.test/neondb";
const RETENTION_DATABASE_URL = "postgresql://ctx_telemetry_retention@db.example.test/neondb";
const RETENTION_CRON = "17 3 * * *";
const MATERIALIZATION_SUCCESS_CODE = "telemetry_history_materialization_succeeded";
const MATERIALIZATION_FAILURE_CODE = "telemetry_history_materialization_failed";
const HMAC_KEY = "telemetry-test-hmac-key-with-32-bytes-minimum";
const WINDOW = { first_received_date: "2026-07-14", last_received_date: "2026-07-22" };

const ENV: Env = {
  TELEMETRY_ANALYTICS_ENVIRONMENT: "production",
  TELEMETRY_DATABASE_URL: INGESTION_DATABASE_URL,
  TELEMETRY_IDENTITY_HMAC_KEY: HMAC_KEY,
  TELEMETRY_IDENTITY_KEY_VERSION: "7",
  TELEMETRY_RETENTION_DATABASE_URL: RETENTION_DATABASE_URL,
  TELEMETRY_RATE_LIMITER: { async limit() { return { success: true }; } },
};

const SCHEDULED_EVENT: ScheduledController = {
  cron: RETENTION_CRON,
  noRetry() {},
  scheduledTime: Date.parse("2026-07-23T03:17:00Z"),
};

afterEach(() => {
  vi.restoreAllMocks();
});

describe("scheduled permanent telemetry history", () => {
  test("uses only the maintenance credential and logs the materialized count", async () => {
    const harness = workerHarness({ collisionReceiptsDeleted: "5", materializedCount: "37" });
    const info = vi.spyOn(console, "info").mockImplementation(() => {});
    const error = vi.spyOn(console, "error").mockImplementation(() => {});

    await harness.worker.scheduled(SCHEDULED_EVENT, ENV);

    expect(harness.createMaintenanceDatabaseClient).toHaveBeenCalledWith(RETENTION_DATABASE_URL);
    expect(harness.createDatabaseClient).not.toHaveBeenCalled();
    expect(harness.deleteExpiredTelemetryEventCollisionReceipts).toHaveBeenCalledOnce();
    expect(harness.materializeProductTelemetryHistory).toHaveBeenCalledOnce();
    expect(info).toHaveBeenCalledWith(MATERIALIZATION_SUCCESS_CODE, {
      collision_receipts_deleted: "5",
      materialized_count: "37",
      ...WINDOW,
    });
    expect(error).not.toHaveBeenCalled();
  });

  test("throws a sanitized stable error after a database failure so the run can retry", async () => {
    const sensitiveDetail = `database unavailable at ${RETENTION_DATABASE_URL}`;
    const harness = workerHarness({ maintenanceError: new Error(sensitiveDetail) });
    const info = vi.spyOn(console, "info").mockImplementation(() => {});
    const error = vi.spyOn(console, "error").mockImplementation(() => {});

    await expect(harness.worker.scheduled(SCHEDULED_EVENT, ENV))
      .rejects.toThrow(MATERIALIZATION_FAILURE_CODE);

    expect(error).toHaveBeenCalledWith(MATERIALIZATION_FAILURE_CODE);
    expect(info).not.toHaveBeenCalled();
    expect(JSON.stringify(error.mock.calls)).not.toContain(sensitiveDetail);
    expect(JSON.stringify(error.mock.calls)).not.toContain(RETENTION_DATABASE_URL);
  });

  test("requires the dedicated maintenance credential instead of falling back to ingestion", async () => {
    const harness = workerHarness();
    vi.spyOn(console, "error").mockImplementation(() => {});
    const envWithoutRetention = { ...ENV };
    delete envWithoutRetention.TELEMETRY_RETENTION_DATABASE_URL;

    await expect(harness.worker.scheduled(SCHEDULED_EVENT, envWithoutRetention))
      .rejects.toThrow(MATERIALIZATION_FAILURE_CODE);

    expect(harness.createMaintenanceDatabaseClient).not.toHaveBeenCalled();
    expect(harness.createDatabaseClient).not.toHaveBeenCalled();
  });

  test("does not expose the retention credential to HTTP ingestion", async () => {
    const harness = workerHarness();
    const response = await harness.worker.fetch(installStageRequest(), ENV);

    expect(response.status).toBe(204);
    expect(harness.createDatabaseClient).not.toHaveBeenCalled();
    expect(harness.createMaintenanceDatabaseClient).not.toHaveBeenCalled();
    expect(harness.queueSendBatch).toHaveBeenCalledOnce();
  });

  test("drains a receipt spike and stops after the first partial batch", async () => {
    const calls: Array<{ params: readonly unknown[]; sql: string }> = [];
    const cleanupResults = ["128", "128", "3"];
    let cleanupCallCount = 0;
    const client: NeonQueryClient = {
      async query<T extends Record<string, unknown> = Record<string, unknown>>(
        sql: string,
        params: readonly (string | number | boolean | null)[] = [],
      ): Promise<T[]> {
        calls.push({ params, sql });
        if (sql.includes("delete_expired_telemetry_event_collision_receipts")) {
          const deletedCount = cleanupResults[cleanupCallCount];
          cleanupCallCount += 1;
          if (deletedCount === undefined) throw new Error("unexpected cleanup query");
          return [{ deleted_count: deletedCount }] as unknown as T[];
        }
        return [{ materialized_count: "12", ...WINDOW }] as unknown as T[];
      },
      async transaction() {
        throw new Error("unexpected transaction");
      },
    };

    const database = new NeonTelemetryMaintenanceDatabase(client);
    const deletedCount = await database.deleteExpiredTelemetryEventCollisionReceipts();
    const materializedCount = await database.materializeProductTelemetryHistory();

    expect(deletedCount).toBe("259");
    expect(materializedCount).toEqual({ materialized_count: "12", ...WINDOW });
    expect(calls).toHaveLength(4);
    for (const call of calls.slice(0, 3)) {
      expect(call.params).toEqual([]);
      expect(call.sql).toMatch(/delete_expired_telemetry_event_collision_receipts/u);
      expect(call.sql).toMatch(/interval '9 days'/u);
      expect(call.sql).toMatch(/128/u);
    }
    expect(calls[3].sql).toMatch(/ctx\.materialize_product_telemetry_history/u);
    expect(calls[3].sql).toMatch(/statement_timestamp\(\) AT TIME ZONE 'utc'/u);
    expect(calls[3].sql).not.toMatch(/delete_expired_raw_product_telemetry/u);
  });

  test("caps receipt cleanup after eight full batches", async () => {
    const cleanupQueries: string[] = [];
    const client: NeonQueryClient = {
      async query<T extends Record<string, unknown> = Record<string, unknown>>(
        sql: string,
      ): Promise<T[]> {
        cleanupQueries.push(sql);
        return [{ deleted_count: "128" }] as unknown as T[];
      },
      async transaction() {
        throw new Error("unexpected transaction");
      },
    };

    const database = new NeonTelemetryMaintenanceDatabase(client);
    const deletedCount = await database.deleteExpiredTelemetryEventCollisionReceipts();

    expect(deletedCount).toBe("1024");
    expect(cleanupQueries).toHaveLength(8);
    for (const sql of cleanupQueries) {
      expect(sql).toMatch(/delete_expired_telemetry_event_collision_receipts/u);
      expect(sql).toMatch(/128/u);
    }
  });

  test.each([
    { result: [] },
    { result: [{ materialized_count: "-1", ...WINDOW }] },
    { result: [{ materialized_count: "2", first_received_date: "secret-canary", last_received_date: "2026-07-22" }] },
    { result: [{ materialized_count: "2", ...WINDOW }, { materialized_count: "3", ...WINDOW }] },
  ])("rejects malformed window receipts %j", async ({ result }) => {
    const client = {
      query: vi.fn(async () => result),
      transaction: vi.fn(),
    } as unknown as NeonQueryClient;
    const database = new NeonTelemetryMaintenanceDatabase(client);
    await expect(database.materializeProductTelemetryHistory())
      .rejects.toThrow("invalid_telemetry_materialization_result");
  });
});

function workerHarness(options: {
  collisionReceiptsDeleted?: string;
  maintenanceError?: Error;
  materializedCount?: string;
} = {}) {
  const insertInstallStageRow = vi.fn(async () => {});
  const insertTelemetryRows = vi.fn(async () => {});
  const insertBlameProductReceipt = vi.fn(async () => {});
  const ingestionDatabase: TelemetryDatabase = {
    insertBlameProductReceipt,
    insertInstallStageRow,
    insertTelemetryRows,
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
  const materializeProductTelemetryHistory = vi.fn(async (): Promise<TelemetryHistoryMaterialization> => {
    if (options.maintenanceError) throw options.maintenanceError;
    return { materialized_count: options.materializedCount ?? "0", ...WINDOW };
  });
  const deleteExpiredTelemetryEventCollisionReceipts = vi.fn(async () => {
    return options.collisionReceiptsDeleted ?? "0";
  });
  const maintenanceDatabase: TelemetryMaintenanceDatabase = {
    deleteExpiredTelemetryEventCollisionReceipts,
    materializeProductTelemetryHistory,
  };
  const createDatabaseClient = vi.fn(() => ingestionDatabase);
  const createMaintenanceDatabaseClient = vi.fn(() => maintenanceDatabase);
  const implementation = createTelemetryWorker({
    createDatabaseClient,
    createMaintenanceDatabaseClient,
  });
  const queueSendBatch = vi.fn(async () => {});
  const worker = {
    fetch(request: Request, env: Env) {
      return implementation.fetch(request, {
        ...env,
        TELEMETRY_INGEST_QUEUE: { sendBatch: queueSendBatch },
      });
    },
    queue: implementation.queue,
    scheduled: implementation.scheduled,
  };
  return {
    createDatabaseClient,
    createMaintenanceDatabaseClient,
    deleteExpiredTelemetryEventCollisionReceipts,
    materializeProductTelemetryHistory,
    queueSendBatch,
    insertInstallStageRow,
    worker,
  };
}

function installStageRequest(): Request {
  return new Request("https://cli.ctx.rs/functions/v1/install-attempt", {
    body: JSON.stringify({
      arch: "x64",
      event_name: "install_stage",
      event_version: 1,
      install_attempt_id: "ia_0123456789abcdef",
      platform: "linux",
      script_family: "posix",
      stage: "installer",
      status: "started",
    }),
    headers: { "content-type": "application/json; charset=utf-8" },
    method: "POST",
  });
}
