import { describe, expect, test, vi } from "vitest";
import healthMonitor from "../config/health-monitor.json";

import {
  NeonTelemetryDatabase,
  TelemetryEventCollisionError,
  type NeonQueryClient,
  type TelemetryDatabase,
  type TelemetryIngestRejection,
} from "../src/database";
import { hmacSha256Hex, sha256Hex } from "../src/hash";
import { readTelemetryQueueHealth } from "../src/queue-health";
import {
  buildInstallStageRow,
  buildTelemetryIngestPlan,
  TelemetryIngestError,
  type InstallStageRow,
  type TelemetryRow,
} from "../src/telemetry-ingest";
import {
  BYTE_BUCKETS,
  COUNT_BUCKETS,
  DURATION_BUCKETS,
  MAX_BODY_BYTES,
  MAX_EVENT_BYTES,
  PROVIDERS,
} from "../src/telemetry-contract";
import {
  createTelemetryWorker,
  type Env,
  type TelemetryRejectionObservation,
} from "../src/worker";

import {
  NOW,
  OCCURRED_AT,
  HMAC_KEY,
  CLIENT_PROFILE_ID,
  DATA_ROOT_ID,
  EVENT_ID,
  INSTALL_ATTEMPT_ID,
  CLOSED_PROVIDERS,
  ENV,
  INGEST_OPTIONS,
  v1Batch,
  operationEvent,
  providerRefreshEvent,
  providerRefreshProperties,
  batchEventId,
  maximalSearchEvent,
  runtimeEvent,
  daemonRunProperties,
  daemonSnapshotProperties,
  daemonCycleProperties,
  mcpOperationEvent,
  mcpRuntimeProperties,
  mcpRuntimeEvent,
  proOperationEvent,
  installStage,
  jsonRequest,
  workerHarness,
  neonHarness,
  canonicalJson,
  uuidV4FromFingerprint,
} from "./worker-test-fixtures";

describe("bounded HTTP ingestion", () => {
  test("reports healthy ingestion without exposing aggregate values", async () => {
    const harness = workerHarness({
      healthSnapshot: {
        compatibilityRejectionMax: 99n,
        deliveryDegradedCount: 0n,
        deliveryDroppedCount: 0n,
        eventCollisionCount: 0n,
        otherRejectionCount: 4n,
        providerRefreshFailureCount: 0n,
      },
    });
    const response = await harness.worker.fetch(new Request(
      "https://cli.ctx.rs/functions/v1/analytics/health",
    ), ENV);

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({
      status: "ok", ingestion: { status: "ok" }, client: { status: "ok" },
    });
    expect(harness.createDatabaseClient).toHaveBeenCalledOnce();
    expect(harness.readIngestHealthSnapshot).toHaveBeenCalledOnce();
    expect(harness.readQueueHealth).toHaveBeenCalledOnce();
    expect(harness.readIngestHealthSnapshot).toHaveBeenCalledWith("production");
    expect(harness.readQueueHealth).toHaveBeenCalledWith(expect.objectContaining({
      TELEMETRY_ANALYTICS_ENVIRONMENT: "production",
    }));
    expect(response.headers.get("cache-control")).toBe("no-store");
  });

  test("reports Queue metric degradation separately from Neon", async () => {
    const harness = workerHarness({ queueHealthy: false });
    const response = await harness.worker.fetch(new Request(
      "https://cli.ctx.rs/functions/v1/analytics/health",
    ), ENV);

    expect(response.status).toBe(503);
    expect(await response.json()).toEqual({
      status: "degraded",
      reason: "telemetry_queue_health_signal",
      ingestion: { status: "degraded", reason: "telemetry_queue_health_signal" },
      client: { status: "ok" },
    });
  });

  test("sanitizes Queue metric authority failures", async () => {
    const sensitiveValues = [
      "Authorization: Bearer test-token",
      "test-token",
      "raw-response-body-canary",
      "event-payload-canary",
      "203.0.113.42",
    ] as const;
    const sensitive = sensitiveValues.join(" ");
    const queueHealthError = await readTelemetryQueueHealth({
      TELEMETRY_ANALYTICS_ENVIRONMENT: "production",
      TELEMETRY_CLOUDFLARE_ACCOUNT_ID: "a".repeat(32),
      TELEMETRY_QUEUE_HEALTH_API_TOKEN: "test-token",
    }, vi.fn(async () => new Response(JSON.stringify({
      success: false,
      errors: [{code: 10_000, message: sensitive}],
      raw: sensitive,
    }), {status: 403})) as typeof fetch).then(() => null, (error: unknown) => error);
    if (!(queueHealthError instanceof Error)) throw new Error("expected_queue_health_error");
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    const harness = workerHarness({ queueHealthError });
    const response = await harness.worker.fetch(new Request(
      "https://cli.ctx.rs/functions/v1/analytics/health",
    ), ENV);

    expect(response.status).toBe(503);
    expect(await response.json()).toEqual({
      status: "degraded",
      reason: "queue_metrics_unavailable",
      ingestion: { status: "degraded", reason: "queue_metrics_unavailable" },
      client: { status: "ok" },
    });
    expect(error).toHaveBeenCalledWith("telemetry_queue_health_unavailable", {
      stage: "inventory",
      kind: "upstream",
      http_status: 403,
      cloudflare_error_code: 10_000,
    });
    for (const value of sensitiveValues) {
      expect(JSON.stringify(error.mock.calls)).not.toContain(value);
    }
  });

  test.each([
    [{
      compatibilityRejectionMax: 100n,
      deliveryDegradedCount: 0n,
      deliveryDroppedCount: 0n,
      eventCollisionCount: 0n,
      otherRejectionCount: 0n,
      providerRefreshFailureCount: 0n,
    }, "degraded", "ok"],
    [{
      compatibilityRejectionMax: 0n,
      deliveryDegradedCount: 0n,
      deliveryDroppedCount: 0n,
      eventCollisionCount: 1n,
      otherRejectionCount: 0n,
      providerRefreshFailureCount: 0n,
    }, "degraded", "ok"],
    [{
      compatibilityRejectionMax: 0n,
      deliveryDegradedCount: 0n,
      deliveryDroppedCount: 0n,
      eventCollisionCount: 0n,
      otherRejectionCount: 5n,
      providerRefreshFailureCount: 0n,
    }, "degraded", "ok"],
    [{
      compatibilityRejectionMax: 0n,
      deliveryDegradedCount: 5n,
      deliveryDroppedCount: 0n,
      eventCollisionCount: 0n,
      otherRejectionCount: 0n,
      providerRefreshFailureCount: 0n,
    }, "ok", "degraded"],
    [{
      compatibilityRejectionMax: 0n,
      deliveryDegradedCount: 0n,
      deliveryDroppedCount: 1n,
      eventCollisionCount: 0n,
      otherRejectionCount: 0n,
      providerRefreshFailureCount: 0n,
    }, "ok", "degraded"],
    [{
      compatibilityRejectionMax: 0n,
      deliveryDegradedCount: 0n,
      deliveryDroppedCount: 0n,
      eventCollisionCount: 0n,
      otherRejectionCount: 0n,
      providerRefreshFailureCount: 5n,
    }, "ok", "degraded"],
  ] as const)("reports bounded rejection degradation for threshold snapshot %o", async (
    healthSnapshot, ingestionStatus, clientStatus,
  ) => {
    const harness = workerHarness({ healthSnapshot });
    const response = await harness.worker.fetch(new Request(
      "https://cli.ctx.rs/functions/v1/analytics/health",
    ), ENV);

    expect(response.status).toBe(ingestionStatus === "ok" ? 200 : 503);
    expect(await response.json()).toEqual({
      status: ingestionStatus,
      ...(ingestionStatus === "ok" ? {} : { reason: "telemetry_health_signal" }),
      ingestion: ingestionStatus === "ok" ? { status: "ok" }
        : { status: "degraded", reason: "telemetry_health_signal" },
      client: clientStatus === "ok" ? { status: "ok" }
        : { status: "degraded", reason: "telemetry_health_signal" },
    });
    expect(harness.createDatabaseClient).toHaveBeenCalledOnce();
    expect(harness.readIngestHealthSnapshot).toHaveBeenCalledOnce();
    expect(harness.readQueueHealth).toHaveBeenCalledOnce();
  });

  test("sanitizes health database failures", async () => {
    const sensitive = `database unavailable at ${ENV.TELEMETRY_DATABASE_URL}`;
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    const harness = workerHarness({ healthError: new Error(sensitive) });
    const response = await harness.worker.fetch(new Request(
      "https://cli.ctx.rs/functions/v1/analytics/health",
    ), ENV);

    expect(response.status).toBe(503);
    expect(await response.json()).toEqual({
      status: "degraded", reason: "database_unavailable",
      ingestion: { status: "degraded", reason: "database_unavailable" },
      client: { status: "unavailable", reason: "database_unavailable" },
    });
    expect(error).toHaveBeenCalledWith("telemetry_ingest_health_unavailable");
    expect(JSON.stringify(error.mock.calls)).not.toContain(sensitive);
    expect(JSON.stringify(error.mock.calls)).not.toContain(ENV.TELEMETRY_DATABASE_URL);
  });

  describe.each([
    ["ingestion", 100n, 0n, "ok"],
    ["client", 0n, 5n, "degraded"],
    ["both", 100n, 5n, "degraded"],
  ] as const)("mixed %s snapshot failure", (_name, compatibilityRejectionMax, providerRefreshFailureCount, clientStatus) => {
    test.each(["degraded", "unavailable"])("preserves snapshot precedence with Queue %s", async (queueState) => {
      const error = vi.spyOn(console, "error").mockImplementation(() => {});
      const sensitive = "queue-token-and-raw-response-canary";
      try {
        const harness = workerHarness({
          healthSnapshot: {
            compatibilityRejectionMax, providerRefreshFailureCount,
            eventCollisionCount: 0n, otherRejectionCount: 0n,
            deliveryDegradedCount: 0n, deliveryDroppedCount: 0n,
          },
          queueHealthy: false,
          queueHealthError: queueState === "unavailable" ? new Error(sensitive) : undefined,
        });
        const response = await harness.worker.fetch(new Request(
          "https://cli.ctx.rs/functions/v1/analytics/health",
        ), ENV);
        const body = await response.json();
        const queueReason = queueState === "unavailable"
          ? "queue_metrics_unavailable" : "telemetry_queue_health_signal";
        expect(response.status).toBe(503);
        expect(body).toEqual({
          status: "degraded", reason: compatibilityRejectionMax === 100n ? "telemetry_health_signal" : queueReason,
          ingestion: {
            status: "degraded",
            reason: compatibilityRejectionMax === 100n ? "telemetry_health_signal" : queueReason,
          },
          client: clientStatus === "ok" ? { status: "ok" }
            : { status: "degraded", reason: "telemetry_health_signal" },
        });
        expect(JSON.stringify(body)).not.toContain(sensitive);
        expect(JSON.stringify(error.mock.calls)).not.toContain(sensitive);
        expect(harness.createDatabaseClient).toHaveBeenCalledOnce();
        expect(harness.readIngestHealthSnapshot).toHaveBeenCalledOnce();
        expect(harness.readQueueHealth).toHaveBeenCalledOnce();
        expect(harness.queueSendBatch).not.toHaveBeenCalled();
        expect(harness.insertTelemetryRows).not.toHaveBeenCalled();
        expect(harness.recordIngestRejection).not.toHaveBeenCalled();
      } finally {
        error.mockRestore();
      }
    });
  });

  test.each([false, true])("preserves database precedence with Queue authority failure=%s", async (unavailable) => {
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    try {
      const harness = workerHarness({
        healthError: new Error("database-secret-canary"), queueHealthy: false,
        queueHealthError: unavailable ? new Error("queue-secret-canary") : undefined,
      });
      const response = await harness.worker.fetch(new Request(
        "https://cli.ctx.rs/functions/v1/analytics/health",
      ), ENV);
      expect(response.status).toBe(503);
      expect(await response.json()).toEqual({
        status: "degraded", reason: "database_unavailable",
        ingestion: { status: "degraded", reason: "database_unavailable" },
        client: { status: "unavailable", reason: "database_unavailable" },
      });
      expect(harness.createDatabaseClient).toHaveBeenCalledOnce();
      expect(harness.readIngestHealthSnapshot).toHaveBeenCalledOnce();
      expect(harness.readQueueHealth).toHaveBeenCalledOnce();
      expect(JSON.stringify(error.mock.calls)).not.toContain("secret-canary");
    } finally {
      error.mockRestore();
    }
  });

  test.each([0n, 5n])("shares the Queue deadline with a delayed database and refresh failures=%s", async (providerRefreshFailureCount) => {
    vi.useFakeTimers();
    // Model AbortSignal.timeout with the same timer clock as the delayed reads.
    const timeout = vi.spyOn(AbortSignal, "timeout").mockImplementation((milliseconds) => {
      const controller = new AbortController();
      setTimeout(() => controller.abort(new DOMException("deadline", "TimeoutError")), milliseconds);
      return controller.signal;
    });
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    try {
      const harness = workerHarness();
      harness.readIngestHealthSnapshot.mockImplementation(async () => {
        await new Promise((resolve) => setTimeout(resolve, 900));
        return {
          compatibilityRejectionMax: 0n, eventCollisionCount: 0n, otherRejectionCount: 0n,
          providerRefreshFailureCount, deliveryDegradedCount: 0n, deliveryDroppedCount: 0n,
        };
      });
      const fetchImpl = vi.fn(async (input: string | URL | Request, init?: RequestInit) => {
        if (String(input).includes("/queues?")) {
          await new Promise((resolve) => setTimeout(resolve, 1_000));
          return new Response(JSON.stringify({ success: true, result: [
            { queue_id: "primary", queue_name: "ctx-telemetry-ingest-prod" },
            { queue_id: "dlq", queue_name: "ctx-telemetry-ingest-prod-dlq" },
          ], result_info: { total_pages: 1 } }));
        }
        const signal = init?.signal;
        if (!signal) throw new Error("missing_queue_deadline");
        return new Promise<Response>((_resolve, reject) => {
          signal.addEventListener("abort", () => reject(signal.reason), { once: true });
        });
      });
      harness.readQueueHealth.mockImplementation(() => readTelemetryQueueHealth({
        TELEMETRY_ANALYTICS_ENVIRONMENT: "production",
        TELEMETRY_CLOUDFLARE_ACCOUNT_ID: "a".repeat(32),
        TELEMETRY_QUEUE_HEALTH_API_TOKEN: "queue-secret-canary",
      }, fetchImpl as typeof fetch));
      const started = Date.now();
      let finished = false;
      const pending = harness.worker.fetch(new Request(
        "https://cli.ctx.rs/functions/v1/analytics/health",
      ), ENV).then((response) => { finished = true; return response; });
      await vi.advanceTimersByTimeAsync(4_499);
      expect(finished).toBe(false);
      expect(fetchImpl).toHaveBeenCalledTimes(5);
      await vi.advanceTimersByTimeAsync(1);
      expect(finished).toBe(true);
      const response = await pending;
      expect(Date.now() - started).toBe(4_500);
      expect(Date.now() - started).toBeLessThan(healthMonitor.healthCheck.timeout * 1_000);
      expect(timeout).toHaveBeenCalledExactlyOnceWith(4_500);
      expect(harness.createDatabaseClient).toHaveBeenCalledOnce();
      expect(harness.readIngestHealthSnapshot).toHaveBeenCalledOnce();
      expect(harness.readQueueHealth).toHaveBeenCalledOnce();
      expect(response.status).toBe(503);
      expect(await response.json()).toEqual({
        status: "degraded",
        reason: "queue_metrics_unavailable",
        ingestion: { status: "degraded", reason: "queue_metrics_unavailable" },
        client: providerRefreshFailureCount === 5n
          ? { status: "degraded", reason: "telemetry_health_signal" } : { status: "ok" },
      });
    } finally {
      timeout.mockRestore();
      error.mockRestore();
      vi.useRealTimers();
    }
  });

  test("client degradation stays visible without declaring an ingestion outage", async () => {
    const harness = workerHarness({ healthSnapshot: {
      compatibilityRejectionMax: 0n, eventCollisionCount: 0n, otherRejectionCount: 0n,
      providerRefreshFailureCount: 5n, deliveryDegradedCount: 0n, deliveryDroppedCount: 0n,
    } });
    const response = await harness.worker.fetch(new Request(
      "https://cli.ctx.rs/functions/v1/analytics/health",
    ), ENV);
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({
      status: "ok", ingestion: { status: "ok" },
      client: { status: "degraded", reason: "telemetry_health_signal" },
    });
  });

  test("keeps the health route read-only and exact", async () => {
    const harness = workerHarness();
    const post = await harness.worker.fetch(new Request(
      "https://cli.ctx.rs/functions/v1/analytics/health",
      { method: "POST" },
    ), ENV);
    const query = await harness.worker.fetch(new Request(
      "https://cli.ctx.rs/functions/v1/analytics/health?detail=1",
    ), ENV);

    expect(post.status).toBe(405);
    expect(post.headers.get("allow")).toBe("GET");
    expect(query.status).toBe(404);
    expect(harness.readIngestHealthSnapshot).not.toHaveBeenCalled();
  });

  test.each([
    "/functions/v1/analytics",
    "/functions/v1/telemetry",
  ])("preserves telemetry POST route %s", async (path) => {
    const harness = workerHarness();
    const response = await harness.worker.fetch(jsonRequest(path, v1Batch([operationEvent()])), ENV);

    expect(response.status).toBe(204);
    expect(harness.queueSendBatch).toHaveBeenCalledOnce();
    expect((await harness.queueMessages())[0]).toMatchObject({ kind: "telemetry_row" });
    expect(response.headers.get("cache-control")).toBe("no-store");
    expect(response.headers.get("x-content-type-options")).toBe("nosniff");
    expect(response.headers.has("access-control-allow-origin")).toBe(false);
  });

  test("accepts 50 events per request and rejects 51", async () => {
    const harness = workerHarness();
    const acceptedEvents = Array.from({ length: 50 }, (_, index) =>
      operationEvent({ event_id: batchEventId(index) })
    );
    const rejectedEvents = Array.from({ length: 51 }, (_, index) =>
      operationEvent({ event_id: batchEventId(index) })
    );

    const accepted = await harness.worker.fetch(
      jsonRequest("/functions/v1/telemetry", v1Batch(acceptedEvents)),
      ENV,
    );
    const rejected = await harness.worker.fetch(
      jsonRequest("/functions/v1/telemetry", v1Batch(rejectedEvents)),
      ENV,
    );

    expect(accepted.status).toBe(204);
    expect(rejected.status).toBe(413);
    expect(harness.queueSendBatch).toHaveBeenCalledOnce();
    const messages = await harness.queueMessages();
    expect(messages).toHaveLength(50);
    expect(messages.every((message) => message.kind === "telemetry_row")).toBe(true);
    expect(messages.map((message) => message.kind === "telemetry_row" && message.row.event_id))
      .toEqual(acceptedEvents.map((event) => event.event_id));
  });

  test("accepts a maximal-field 50-event batch above the former 64 KiB cap", async () => {
    const harness = workerHarness();
    const events = Array.from({ length: 50 }, (_, index) =>
      maximalSearchEvent(index)
    );
    const body = JSON.stringify(v1Batch(events));
    const bodyBytes = new TextEncoder().encode(body).byteLength;

    expect(bodyBytes).toBeGreaterThan(64 * 1024);
    expect(bodyBytes).toBeLessThanOrEqual(MAX_BODY_BYTES);
    expect(events.every((event) =>
      new TextEncoder().encode(JSON.stringify(event)).byteLength <= MAX_EVENT_BYTES
    )).toBe(true);

    const response = await harness.worker.fetch(new Request(
      "https://api.example.test/functions/v1/telemetry",
      {
        method: "POST",
        body,
        headers: { "content-type": "application/json; charset=utf-8" },
      },
    ), ENV);

    expect(response.status, await response.text()).toBe(204);
    expect(harness.queueSendBatch).toHaveBeenCalledOnce();
    const messages = await harness.queueMessages();
    expect(messages).toHaveLength(50);
    expect(messages.every((message) => message.kind === "telemetry_row")).toBe(true);
    expect(messages.map((message) => message.kind === "telemetry_row" && message.row.event_id))
      .toEqual(events.map((event) => event.event_id));
  });

  test("preserves install-attempt POST route", async () => {
    const harness = workerHarness();
    const response = await harness.worker.fetch(
      jsonRequest("/functions/v1/install-attempt", installStage()),
      ENV,
    );

    expect(response.status).toBe(204);
    expect((await harness.queueMessages())[0]).toMatchObject({ kind: "install_stage_row" });
  });

  test.each([
    ["query", jsonRequest("/functions/v1/telemetry?debug=1", v1Batch([operationEvent()])), 400],
    ["origin", jsonRequest("/functions/v1/telemetry", v1Batch([operationEvent()]), { Origin: "https://ctx.rs" }), 400],
    ["method", new Request("https://api.example.test/functions/v1/telemetry", { method: "GET" }), 405],
    ["media", new Request("https://api.example.test/functions/v1/telemetry", { method: "POST", body: "{}", headers: { "content-type": "text/plain" } }), 415],
    ["compression", jsonRequest("/functions/v1/telemetry", v1Batch([operationEvent()]), { "content-encoding": "gzip" }), 415],
    ["json", new Request("https://api.example.test/functions/v1/telemetry", { method: "POST", body: "{", headers: { "content-type": "application/json" } }), 400],
    ["declared size", jsonRequest("/functions/v1/telemetry", {}, { "content-length": String(MAX_BODY_BYTES + 1) }), 413],
  ])("rejects invalid %s requests", async (_name, request, status) => {
    const harness = workerHarness();
    const response = await harness.worker.fetch(request, ENV);
    expect(response.status).toBe(status);
    expect(response.headers.get("cache-control")).toBe("no-store");
  });

  test("enforces the actual contract-derived body limit", async () => {
    const harness = workerHarness();
    const response = await harness.worker.fetch(new Request(
      "https://api.example.test/functions/v1/telemetry",
      {
        method: "POST",
        body: JSON.stringify({ padding: "x".repeat(MAX_BODY_BYTES + 1) }),
        headers: { "content-type": "application/json" },
      },
    ), ENV);
    expect(response.status).toBe(413);
  });

  test("returns 422 for schema errors and 503 for Queue admission failures", async () => {
    const schemaHarness = workerHarness();
    const invalid = v1Batch([operationEvent({ properties: { output: "human", query: "raw" } })]);
    expect((await schemaHarness.worker.fetch(jsonRequest("/functions/v1/telemetry", invalid), ENV)).status)
      .toBe(422);

    const queueHarness = workerHarness({ queueError: new Error("queue unavailable") });
    expect((await queueHarness.worker.fetch(
      jsonRequest("/functions/v1/telemetry", v1Batch([operationEvent()])),
      ENV,
    )).status).toBe(503);

    expect(schemaHarness.observeRejection).toHaveBeenCalledWith(expect.objectContaining({
      category: "schema",
      code: "unknown_operation_property",
      endpoint: "telemetry_batch",
      event_family: "operation_completed",
      rejection_class: "invalid_properties",
      status: 422,
    }));
    expect(queueHarness.observeRejection).toHaveBeenCalledWith(expect.objectContaining({
      category: "pre_commit",
      code: "queue_admission_failed",
      endpoint: "telemetry_batch",
      event_family: "operation_completed",
      rejection_class: "queue_admission",
      status: 503,
    }));
    expect(schemaHarness.recordIngestRejection).toHaveBeenCalledWith({
      analytics_environment: "production",
      endpoint: "telemetry_batch",
      event_family: "operation_completed",
      rejection_class: "invalid_properties",
      rejection_code: "unknown_operation_property",
      app_version: "0.26.0",
      field_shape_fingerprint: expect.stringMatching(/^[0-9a-f]{64}$/u),
      field_shape_overflow: false,
      provider_classification: "neutral",
      size_bucket: expect.stringMatching(/^(?:lt_1kb|1kb_8kb)$/u),
    });
    expect(queueHarness.recordIngestRejection).not.toHaveBeenCalled();
  });

  test("does not return success before durable Queue admission resolves and never opens Neon", async () => {
    let release: (() => void) | undefined;
    const pending = new Promise<void>((resolve) => { release = resolve; });
    const harness = workerHarness({ queuePromise: pending });
    let settled = false;
    const responsePromise = harness.worker.fetch(
      jsonRequest("/functions/v1/telemetry", v1Batch([operationEvent()])),
      ENV,
    ).then((response) => {
      settled = true;
      return response;
    });

    await vi.waitFor(() => expect(harness.queueSendBatch).toHaveBeenCalledOnce());
    expect(settled).toBe(false);
    expect(harness.createDatabaseClient).not.toHaveBeenCalled();
    release?.();
    expect((await responsePromise).status).toBe(204);
  });

  test("fails closed when Worker identity configuration is absent", async () => {
    const harness = workerHarness();
    const response = await harness.worker.fetch(
      jsonRequest("/functions/v1/telemetry", v1Batch([operationEvent()])),
      {},
    );
    expect(response.status).toBe(500);
    expect(harness.queueSendBatch).not.toHaveBeenCalled();
    expect(harness.observeRejection).toHaveBeenCalledWith(expect.objectContaining({
      category: "pre_commit",
      code: "telemetry_env_not_configured",
      endpoint: "telemetry_batch",
      event_family: "batch",
      rejection_class: "other",
      status: 500,
    }));
    expect(harness.recordIngestRejection).not.toHaveBeenCalled();
  });

  test("fails closed without falling back to per-event Queue send", async () => {
    const send = vi.fn(async () => {});
    const worker = createTelemetryWorker({ now: () => NOW });
    const response = await worker.fetch(
      jsonRequest("/functions/v1/telemetry", v1Batch([operationEvent()])),
      {
        ...ENV,
        TELEMETRY_INGEST_QUEUE: { send } as unknown as NonNullable<Env["TELEMETRY_INGEST_QUEUE"]>,
      },
    );

    expect(response.status).toBe(500);
    expect(send).not.toHaveBeenCalled();
  });

  test("fails closed when anonymous rate limiting is unavailable or denies the request", async () => {
    const unavailableHarness = workerHarness();
    const unavailableEnv: Env = { ...ENV };
    delete unavailableEnv.TELEMETRY_RATE_LIMITER;
    const unavailable = await unavailableHarness.worker.fetch(
      jsonRequest("/functions/v1/telemetry", v1Batch([operationEvent()])),
      unavailableEnv,
    );
    expect(unavailable.status).toBe(503);
    expect(unavailableHarness.queueSendBatch).not.toHaveBeenCalled();

    const limit = vi.fn(async (_options: { readonly key: string }) => ({ success: false }));
    const deniedHarness = workerHarness();
    const denied = await deniedHarness.worker.fetch(
      jsonRequest(
        "/functions/v1/telemetry",
        v1Batch([operationEvent()]),
        { "cf-connecting-ip": "203.0.113.42" },
      ),
      { ...ENV, TELEMETRY_RATE_LIMITER: { limit } },
    );
    expect(denied.status).toBe(429);
    expect(denied.headers.get("retry-after")).toBe("60");
    expect(deniedHarness.queueSendBatch).not.toHaveBeenCalled();
    expect(unavailableHarness.observeRejection).toHaveBeenCalledWith({
      category: "rate_limit",
      code: "rate_limit_unavailable",
      endpoint: "telemetry_batch",
      event_family: "batch",
      rejection_class: "other",
      status: 503,
    });
    expect(deniedHarness.observeRejection).toHaveBeenCalledWith({
      category: "rate_limit",
      code: "rate_limited",
      endpoint: "telemetry_batch",
      event_family: "batch",
      rejection_class: "rate_limited",
      status: 429,
    });
    expect(unavailableHarness.recordIngestRejection).not.toHaveBeenCalled();
    expect(deniedHarness.recordIngestRejection).not.toHaveBeenCalled();
    const key = limit.mock.calls[0]?.[0].key;
    expect(key).toMatch(/^\/functions\/v1\/telemetry:[a-f0-9]{32}$/u);
    expect(key).not.toContain("203.0.113.42");
    expect(JSON.stringify(deniedHarness.observeRejection.mock.calls))
      .not.toContain("203.0.113.42");
  });

  test("rate-limits malformed traffic before any Neon-backed rejection path", async () => {
    const limit = vi.fn(async () => ({ success: false }));
    const harness = workerHarness();
    const response = await harness.worker.fetch(
      new Request("https://api.example.test/functions/v1/telemetry?debug=1", {
        method: "GET",
        headers: { origin: "https://ctx.rs" },
      }),
      { ...ENV, TELEMETRY_RATE_LIMITER: { limit } },
    );

    expect(response.status).toBe(429);
    expect(limit).toHaveBeenCalledOnce();
    expect(harness.recordIngestRejection).not.toHaveBeenCalled();
    expect(harness.queueSendBatch).not.toHaveBeenCalled();
  });

  test("rejection observations contain only bounded server-owned dimensions", async () => {
    const harness = workerHarness();
    const rawValue = "/private/path?token=secret";
    const response = await harness.worker.fetch(
      jsonRequest(
        "/functions/v1/telemetry",
        v1Batch([operationEvent({
          properties: { output: "human", path: rawValue },
        })]),
      ),
      ENV,
    );

    expect(response.status).toBe(422);
    expect(harness.observeRejection).toHaveBeenCalledOnce();
    expect(harness.observeRejection).toHaveBeenCalledWith(expect.objectContaining({
      category: "schema",
      code: "unknown_operation_property",
      endpoint: "telemetry_batch",
      event_family: "operation_completed",
      app_version: "0.26.0",
      field_shape_overflow: false,
      provider_classification: "neutral",
      rejection_class: "invalid_properties",
      size_bucket: expect.stringMatching(/^(?:lt_1kb|1kb_8kb)$/u),
      status: 422,
    }));
    const observation = harness.observeRejection.mock.calls[0][0];
    expect(observation.field_shape).toContain("batch.app_version:string");
    expect(observation.field_shape).toContain("event.event_name:string");
    expect(observation.field_shape).toContain("event.properties:object");
    expect(observation.field_shape).toContain("properties.output:string");
    expect(observation.field_shape).not.toContain("properties.path:string");
    expect(JSON.stringify(harness.observeRejection.mock.calls)).not.toContain(rawValue);
    expect(JSON.stringify(harness.observeRejection.mock.calls)).not.toContain(CLIENT_PROFILE_ID);
    expect(JSON.stringify(harness.observeRejection.mock.calls)).not.toContain(DATA_ROOT_ID);
    expect(JSON.stringify(harness.recordIngestRejection.mock.calls)).not.toContain(rawValue);
    expect(JSON.stringify(harness.recordIngestRejection.mock.calls)).not.toContain(CLIENT_PROFILE_ID);
    expect(JSON.stringify(harness.recordIngestRejection.mock.calls)).not.toContain(DATA_ROOT_ID);
  });

  test("suppresses exact Core versions from valid and malformed Blame rejections", async () => {
    const currentBlame = operationEvent({
      operation: "blame",
      surface: "pro_host",
      properties: {
        blame_schema_version: 1,
        blame_semantics_version: 1,
        blame_surface: "cli",
        blame_target_kind: "file",
        blame_request_kind: "first_request",
        blame_result_state: "possible",
        blame_freshness: "current",
        blame_has_more: false,
        blame_output_served: true,
        blame_pro_version: "1.2.3",
        blame_pro_protocol_version: 3,
      },
    });
    const harness = workerHarness();
    const response = await harness.worker.fetch(
      jsonRequest("/functions/v1/analytics", v1Batch([currentBlame])),
      ENV,
    );

    expect(response.status).toBe(422);
    expect(harness.observeRejection).toHaveBeenCalledWith(expect.objectContaining({
      code: "blame_installation_proof_required",
      event_family: "operation_completed",
    }));
    expect(harness.observeRejection.mock.calls[0][0]).not.toHaveProperty("app_version");

    const malformed = structuredClone(currentBlame);
    (malformed.properties as Record<string, unknown>).blame_pro_version = true;
    const malformedHarness = workerHarness();
    const malformedResponse = await malformedHarness.worker.fetch(
      jsonRequest("/functions/v1/analytics", v1Batch([malformed])),
      ENV,
    );

    expect(malformedResponse.status).toBe(422);
    expect(malformedHarness.observeRejection.mock.calls[0][0]).not.toHaveProperty(
      "app_version",
    );
  });

  test("classifies unfamiliar providers without logging their value", async () => {
    const harness = workerHarness();
    const provider = "future_secret_harness";
    const event = providerRefreshEvent();
    event.properties = {
      ...(event.properties as Record<string, unknown>),
      provider,
      secret_field: "must-not-log",
    };

    const response = await harness.worker.fetch(
      jsonRequest("/functions/v1/telemetry", v1Batch([event])),
      ENV,
    );

    expect(response.status).toBe(422);
    expect(harness.observeRejection).toHaveBeenCalledWith(expect.objectContaining({
      app_version: "0.26.0",
      code: "unknown_provider_refresh_property",
      event_family: "provider_refresh_completed",
      provider_classification: "unrecognized",
    }));
    const serialized = JSON.stringify(harness.observeRejection.mock.calls);
    expect(serialized).not.toContain(provider);
    expect(serialized).not.toContain("must-not-log");
    expect(serialized).not.toContain("secret_field");
  });

  test("replaces an unbounded ingest error code before observing or returning it", async () => {
    const rawError = "/private/path?token=secret";
    const harness = workerHarness({ queueError: new TelemetryIngestError(422, rawError) });
    const response = await harness.worker.fetch(
      jsonRequest("/functions/v1/telemetry", v1Batch([operationEvent()])),
      ENV,
    );

    expect(response.status).toBe(422);
    expect(await response.json()).toEqual({ error: "unknown_rejection" });
    expect(harness.observeRejection).toHaveBeenCalledWith(expect.objectContaining({
      category: "schema",
      code: "unknown_rejection",
      endpoint: "telemetry_batch",
      event_family: "operation_completed",
      rejection_class: "invalid_properties",
      status: 422,
    }));
    expect(JSON.stringify(harness.observeRejection.mock.calls)).not.toContain(rawError);
    expect(JSON.stringify(harness.recordIngestRejection.mock.calls)).not.toContain(rawError);
  });

  test("keeps the original rejection response when bounded counter recording fails", async () => {
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});
    const harness = workerHarness({ rejectionError: new Error("database unavailable") });
    const response = await harness.worker.fetch(
      jsonRequest(
        "/functions/v1/telemetry",
        v1Batch([operationEvent({ properties: { output: "human", query: "raw" } })]),
      ),
      ENV,
    );

    expect(response.status).toBe(422);
    expect(await response.json()).toEqual({ error: "unknown_operation_property" });
    expect(consoleError).toHaveBeenCalledWith("telemetry_rejection_observer_failed");
    consoleError.mockRestore();
  });

  test("returns a rejection without waiting for the bounded Neon counter", async () => {
    let release: (() => void) | undefined;
    const pendingCounter = new Promise<void>((resolve) => {
      release = resolve;
    });
    const harness = workerHarness({ rejectionPromise: pendingCounter });
    const backgroundTasks: Promise<void>[] = [];

    const response = await harness.worker.fetch(
      jsonRequest(
        "/functions/v1/telemetry",
        v1Batch([operationEvent({ properties: { output: "human", query: "raw" } })]),
      ),
      ENV,
      {
        waitUntil(task) {
          backgroundTasks.push(task);
        },
      },
    );

    expect(response.status).toBe(422);
    expect(backgroundTasks).toHaveLength(1);
    release?.();
    await Promise.all(backgroundTasks);
    expect(harness.recordIngestRejection).toHaveBeenCalledOnce();
  });
});

describe("Neon authoritative writes", () => {
  test("writes a telemetry batch in one statement with typed columns and conflict guard", async () => {
    const { rows } = await buildTelemetryIngestPlan(v1Batch([
      operationEvent(),
      runtimeEvent({ event_id: "44444444-4444-4444-8444-444444444444" }),
    ]), INGEST_OPTIONS);
    const neon = neonHarness();
    const database = new NeonTelemetryDatabase(neon.client);

    await database.insertTelemetryRows(rows);

    expect(neon.calls).toHaveLength(2);
    const [sql, params] = neon.calls[0];
    expect(sql).toContain("INSERT INTO ctx.telemetry_event");
    expect(sql).toContain("schema_version");
    expect(sql).toContain("client_profile_id_hash");
    expect(sql).toContain("data_root_id_hash");
    expect(sql).toContain("identity_key_version");
    expect(sql).toContain("payload_fingerprint");
    expect(sql).toContain("ON CONFLICT (event_id) DO NOTHING");
    expect(sql).not.toContain("ingested_at");
    expect(params).not.toContain(CLIENT_PROFILE_ID);
    expect(params).not.toContain(DATA_ROOT_ID);
    expect(neon.calls[1][0]).toContain("stored.payload_fingerprint IS NULL");
    expect(neon.calls[1][0]).toContain("stored.payload_fingerprint = incoming.payload_fingerprint");
  });

  test("maps only the atomic telemetry conflict guard to collision", async () => {
    const { rows } = await buildTelemetryIngestPlan(v1Batch([operationEvent()]), INGEST_OPTIONS);
    const database = new NeonTelemetryDatabase(neonHarness({
      transactionError: { code: "22012" },
    }).client);
    await expect(database.insertTelemetryRows(rows)).rejects.toBeInstanceOf(TelemetryEventCollisionError);
  });

  test("writes canonical install stages to the additive ledger contract", async () => {
    const row = await buildInstallStageRow(installStage(), INGEST_OPTIONS);
    const neon = neonHarness();
    const database = new NeonTelemetryDatabase(neon.client);

    await database.insertInstallStageRow(row);

    expect(neon.calls).toHaveLength(2);
    const [sql, params] = neon.calls[0];
    expect(sql).toContain("INSERT INTO ctx.install_attempt_event");
    expect(sql).toContain("script_family");
    expect(sql).toContain("ON CONFLICT (event_id) WHERE event_id IS NOT NULL DO NOTHING");
    expect(sql).not.toContain("ingested_at");
    expect(params).not.toContain(INSTALL_ATTEMPT_ID);
  });

  test("writes legacy install stages through the existing idempotent conflict guard", async () => {
    const row = await buildInstallStageRow({
      install_attempt_id: INSTALL_ATTEMPT_ID,
      stage: "artifact_download_completed",
      status: "completed",
      error_kind: "",
      platform: "linux-x64",
      channel: "stable",
      version: "0.26.0",
    }, INGEST_OPTIONS);
    const neon = neonHarness();
    const database = new NeonTelemetryDatabase(neon.client);

    await database.insertInstallStageRow(row);

    expect(neon.calls).toHaveLength(2);
    expect(neon.calls[0]?.[0]).toContain("INSERT INTO ctx.install_attempt_event");
    expect(neon.calls[0]?.[0]).toContain(
      "ON CONFLICT (event_id) WHERE event_id IS NOT NULL DO NOTHING",
    );
    expect(neon.calls[1]?.[0]).toContain("payload_fingerprint");
    expect(neon.calls.flatMap(([, params]) => params)).toContain(row.event_id);
    expect(neon.calls.flatMap(([, params]) => params)).toContain(row.payload_fingerprint);
  });

  test("records only the migration's bounded rejection dimensions", async () => {
    const neon = neonHarness();
    const database = new NeonTelemetryDatabase(neon.client);

    await database.recordIngestRejection({
      analytics_environment: "production",
      endpoint: "telemetry_batch",
      event_family: "provider_refresh_completed",
      rejection_class: "invalid_properties",
      rejection_code: "unknown_operation_property",
      app_version: "0.26.0",
      field_shape_fingerprint: "none",
      field_shape_overflow: false,
      provider_classification: "neutral",
      size_bucket: "lt_1kb",
    });

    expect(neon.calls).toHaveLength(1);
    expect(neon.calls[0][0]).toContain(
      "$1, $2, $3, $4, $5, $6, $7, $8, $9, $10",
    );
    expect(neon.calls[0][1]).toEqual([
      "production",
      "telemetry_batch",
      "provider_refresh_completed",
      "invalid_properties",
      "unknown_operation_property",
      "0.26.0",
      "none",
      false,
      "neutral",
      "lt_1kb",
    ]);
    expect(JSON.stringify(neon.calls)).not.toContain(CLIENT_PROFILE_ID);
    expect(JSON.stringify(neon.calls)).not.toContain(DATA_ROOT_ID);
  });

  test("reads and validates the bounded health snapshot", async () => {
    const calls: [string, readonly unknown[]][] = [];
    const client: NeonQueryClient = {
      async query<T extends Record<string, unknown> = Record<string, unknown>>(
        sql: string,
        params: readonly (string | number | boolean | null)[] = [],
      ): Promise<T[]> {
        calls.push([sql, params]);
        return [{
          compatibility_rejection_max: "99",
          delivery_degraded_count: "0",
          delivery_dropped_count: "0",
          event_collision_count: "0",
          other_rejection_count: "4",
          provider_refresh_failure_count: "0",
        }] as unknown as T[];
      },
      async transaction() {
        throw new Error("unexpected transaction");
      },
    };
    const database = new NeonTelemetryDatabase(client);

    await expect(database.readIngestHealthSnapshot("staging")).resolves.toEqual({
      compatibilityRejectionMax: 99n,
      deliveryDegradedCount: 0n,
      deliveryDroppedCount: 0n,
      eventCollisionCount: 0n,
      otherRejectionCount: 4n,
      providerRefreshFailureCount: 0n,
    });
    expect(calls).toHaveLength(1);
    expect(calls[0][0]).toContain("ctx.telemetry_ingest_health_snapshot($1)");
    expect(calls[0][1]).toEqual(["staging"]);
  });
});
