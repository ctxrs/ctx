import { readFileSync } from "node:fs";

import { describe, expect, test } from "vitest";

import {
  CURRENT_CLI_OPERATIONS,
  CURRENT_MCP_OPERATIONS,
  INSTALL_PLATFORMS,
  OPERATING_SYSTEMS,
} from "../src/telemetry-contract";
import { createTelemetryWorker } from "../src/worker";
import { withCapturedQueue } from "./queue-capture.mjs";

const MATRIX = JSON.parse(readFileSync(
  new URL("./fixtures/public-telemetry-v1/typed-variant-matrix.valid.json", import.meta.url),
  "utf8",
));
const NATIVEPATH_PROTOCOL = JSON.parse(readFileSync(
  new URL(
    "./fixtures/public-telemetry-v1/nativepath-protocol-operation-frames.valid.json",
    import.meta.url,
  ),
  "utf8",
));
const NOW = new Date("2026-07-25T22:35:00.000Z");
const OCCURRED_AT = "2026-07-25T22:34:00Z";
const ENV = {
  TELEMETRY_ANALYTICS_ENVIRONMENT: "staging",
  TELEMETRY_DATABASE_URL: "postgresql://telemetry.example.test/db",
  TELEMETRY_IDENTITY_HMAC_KEY: "typed-variant-hmac-key-with-32-bytes-minimum",
  TELEMETRY_IDENTITY_KEY_VERSION: "1",
  TELEMETRY_RATE_LIMITER: { async limit() { return { success: true }; } },
};

describe("exhaustive public typed telemetry parity", () => {
  test("pins unique closed operation and enum coverage", () => {
    expect(MATRIX.schema_version).toBe(1);
    expect(MATRIX.outcomes).toEqual(["success", "failure"]);
    expect(MATRIX.operating_systems).toEqual(["linux", "macos", "windows", "freebsd"]);
    expect([...OPERATING_SYSTEMS]).toEqual(MATRIX.operating_systems);
    expect(MATRIX.install_stage.platforms).toEqual(["linux", "macos", "windows"]);
    expect([...INSTALL_PLATFORMS]).toEqual(MATRIX.install_stage.platforms);
    expect(unique(MATRIX.operations.cli.map(({ operation }) => operation))).toEqual([
      "setup", "semantic_enable", "semantic_status", "semantic_disable", "status", "index",
      "sources", "import", "show", "locate", "search", "docs", "integration", "upgrade",
      "doctor",
    ]);
    // Preserve the released matrix; ordinary 1.5 Blame has its own producer
    // fixture and HTTP/Queue parity coverage in ordinary-blame-admission.
    expect([...CURRENT_CLI_OPERATIONS]).toEqual([
      ...unique(MATRIX.operations.cli.map(({ operation }) => operation)), "blame",
    ]);
    expect(unique(MATRIX.operations.daemon.map(({ operation }) => operation))).toEqual([
      "enable", "disable", "status", "run_once",
    ]);
    expect(unique(MATRIX.operations.mcp.map(({ operation }) => operation))).toEqual([
      "status", "sources", "search", "show_session", "show_event", "query_events", "blame", "pro_status",
      "unknown", "missing",
    ]);
    expect([...CURRENT_MCP_OPERATIONS]).toEqual(
      unique(MATRIX.operations.mcp.map(({ operation }) => operation)),
    );
    expect(MATRIX.pro_host.operations).toEqual([
      "lifecycle", "materialize", "status", "blame",
    ]);
    expect(Object.keys(MATRIX.pro_host).filter((key) => key.startsWith("query_"))).toEqual([]);
    expect(MATRIX.provider_refresh.providers).toHaveLength(47);
    for (const values of enumDimensions()) expect(unique(values)).toEqual(values);
  });

  test("derives NativePath status/blame telemetry from every public protocol frame variant", () => {
    expect(NATIVEPATH_PROTOCOL.capabilities).toContain("query");
    expect(NATIVEPATH_PROTOCOL.host_message_kinds).toContain("blame");
    expect(NATIVEPATH_PROTOCOL.helper_message_kinds).toContain("blame");
    expect(NATIVEPATH_PROTOCOL.host_message_kinds).not.toContain("query");
    expect(NATIVEPATH_PROTOCOL.helper_message_kinds).not.toContain("query");
    expect(NATIVEPATH_PROTOCOL.host_request_frames).toHaveLength(17);
    expect(NATIVEPATH_PROTOCOL.helper_response_frames).toHaveLength(19);
    expect(protocolBlameTargets()).toEqual(new Set(MATRIX.pro_host.blame_target_kinds));
    expect(NATIVEPATH_PROTOCOL.helper_response_frames.filter((name) => name.startsWith("status_")))
      .toEqual([
        "status_needs_rebuild",
        "status_needs_resume",
        "status_not_materialized",
        "status_partial",
        "status_ready",
      ]);
    expect(MATRIX.pro_host.operations).toContain("status");
    expect(MATRIX.pro_host.operations).toContain("blame");
    expect(MATRIX.pro_host.operations).not.toContain("query");
  });

  test("passes every operation, outcome, lifecycle, failure, provider, and runtime variant", async () => {
    const candidates = telemetryCandidates();
    const batches = chunks(candidates, 50);
    const harness = workerHarness();

    expect(candidates).toHaveLength(221);
    expect(batches.map((events) => events.length)).toEqual([50, 50, 50, 50, 21]);
    for (const events of batches) {
      const response = await harness.worker.fetch(
        jsonRequest("/functions/v1/telemetry", canonicalBatch(events)),
        ENV,
      );
      expect(response.status, await response.text()).toBe(204);
    }

    const rows = harness.telemetryWrites.flat();
    expect(rows).toHaveLength(candidates.length);
    expect(unique(rows.filter(({ event_name }) => event_name === "operation_completed")
      .map(({ surface, properties }) => `${surface}:${properties.operation}`)))
      .toEqual(expect.arrayContaining([
        ...MATRIX.operations.cli.map(({ operation }) => `cli:${operation}`),
        ...MATRIX.operations.daemon.map(({ operation }) => `daemon:${operation}`),
        ...MATRIX.operations.mcp.map(({ operation }) => `mcp:${operation}`),
        ...MATRIX.pro_host.operations.map((operation) => `pro_host:${operation}`),
      ]));
    const providerRows = rows.filter(
      ({ event_name }) => event_name === "provider_refresh_completed",
    );
    expect(providerRows).toHaveLength(47);
    expectProviderCoverage(providerRows);
    expectProCoverage(rows);
    expect(new Set(rows.map(({ status }) => status))).toEqual(new Set(MATRIX.outcomes));
    expect(rows.find(({ surface, properties }) => (
      surface === "pro_host"
        && properties.operation === "blame"
        && properties.blame_target_kind === "pull_request"
    ))).toMatchObject({
      activity_class: expect.stringMatching(/^(?:product_activity|product_value)$/u),
      schema_version: 1,
    });
  });

  test.each(["1.1.1", "1.2.3", "1.2.4"])(
    "accepts the released event-queries docs topic from %s",
    async (version) => {
      const docs = MATRIX.operations.cli.find(({ properties }) => (
        properties.topic === "event-queries"
      ));
      const event = {
        event_id: eventId(899),
        event_name: "operation_completed",
        event_version: 1,
        occurred_at: OCCURRED_AT,
        surface: "cli",
        operation: "docs",
        outcome: "success",
        duration_bucket: "lt_1s",
        properties: structuredClone(docs.properties),
      };
      const harness = workerHarness();

      const response = await harness.worker.fetch(
        jsonRequest("/functions/v1/telemetry", {
          ...canonicalBatch([event]),
          app_version: version,
        }),
        ENV,
      );

      expect(response.status, await response.text()).toBe(204);
      expect(harness.telemetryWrites[0][0].properties.topic).toBe("event-queries");
    },
  );

  test("separates retired SQL ingestion from the current CLI and MCP contracts", async () => {
    expect(CURRENT_CLI_OPERATIONS.has("sql")).toBe(false);
    expect(CURRENT_MCP_OPERATIONS.has("sql")).toBe(false);

    const historicalEvents = [
      {
        event_id: eventId(901),
        event_name: "operation_completed",
        event_version: 1,
        occurred_at: OCCURRED_AT,
        surface: "cli",
        operation: "sql",
        outcome: "success",
        duration_bucket: "lt_1s",
        properties: { output: "json", input: "inline", output_format: "json" },
      },
      {
        event_id: eventId(902),
        event_name: "operation_completed",
        event_version: 1,
        occurred_at: OCCURRED_AT,
        surface: "mcp",
        operation: "sql",
        outcome: "success",
        duration_bucket: "lt_1s",
        properties: { method: "tools_call", tool: "sql" },
      },
    ];
    const harness = workerHarness();
    const response = await harness.worker.fetch(
      jsonRequest("/functions/v1/telemetry", canonicalBatch(historicalEvents)),
      ENV,
    );
    expect(response.status, await response.text()).toBe(204);
    expect(harness.telemetryWrites.flat()).toHaveLength(2);
  });

  test("accepts every general CLI OS without widening managed installer platforms", async () => {
    const harness = workerHarness();
    for (const [index, os] of MATRIX.operating_systems.entries()) {
      const event = {
        event_id: eventId(950 + index),
        event_name: "operation_completed",
        event_version: 1,
        occurred_at: OCCURRED_AT,
        surface: "cli",
        operation: "status",
        outcome: "success",
        duration_bucket: "lt_1s",
        properties: { output: "json" },
      };
      const response = await harness.worker.fetch(
        jsonRequest("/functions/v1/telemetry", canonicalBatch([event], os)),
        ENV,
      );
      expect(response.status, `${os}: ${await response.text()}`).toBe(204);
    }
    expect(harness.telemetryWrites.flat().map(({ os }) => os)).toEqual(
      MATRIX.operating_systems,
    );
  });

  test("passes every standalone install stage/status pair", async () => {
    const harness = workerHarness();
    const { architectures, platforms, script_families: scriptFamilies } = MATRIX.install_stage;
    for (const [index, [stage, status]] of MATRIX.install_stage.stage_status_pairs.entries()) {
      const response = await harness.worker.fetch(
        jsonRequest("/functions/v1/install-attempt", {
          event_name: "install_stage",
          event_version: 1,
          install_attempt_id: `ia_variant_${String(index).padStart(3, "0")}`,
          stage,
          status,
          platform: platforms[index % platforms.length],
          arch: architectures[index % architectures.length],
          script_family: scriptFamilies[index % scriptFamilies.length],
        }),
        ENV,
      );
      expect(response.status, `${stage}:${status} ${await response.text()}`).toBe(204);
    }
    expect(harness.installWrites).toHaveLength(MATRIX.install_stage.stage_status_pairs.length);
  });
});

function telemetryCandidates() {
  let sequence = 0;
  const event = (eventName, surface, operation, outcome, properties, durationBucket = "lt_1s") => ({
    event_id: eventId(++sequence),
    event_name: eventName,
    event_version: 1,
    occurred_at: OCCURRED_AT,
    surface,
    operation,
    outcome,
    duration_bucket: durationBucket,
    properties: structuredClone(properties),
  });

  const candidates = [];
  for (const surface of ["cli", "daemon", "mcp"]) {
    for (const variant of MATRIX.operations[surface]) {
      for (const outcome of variant.outcome ? [variant.outcome] : MATRIX.outcomes) {
        candidates.push(event(
          "operation_completed",
          surface,
          variant.operation,
          outcome,
          variant.properties,
        ));
      }
    }
  }
  candidates.push(...proCandidates(event));
  for (const surface of ["daemon", "mcp"]) {
    for (const variant of MATRIX.runtime[surface]) {
      for (const outcome of MATRIX.outcomes) {
        candidates.push(event(
          "runtime_observation",
          surface,
          variant.operation,
          outcome,
          variant.properties,
        ));
      }
    }
  }
  candidates.push(...mcpFailureCandidates(event));
  candidates.push(...runtimeDimensionCandidates(event));
  candidates.push(...providerCandidates(event));
  return candidates;
}

function proCandidates(event) {
  const pro = MATRIX.pro_host;
  const length = pro.failure_buckets.length + 1;
  const candidates = [];
  for (let index = 0; index < length; index += 1) {
    const failure = index === 0 ? undefined : pro.failure_buckets[index - 1];
    const outcome = failure ? "failure" : "success";
    const accessState = at(pro.access_states, index);
    const helper = at(pro.helper_connection_outcomes, index);

    candidates.push(event("operation_completed", "pro_host", "lifecycle", outcome, compact({
      lifecycle_operation: at(pro.lifecycle_operations, index),
      access_state: accessState,
      helper_connection_outcome: helper,
      reconcile_outcome: at(pro.reconcile_outcomes, index),
      uninstall_data_disposition: at(pro.uninstall_data_dispositions, index),
      lifecycle_failure_bucket: failure,
    })));

    candidates.push(event("operation_completed", "pro_host", "materialize", outcome, compact({
      materialization_commit: at(pro.materialization_commits, index),
      materialization_freshness: at(pro.freshness, index),
      materialization_result: failure ? "failed" : "completed",
      helper_connection_outcome: helper,
      materialization_failure_bucket: failure,
    })));

    candidates.push(event("operation_completed", "pro_host", "status", outcome, compact({
      status_surface: at(pro.surfaces, index),
      access_state: accessState,
      helper_connection_outcome: helper,
      status_failure_bucket: failure,
    })));

    candidates.push(event("operation_completed", "pro_host", "blame", outcome, compact({
      blame_target_kind: at(pro.blame_target_kinds, index),
      blame_surface: at(pro.surfaces, index),
      blame_result_count_bucket: index % 2 === 0 ? "2-5" : "0",
      blame_has_more: index % 3 === 0,
      blame_failure_bucket: failure,
    })));
  }
  return candidates;
}

function mcpFailureCandidates(event) {
  const mcp = MATRIX.mcp_dimensions;
  return mcp.error_classes.map((errorClass, index) => event(
    "operation_completed",
    "mcp",
    "unknown",
    "failure",
    {
      method: "tools_call",
      tool: "unknown",
      error_layer: at(mcp.error_layers, index),
      error_class: errorClass,
      response_bound: at(mcp.response_bounds, index),
    },
  ));
}

function runtimeDimensionCandidates(event) {
  const stopped = MATRIX.runtime.mcp.find(({ operation }) => operation === "stopped");
  const cycle = MATRIX.runtime.daemon.find(({ operation }) => operation === "cycle");
  return [
    ...MATRIX.mcp_dimensions.stop_reasons.map((stopReason) => event(
      "runtime_observation",
      "mcp",
      "stopped",
      stopReason === "eof" ? "success" : "failure",
      { ...stopped.properties, stop_reason: stopReason },
    )),
    ...MATRIX.daemon_dimensions.cycle_results.map((cycleResult) => event(
      "runtime_observation",
      "daemon",
      "cycle",
      cycleResult === "failure" ? "failure" : "success",
      { ...cycle.properties, cycle_result: cycleResult },
    )),
  ];
}

function providerCandidates(event) {
  const provider = MATRIX.provider_refresh;
  const nonNoneScopes = provider.failure_scopes.filter((value) => value !== "none");
  return provider.providers.map((providerName, index) => {
    const refreshResult = at(provider.refresh_results, index);
    const failureType = at(provider.failure_types, index);
    const failureScope = failureType === "none" ? "none" : at(nonNoneScopes, index);
    const workKind = at(provider.work_kinds, index);
    const properties = compact({
      provider: providerName,
      trigger: at(provider.triggers, index),
      source_mode: at(provider.source_modes, index),
      change: at(provider.changes, index),
      content_evidence: at(provider.content_evidence, index),
      work_kind: workKind,
      refresh_result: refreshResult,
      core_result: at(provider.core_results, index),
      canonical_pro_result: at(provider.pro_results, index),
      output_pro_result: at(provider.pro_results, index + 3),
      failure_scope: failureScope,
      failure_type: failureType,
      work_remaining: index % 2 === 0,
      retired_records_bucket: index % 2 === 0 ? at(provider.count_buckets, index) : undefined,
      sources_bucket: at(provider.count_buckets, index),
      source_files_bucket: at(provider.count_buckets, index + 1),
      sessions_bucket: at(provider.count_buckets, index + 2),
      events_bucket: at(provider.count_buckets, index + 3),
      edges_bucket: at(provider.count_buckets, index + 4),
      skips_bucket: at(provider.count_buckets, index + 5),
      rejections_bucket: at(provider.count_buckets, index + 6),
      failures_bucket: at(provider.count_buckets, index + 7),
      bytes_bucket: at(provider.byte_buckets, index),
      cpu_duration_bucket: at(provider.duration_buckets, index + 1),
      observed_process_peak_rss_bucket: at(provider.byte_buckets, index + 3),
    });
    return event(
      "provider_refresh_completed",
      at(provider.surfaces, index),
      "refresh",
      refreshResult === "failure" ? "failure" : "success",
      properties,
      at(provider.duration_buckets, index),
    );
  });
}

function enumDimensions() {
  const provider = MATRIX.provider_refresh;
  const pro = MATRIX.pro_host;
  return [
    MATRIX.mcp_dimensions.error_layers,
    MATRIX.mcp_dimensions.error_classes,
    MATRIX.mcp_dimensions.response_bounds,
    MATRIX.mcp_dimensions.stop_reasons,
    MATRIX.daemon_dimensions.cycle_results,
    pro.lifecycle_operations,
    pro.access_states,
    pro.helper_connection_outcomes,
    pro.reconcile_outcomes,
    pro.uninstall_data_dispositions,
    pro.materialization_commits,
    pro.freshness,
    pro.materialization_results,
    pro.surfaces,
    pro.failure_buckets,
    pro.blame_target_kinds,
    provider.providers,
    provider.surfaces,
    provider.triggers,
    provider.source_modes,
    provider.changes,
    provider.content_evidence,
    provider.work_kinds,
    provider.refresh_results,
    provider.core_results,
    provider.pro_results,
    provider.failure_scopes,
    provider.failure_types,
    provider.count_buckets,
    provider.byte_buckets,
    provider.duration_buckets,
  ];
}

function expectProviderCoverage(rows) {
  const provider = MATRIX.provider_refresh;
  for (const [property, expected] of [
    ["provider", provider.providers],
    ["trigger", provider.triggers],
    ["source_mode", provider.source_modes],
    ["change", provider.changes],
    ["content_evidence", provider.content_evidence],
    ["work_kind", provider.work_kinds],
    ["refresh_result", provider.refresh_results],
    ["core_result", provider.core_results],
    ["canonical_pro_result", provider.pro_results],
    ["output_pro_result", provider.pro_results],
    ["failure_scope", provider.failure_scopes],
    ["failure_type", provider.failure_types],
    ["sources_bucket", provider.count_buckets],
    ["source_files_bucket", provider.count_buckets],
    ["bytes_bucket", provider.byte_buckets],
    ["cpu_duration_bucket", provider.duration_buckets],
    ["observed_process_peak_rss_bucket", provider.byte_buckets],
  ]) {
    const actual = rows.map(({ properties }) => properties[property] ?? null);
    expect(new Set(actual), `provider coverage for ${property}`).toEqual(new Set(expected));
  }
  expect(new Set(rows.map(({ surface }) => surface))).toEqual(new Set(provider.surfaces));
  expect(new Set(rows.map(({ duration_bucket }) => duration_bucket)))
    .toEqual(new Set(provider.duration_buckets));
}

function expectProCoverage(rows) {
  const pro = MATRIX.pro_host;
  const proRows = rows.filter(({ surface }) => surface === "pro_host");
  for (const operation of pro.operations) {
    expect(proRows.filter(({ properties }) => properties.operation === operation))
      .toHaveLength(pro.failure_buckets.length + 1);
  }
  for (const [operation, property, expected] of [
    ["lifecycle", "lifecycle_operation", pro.lifecycle_operations],
    ["lifecycle", "access_state", pro.access_states],
    ["lifecycle", "helper_connection_outcome", pro.helper_connection_outcomes],
    ["lifecycle", "reconcile_outcome", pro.reconcile_outcomes],
    ["lifecycle", "uninstall_data_disposition", pro.uninstall_data_dispositions],
    ["materialize", "materialization_commit", pro.materialization_commits],
    ["materialize", "materialization_freshness", pro.freshness],
    ["materialize", "materialization_result", pro.materialization_results],
    ["status", "status_surface", pro.surfaces],
    ["blame", "blame_target_kind", pro.blame_target_kinds],
    ["blame", "blame_surface", pro.surfaces],
  ]) {
    const actual = proRows
      .filter(({ properties }) => properties.operation === operation)
      .map(({ properties }) => properties[property]);
    expect(new Set(actual), `Pro coverage for ${property}`).toEqual(new Set(expected));
  }
  for (const [operation, property] of [
    ["lifecycle", "lifecycle_failure_bucket"],
    ["materialize", "materialization_failure_bucket"],
    ["status", "status_failure_bucket"],
    ["blame", "blame_failure_bucket"],
  ]) {
    const actual = proRows
      .filter(({ properties }) => properties.operation === operation)
      .map(({ properties }) => properties[property])
      .filter(Boolean);
    expect(new Set(actual), `Pro coverage for ${property}`)
      .toEqual(new Set(pro.failure_buckets));
  }
}

function protocolBlameTargets() {
  const frames = [
    ...NATIVEPATH_PROTOCOL.host_request_frames,
    ...NATIVEPATH_PROTOCOL.helper_response_frames,
  ].filter((name) => name.startsWith("blame_"));
  return new Set(frames.map((name) => {
    if (name.startsWith("blame_file")) return "file";
    if (name === "blame_commit") return "commit";
    if (name.startsWith("blame_pull_request")) return "pull_request";
    throw new Error(`unmapped public blame frame: ${name}`);
  }));
}

function canonicalBatch(events, os = "linux") {
  return {
    client_profile_id: "11111111-1111-4111-8111-111111111111",
    data_root_id: "22222222-2222-4222-8222-222222222222",
    app_version: "0.27.0",
    os,
    arch: "x86_64",
    events,
  };
}

function eventId(sequence) {
  return `10000000-0000-4000-8000-${String(sequence).padStart(12, "0")}`;
}

function chunks(values, size) {
  const out = [];
  for (let index = 0; index < values.length; index += size) {
    out.push(values.slice(index, index + size));
  }
  return out;
}

function unique(values) {
  return [...new Set(values)];
}

function at(values, index) {
  return values[index % values.length];
}

function compact(value) {
  return Object.fromEntries(Object.entries(value).filter(([, item]) => item !== undefined && item !== null));
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
