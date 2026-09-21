import { readFileSync } from "node:fs";

import { describe, expect, test } from "vitest";

import { createTelemetryWorker } from "../src/worker";
import { withCapturedQueue } from "./queue-capture.mjs";

const FIXTURE_ROOT = new URL("./fixtures/public-telemetry-v1/", import.meta.url);
const NOW = new Date("2026-07-22T12:35:00Z");
const ENV = {
  TELEMETRY_ANALYTICS_ENVIRONMENT: "production",
  TELEMETRY_DATABASE_URL: "postgresql://telemetry.example.test/db",
  TELEMETRY_IDENTITY_HMAC_KEY: "fixture-test-hmac-key-with-32-bytes-minimum",
  TELEMETRY_IDENTITY_KEY_VERSION: "1",
  TELEMETRY_RATE_LIMITER: { async limit() { return { success: true }; } },
};

const operation = readFixture("operation_completed.valid.json");
const analyticsDelivery = readFixture("analytics_delivery_observation.valid.json");
const providerRefresh = readFixture("provider_refresh_completed.valid.json");
const providerRefreshPrivacyObservability = readFixture(
  "provider_refresh_completed.privacy-observability.valid.json",
);
const providerRefreshTypedV1 = readFixture("provider_refresh_completed.typed-v1.valid.json");
const providerRefreshCorpusStockV111 = readFixture(
  "provider_refresh_completed.corpus-stock-v1.1.1.valid.json",
);
const providerRefreshV120 = readFixture("provider_refresh_completed.v1.2.0.valid.json");
const runtimeObservation = readFixture("runtime_observation.valid.json");
const runtimeStorageObservation = readFixture("runtime_observation_storage.valid.json");
const installStage = readFixture("install_stage.valid.json");
const searchOperationV102 = readFixture("search_operation_completed.v1.0.2.valid.json");
const unknownMcpRequestV111 = readFixture("mcp_unknown_request.v1.1.1.valid.json");
const listEventsOperationV100 = readFixture("list_events_operation_completed.v1.0.0.valid.json");
const listEventsBrokenPipeOperationV100 = readFixture(
  "list_events_broken_pipe_operation_completed.v1.0.0.valid.json",
);
const queryEventsOperationV100 = readFixture("mcp_query_events.v1.0.0.valid.json");
const TYPED_V1_RELEASE_VERSIONS = Object.freeze(["1.0.0", "1.0.1", "1.0.2"]);
const COMMON_TELEMETRY_FIXTURES = Object.freeze([
  operation,
  providerRefresh,
  runtimeObservation,
]);

describe("pinned public telemetry fixtures", () => {
  test("accepts the public daemon storage observation without changing the event family", async () => {
    const harness = workerHarness();
    const response = await harness.worker.fetch(
      jsonRequest(
        "/functions/v1/telemetry",
        canonicalBatch([runtimeStorageObservation], "1.3.2"),
      ),
      ENV,
    );

    expect(response.status, await response.text()).toBe(204);
    expect(harness.telemetryWrites[0][0]).toMatchObject({
      event_name: "runtime_observation",
      surface: "daemon",
      activity_class: "operational",
      properties: {
        filesystem_total_bytes_bucket: "1tb-2tb",
        filesystem_available_bytes_bucket: "250gb-500gb",
        filesystem_available_fraction_bucket: "20pct-40pct",
        core_active_logical_bytes_bucket: "5gb-10gb",
        core_certified_source_bytes_bucket: "10gb-25gb",
        core_logical_amplification_bucket: "0_25x-0_35x",
        filesystem_available_to_active_core_ratio_bucket: "4x+",
      },
    });
  });

  test("accepts the content-free analytics delivery observation", async () => {
    const harness = workerHarness();
    const response = await harness.worker.fetch(
      jsonRequest(
        "/functions/v1/telemetry",
        canonicalBatch([analyticsDelivery], "1.2.1"),
      ),
      ENV,
    );

    expect(response.status, await response.text()).toBe(204);
    expect(harness.telemetryWrites[0][0]).toMatchObject({
      event_name: "analytics_delivery_observation",
      surface: "cli",
      activity_class: "operational",
      status: "failure",
      success: false,
      properties: {
        operation: "outbox",
        outcome: "failure",
        queued_count_bucket: "2-5",
        retry_attempt_count_bucket: "2-5",
        dropped_count_bucket: "1",
        oldest_queued_age_bucket: "lt_10m",
        failure_class: "transport",
      },
    });
  });

  test("accepts the current structured provider failure diagnostics", async () => {
    const harness = workerHarness();
    const response = await harness.worker.fetch(
      jsonRequest(
        "/functions/v1/telemetry",
        canonicalBatch([providerRefreshPrivacyObservability], "1.2.1"),
      ),
      ENV,
    );

    expect(response.status, await response.text()).toBe(204);
    expect(harness.telemetryWrites[0][0].properties).toMatchObject({
      failure_code: "none",
      retryable: false,
    });
  });

  test.each(TYPED_V1_RELEASE_VERSIONS)(
    "replays the canonical typed-v1 fixtures as released $version",
    async (version) => {
      const telemetryFixtures = version === "1.0.2"
        ? [...COMMON_TELEMETRY_FIXTURES, searchOperationV102]
        : COMMON_TELEMETRY_FIXTURES;
      const harness = workerHarness();
      const telemetryResponse = await harness.worker.fetch(
        jsonRequest(
          "/functions/v1/telemetry",
          canonicalBatch(telemetryFixtures, version),
        ),
        ENV,
      );
      const installResponse = await harness.worker.fetch(
        jsonRequest("/functions/v1/install-attempt", installStage),
        ENV,
      );

      expect(telemetryResponse.status, await telemetryResponse.text()).toBe(204);
      expect(installResponse.status, await installResponse.text()).toBe(204);
      expect(harness.telemetryWrites).toHaveLength(1);
      expect(harness.telemetryWrites[0].map((row) => row.event_name).sort()).toEqual(
        (version === "1.0.2"
          ? [
              "operation_completed",
              "provider_refresh_completed",
              "runtime_observation",
              "operation_completed",
            ]
          : [
              "operation_completed",
              "provider_refresh_completed",
              "runtime_observation",
            ]).sort(),
      );
      expect(harness.telemetryWrites[0].map((row) => row.app_version))
        .toEqual(telemetryFixtures.map(() => version));
      const historicalProvider = harness.telemetryWrites[0]
        .find((row) => row.event_name === "provider_refresh_completed");
      expect(historicalProvider?.properties).not.toHaveProperty("failure_code");
      expect(historicalProvider?.properties).not.toHaveProperty("retryable");
      expect(harness.installWrites).toHaveLength(1);
    },
  );

  test("accepts both the prior typed-v1 and released 1.0.2 provider refresh shapes", async () => {
    const harness = workerHarness();
    const priorResponse = await harness.worker.fetch(
      jsonRequest(
        "/functions/v1/telemetry",
        canonicalBatch([providerRefreshTypedV1]),
      ),
      ENV,
    );
    const expandedResponse = await harness.worker.fetch(
      jsonRequest(
        "/functions/v1/telemetry",
        canonicalBatch([providerRefresh]),
      ),
      ENV,
    );

    expect(priorResponse.status, await priorResponse.text()).toBe(204);
    expect(expandedResponse.status, await expandedResponse.text()).toBe(204);
    expect(harness.telemetryWrites).toHaveLength(2);
    expect(harness.telemetryWrites[0][0].properties).not.toHaveProperty("content_evidence");
    expect(harness.telemetryWrites[1][0].properties).toMatchObject({
      content_evidence: "accepted",
      work_kind: "append",
      refresh_result: "complete",
      core_result: "complete",
      failure_scope: "none",
      failure_type: "none",
      retired_records_bucket: "0",
      source_files_bucket: "6-20",
      cpu_duration_bucket: "lt_1s",
      observed_process_peak_rss_bucket: "100mb-1gb",
    });
    expect(harness.telemetryWrites[1][0].properties)
      .not.toHaveProperty("canonical_pro_result");
    expect(harness.telemetryWrites[1][0].properties)
      .not.toHaveProperty("output_pro_result");
  });

  test("accepts the bounded transient 1.1.1 provider corpus-stock shape", async () => {
    const harness = workerHarness();
    const response = await harness.worker.fetch(
      jsonRequest(
        "/functions/v1/telemetry",
        canonicalBatch([providerRefreshCorpusStockV111], "1.1.1"),
      ),
      ENV,
    );

    expect(response.status, await response.text()).toBe(204);
    expect(harness.telemetryWrites[0][0].properties).toMatchObject({
      corpus_stock_indexed_documents_bucket: "21-100",
      corpus_stock_retained_records_bucket: "6-20",
      corpus_stock_rejected_records_bucket: "2-5",
      corpus_stock_certified_source_bytes_bucket: "lt_100kb",
      corpus_transition_removed_sources_bucket: "1",
    });
  });

  test("accepts the released 1.2 provider refresh shape", async () => {
    const harness = workerHarness();
    const response = await harness.worker.fetch(
      jsonRequest(
        "/functions/v1/telemetry",
        canonicalBatch([providerRefreshV120], "1.2.0"),
      ),
      ENV,
    );

    expect(response.status, await response.text()).toBe(204);
    expect(harness.telemetryWrites[0][0].properties).toMatchObject({
      records_bucket: "6-20",
      logical_bytes_bucket: "lt_100kb",
      refresh_result: "complete",
      core_result: "complete",
      failure_scope: "none",
      failure_type: "none",
    });
    expect(harness.telemetryWrites[0][0].properties).not.toHaveProperty("content_evidence");
  });

  test.each(["1.1.1", "1.2.0"])(
    "accepts the released unknown MCP request shape for $version",
    async (version) => {
      const harness = workerHarness();
      const batch = structuredClone(unknownMcpRequestV111);
      batch.app_version = version;
      const response = await harness.worker.fetch(
        jsonRequest("/functions/v1/telemetry", batch),
        ENV,
      );

      expect(response.status, await response.text()).toBe(204);
      expect(harness.telemetryWrites).toHaveLength(1);
      expect(harness.telemetryWrites[0]).toHaveLength(1);
      expect(harness.telemetryWrites[0][0]).toMatchObject({
        app_version: version,
        event_name: "operation_completed",
        surface: "mcp",
        status: "failure",
        properties: {
          operation: "missing",
          method: "unknown",
          tool: "missing",
          error_layer: "json_rpc",
          error_class: "method_not_found",
        },
      });
    },
  );

  test.each(["1.0.0", "1.1.1", "1.2.0"])(
    "accepts released list-events and MCP query-events shapes for $version",
    async (version) => {
      const harness = workerHarness();
      const response = await harness.worker.fetch(
        jsonRequest(
          "/functions/v1/telemetry",
          canonicalBatch([listEventsOperationV100, queryEventsOperationV100], version),
        ),
        ENV,
      );

      expect(response.status, await response.text()).toBe(204);
      expect(harness.telemetryWrites[0].map(({ surface, properties }) => (
        `${surface}:${properties.operation}`
      )).sort()).toEqual(["cli:show", "mcp:query_events"]);
      const cli = harness.telemetryWrites[0].find(({ surface }) => surface === "cli");
      const mcp = harness.telemetryWrites[0].find(({ surface }) => surface === "mcp");
      expect(cli.properties.target_kind).toBe("events");
      expect(mcp.properties).toMatchObject({
        method: "tools_call",
        tool: "query_events",
        result_count_bucket: "2-5",
        result_truncated: false,
        response_bound: "within_limit",
      });
    },
  );

  test("keeps the released list-events target scoped to show operations", async () => {
    const harness = workerHarness();
    const event = structuredClone(listEventsOperationV100);
    event.operation = "locate";
    event.properties.output_format = "json";
    event.properties.provider_lookup = false;
    delete event.properties.writes_out_file;
    delete event.properties.events_returned_bucket;
    const response = await harness.worker.fetch(
      jsonRequest("/functions/v1/telemetry", canonicalBatch([event], "1.2.0")),
      ENV,
    );

    expect(response.status).toBe(422);
    expect(await response.text()).toContain("invalid_target_kind");
    expect(harness.telemetryWrites).toHaveLength(0);
  });

  test.each(["1.0.0", "1.1.1", "1.2.0"])(
    "accepts the released count-omitted list-events success for $version",
    async (version) => {
      const harness = workerHarness();
      const response = await harness.worker.fetch(
        jsonRequest(
          "/functions/v1/telemetry",
          canonicalBatch([listEventsBrokenPipeOperationV100], version),
        ),
        ENV,
      );

      expect(response.status, await response.text()).toBe(204);
      expect(harness.telemetryWrites[0][0].properties).toMatchObject({
        operation: "show",
        target_kind: "events",
      });
      expect(harness.telemetryWrites[0][0].properties)
        .not.toHaveProperty("events_returned_bucket");
    },
  );

  test.each([
    ["non-JSON format", { output_format: "text" }],
    ["file output", { writes_out_file: true }],
    ["resource field", { resource_kind: "file" }],
    ["transcript field", { transcript_mode: "full" }],
    ["window field", { window_bucket: "2-5" }],
  ])("rejects impossible list-events shape: %s", async (_name, replacement) => {
    const harness = workerHarness();
    const event = structuredClone(listEventsOperationV100);
    Object.assign(event.properties, replacement);
    for (const [key, value] of Object.entries(event.properties)) {
      if (value === undefined) delete event.properties[key];
    }
    const response = await harness.worker.fetch(
      jsonRequest("/functions/v1/telemetry", canonicalBatch([event], "1.2.0")),
      ENV,
    );

    expect(response.status).toBe(422);
    expect(await response.text()).toContain("invalid_list_events_shape");
    expect(harness.telemetryWrites).toHaveLength(0);
  });

  test.each([
    ["SQL column count", { column_count_bucket: "2-5" }],
    ["show-event truncation", { events_truncated: false }],
    ["missing successful result count", { result_count_bucket: undefined }],
    ["missing successful zero-result flag", { zero_result: undefined }],
    ["missing successful truncation flag", { result_truncated: undefined }],
    ["inconsistent zero-result flag", { zero_result: true }],
    ["successful replacement", { response_bound: "replaced" }],
  ])("rejects impossible MCP query-events shape: %s", async (_name, replacement) => {
    const harness = workerHarness();
    const event = structuredClone(queryEventsOperationV100);
    Object.assign(event.properties, replacement);
    for (const [key, value] of Object.entries(event.properties)) {
      if (value === undefined) delete event.properties[key];
    }
    const response = await harness.worker.fetch(
      jsonRequest("/functions/v1/telemetry", canonicalBatch([event], "1.2.0")),
      ENV,
    );

    expect(response.status).toBe(422);
    expect(await response.text()).toContain("invalid_query_events_shape");
    expect(harness.telemetryWrites).toHaveLength(0);
  });

  test.each([
    ["invalid request", "within_limit"],
    ["output-limit replacement", "replaced"],
    ["response failure", undefined],
  ])("accepts released MCP query-events failure: %s", async (_name, responseBound) => {
    const harness = workerHarness();
    const event = structuredClone(queryEventsOperationV100);
    event.outcome = "failure";
    event.properties.error_layer = responseBound === undefined ? "response" : "tool";
    event.properties.error_class = responseBound === undefined
      ? "response_write"
      : "tool_failure";
    for (const key of ["result_count_bucket", "zero_result", "result_truncated"]) {
      delete event.properties[key];
    }
    if (responseBound === undefined) delete event.properties.response_bound;
    else event.properties.response_bound = responseBound;
    const response = await harness.worker.fetch(
      jsonRequest("/functions/v1/telemetry", canonicalBatch([event], "1.2.0")),
      ENV,
    );

    expect(response.status, await response.text()).toBe(204);
    expect(harness.telemetryWrites[0][0].properties).toMatchObject({
      method: "tools_call",
      tool: "query_events",
      error_layer: event.properties.error_layer,
      error_class: event.properties.error_class,
    });
  });

  test("accepts an attributable CPU receipt without duplicating a process peak", async () => {
    const harness = workerHarness();
    const event = structuredClone(providerRefresh);
    delete event.properties.observed_process_peak_rss_bucket;
    const response = await harness.worker.fetch(
      jsonRequest("/functions/v1/telemetry", canonicalBatch([event])),
      ENV,
    );

    expect(response.status, await response.text()).toBe(204);
    expect(harness.telemetryWrites[0][0].properties).toMatchObject({
      cpu_duration_bucket: "lt_1s",
    });
    expect(harness.telemetryWrites[0][0].properties)
      .not.toHaveProperty("observed_process_peak_rss_bucket");
  });

  test("rejects an observed process peak without its measurement window", async () => {
    const harness = workerHarness();
    const event = structuredClone(providerRefresh);
    delete event.properties.cpu_duration_bucket;
    const response = await harness.worker.fetch(
      jsonRequest("/functions/v1/telemetry", canonicalBatch([event])),
      ENV,
    );

    expect(response.status).toBe(422);
    expect(await response.text()).toContain("orphaned_observed_process_peak_rss");
    expect(harness.telemetryWrites).toHaveLength(0);
  });

  test.each([
    "1gb+",
    "1gb-10gb",
    "10gb-100gb",
    "1gb-2gb",
    "2gb-5gb",
    "5gb-10gb",
    "10gb-25gb",
    "25gb-50gb",
    "50gb-100gb",
    "100gb+",
  ])("accepts compatible large-source byte cohort %s", async (bucket) => {
    const harness = workerHarness();
    const event = structuredClone(providerRefresh);
    event.properties.bytes_bucket = bucket;
    const response = await harness.worker.fetch(
      jsonRequest("/functions/v1/telemetry", canonicalBatch([event])),
      ENV,
    );

    expect(response.status, await response.text()).toBe(204);
    expect(harness.telemetryWrites[0][0].properties.bytes_bucket).toBe(bucket);
  });

  test.each([
    ["CLI event without output", withoutProperty(operation, "output")],
    ["provider refresh without its aggregate", { ...providerRefresh, properties: {} }],
    ["daemon liveness without its snapshot", { ...runtimeObservation, properties: {} }],
  ])("rejects the wrong shape: %s", async (_name, event) => {
    const harness = workerHarness();
    const response = await harness.worker.fetch(
      jsonRequest("/functions/v1/telemetry", canonicalBatch([event])),
      ENV,
    );

    expect(response.status).toBe(422);
    expect(harness.telemetryWrites).toHaveLength(0);
  });

  test("rejects an install-stage body polluted with batch-envelope fields", async () => {
    const harness = workerHarness();
    const response = await harness.worker.fetch(
      jsonRequest("/functions/v1/install-attempt", { ...installStage, surface: "cli" }),
      ENV,
    );

    expect(response.status).toBe(422);
    expect(harness.installWrites).toHaveLength(0);
  });
});

function readFixture(name) {
  return JSON.parse(readFileSync(new URL(name, FIXTURE_ROOT), "utf8"));
}

function canonicalBatch(events, appVersion = "1.0.2") {
  return {
    client_profile_id: "11111111-1111-4111-8111-111111111111",
    data_root_id: "22222222-2222-4222-8222-222222222222",
    app_version: appVersion,
    os: "linux",
    arch: "x86_64",
    events,
  };
}

function withoutProperty(event, property) {
  const copy = structuredClone(event);
  delete copy.properties[property];
  return copy;
}

function jsonRequest(path, body) {
  return new Request(`https://api.example.test${path}`, {
    method: "POST",
    body: JSON.stringify(body),
    headers: { "content-type": "application/json; charset=utf-8" },
  });
}

function workerHarness() {
  const telemetryWrites = [];
  const installWrites = [];
  const database = {
    async insertTelemetryRows(rows) {
      telemetryWrites.push(rows);
    },
    async insertInstallStageRow(row) {
      installWrites.push(row);
    },
  };
  const worker = withCapturedQueue(createTelemetryWorker({
    createDatabaseClient: () => database,
    now: () => NOW,
  }), { installWrites, telemetryWrites });
  return { installWrites, telemetryWrites, worker };
}
