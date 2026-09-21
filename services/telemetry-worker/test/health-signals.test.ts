import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";

import type { Env } from "../src/worker";
import { ENV, workerHarness } from "./worker-test-fixtures";

// Keep route names, thresholds, and expected responses independent of production helpers.
const SIGNALS = [
  "producer_configuration", "database_availability", "compatibility_rejections",
  "event_collisions", "other_rejections", "queue_health", "queue_metrics",
] as const;
type Signal = typeof SIGNALS[number];
type Status = "ok" | "degraded" | "unavailable";
type Harness = ReturnType<typeof workerHarness>;
const HEALTH_URL = "https://api.example.test/functions/v1/analytics/health";
const OK: Record<Signal, Status> = {
  producer_configuration: "ok", database_availability: "ok",
  compatibility_rejections: "ok", event_collisions: "ok", other_rejections: "ok",
  queue_health: "ok", queue_metrics: "ok",
};
const BELOW_THRESHOLDS = {
  compatibilityRejectionMax: 99n, eventCollisionCount: 0n, otherRejectionCount: 4n,
  providerRefreshFailureCount: 4n, deliveryDegradedCount: 4n, deliveryDroppedCount: 0n,
};
const WIRE_STATUS = {
  ok: { http: 200, alert: false },
  degraded: { http: 503, alert: true },
  unavailable: { http: 200, alert: false },
} as const;

function expectNoWrites(harness: Harness) {
  for (const write of [
    harness.insertTelemetryRows, harness.insertInstallStageRow,
    harness.insertBlameProductReceipt, harness.recordIngestRejection,
    harness.observeRejection, harness.queueSendBatch,
  ]) expect(write).not.toHaveBeenCalled();
  expect(harness.queueBodies).toEqual([]);
}

async function expectSignal(harness: Harness, signal: Signal, status: Status, env: Env = ENV) {
  const waitUntil = vi.fn();
  const response = await harness.worker.fetch(
    new Request(`${HEALTH_URL}/${signal}`), env, { waitUntil },
  );
  expect(response.status).toBe(WIRE_STATUS[status].http);
  expect(await response.json()).toEqual({ signal, status, alert: WIRE_STATUS[status].alert });
  expect(response.headers.get("cache-control")).toBe("no-store");
  expect(response.headers.get("x-content-type-options")).toBe("nosniff");
  expect(response.headers.has("access-control-allow-origin")).toBe(false);
  expect(waitUntil).not.toHaveBeenCalled();
  expectNoWrites(harness);
}

async function expectSignals(harness: Harness, expected: Record<Signal, Status>, env: Env = ENV) {
  for (const signal of SIGNALS) await expectSignal(harness, signal, expected[signal], env);
}

beforeEach(() => {
  vi.stubGlobal("fetch", vi.fn(async () => { throw new Error("unexpected_live_request"); }));
  vi.spyOn(console, "error").mockImplementation(() => {});
});

afterEach(() => {
  try {
    expect(fetch).not.toHaveBeenCalled();
  } finally {
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  }
});

describe("independent service-health routes", () => {
  test.each(SIGNALS)("%s reads only its owning dependency", async (signal) => {
    const harness = workerHarness({ healthSnapshot: { ...BELOW_THRESHOLDS } });
    const limit = vi.fn(async () => ({ success: false }));
    await expectSignal(harness, signal, "ok", { ...ENV, TELEMETRY_RATE_LIMITER: { limit } });
    const databaseReads = [
      "database_availability", "compatibility_rejections", "event_collisions", "other_rejections",
    ].includes(signal) ? 1 : 0;
    expect(harness.createDatabaseClient).toHaveBeenCalledTimes(databaseReads);
    expect(harness.readIngestHealthSnapshot).toHaveBeenCalledTimes(databaseReads);
    if (databaseReads) {
      expect(harness.createDatabaseClient).toHaveBeenCalledWith(ENV.TELEMETRY_DATABASE_URL);
      expect(harness.readIngestHealthSnapshot).toHaveBeenCalledWith("production");
    }
    expect(harness.readQueueHealth).toHaveBeenCalledTimes(
      signal === "queue_health" || signal === "queue_metrics" ? 1 : 0,
    );
    expect(limit).not.toHaveBeenCalled();
  });

  test.each([
    ["compatibility_rejections", { compatibilityRejectionMax: 100n }],
    ["event_collisions", { eventCollisionCount: 1n }],
    ["other_rejections", { otherRejectionCount: 5n }],
  ] as const)("only %s alerts at its unchanged threshold", async (signal, change) => {
    const harness = workerHarness({ healthSnapshot: { ...BELOW_THRESHOLDS, ...change } });
    await expectSignals(harness, { ...OK, [signal]: "degraded" });
    const response = await harness.worker.fetch(new Request(HEALTH_URL), ENV);
    expect(response.status).toBe(503);
    expect(await response.json()).toEqual({
      status: "degraded", reason: "telemetry_health_signal",
      ingestion: { status: "degraded", reason: "telemetry_health_signal" },
      client: { status: "ok" },
    });
  });

  test("a collision can start, recover, and recur while compatibility remains active", async () => {
    const healthSnapshot = { ...BELOW_THRESHOLDS };
    const harness = workerHarness({ healthSnapshot });
    await expectSignals(harness, OK);
    healthSnapshot.compatibilityRejectionMax = 100n;
    await expectSignals(harness, { ...OK, compatibility_rejections: "degraded" });
    for (const [count, status] of [[1n, "degraded"], [0n, "ok"], [1n, "degraded"]] as const) {
      healthSnapshot.eventCollisionCount = count;
      await expectSignals(harness, {
        ...OK, compatibility_rejections: "degraded", event_collisions: status,
      });
    }
    healthSnapshot.compatibilityRejectionMax = 99n;
    await expectSignals(harness, { ...OK, event_collisions: "degraded" });
    healthSnapshot.eventCollisionCount = 0n;
    await expectSignals(harness, OK);
  });

  test("reports simultaneous DB rejection and Queue degradation independently", async () => {
    const harness = workerHarness({
      healthSnapshot: {
        ...BELOW_THRESHOLDS, compatibilityRejectionMax: 100n,
        eventCollisionCount: 1n, otherRejectionCount: 5n,
      },
      queueHealthy: false,
    });
    await expectSignals(harness, {
      ...OK, compatibility_rejections: "degraded", event_collisions: "degraded",
      other_rejections: "degraded", queue_health: "degraded",
    });
  });

  test.each([
    ["identity", { TELEMETRY_IDENTITY_HMAC_KEY: undefined }],
    ["rate limiter", { TELEMETRY_RATE_LIMITER: undefined }],
  ] as const)("missing producer %s does not mask independent database and Queue faults", async (_name, change) => {
    const harness = workerHarness({
      healthSnapshot: { ...BELOW_THRESHOLDS, eventCollisionCount: 1n }, queueHealthy: false,
    });
    await expectSignals(harness, {
      ...OK, producer_configuration: "degraded", event_collisions: "degraded", queue_health: "degraded",
    }, { ...ENV, ...change });
    expect(harness.readIngestHealthSnapshot).toHaveBeenCalledTimes(4);
    expect(harness.readQueueHealth).toHaveBeenCalledTimes(2);
  });

  test.each(["connect", "snapshot"] as const)("DB %s failure does not mask Queue health", async (stage) => {
    const harness = workerHarness({ queueHealthy: false });
    const failure = new Error("postgresql://test:test@db.example.test/db");
    if (stage === "connect") {
      harness.createDatabaseClient.mockImplementation(() => { throw failure; });
    } else {
      harness.readIngestHealthSnapshot.mockRejectedValue(failure);
    }
    await expectSignals(harness, {
      ...OK, database_availability: "degraded", compatibility_rejections: "unavailable",
      event_collisions: "unavailable", other_rejections: "unavailable", queue_health: "degraded",
    });
    expect(harness.readQueueHealth).toHaveBeenCalledTimes(2);
    expect(JSON.stringify(vi.mocked(console.error).mock.calls)).not.toContain(failure.message);
  });

  test("an absent database URL alerts on producer configuration and leaves database signals unavailable", async () => {
    const harness = workerHarness();
    const env = { ...ENV, TELEMETRY_DATABASE_URL: undefined };
    await expectSignal(harness, "producer_configuration", "degraded", env);
    await expectSignal(harness, "database_availability", "unavailable", env);
    for (const signal of ["compatibility_rejections", "event_collisions", "other_rejections"] as const) {
      await expectSignal(harness, signal, "unavailable", env);
    }
    expect(harness.createDatabaseClient).not.toHaveBeenCalled();
    expect(harness.readQueueHealth).not.toHaveBeenCalled();
  });

  test("Queue metric failures own the alert and recover independently of DB rejections", async () => {
    const options = {
      healthSnapshot: { ...BELOW_THRESHOLDS, eventCollisionCount: 1n },
      queueHealthy: false,
      queueHealthError: undefined as Error | undefined,
    };
    const harness = workerHarness(options);
    await expectSignals(harness, { ...OK, event_collisions: "degraded", queue_health: "degraded" });
    options.queueHealthError = new Error("Bearer queue-secret raw-payload 203.0.113.42");
    await expectSignals(harness, {
      ...OK, event_collisions: "degraded", queue_health: "unavailable", queue_metrics: "degraded",
    });
    options.queueHealthError = undefined;
    options.queueHealthy = true;
    await expectSignals(harness, { ...OK, event_collisions: "degraded" });
    options.queueHealthError = new Error("Bearer queue-secret raw-payload 203.0.113.42");
    await expectSignals(harness, {
      ...OK, event_collisions: "degraded", queue_health: "unavailable", queue_metrics: "degraded",
    });
    for (const sensitive of ["queue-secret", "raw-payload", "203.0.113.42"]) {
      expect(JSON.stringify(vi.mocked(console.error).mock.calls)).not.toContain(sensitive);
    }
  });

  test.each([
    ["below thresholds", {}, "ok"],
    ["old client refresh", { providerRefreshFailureCount: 5n }, "degraded"],
    ["old client delivery backlog", { deliveryDegradedCount: 5n }, "degraded"],
    ["old client delivery drop", { deliveryDroppedCount: 1n }, "degraded"],
    ["all old client failures", {
      providerRefreshFailureCount: 500n, deliveryDegradedCount: 500n, deliveryDroppedCount: 100n,
    }, "degraded"],
  ] as const)("%s leaves every service signal and aggregate ingestion healthy", async (_name, change, clientStatus) => {
    const harness = workerHarness({ healthSnapshot: { ...BELOW_THRESHOLDS, ...change } });
    await expectSignals(harness, OK);
    const response = await harness.worker.fetch(new Request(HEALTH_URL), ENV);
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({
      status: "ok", ingestion: { status: "ok" },
      client: clientStatus === "ok" ? { status: "ok" }
        : { status: "degraded", reason: "telemetry_health_signal" },
    });
    expectNoWrites(harness);
  });

  test("all service signals clear when only old-client failures remain", async () => {
    const healthSnapshot = {
      ...BELOW_THRESHOLDS, compatibilityRejectionMax: 100n, eventCollisionCount: 1n,
      otherRejectionCount: 5n, providerRefreshFailureCount: 5n,
      deliveryDegradedCount: 5n, deliveryDroppedCount: 1n,
    };
    const options = { healthSnapshot, queueHealthy: false };
    const harness = workerHarness(options);
    await expectSignals(harness, {
      ...OK, compatibility_rejections: "degraded", event_collisions: "degraded",
      other_rejections: "degraded", queue_health: "degraded",
    });
    Object.assign(healthSnapshot, {
      compatibilityRejectionMax: 0n, eventCollisionCount: 0n, otherRejectionCount: 0n,
    });
    options.queueHealthy = true;
    await expectSignals(harness, OK);
    const response = await harness.worker.fetch(new Request(HEALTH_URL), ENV);
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({
      status: "ok", ingestion: { status: "ok" },
      client: { status: "degraded", reason: "telemetry_health_signal" },
    });
  });

  test.each(SIGNALS)("%s rejects non-exact paths, queries, and methods without side effects", async (signal) => {
    const harness = workerHarness();
    const limit = vi.fn(async () => ({ success: true }));
    for (const [suffix, method, status] of [
      [`/${signal}/`, "GET", 404], [`/${signal}/extra`, "GET", 404],
      [`/${signal}?detail=private-query`, "GET", 404],
      [`/${signal}`, "POST", 405], [`/${signal}`, "HEAD", 405], [`/${signal}`, "OPTIONS", 405],
    ] as const) {
      const response = await harness.worker.fetch(new Request(`${HEALTH_URL}${suffix}`, {
        method, headers: { origin: "https://ctx.rs", "cf-connecting-ip": "203.0.113.42" },
      }), { ...ENV, TELEMETRY_RATE_LIMITER: { limit } });
      expect(response.status).toBe(status);
      expect(await response.json()).toEqual({ error: status === 405 ? "method_not_allowed" : "not_found" });
      if (status === 405) expect(response.headers.get("allow")).toBe("GET");
    }
    expect(limit).not.toHaveBeenCalled();
    expect(harness.createDatabaseClient).not.toHaveBeenCalled();
    expect(harness.readIngestHealthSnapshot).not.toHaveBeenCalled();
    expect(harness.readQueueHealth).not.toHaveBeenCalled();
    expectNoWrites(harness);
  });

  test.each(["/", "/unknown", "/client", "/queue_health_extra", "/QUEUE_HEALTH"])(
    "unknown signal suffix %s is not an alias for aggregate health", async (suffix) => {
      const harness = workerHarness();
      const response = await harness.worker.fetch(new Request(`${HEALTH_URL}${suffix}`), ENV);
      expect(response.status).toBe(404);
      expect(await response.json()).toEqual({ error: "not_found" });
      expect(harness.createDatabaseClient).not.toHaveBeenCalled();
      expect(harness.readQueueHealth).not.toHaveBeenCalled();
      expectNoWrites(harness);
    },
  );
});
