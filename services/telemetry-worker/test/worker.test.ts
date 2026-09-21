import { describe, expect, test, vi } from "vitest";

import {
  NeonTelemetryDatabase,
  TelemetryEventCollisionError,
  type NeonQueryClient,
  type TelemetryDatabase,
  type TelemetryIngestRejection,
} from "../src/database";
import { hmacSha256Hex, sha256Hex } from "../src/hash";
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
  CURRENT_PROVIDERS,
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
  CURRENT_PROVIDER_WIRE_NAMES,
  HISTORICAL_PROVIDER_WIRE_NAMES,
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

describe("static telemetry contracts", () => {
  test("accepts operation, provider refresh, and runtime v1 fixtures", async () => {
    const operation = await buildTelemetryIngestPlan(v1Batch([operationEvent()]), INGEST_OPTIONS);
    const refresh = await buildTelemetryIngestPlan(v1Batch([providerRefreshEvent()]), INGEST_OPTIONS);
    const runtime = await buildTelemetryIngestPlan(v1Batch([runtimeEvent()]), INGEST_OPTIONS);

    expect(operation.rows[0]).toMatchObject({
      activity_class: "product_value",
      broker_install_id_hash: null,
      event_name: "operation_completed",
      install_id_hash: null,
      origin_install_id_hash: null,
      schema_version: 1,
      traffic_class: "unclassified_public",
    });
    expect(refresh.rows[0]).toMatchObject({
      activity_class: "automatic",
      event_name: "provider_refresh_completed",
      provider_id: "codex",
      schema_version: 1,
    });
    expect(runtime.rows[0]).toMatchObject({
      activity_class: "liveness",
      event_name: "runtime_observation",
      schema_version: 1,
    });
  });

  test("keeps environment, traffic, and passive activity server-owned", async () => {
    const plan = await buildTelemetryIngestPlan(v1Batch([
      operationEvent({ operation: "status", properties: { output: "human" } }),
      operationEvent({
        event_id: "44444444-4444-4444-8444-444444444444",
        operation: "setup",
        properties: {
          output: "json",
          no_daemon: false,
          wait: false,
          progress_mode: "none",
        },
      }),
      runtimeEvent({
        event_id: "55555555-5555-4555-8555-555555555555",
        operation: "cycle",
        properties: daemonCycleProperties(),
      }),
    ]), {
      ...INGEST_OPTIONS,
      analyticsEnvironment: "staging",
    });

    expect(plan.rows.map((row) => row.activity_class)).toEqual([
      "status",
      "setup",
      "operational",
    ]);
    expect(plan.rows.every((row) => row.analytics_environment === "staging")).toBe(true);
    expect(plan.rows.every((row) => row.traffic_class === "synthetic")).toBe(true);

    for (const assertedClassification of [
      { client_profile_class: "internal" },
      { data_root_class: "ci" },
      { traffic_class: "user" },
    ]) {
      await expect(buildTelemetryIngestPlan({
        ...v1Batch([operationEvent()]),
        ...assertedClassification,
      }, INGEST_OPTIONS)).rejects.toMatchObject({ code: "unknown_batch_field" });
    }
  });

  test("accepts semantic lifecycle operations and semantic daemon provenance", async () => {
    const plan = await buildTelemetryIngestPlan(v1Batch([
      operationEvent({
        operation: "semantic_enable",
        properties: { output: "human" },
      }),
      operationEvent({
        event_id: "44444444-4444-4444-8444-444444444444",
        operation: "semantic_status",
        properties: { output: "json" },
      }),
      operationEvent({
        event_id: "55555555-5555-4555-8555-555555555555",
        operation: "semantic_disable",
        properties: { output: "human" },
      }),
      runtimeEvent({
        event_id: "66666666-6666-4666-8666-666666666666",
        operation: "ready",
        properties: {
          ...daemonRunProperties(),
          trigger_command: "semantic",
        },
      }),
    ]), INGEST_OPTIONS);

    expect(plan.rows.map((row) => row.properties.operation)).toEqual([
      "semantic_enable", "semantic_status", "semantic_disable", "ready",
    ]);
    expect(plan.rows.map((row) => row.activity_class)).toEqual([
      "setup", "status", "setup", "operational",
    ]);
  });

  test("classifies foreground value independently from attached maintenance work", async () => {
    const search = operationEvent({
      properties: {
        ...(operationEvent().properties as Record<string, unknown>),
        search_refresh_mode: "background",
      },
    });
    const show = operationEvent({
      event_id: "44444444-4444-4444-8444-444444444444",
      operation: "show",
      properties: {
        output: "human",
        target_kind: "session",
        output_format: "text",
        writes_out_file: false,
        provider_lookup: false,
        auto_upgrade_probe: true,
        auto_upgrade_due: false,
        auto_upgrade_spawned: false,
        auto_upgrade_spawn_status: "not_due",
        auto_upgrade_channel: "stable",
      },
    });

    const plan = await buildTelemetryIngestPlan(v1Batch([search, show]), INGEST_OPTIONS);

    expect(plan.rows.map((row) => row.activity_class)).toEqual([
      "product_value",
      "product_value",
    ]);
  });

  test("accepts current enums and retired fields from released producers", async () => {
    const plan = await buildTelemetryIngestPlan(v1Batch([
      operationEvent({
        operation: "show",
        properties: {
          output: "human",
          target_kind: "resource",
          resource_kind: "pull_request",
          output_format: "jsonl",
          writes_out_file: false,
          provider_lookup: false,
        },
      }),
      operationEvent({
        event_id: "44444444-4444-4444-8444-444444444444",
        operation: "setup",
        properties: {
          output: "human",
          catalog_only: false,
          no_daemon: false,
          wait: true,
          progress_mode: "plain",
          providers_detected_bucket: "2-5",
        },
      }),
      operationEvent({
        event_id: "55555555-5555-4555-8555-555555555555",
        operation: "sources",
        properties: { output: "human", all_sources: true, show_missing: false },
      }),
    ]), INGEST_OPTIONS);
    expect(plan.rows).toHaveLength(3);
  });

  test("accepts final provider lifecycle/outcome/failure fields and rejects stale vocabulary", async () => {
    const daemon = providerRefreshEvent({
      event_id: "44444444-4444-4444-8444-444444444444",
      surface: "daemon",
      properties: {
        ...(providerRefreshEvent().properties as Record<string, unknown>),
        trigger: "daemon",
      },
    });
    const partial = providerRefreshEvent({
      event_id: "55555555-5555-4555-8555-555555555555",
      properties: {
        ...(providerRefreshEvent().properties as Record<string, unknown>),
        content_evidence: "mixed",
        work_kind: "append",
        refresh_result: "partial",
        core_result: "partial",
        canonical_pro_result: "complete",
        output_pro_result: "behind",
        failure_scope: "record",
        failure_type: "record_rejection",
        retired_records_bucket: "10k-100k",
        source_files_bucket: "1k-10k",
      },
    });
    const failed = providerRefreshEvent({
      event_id: "66666666-6666-4666-8666-666666666666",
      outcome: "failure",
      properties: {
        ...(providerRefreshEvent().properties as Record<string, unknown>),
        refresh_result: "failure",
        core_result: "failure",
        canonical_pro_result: "failure",
        output_pro_result: "unavailable",
        failure_scope: "source",
        failure_type: "store",
      },
    });
    const plan = await buildTelemetryIngestPlan(
      v1Batch([providerRefreshEvent(), daemon, partial, failed]),
      INGEST_OPTIONS,
    );
    expect(plan.rows.map((row) => [row.surface, row.activity_class])).toEqual([
      ["cli", "automatic"],
      ["daemon", "automatic"],
      ["cli", "operational"],
      ["cli", "operational"],
    ]);

    const finalWithoutOptionalEvidence = providerRefreshEvent();
    delete (finalWithoutOptionalEvidence.properties as Record<string, unknown>).work_kind;
    delete (finalWithoutOptionalEvidence.properties as Record<string, unknown>)
      .retired_records_bucket;
    await expect(buildTelemetryIngestPlan(
      v1Batch([finalWithoutOptionalEvidence]),
      INGEST_OPTIONS,
    )).resolves.toMatchObject({ rows: [{ activity_class: "automatic" }] });

    const priorTypedShape = providerRefreshEvent();
    const priorProperties = priorTypedShape.properties as Record<string, unknown>;
    for (const key of [
      "content_evidence", "work_kind", "refresh_result", "core_result", "canonical_pro_result",
      "output_pro_result", "failure_scope", "failure_type", "retired_records_bucket",
      "source_files_bucket",
    ]) {
      delete priorProperties[key];
    }
    await expect(buildTelemetryIngestPlan(
      v1Batch([priorTypedShape]),
      INGEST_OPTIONS,
    )).resolves.toMatchObject({ rows: [{ activity_class: "automatic" }] });

    const currentCoreShape = providerRefreshEvent();
    delete (currentCoreShape.properties as Record<string, unknown>).canonical_pro_result;
    delete (currentCoreShape.properties as Record<string, unknown>).output_pro_result;
    await expect(buildTelemetryIngestPlan(
      v1Batch([currentCoreShape]),
      INGEST_OPTIONS,
    )).resolves.toMatchObject({ rows: [{ activity_class: "automatic" }] });

    const sparseProviderReceipt = providerRefreshEvent({
      surface: "daemon",
      properties: {
        ...(providerRefreshEvent().properties as Record<string, unknown>),
        trigger: "daemon",
      },
    });
    for (const key of ["source_files_bucket", "events_bucket", "edges_bucket", "skips_bucket"]) {
      delete (sparseProviderReceipt.properties as Record<string, unknown>)[key];
    }
    delete (sparseProviderReceipt.properties as Record<string, unknown>).canonical_pro_result;
    delete (sparseProviderReceipt.properties as Record<string, unknown>).output_pro_result;
    const providerNeutralReceipt = providerRefreshEvent({
      event_id: "77777777-7777-4777-8777-777777777777",
      surface: "daemon",
      properties: {
        ...(providerRefreshEvent().properties as Record<string, unknown>),
        trigger: "daemon",
      },
    });
    for (const key of [
      "provider", "source_mode", "sources_bucket", "source_files_bucket", "sessions_bucket",
      "events_bucket", "edges_bucket", "skips_bucket", "rejections_bucket", "failures_bucket",
      "bytes_bucket", "canonical_pro_result", "output_pro_result",
    ]) {
      delete (providerNeutralReceipt.properties as Record<string, unknown>)[key];
    }
    const sparsePlan = await buildTelemetryIngestPlan(
      v1Batch([sparseProviderReceipt, providerNeutralReceipt]),
      INGEST_OPTIONS,
    );
    expect(sparsePlan.rows.map((row) => row.provider_id)).toEqual(["codex", null]);
    expect(sparsePlan.rows[0].properties).not.toHaveProperty("events_bucket");
    expect(sparsePlan.rows[1].properties).not.toHaveProperty("provider");

    const incompleteFinalShape = providerRefreshEvent();
    delete (incompleteFinalShape.properties as Record<string, unknown>).failure_type;
    await expect(buildTelemetryIngestPlan(
      v1Batch([incompleteFinalShape]),
      INGEST_OPTIONS,
    )).rejects.toMatchObject({ code: "incomplete_provider_refresh_outcome" });

    await expect(buildTelemetryIngestPlan(v1Batch([
      providerRefreshEvent({ operation: "search" }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: "invalid_provider_refresh_operation" });
    await expect(buildTelemetryIngestPlan(v1Batch([
      providerRefreshEvent({
        properties: {
          provider: "codex",
          trigger: "search",
          source_origin: "detected",
          ingestion_mode: "incremental",
          available: true,
          selected: true,
          importable: true,
          work_remaining: false,
        },
      }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: "unknown_provider_refresh_property" });
    await expect(buildTelemetryIngestPlan(v1Batch([
      providerRefreshEvent({
        outcome: "success",
        properties: {
          ...(providerRefreshEvent().properties as Record<string, unknown>),
          refresh_result: "failure",
        },
      }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: "inconsistent_refresh_outcome" });
    await expect(buildTelemetryIngestPlan(v1Batch([
      providerRefreshEvent({
        properties: {
          ...(providerRefreshEvent().properties as Record<string, unknown>),
          failure_scope: "source",
          failure_type: "none",
        },
      }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: "inconsistent_provider_failure" });
  });

  test("accepts every final provider refresh enum and sparse optional receipt facts", async () => {
    const enumCases = new Map<string, readonly string[]>([
      ["content_evidence", ["none", "accepted", "mixed", "unknown"]],
      ["work_kind", ["no_op", "fresh", "append", "rewrite", "truncate", "replace", "retire", "mixed"]],
      ["refresh_result", ["complete", "partial", "failure"]],
      ["core_result", ["no_op", "complete", "partial", "failure", "unknown"]],
      ["canonical_pro_result", [
        "not_requested", "unavailable", "no_op", "complete", "partial", "behind", "failure", "unknown",
      ]],
      ["output_pro_result", [
        "not_requested", "unavailable", "no_op", "complete", "partial", "behind", "failure", "unknown",
      ]],
      ["failure_scope", ["none", "record", "source", "system", "mixed", "unknown"]],
      ["failure_type", [
        "none", "record_rejection", "unsupported_schema", "not_found", "permission",
        "source_database", "malformed_source", "store", "worker_panic", "system_io", "system",
        "other", "mixed", "unknown",
      ]],
    ]);
    for (const [field, values] of enumCases) {
      for (const value of values) {
        const properties = providerRefreshProperties({ [field]: value });
        if (field === "failure_scope" && value !== "none") properties.failure_type = "unknown";
        if (field === "failure_type" && value !== "none") properties.failure_scope = "unknown";
        await expect(buildTelemetryIngestPlan(v1Batch([
          providerRefreshEvent({
            outcome: field === "refresh_result" && value === "failure" ? "failure" : "success",
            properties,
          }),
        ]), INGEST_OPTIONS)).resolves.toMatchObject({ rows: [{ schema_version: 1 }] });
      }
    }

    const withoutOptionals = providerRefreshProperties();
    delete withoutOptionals.work_kind;
    delete withoutOptionals.retired_records_bucket;
    await expect(buildTelemetryIngestPlan(v1Batch([
      providerRefreshEvent({ properties: withoutOptionals }),
    ]), INGEST_OPTIONS)).resolves.toMatchObject({ rows: [{ schema_version: 1 }] });

    for (const required of [
      "refresh_result", "core_result", "failure_scope", "failure_type",
    ]) {
      const missingRequired = providerRefreshProperties();
      delete missingRequired[required];
      await expect(buildTelemetryIngestPlan(v1Batch([
        providerRefreshEvent({ properties: missingRequired }),
      ]), INGEST_OPTIONS)).rejects.toMatchObject({ status: 422 });
    }

    const withoutRetiredContentEvidence = providerRefreshProperties();
    delete withoutRetiredContentEvidence.content_evidence;
    await expect(buildTelemetryIngestPlan(v1Batch([
      providerRefreshEvent({ properties: withoutRetiredContentEvidence }),
    ]), INGEST_OPTIONS)).resolves.toMatchObject({ rows: [{ schema_version: 1 }] });

    const incompleteProEnrichment = providerRefreshProperties();
    delete incompleteProEnrichment.output_pro_result;
    await expect(buildTelemetryIngestPlan(v1Batch([
      providerRefreshEvent({ properties: incompleteProEnrichment }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: "incomplete_provider_refresh_outcome" });

    const proOnlyFinalShape = providerRefreshProperties();
    for (const key of [
      "content_evidence", "refresh_result", "core_result", "failure_scope", "failure_type",
      "output_pro_result", "work_kind", "retired_records_bucket", "cpu_duration_bucket",
      "observed_process_peak_rss_bucket",
    ]) {
      delete proOnlyFinalShape[key];
    }
    await expect(buildTelemetryIngestPlan(v1Batch([
      providerRefreshEvent({ properties: proOnlyFinalShape }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: "incomplete_provider_refresh_outcome" });
  });

  test.each([
    ["content_evidence", "present", "invalid_content_evidence"],
    ["work_kind", "incremental", "invalid_work_kind"],
    ["refresh_result", "success", "invalid_refresh_result"],
    ["core_result", "skipped", "invalid_core_result"],
    ["canonical_pro_result", "requested", "invalid_canonical_pro_result"],
    ["output_pro_result", "stale", "invalid_output_pro_result"],
    ["failure_scope", "provider", "invalid_failure_scope"],
    ["failure_type", "raw_error", "invalid_failure_type"],
  ])("rejects open provider refresh enum %s=%s", async (field, value, code) => {
    await expect(buildTelemetryIngestPlan(v1Batch([
      providerRefreshEvent({ properties: providerRefreshProperties({ [field]: value }) }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code, status: 422 });
  });

  test("keeps manifest current and historical provider names in the accepted vocabulary", async () => {
    expect(Array.from(CURRENT_PROVIDERS)).toEqual(CURRENT_PROVIDER_WIRE_NAMES);
    expect(HISTORICAL_PROVIDER_WIRE_NAMES).toEqual(["windsurf", "trae"]);
    expect(Array.from(PROVIDERS)).toEqual(CLOSED_PROVIDERS);
    const plan = await buildTelemetryIngestPlan(v1Batch(CLOSED_PROVIDERS.map((provider, index) =>
      providerRefreshEvent({
        event_id: batchEventId(index),
        properties: providerRefreshProperties({ provider }),
      })
    )), INGEST_OPTIONS);

    expect(plan.rows).toHaveLength(49);
    expect(plan.rows.map((row) => row.provider_id)).toEqual(CLOSED_PROVIDERS);
  });

  test("normalizes unfamiliar typed-v1 provider names instead of rejecting the event", async () => {
    const refresh = await buildTelemetryIngestPlan(v1Batch([
      providerRefreshEvent({
        properties: providerRefreshProperties({ provider: "future_harness" }),
      }),
    ]), INGEST_OPTIONS);
    const operation = operationEvent();
    operation.properties = {
      ...(operation.properties as Record<string, unknown>),
      has_provider_filter: true,
      provider_filter: "future_harness",
    };
    const filtered = await buildTelemetryIngestPlan(v1Batch([operation]), INGEST_OPTIONS);

    expect(refresh.rows[0].provider_id).toBe("unknown");
    expect(refresh.rows[0].properties.provider).toBe("unknown");
    expect(filtered.rows[0].provider_id).toBe("unknown");
    expect(filtered.rows[0].properties.provider_filter).toBe("unknown");
  });

  test("rejects malformed provider text while preserving provider-neutral refreshes", async () => {
    await expect(buildTelemetryIngestPlan(v1Batch([
      providerRefreshEvent({
        properties: providerRefreshProperties({ provider: "/private/path" }),
      }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: "invalid_provider", status: 422 });

    const properties = providerRefreshProperties();
    delete properties.provider;
    const neutral = await buildTelemetryIngestPlan(v1Batch([
      providerRefreshEvent({ properties }),
    ]), INGEST_OPTIONS);
    expect(neutral.rows[0].provider_id).toBeNull();
    expect(neutral.rows[0].properties).not.toHaveProperty("provider");
  });

  test("keeps the final expanded buckets in the current typed-v1 vocabulary", async () => {
    expect(Array.from(DURATION_BUCKETS)).toEqual([
      "unknown", "lt_100ms", "lt_1s", "lt_5s", "lt_30s", "lt_2m", "lt_10m",
      "lt_1h", "gte_1h",
    ]);
    expect(Array.from(COUNT_BUCKETS)).toEqual([
      "0", "1", "2-5", "6-20", "21-100", "101-1k", "1k+", "1k-10k", "10k-100k",
      "100k-1m", "1m+",
    ]);
    expect(Array.from(BYTE_BUCKETS)).toEqual([
      "0", "lt_100kb", "100kb-1mb", "1mb-10mb", "10mb-100mb", "100mb-1gb", "1gb+",
      "1gb-10gb", "10gb-100gb",
      "1gb-2gb", "2gb-5gb", "5gb-10gb", "10gb-25gb", "25gb-50gb", "50gb-100gb",
      "100gb+",
    ]);

    await expect(buildTelemetryIngestPlan(v1Batch([
      providerRefreshEvent({
        duration_bucket: "unknown",
        properties: providerRefreshProperties({
          retired_records_bucket: "1m+",
          source_files_bucket: "100k-1m",
          events_bucket: "1m+",
          bytes_bucket: "100gb+",
        }),
      }),
    ]), INGEST_OPTIONS)).resolves.toMatchObject({ rows: [{ duration_bucket: "unknown" }] });

  });

  test("accepts exact daemon operation/runtime shapes and rejects stale facts", async () => {
    const events = [
      operationEvent({ surface: "daemon", operation: "enable", properties: {} }),
      operationEvent({
        event_id: "44444444-4444-4444-8444-444444444444",
        surface: "daemon",
        operation: "disable",
        properties: {},
      }),
      operationEvent({
        event_id: "55555555-5555-4555-8555-555555555555",
        surface: "daemon",
        operation: "status",
        properties: {},
      }),
      operationEvent({
        event_id: "66666666-6666-4666-8666-666666666666",
        surface: "daemon",
        operation: "run_once",
        properties: daemonRunProperties(),
      }),
      runtimeEvent({
        event_id: "77777777-7777-4777-8777-777777777777",
        operation: "ready",
        properties: daemonRunProperties(),
      }),
      runtimeEvent({
        event_id: "88888888-8888-4888-8888-888888888888",
        operation: "stopped",
      }),
      runtimeEvent({
        event_id: "99999999-9999-4999-8999-999999999999",
        operation: "recovered",
      }),
      runtimeEvent({
        event_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        operation: "failed",
        outcome: "failure",
      }),
      runtimeEvent({
        event_id: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
        operation: "cycle",
        properties: daemonCycleProperties(),
      }),
      runtimeEvent({ event_id: "cccccccc-cccc-4ccc-8ccc-cccccccccccc" }),
    ];
    const plan = await buildTelemetryIngestPlan(v1Batch(events), INGEST_OPTIONS);
    expect(plan.rows.map((row) => row.activity_class)).toEqual([
      "setup", "setup", "status", "automatic", "operational", "operational", "operational",
      "operational", "operational", "liveness",
    ]);

    await expect(buildTelemetryIngestPlan(v1Batch([
      operationEvent({
        surface: "daemon",
        operation: "run_once",
        properties: { start_mode: "autostart", supervisor: "process" },
      }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: "invalid_start_mode" });
    await expect(buildTelemetryIngestPlan(v1Batch([
      runtimeEvent({ properties: {} }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: "invalid_start_mode" });
  });

  test("accepts exact MCP operation/runtime shapes and keeps malformed calls passive", async () => {
    const plan = await buildTelemetryIngestPlan(v1Batch([
      mcpOperationEvent("search", {
        method: "tools_call",
        tool: "search",
        result_count_bucket: "2-5",
        zero_result: false,
        result_truncated: false,
      }),
      mcpOperationEvent("unknown", {
        method: "unknown",
        tool: "unknown",
        error_layer: "json_rpc",
        error_class: "method_not_found",
      }, {
        event_id: "44444444-4444-4444-8444-444444444444",
        outcome: "failure",
      }),
      mcpOperationEvent("missing", {
        method: "missing",
        tool: "missing",
        error_layer: "input",
        error_class: "invalid_json",
      }, {
        event_id: "55555555-5555-4555-8555-555555555555",
        outcome: "failure",
      }),
      mcpOperationEvent("missing", {
        method: "tools_call",
        tool: "missing",
        error_layer: "json_rpc",
        error_class: "missing_tool",
      }, {
        event_id: "88888888-8888-4888-8888-888888888888",
        outcome: "failure",
      }),
      mcpOperationEvent("status", {
        method: "tools_call",
        tool: "status",
      }, {
        event_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
      }),
      mcpOperationEvent("pro_status", {
        method: "tools_call",
        tool: "pro_status",
      }, {
        event_id: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
      }),
      mcpRuntimeEvent("initialized", mcpRuntimeProperties(true), {
        event_id: "66666666-6666-4666-8666-666666666666",
      }),
      mcpRuntimeEvent("stopped", {
        ...mcpRuntimeProperties(true),
        stop_reason: "eof",
      }, {
        event_id: "77777777-7777-4777-8777-777777777777",
      }),
    ]), INGEST_OPTIONS);
    expect(plan.rows.map((row) => row.activity_class)).toEqual([
      "product_value", "operational", "operational", "operational", "status", "operational",
      "operational", "operational",
    ]);

    await expect(buildTelemetryIngestPlan(v1Batch([
      mcpOperationEvent("tool_call", { method: "tools_call", tool: "search" }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: "invalid_operation" });
    await expect(buildTelemetryIngestPlan(v1Batch([
      mcpOperationEvent("search", { method: "tools_call", tool: "status" }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: "mcp_tool_operation_mismatch" });
    const missingCount = mcpRuntimeProperties(true);
    delete missingCount.telemetry_dropped_count_bucket;
    await expect(buildTelemetryIngestPlan(v1Batch([
      mcpRuntimeEvent("initialized", missingCount),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({
      code: "invalid_telemetry_dropped_count_bucket",
    });
  });

  test("accepts exact Pro lifecycle/materialization/status/blame shapes", async () => {
    const materialization = {
      materialization_commit: "committed",
      materialization_freshness: "current",
      materialization_result: "completed",
      helper_connection_outcome: "connected",
    };
    const plan = await buildTelemetryIngestPlan(v1Batch([
      proOperationEvent("lifecycle", {
        lifecycle_operation: "setup",
        access_state: "active",
        helper_connection_outcome: "connected",
        reconcile_outcome: "installed",
      }),
      proOperationEvent("materialize", materialization, {
        event_id: "44444444-4444-4444-8444-444444444444",
      }),
      proOperationEvent("status", {
        status_surface: "mcp",
        access_state: "offline_grace",
        helper_connection_outcome: "connected",
      }, {
        event_id: "55555555-5555-4555-8555-555555555555",
      }),
      proOperationEvent("blame", {
        blame_target_kind: "pull_request",
        blame_surface: "mcp",
        blame_result_count_bucket: "2-5",
        blame_has_more: true,
      }, {
        event_id: "66666666-6666-4666-8666-666666666666",
      }),
    ]), INGEST_OPTIONS);
    expect(plan.rows.map((row) => row.activity_class)).toEqual([
      "operational", "automatic", "operational", "product_value",
    ]);

    await expect(buildTelemetryIngestPlan(v1Batch([
      proOperationEvent("status", {
        status_surface: "mcp",
        helper_connection_outcome: "connected",
        query_kind: "status",
      }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: "unknown_pro_status_property" });
    await expect(buildTelemetryIngestPlan(v1Batch([
      proOperationEvent("blame", {
        blame_target_kind: "issue",
        blame_surface: "cli",
        blame_result_count_bucket: "0",
        blame_has_more: false,
      }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: "invalid_blame_target_kind" });
    await expect(buildTelemetryIngestPlan(v1Batch([
      proOperationEvent("status", {
        status_surface: "cli",
        helper_connection_outcome: "connected",
        query_kind: "status",
      }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: "unknown_pro_status_property" });
    await expect(buildTelemetryIngestPlan(v1Batch([
      proOperationEvent("lifecycle", {
        lifecycle_operation: "status",
        access_state: "free",
        helper_connection_outcome: "connected",
        reconcile_outcome: "current",
      }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: "invalid_access_state" });
  });

  test("drops only retired 1.1.0 Pro materialization fields and preserves the commit enum", async () => {
    const retired = {
      materialization_mode: "incremental",
      materialization_batch_count_bucket: "1",
      materialization_input_count_bucket: "6-20",
      materialization_output_count_bucket: "6-20",
      materialization_lag_bucket: "0",
    };
    const batch = v1Batch([proOperationEvent("materialize", {
      ...retired,
      materialization_commit: "committed",
      materialization_freshness: "current",
      materialization_result: "completed",
      helper_connection_outcome: "connected",
    })]);
    batch.app_version = "1.1.0";

    const plan = await buildTelemetryIngestPlan(batch, INGEST_OPTIONS);

    expect(plan.rows[0]?.properties).toMatchObject({
      helper_connection_outcome: "connected",
      materialization_commit: "committed",
      materialization_freshness: "current",
      materialization_result: "completed",
    });
    for (const key of Object.keys(retired)) {
      expect(plan.rows[0]?.properties).not.toHaveProperty(key);
    }

    for (const commit of ["not_committed", "no_op", "committed", "replayed", "mixed"]) {
      const commitBatch = v1Batch([proOperationEvent("materialize", {
        materialization_commit: commit,
        materialization_freshness: "current",
        materialization_result: "completed",
        helper_connection_outcome: "connected",
      })]);
      commitBatch.app_version = "1.1.0";
      const commitPlan = await buildTelemetryIngestPlan(commitBatch, INGEST_OPTIONS);
      expect(commitPlan.rows[0]?.properties.materialization_commit).toBe(commit);
    }
  });

  test("drops retired 1.1.0 materialization fields flattened into lifecycle terminals", async () => {
    const batch = v1Batch([proOperationEvent("lifecycle", {
      lifecycle_operation: "setup",
      access_state: "active",
      helper_connection_outcome: "connected",
      reconcile_outcome: "installed",
      materialization_mode: "incremental",
      materialization_commit: "committed",
      materialization_freshness: "current",
      materialization_result: "completed",
      materialization_batch_count_bucket: "1",
      materialization_input_count_bucket: "6-20",
      materialization_output_count_bucket: "6-20",
      materialization_lag_bucket: "0",
    })]);
    batch.app_version = "1.1.0";

    const plan = await buildTelemetryIngestPlan(batch, INGEST_OPTIONS);

    expect(plan.rows[0]?.properties).toMatchObject({
      lifecycle_operation: "setup",
      materialization_commit: "committed",
      materialization_freshness: "current",
      materialization_result: "completed",
    });
    for (const key of [
      "materialization_mode",
      "materialization_batch_count_bucket",
      "materialization_input_count_bucket",
      "materialization_output_count_bucket",
      "materialization_lag_bucket",
    ]) expect(plan.rows[0]?.properties).not.toHaveProperty(key);
  });

  test.each(["1.1.0-rc.1", "1.1.1"])(
    "rejects retired Pro materialization fields for app version %s",
    async (appVersion) => {
      const batch = v1Batch([proOperationEvent("materialize", {
        materialization_commit: "committed",
        materialization_freshness: "current",
        materialization_result: "completed",
        helper_connection_outcome: "connected",
        materialization_mode: "incremental",
      })]);
      batch.app_version = appVersion;

      await expect(buildTelemetryIngestPlan(batch, INGEST_OPTIONS)).rejects.toMatchObject({
        code: "unknown_pro_materialization_property",
        status: 422,
      });
    },
  );

  test.each([
    ["materialization_mode", { unbounded: true }],
    ["materialization_batch_count_bucket", "unexpected"],
    ["materialization_input_count_bucket", "unexpected"],
    ["materialization_output_count_bucket", "unexpected"],
    ["materialization_lag_bucket", "unexpected"],
  ])("rejects malformed retired 1.1.0 Pro materialization field %s", async (key, value) => {
    const batch = v1Batch([proOperationEvent("materialize", {
      materialization_commit: "committed",
      materialization_freshness: "current",
      materialization_result: "completed",
      helper_connection_outcome: "connected",
      [key]: value,
    })]);
    batch.app_version = "1.1.0";

    await expect(buildTelemetryIngestPlan(batch, INGEST_OPTIONS)).rejects.toMatchObject({
      code: `invalid_${key}`,
      status: 422,
    });
  });

  test("enforces shared capability/install-manager coupling and rejects unused top-level failures", async () => {
    const capabilities = {
      capability_snapshot_schema: 1,
      available_parallelism_bucket: "5-8",
      host_memory_bucket: "16-32gb",
      cpu_vector_tier: "avx2",
      acceleration_candidate: "not_detected",
    };
    await expect(buildTelemetryIngestPlan(v1Batch([
      operationEvent({
        install_attempt_id: INSTALL_ATTEMPT_ID,
        properties: {
          ...(operationEvent().properties as Record<string, unknown>),
          ...capabilities,
          install_manager: "ctx-hosted-installer",
        },
      }),
    ]), INGEST_OPTIONS)).resolves.toMatchObject({ rows: [{ schema_version: 1 }] });

    await expect(buildTelemetryIngestPlan(v1Batch([
      operationEvent({
        properties: {
          ...(operationEvent().properties as Record<string, unknown>),
          capability_snapshot_schema: 1,
        },
      }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: "incomplete_capability_snapshot" });
    await expect(buildTelemetryIngestPlan(v1Batch([
      operationEvent({
        properties: {
          ...(operationEvent().properties as Record<string, unknown>),
          install_manager: "ctx-hosted-installer",
        },
      }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: "inconsistent_install_manager" });
    await expect(buildTelemetryIngestPlan(v1Batch([
      operationEvent({ install_attempt_id: INSTALL_ATTEMPT_ID }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: "inconsistent_install_manager" });
    await expect(buildTelemetryIngestPlan(v1Batch([
      operationEvent({ error_code: "internal" }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: "unknown_event_field" });
  });

  test("accepts canonical non-v4 envelope identities and rejects malformed or nil UUIDs", async () => {
    const compatible = v1Batch([operationEvent()]);
    compatible.client_profile_id = "018f1f2e-7b3c-7abc-8def-0123456789ab";
    compatible.data_root_id = "11111111-1111-1111-8111-111111111111";
    await expect(buildTelemetryIngestPlan(compatible, INGEST_OPTIONS)).resolves.toMatchObject({
      rows: [{ schema_version: 1 }],
    });

    for (const clientProfileId of [
      "not-a-uuid",
      "00000000-0000-0000-0000-000000000000",
      "018F1F2E-7B3C-7ABC-8DEF-0123456789AB",
    ]) {
      await expect(buildTelemetryIngestPlan({
        ...v1Batch([operationEvent()]),
        client_profile_id: clientProfileId,
      }, INGEST_OPTIONS)).rejects.toMatchObject({ code: "invalid_client_profile_id" });
    }
    for (const dataRootId of ["bad", "00000000-0000-0000-0000-000000000000"]) {
      await expect(buildTelemetryIngestPlan({
        ...v1Batch([operationEvent()]),
        data_root_id: dataRootId,
      }, INGEST_OPTIONS)).rejects.toMatchObject({ code: "invalid_data_root_id" });
    }
  });

  test("enforces the exact auto-upgrade and deprecated-control sidecars", async () => {
    const validStates = [
      ["auto_disabled", false, false],
      ["ci", false, false],
      ["background_child", false, false],
      ["not_due", false, false],
      ["marker_invalid", true, false],
      ["current_exe_error", true, false],
      ["spawned", true, true],
      ["spawn_failed", true, false],
    ] as const;
    for (const [status, due, spawned] of validStates) {
      await expect(buildTelemetryIngestPlan(v1Batch([
        operationEvent({
          properties: {
            ...(operationEvent().properties as Record<string, unknown>),
            auto_upgrade_probe: true,
            auto_upgrade_due: due,
            auto_upgrade_spawned: spawned,
            auto_upgrade_spawn_status: status,
            auto_upgrade_channel: "stable",
          },
        }),
      ]), INGEST_OPTIONS)).resolves.toMatchObject({ rows: [{ activity_class: "product_value" }] });
    }
    await expect(buildTelemetryIngestPlan(v1Batch([
      operationEvent({
        properties: {
          ...(operationEvent().properties as Record<string, unknown>),
          auto_upgrade_probe: true,
        },
      }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: "incomplete_auto_upgrade" });
    await expect(buildTelemetryIngestPlan(v1Batch([
      operationEvent({
        properties: {
          ...(operationEvent().properties as Record<string, unknown>),
          auto_upgrade_probe: false,
          auto_upgrade_due: true,
          auto_upgrade_spawned: true,
          auto_upgrade_spawn_status: "spawned",
          auto_upgrade_channel: "stable",
        },
      }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: "invalid_auto_upgrade_probe" });
    await expect(buildTelemetryIngestPlan(v1Batch([
      operationEvent({
        properties: {
          ...(operationEvent().properties as Record<string, unknown>),
          auto_upgrade_probe: true,
          auto_upgrade_due: false,
          auto_upgrade_spawned: true,
          auto_upgrade_spawn_status: "spawned",
          auto_upgrade_channel: "stable",
        },
      }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: "invalid_auto_upgrade_state" });
    await expect(buildTelemetryIngestPlan(v1Batch([
      operationEvent({
        properties: {
          ...(operationEvent().properties as Record<string, unknown>),
          auto_upgrade_probe: true,
          auto_upgrade_due: false,
          auto_upgrade_spawned: false,
          auto_upgrade_spawn_status: "env_disabled",
          auto_upgrade_channel: "stable",
        },
      }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: "invalid_auto_upgrade_spawn_status" });
    await expect(buildTelemetryIngestPlan(v1Batch([
      operationEvent({
        outcome: "failure",
        properties: {
          ...(operationEvent().properties as Record<string, unknown>),
          auto_upgrade_probe: true,
          auto_upgrade_due: false,
          auto_upgrade_spawned: false,
          auto_upgrade_spawn_status: "not_due",
          auto_upgrade_channel: "stable",
        },
      }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: "unexpected_auto_upgrade" });
    await expect(buildTelemetryIngestPlan(v1Batch([
      operationEvent({
        operation: "status",
        properties: {
          output: "human",
          auto_upgrade_probe: true,
          auto_upgrade_due: false,
          auto_upgrade_spawned: false,
          auto_upgrade_spawn_status: "not_due",
          auto_upgrade_channel: "stable",
        },
      }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: "unexpected_auto_upgrade" });
    await expect(buildTelemetryIngestPlan(v1Batch([
      operationEvent({
        properties: {
          ...(operationEvent().properties as Record<string, unknown>),
          deprecated_daemon_control: false,
        },
      }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: "invalid_deprecated_daemon_control" });
    await expect(buildTelemetryIngestPlan(v1Batch([
      operationEvent({
        properties: {
          ...(operationEvent().properties as Record<string, unknown>),
          deprecated_upgrade_control: true,
        },
      }),
    ]), INGEST_OPTIONS)).resolves.toMatchObject({ rows: [{ schema_version: 1 }] });
  });

  test("classifies foreground discriminator variants instead of only operation names", async () => {
    const plan = await buildTelemetryIngestPlan(v1Batch([
      operationEvent({
        operation: "integration",
        properties: { output: "human", integration_action: "status" },
      }),
      operationEvent({
        event_id: "44444444-4444-4444-8444-444444444444",
        operation: "index",
        properties: { output: "human", index_operation: "status" },
      }),
      operationEvent({
        event_id: "55555555-5555-4555-8555-555555555555",
        operation: "upgrade",
        properties: {
          output: "human",
          upgrade_mode: "manual",
          upgrade_operation: "apply",
          dry_run: false,
          upgrade_status: "applied",
          upgrade_applied: true,
          upgrade_scheduled: false,
          update_available: false,
          update_was_available: true,
          upgrade_attempt_id: "ua_manual_apply",
          managed_install: true,
          self_upgrade_allowed: true,
          auto_upgrade_allowed: true,
          upgrade_warning_count_bucket: "0",
          upgrade_channel: "stable",
        },
      }),
      operationEvent({
        event_id: "66666666-6666-4666-8666-666666666666",
        operation: "upgrade",
        properties: {
          output: "human",
          upgrade_mode: "manual",
          upgrade_operation: "status",
          dry_run: false,
          upgrade_status: "status_checked",
          upgrade_applied: false,
          upgrade_scheduled: false,
          update_available: false,
        },
      }),
      operationEvent({
        event_id: "77777777-7777-4777-8777-777777777777",
        operation: "upgrade",
        properties: {
          upgrade_mode: "auto",
          upgrade_operation: "apply",
          dry_run: false,
          upgrade_status: "applied",
          upgrade_applied: true,
          upgrade_scheduled: false,
          update_available: false,
          update_was_available: true,
          upgrade_attempt_id: "ua_auto_apply",
          managed_install: true,
          self_upgrade_allowed: true,
          auto_upgrade_allowed: true,
          upgrade_warning_count_bucket: "0",
          upgrade_channel: "stable",
        },
      }),
    ]), INGEST_OPTIONS);
    expect(plan.rows.map((row) => row.activity_class)).toEqual([
      "status", "status", "product_activity", "status", "automatic",
    ]);
  });

  test("accepts the released index mode operation telemetry", async () => {
    const batch = v1Batch([
      operationEvent({
        operation: "index",
        properties: { output: "human", index_operation: "mode" },
      }),
    ]);
    batch.app_version = "1.2.4";
    await expect(buildTelemetryIngestPlan(batch, INGEST_OPTIONS))
      .resolves.toMatchObject({ rows: [{ app_version: "1.2.4", schema_version: 1 }] });
  });

  test("uses domain-separated HMAC identities and persists the key version", async () => {
    const sameIdentityBatch = v1Batch([operationEvent()]);
    sameIdentityBatch.data_root_id = CLIENT_PROFILE_ID;
    const { rows: [row] } = await buildTelemetryIngestPlan(sameIdentityBatch, INGEST_OPTIONS);
    const expectedProfile = await hmacSha256Hex(
      HMAC_KEY,
      "ctx.telemetry.client-profile.v1",
      CLIENT_PROFILE_ID,
    );

    expect(row.client_profile_id_hash).toBe(expectedProfile);
    expect(row.data_root_id_hash).not.toBe(expectedProfile);
    expect(row.identity_key_version).toBe(7);
    expect(JSON.stringify(row)).not.toContain(CLIENT_PROFILE_ID);
    expect(JSON.stringify(row)).not.toContain(INSTALL_ATTEMPT_ID);
    expect(row.payload_fingerprint).toMatch(/^[a-f0-9]{64}$/u);
  });

  test("keys batch fingerprints over canonical payloads with environment and schema domains", async () => {
    const event = operationEvent();
    const payload = v1Batch([event]);
    const { rows: [production] } = await buildTelemetryIngestPlan(payload, INGEST_OPTIONS);
    const { rows: [staging] } = await buildTelemetryIngestPlan(payload, {
      ...INGEST_OPTIONS,
      analyticsEnvironment: "staging",
    });
    const { rows: [rotatedKey] } = await buildTelemetryIngestPlan(payload, {
      ...INGEST_OPTIONS,
      identityHmacKey: "rotated-telemetry-test-hmac-key-with-32-bytes",
      identityKeyVersion: 8,
    });
    const envelope = {
      client_profile_id: CLIENT_PROFILE_ID,
      data_root_id: DATA_ROOT_ID,
      app_version: "0.26.0",
      os: "linux",
      arch: "x86_64",
    };
    const normalizedPayload = {
      envelope,
      event: {
        ...event,
        occurred_at: new Date(OCCURRED_AT).toISOString(),
      },
    };
    const canonicalPayload = canonicalJson(normalizedPayload);
    const priorUnkeyedFingerprint = await sha256Hex(canonicalJson({
      analytics_environment: "production",
      payload: normalizedPayload,
    }));

    expect(production.payload_fingerprint).toBe(await hmacSha256Hex(
      HMAC_KEY,
      "ctx.telemetry.payload-fingerprint.production.telemetry-event.v1",
      canonicalPayload,
    ));
    expect(production.payload_fingerprint).not.toBe(priorUnkeyedFingerprint);
    expect(production.payload_fingerprint).not.toBe(await hmacSha256Hex(
      HMAC_KEY,
      "ctx.telemetry.payload-fingerprint.production.install-stage.v1",
      canonicalPayload,
    ));
    expect(staging.payload_fingerprint).not.toBe(production.payload_fingerprint);
    expect(rotatedKey.payload_fingerprint).not.toBe(production.payload_fingerprint);
    expect(JSON.stringify(production)).not.toContain(CLIENT_PROFILE_ID);
    expect(JSON.stringify(production)).not.toContain(DATA_ROOT_ID);
  });

  test("normalizes key ordering, deduplicates identical IDs, and rejects collisions", async () => {
    const event = operationEvent();
    const reordered = {
      properties: Object.fromEntries(
        Object.entries(event.properties as Record<string, unknown>).reverse(),
      ),
      duration_bucket: event.duration_bucket,
      outcome: event.outcome,
      operation: event.operation,
      surface: event.surface,
      install_attempt_id: event.install_attempt_id,
      occurred_at: event.occurred_at,
      event_version: event.event_version,
      event_name: event.event_name,
      event_id: event.event_id,
    };
    const first = await buildTelemetryIngestPlan(v1Batch([event]), INGEST_OPTIONS);
    const second = await buildTelemetryIngestPlan(v1Batch([reordered]), INGEST_OPTIONS);
    const duplicate = await buildTelemetryIngestPlan(v1Batch([event, reordered]), INGEST_OPTIONS);

    expect(second.rows[0].payload_fingerprint).toBe(first.rows[0].payload_fingerprint);
    expect(duplicate.rows).toHaveLength(1);

    await expect(buildTelemetryIngestPlan(v1Batch([
      event,
      operationEvent({
        properties: {
          ...(event.properties as Record<string, unknown>),
          zero_result: true,
        },
      }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({
      code: "event_id_collision",
      status: 409,
    });
  });

  test("accepts the exact install_stage v1 contract and detailed legacy route fixture", async () => {
    const canonical = await buildInstallStageRow(installStage(), INGEST_OPTIONS);
    const retry = await buildInstallStageRow(installStage(), {
      ...INGEST_OPTIONS,
      now: () => new Date("2026-07-22T18:35:00.000Z"),
    });
    const legacy = await buildInstallStageRow({
      install_attempt_id: INSTALL_ATTEMPT_ID,
      stage: "artifact_download_completed",
      status: "completed",
      error_kind: "",
      platform: "linux-x64",
      channel: "stable",
      version: "0.26.0",
    }, INGEST_OPTIONS);
    const legacyRetry = await buildInstallStageRow({
      install_attempt_id: INSTALL_ATTEMPT_ID,
      stage: "artifact_download_completed",
      status: "completed",
      error_kind: "",
      platform: "linux-x64",
      channel: "stable",
      version: "0.26.0",
    }, {
      ...INGEST_OPTIONS,
      now: () => new Date("2026-07-22T19:35:00.000Z"),
    });
    const legacyFreeBsd = await buildInstallStageRow({
      install_attempt_id: INSTALL_ATTEMPT_ID,
      stage: "artifact_download_completed",
      status: "completed",
      error_kind: "",
      platform: "freebsd-x64",
      channel: "stable",
      version: "0.25.0",
    }, INGEST_OPTIONS);

    expect(canonical).toMatchObject({
      analytics_environment: "production",
      arch: "x64",
      event_name: "install_stage",
      event_version: 1,
      platform: "linux",
      schema_version: 1,
      script_family: "posix",
      stage: "installer",
      status: "started",
      traffic_class: "unclassified_public",
    });
    expect(canonical.event_id).toMatch(/^[0-9a-f-]{36}$/u);
    expect(retry.event_id).toBe(canonical.event_id);
    expect(retry.payload_fingerprint).toBe(canonical.payload_fingerprint);
    expect(JSON.stringify(canonical)).not.toContain(INSTALL_ATTEMPT_ID);
    const staging = await buildInstallStageRow(installStage(), {
      ...INGEST_OPTIONS,
      analyticsEnvironment: "staging",
    });
    expect(staging).toMatchObject({
      analytics_environment: "staging",
      traffic_class: "synthetic",
    });
    const canonicalPayload = canonicalJson(installStage());
    const priorUnkeyedFingerprint = await sha256Hex(canonicalJson({
      analytics_environment: "production",
      payload: installStage(),
    }));
    expect(canonical.payload_fingerprint).toBe(await hmacSha256Hex(
      HMAC_KEY,
      "ctx.telemetry.payload-fingerprint.production.install-stage.v1",
      canonicalPayload,
    ));
    expect(canonical.payload_fingerprint).not.toBe(priorUnkeyedFingerprint);
    expect(canonical.event_id).not.toBe(uuidV4FromFingerprint(priorUnkeyedFingerprint));
    expect(staging.payload_fingerprint).not.toBe(canonical.payload_fingerprint);
    expect(staging.event_id).not.toBe(canonical.event_id);
    expect(legacy).toMatchObject({
      event_name: null,
      event_version: null,
      received_at: null,
      schema_version: null,
      stage: "artifact_download_completed",
      traffic_class: null,
    });
    expect(legacy.event_id).toMatch(/^[0-9a-f-]{36}$/u);
    expect(legacy.payload_fingerprint).toMatch(/^[0-9a-f]{64}$/u);
    expect(legacyRetry.event_id).toBe(legacy.event_id);
    expect(legacyRetry.payload_fingerprint).toBe(legacy.payload_fingerprint);
    expect(legacyFreeBsd).toMatchObject({
      event_name: null,
      platform: "freebsd-x64",
      schema_version: null,
    });
  });

  test.each([
    ["installer", "started"],
    ["artifact_download", "completed"],
    ["binary_install", "completed"],
    ["skill_install", "skipped"],
    ["setup", "failed"],
    ["uninstall", "completed"],
  ])("accepts canonical install pair %s/%s", async (stage, status) => {
    await expect(buildInstallStageRow(installStage({ stage, status }), INGEST_OPTIONS)).resolves.toMatchObject({
      stage,
      status,
    });
  });

  test("keeps FreeBSD out of typed installer events", async () => {
    await expect(buildInstallStageRow(installStage({ platform: "freebsd" }), INGEST_OPTIONS)).rejects.toMatchObject({
      code: "invalid_install_platform",
      status: 422,
    });
  });

  test.each([
    ["event", () => v1Batch([operationEvent({ command: "ctx search secret" })])],
    ["properties", () => v1Batch([operationEvent({ properties: { output: "human", query: "secret" } })])],
    ["batch", () => ({ ...v1Batch([operationEvent()]), repository: "private/repo" })],
  ])("rejects unknown content-bearing %s fields", async (_name, fixture) => {
    await expect(buildTelemetryIngestPlan(fixture(), INGEST_OPTIONS)).rejects.toMatchObject({ status: 422 });
  });

  test.each([
    ["content", "raw transcript"],
    ["path", "/private/history.jsonl"],
    ["source_id", "source-secret"],
    ["session_id", "session-secret"],
    ["record_id", "record-secret"],
    ["locator", "opaque-locator"],
    ["cursor", "opaque-cursor"],
    ["provider_key", "provider-secret"],
    ["source_format", "private-format"],
    ["ingestion_mode", "incremental"],
    ["ingestion_engine", "nativepath"],
    ["rewrite_reason", "private reason"],
    ["error", "raw error"],
    ["error_message", "raw stack trace"],
    ["duration_ms", 1234],
    ["bytes", 123456],
  ])("rejects privacy-denied provider refresh property %s", async (field, value) => {
    await expect(buildTelemetryIngestPlan(v1Batch([
      providerRefreshEvent({ properties: providerRefreshProperties({ [field]: value }) }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({
      code: "unknown_provider_refresh_property",
      status: 422,
    });
  });

  test("rejects invalid enums, UUIDs, timestamps, and raw error fields", async () => {
    await expect(buildTelemetryIngestPlan(v1Batch([
      operationEvent({ outcome: "ok" }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: "invalid_outcome", status: 422 });
    await expect(buildTelemetryIngestPlan({
      ...v1Batch([operationEvent()]),
      client_profile_id: "not-a-uuid",
    }, INGEST_OPTIONS)).rejects.toMatchObject({ code: "invalid_client_profile_id", status: 422 });
    await expect(buildTelemetryIngestPlan(v1Batch([
      operationEvent({ occurred_at: "2026-07-22T18:34:01Z" }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: "invalid_occurred_at_precision", status: 422 });
    await expect(buildTelemetryIngestPlan(v1Batch([
      operationEvent({ occurred_at: "2026-02-30T18:34:00Z" }),
    ]), {
      ...INGEST_OPTIONS,
      now: () => new Date("2026-03-02T18:35:00.000Z"),
    })).rejects.toMatchObject({ code: "invalid_occurred_at", status: 422 });
    await expect(buildTelemetryIngestPlan(v1Batch([
      operationEvent({ error: "raw stack trace" }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: "unknown_event_field", status: 422 });
  });

  test("rejects invalid canonical install fields and stage/status pairs", async () => {
    await expect(buildInstallStageRow(installStage({ path: "/tmp/ctx" }), INGEST_OPTIONS))
      .rejects.toMatchObject({ code: "unknown_install_stage_field", status: 422 });
    await expect(buildInstallStageRow(installStage({ stage: "binary_installed" }), INGEST_OPTIONS))
      .rejects.toMatchObject({ code: "invalid_install_stage", status: 422 });
    await expect(buildInstallStageRow(installStage({ stage: "binary_install", status: "failed" }), INGEST_OPTIONS))
      .rejects.toMatchObject({ code: "invalid_install_stage_status", status: 422 });
    const missingArch = installStage();
    delete missingArch.arch;
    await expect(buildInstallStageRow(missingArch, INGEST_OPTIONS))
      .rejects.toMatchObject({ code: "invalid_install_arch", status: 422 });
  });

  test("enforces batch, event-size, and depth bounds", async () => {
    const tooMany = Array.from({ length: 51 }, (_, index) => operationEvent({
      event_id: `33333333-3333-4333-8${index.toString().padStart(3, "0")}-333333333333`,
    }));
    await expect(buildTelemetryIngestPlan(
      v1Batch(tooMany.slice(0, 50)),
      INGEST_OPTIONS,
    )).resolves.toMatchObject({ rows: { length: 50 } });
    await expect(buildTelemetryIngestPlan(v1Batch(tooMany), INGEST_OPTIONS))
      .rejects.toMatchObject({ code: "too_many_events", status: 413 });
    await expect(buildTelemetryIngestPlan(v1Batch([
      operationEvent({ ignored: "x".repeat(9 * 1024) }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: "event_too_large", status: 413 });
    await expect(buildTelemetryIngestPlan({
      ...v1Batch([operationEvent()]),
      nested: { one: { two: { three: { four: { five: { six: { seven: true } } } } } } },
    }, INGEST_OPTIONS)).rejects.toMatchObject({ code: "payload_too_deep", status: 422 });
  });

  test("accepts final large-store buckets and isolates pre-convergence duration compatibility", async () => {
    const plan = await buildTelemetryIngestPlan(v1Batch([
      providerRefreshEvent({
        duration_bucket: "gte_1h",
        properties: {
          ...(providerRefreshEvent().properties as Record<string, unknown>),
          sessions_bucket: "1m+",
          events_bucket: "100k-1m",
          bytes_bucket: "100gb+",
        },
      }),
    ]), INGEST_OPTIONS);

    expect(plan.rows.map((row) => row.duration_bucket)).toEqual(["gte_1h"]);
    await expect(buildTelemetryIngestPlan(v1Batch([
      providerRefreshEvent({ duration_bucket: "gte_30s" }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({
      code: "invalid_duration_bucket",
      status: 422,
    });
  });
});
