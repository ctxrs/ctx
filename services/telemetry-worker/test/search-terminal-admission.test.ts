import { describe, expect, test } from "vitest";

import { buildTelemetryIngestPlan } from "../src/telemetry-ingest";
import { SEARCH_TERMINAL_PROPERTY_KEYS } from "../src/telemetry-contract";
import {
  INGEST_OPTIONS,
  batchEventId,
  mcpOperationEvent,
  operationEvent,
  proOperationEvent,
  v1Batch,
} from "./worker-test-fixtures";

const COUNT_BUCKET_FIELDS = [
  "search_retrieval_round_count_bucket",
  "search_query_execution_count_bucket",
  "search_candidate_rows_total_bucket",
  "search_candidate_records_decoded_bucket",
  "search_final_candidate_pool_bucket",
  "search_candidate_session_count_bucket",
  "search_candidate_literal_root_family_count_bucket",
  "search_provider_copy_candidate_count_bucket",
] as const;
const STOP_REASONS = ["decisive", "exhausted", "candidate_cap", "fixed_pool"] as const;
const FAILURE_PHASES = [
  "preparation",
  "refresh",
  "generation_open",
  "query_preparation",
  "semantic_retrieval",
  "index_query_decode",
  "result_projection",
  "render",
  "output",
] as const;

function cliSearchEvent(
  searchProperties: Record<string, unknown>,
  index = 0,
): Record<string, unknown> {
  const base = operationEvent();
  return {
    ...base,
    event_id: batchEventId(index),
    properties: {
      ...(base.properties as Record<string, unknown>),
      ...searchProperties,
    },
  };
}

function mcpSearchEvent(
  searchProperties: Record<string, unknown>,
  index = 0,
): Record<string, unknown> {
  return mcpOperationEvent("search", {
    method: "tools_call",
    tool: "search",
    ...searchProperties,
  }, { event_id: batchEventId(index) });
}

async function admitCli(searchProperties: Record<string, unknown>) {
  return await buildTelemetryIngestPlan(
    v1Batch([cliSearchEvent(searchProperties)]),
    INGEST_OPTIONS,
  );
}

async function admitMcp(searchProperties: Record<string, unknown>) {
  return await buildTelemetryIngestPlan(
    v1Batch([mcpSearchEvent(searchProperties)]),
    INGEST_OPTIONS,
  );
}

const SEARCH_ADMITTERS = [admitCli, admitMcp] as const;
const SEARCH_EVENT_FACTORIES = [cliSearchEvent, mcpSearchEvent] as const;

describe("Search terminal-health admission", () => {
  test("preserves old-client missingness for CLI and MCP Search events", async () => {
    const plan = await buildTelemetryIngestPlan(v1Batch([
      operationEvent({ event_id: batchEventId(0) }),
      mcpOperationEvent("search", {
        method: "tools_call",
        tool: "search",
        result_count_bucket: "0",
        zero_result: true,
      }, { event_id: batchEventId(1) }),
    ]), INGEST_OPTIONS);

    expect(plan.rows).toHaveLength(2);
    for (const row of plan.rows) {
      for (const key of SEARCH_TERMINAL_PROPERTY_KEYS) {
        expect(row.properties).not.toHaveProperty(key);
      }
    }
  });

  test("accepts the same sparse CLI and MCP facts and every zero bucket", async () => {
    const zeroBuckets = Object.fromEntries(COUNT_BUCKET_FIELDS.map((key) => [key, "0"]));
    const fullProperties = {
      search_output_duration_bucket: "lt_100ms",
      search_output_served: true,
      ...zeroBuckets,
      search_candidate_core_bytes_decoded_bucket: "0",
      search_candidate_pool_truncated: false,
      search_stop_reason: "decisive",
      search_failure_phase: "render",
      search_candidate_session_count_bucket: "6-20",
      search_largest_session_candidate_share_bucket: "51-75pct",
      search_literal_root_concentration_availability: "observed",
      search_candidate_literal_root_family_count_bucket: "2-5",
      search_literal_root_candidate_coverage_bucket: "76-99pct",
      search_largest_literal_root_candidate_share_bucket: "26-50pct",
      search_provider_copy_candidate_count_bucket: "1",
      search_provider_copy_candidate_share_bucket: "1-25pct",
      search_copy_cluster_availability: "not_constructed_v1",
      search_diversification_status: "applied",
      search_diversification_changed_final_top_n: true,
    };

    for (const admit of SEARCH_ADMITTERS) {
      const sparse = await admit({ search_output_served: true });
      expect(sparse.rows[0].properties).toMatchObject({ search_output_served: true });
      const full = await admit(fullProperties);
      expect(full.rows[0].properties).toMatchObject(fullProperties);
    }
  });

  test("accepts exact full CLI and MCP producer-shaped search events", async () => {
    const common = {
      refresh_duration_bucket: "lt_1s",
      search_refresh_source_count_bucket: "2-5",
      query_duration_bucket: "lt_100ms",
      search_backend_requested: "hybrid",
      search_backend_effective: "lexical",
      search_output_duration_bucket: "lt_100ms",
      search_output_served: false,
      search_retrieval_round_count_bucket: "1",
      search_query_execution_count_bucket: "1",
      search_candidate_rows_total_bucket: "101-1k",
      search_candidate_records_decoded_bucket: "21-100",
      search_candidate_core_bytes_decoded_bucket: "1gb-2gb",
      search_final_candidate_pool_bucket: "6-20",
      search_candidate_pool_truncated: true,
      search_stop_reason: "candidate_cap",
      search_failure_phase: "render",
    };
    const cli = { ...common, search_refresh_status: "existing_generation" };
    const mcp = { ...common, search_refresh_status: "daemon_unavailable" };

    await expect(admitCli(cli)).resolves.toMatchObject({ rows: [{ properties: cli }] });
    await expect(admitMcp(mcp)).resolves.toMatchObject({ rows: [{ properties: mcp }] });
  });

  test("accepts every frozen CLI and MCP enum without extra invariants", async () => {
    for (const eventFactory of SEARCH_EVENT_FACTORIES) {
      const events = [
        ...STOP_REASONS.map((search_stop_reason, index) =>
          eventFactory({ search_stop_reason }, index)
        ),
        ...FAILURE_PHASES.map((search_failure_phase, index) =>
          eventFactory({ search_failure_phase }, STOP_REASONS.length + index)
        ),
      ];

      const plan = await buildTelemetryIngestPlan(v1Batch(events), INGEST_OPTIONS);
      expect(plan.rows).toHaveLength(STOP_REASONS.length + FAILURE_PHASES.length);
    }
  });

  test.each([
    ["search_output_duration_bucket", "lt_42ms"],
    ["search_output_served", "false"],
    ["search_retrieval_round_count_bucket", 0],
    ["search_query_execution_count_bucket", "many"],
    ["search_retrieval_round_count_bucket", "1k+"],
    ["search_refresh_source_count_bucket", "1k+"],
    ["search_candidate_rows_total_bucket", "-1"],
    ["search_candidate_records_decoded_bucket", null],
    ["search_final_candidate_pool_bucket", true],
    ["search_candidate_core_bytes_decoded_bucket", "42kb"],
    ["search_candidate_core_bytes_decoded_bucket", "1gb+"],
    ["search_candidate_core_bytes_decoded_bucket", "1gb-10gb"],
    ["search_candidate_core_bytes_decoded_bucket", "10gb-100gb"],
    ["search_candidate_pool_truncated", 0],
    ["search_stop_reason", "limit"],
    ["search_failure_phase", "query"],
    ["search_candidate_session_count_bucket", "many"],
    ["search_largest_session_candidate_share_bucket", "half"],
    ["search_literal_root_concentration_availability", "unknown"],
    ["search_copy_cluster_availability", "constructed"],
    ["search_diversification_status", "changed"],
    ["search_diversification_changed_final_top_n", "true"],
  ])("rejects malformed %s", async (field, value) => {
    for (const admit of SEARCH_ADMITTERS) {
      await expect(admit({ [field]: value })).rejects.toMatchObject({
        code: `invalid_${field}`,
        status: 422,
      });
    }
  });

  test.each([
    ["search_output_duration_ms", 1],
    ["search_candidate_rows_total", 1],
    ["search_target", "/private/repository/file.ts"],
  ])("rejects unknown or unbounded field %s", async (field, value) => {
    await expect(admitCli({ [field]: value })).rejects.toMatchObject({
      code: "unknown_operation_property",
      status: 422,
    });
    await expect(admitMcp({ [field]: value })).rejects.toMatchObject({
      code: "unknown_mcp_operation_property",
      status: 422,
    });
  });

  test.each([
    ["search_output_duration_bucket", "unknown"],
    ["search_output_served", true],
    ["search_retrieval_round_count_bucket", "0"],
    ["search_query_execution_count_bucket", "1"],
    ["search_candidate_rows_total_bucket", "2-5"],
    ["search_candidate_records_decoded_bucket", "6-20"],
    ["search_final_candidate_pool_bucket", "21-100"],
    ["search_candidate_core_bytes_decoded_bucket", "lt_100kb"],
    ["search_candidate_pool_truncated", false],
    ["search_stop_reason", "fixed_pool"],
    ["search_failure_phase", "preparation"],
  ])("rejects Search-only field %s on non-Search operations", async (field, value) => {
    await expect(buildTelemetryIngestPlan(v1Batch([
      operationEvent({
        operation: "status",
        properties: { output: "human", [field]: value },
      }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({
      code: "unknown_operation_property",
      status: 422,
    });
    await expect(buildTelemetryIngestPlan(v1Batch([
      mcpOperationEvent("status", {
        method: "tools_call",
        tool: "status",
        [field]: value,
      }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({
      code: "unknown_mcp_operation_property",
      status: 422,
    });
  });

  test("does not widen Pro-host operation properties", async () => {
    await expect(buildTelemetryIngestPlan(v1Batch([
      proOperationEvent("query", { search_output_served: true }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({
      code: "unknown_pro_query_property",
      status: 422,
    });
  });

  test("enforces one complete concentration receipt and V1 availability semantics", async () => {
    const common = {
      search_final_candidate_pool_bucket: "6-20",
      search_candidate_session_count_bucket: "2-5",
      search_largest_session_candidate_share_bucket: "51-75pct",
      search_provider_copy_candidate_count_bucket: "1",
      search_provider_copy_candidate_share_bucket: "1-25pct",
      search_copy_cluster_availability: "not_constructed_v1",
      search_diversification_status: "applied",
    };
    const observed = {
      ...common,
      search_literal_root_concentration_availability: "observed",
      search_candidate_literal_root_family_count_bucket: "1",
      search_literal_root_candidate_coverage_bucket: "76-99pct",
      search_largest_literal_root_candidate_share_bucket: "51-75pct",
      search_diversification_changed_final_top_n: false,
    };
    const dense = {
      ...common,
      search_literal_root_concentration_availability: "not_observed_dense",
      search_diversification_status: "not_applicable",
    };
    for (const admit of SEARCH_ADMITTERS) {
      await expect(admit(observed)).resolves.toMatchObject({ rows: [{ properties: observed }] });
      await expect(admit(dense)).resolves.toMatchObject({ rows: [{ properties: dense }] });
      await expect(admit({
        search_candidate_session_count_bucket: "2-5",
      })).rejects.toMatchObject({ code: "incomplete_search_concentration", status: 422 });
      const { search_final_candidate_pool_bucket: _omitted, ...withoutPool } = observed;
      await expect(admit(withoutPool)).rejects.toMatchObject({
        code: "incomplete_search_concentration",
        status: 422,
      });
      await expect(admit({
        ...dense,
        search_candidate_literal_root_family_count_bucket: "1",
      })).rejects.toMatchObject({
        code: "inconsistent_search_literal_root_concentration",
        status: 422,
      });
      await expect(admit({
        ...dense,
        search_diversification_status: "indeterminate",
      })).rejects.toMatchObject({
        code: "inconsistent_search_diversification",
        status: 422,
      });
      await expect(admit({
        ...dense,
        search_diversification_changed_final_top_n: true,
      })).rejects.toMatchObject({
        code: "inconsistent_search_diversification",
        status: 422,
      });
      const {
        search_diversification_changed_final_top_n: _omittedDecision,
        ...withoutDecision
      } = observed;
      await expect(admit(withoutDecision)).rejects.toMatchObject({
        code: "inconsistent_search_diversification",
        status: 422,
      });
    }
  });

  test("admits independently observed output and failure facts", async () => {
    for (const admit of SEARCH_ADMITTERS) {
      for (const accepted of [
        { search_output_served: true },
        { search_output_served: false },
        { search_failure_phase: "output" },
        { search_failure_phase: "render" },
        { search_output_served: true, search_failure_phase: "render" },
        { search_output_served: false, search_failure_phase: "output" },
        { search_output_served: false, search_failure_phase: "render" },
        { search_output_served: true, search_failure_phase: "output" },
      ]) {
        await expect(admit(accepted)).resolves.toMatchObject({
          rows: [{ properties: accepted }],
        });
      }
    }
  });
});
