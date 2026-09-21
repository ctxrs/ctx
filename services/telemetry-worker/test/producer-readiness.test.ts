import { describe, expect, test, vi } from "vitest";
import { createTelemetryWorker, type Env } from "../src/worker";
import { ENV, jsonRequest, operationEvent, v1Batch } from "./worker-test-fixtures";

describe("producer configuration readiness", () => {
  const invalidConfigurations: [string, Partial<Env>][] = [
    ["missing database", { TELEMETRY_DATABASE_URL: undefined }],
    ["invalid environment", { TELEMETRY_ANALYTICS_ENVIRONMENT: "dev" }],
    ["missing HMAC", { TELEMETRY_IDENTITY_HMAC_KEY: undefined }],
    ["short HMAC", { TELEMETRY_IDENTITY_HMAC_KEY: "secret-canary" }],
    ["blank HMAC", { TELEMETRY_IDENTITY_HMAC_KEY: " ".repeat(32) }],
    ["missing key version", { TELEMETRY_IDENTITY_KEY_VERSION: undefined }],
    ["zero key version", { TELEMETRY_IDENTITY_KEY_VERSION: "0" }],
    ["fractional key version", { TELEMETRY_IDENTITY_KEY_VERSION: "1.5" }],
    ["oversized key version", { TELEMETRY_IDENTITY_KEY_VERSION: "2147483648" }],
    ["missing Queue", { TELEMETRY_INGEST_QUEUE: undefined }],
    ["invalid Queue", { TELEMETRY_INGEST_QUEUE: {} as NonNullable<Env["TELEMETRY_INGEST_QUEUE"]> }],
    ["missing limiter", { TELEMETRY_RATE_LIMITER: undefined }],
    ["invalid limiter", { TELEMETRY_RATE_LIMITER: {} as NonNullable<Env["TELEMETRY_RATE_LIMITER"]> }],
  ];

  test.each(invalidConfigurations)("degrades before dependency reads for %s", async (_name, change) => {
    const harness = readinessHarness();
    const env = { ...harness.env, ...change };
    const response = await harness.worker.fetch(healthRequest(), env);
    expect(response.status).toBe(503);
    expect(await response.json()).toEqual({
      status: "degraded", reason: "producer_not_configured",
      ingestion: { status: "degraded", reason: "producer_not_configured" },
      client: { status: "unavailable", reason: "producer_not_configured" },
    });
    expect(harness.createDatabaseClient).not.toHaveBeenCalled();
    expect(harness.readQueueHealth).not.toHaveBeenCalled();
    expect(harness.limit).not.toHaveBeenCalled();
    expect(harness.sendBatch).not.toHaveBeenCalled();

    const ingestion = await harness.worker.fetch(
      jsonRequest("/functions/v1/telemetry", v1Batch([operationEvent()])), env,
    );
    expect(ingestion.status).toBeGreaterThanOrEqual(500);
    expect(harness.sendBatch).not.toHaveBeenCalled();
  });

  test.each(["staging", "production"])("checks %s without invoking producer bindings", async (environment) => {
    const harness = readinessHarness();
    const response = await harness.worker.fetch(healthRequest(), {
      ...harness.env, TELEMETRY_ANALYTICS_ENVIRONMENT: environment,
      TELEMETRY_IDENTITY_HMAC_KEY: "x".repeat(32),
      TELEMETRY_IDENTITY_KEY_VERSION: "2147483647",
    });
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({
      status: "ok", ingestion: { status: "ok" }, client: { status: "ok" },
    });
    expect(harness.createDatabaseClient).toHaveBeenCalledOnce();
    expect(harness.readIngestHealthSnapshot).toHaveBeenCalledOnce();
    expect(harness.readIngestHealthSnapshot).toHaveBeenCalledWith(environment);
    expect(harness.readQueueHealth).toHaveBeenCalledOnce();
    expect(harness.limit).not.toHaveBeenCalled();
    expect(harness.sendBatch).not.toHaveBeenCalled();
  });
});

function healthRequest() {
  return new Request("https://telemetry.example.test/functions/v1/analytics/health");
}

function readinessHarness() {
  const readIngestHealthSnapshot = vi.fn(async () => ({
    compatibilityRejectionMax: 0n, deliveryDegradedCount: 0n, deliveryDroppedCount: 0n,
    eventCollisionCount: 0n, otherRejectionCount: 0n, providerRefreshFailureCount: 0n,
  }));
  const createDatabaseClient = vi.fn(() => ({
    readIngestHealthSnapshot,
    insertTelemetryRows: vi.fn(), insertInstallStageRow: vi.fn(), insertBlameProductReceipt: vi.fn(),
  }));
  const readQueueHealth = vi.fn(async () => true);
  const limit = vi.fn(async () => ({ success: true }));
  const sendBatch = vi.fn(async () => {});
  return {
    createDatabaseClient, readIngestHealthSnapshot, readQueueHealth, limit, sendBatch,
    env: { ...ENV, TELEMETRY_RATE_LIMITER: { limit }, TELEMETRY_INGEST_QUEUE: { sendBatch } },
    worker: createTelemetryWorker({ createDatabaseClient, readQueueHealth }),
  };
}
