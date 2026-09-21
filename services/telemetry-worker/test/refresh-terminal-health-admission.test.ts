import { describe, expect, test } from "vitest";

import {
  ENV,
  jsonRequest,
  operationEvent,
  providerRefreshEvent,
  runtimeEvent,
  v1Batch,
  workerHarness,
} from "./worker-test-fixtures";

const SIDECAR_KEYS = [
  "refresh_queue_wait_duration_bucket",
  "refresh_discovery_duration_bucket",
  "refresh_scan_stage_duration_bucket",
  "refresh_commit_duration_bucket",
  "refresh_coalesced_request_count_bucket",
  "refresh_successor_pending",
  "refresh_configured_indexing_mode",
  "refresh_daemon_trigger_kind",
  "refresh_reconciliation_demand",
  "refresh_retained_previous_generation",
  "refresh_processed_sessions_bucket",
  "refresh_processed_messages_bucket",
  "refresh_processed_tool_calls_bucket",
  "refresh_processed_bytes_bucket",
] as const;

function daemonRefreshEvent(
  properties: Record<string, unknown> = {},
  event: Record<string, unknown> = {},
): Record<string, unknown> {
  return providerRefreshEvent({
    surface: "daemon",
    properties: {
      ...(providerRefreshEvent().properties as Record<string, unknown>),
      trigger: "daemon",
      ...properties,
    },
    ...event,
  });
}

describe("provider refresh terminal-health admission", () => {
  test("queues a sparse sidecar without changing base-event compatibility", async () => {
    const harness = workerHarness();
    const response = await harness.worker.fetch(
      jsonRequest("/functions/v1/telemetry", v1Batch([
        daemonRefreshEvent({
          refresh_queue_wait_duration_bucket: "lt_100ms",
          refresh_coalesced_request_count_bucket: "0",
          refresh_successor_pending: false,
          refresh_configured_indexing_mode: "automatic",
          refresh_daemon_trigger_kind: "periodic_reconciliation",
          refresh_reconciliation_demand: "exhaustive",
          refresh_processed_sessions_bucket: "2-5",
          refresh_processed_messages_bucket: "21-100",
          refresh_processed_tool_calls_bucket: "6-20",
          refresh_processed_bytes_bucket: "1mb-10mb",
        }),
        daemonRefreshEvent({}, {
          event_id: "44444444-4444-4444-8444-444444444444",
        }),
      ])),
      ENV,
    );

    expect(response.status, await response.text()).toBe(204);
    const messages = await harness.queueMessages();
    const rows = messages.map((message) => {
      if (message.kind !== "telemetry_row") throw new Error("expected_rows");
      return message.row;
    });
    expect(rows).toHaveLength(2);
    expect(rows?.[0]?.properties).toMatchObject({
      refresh_queue_wait_duration_bucket: "lt_100ms",
      refresh_coalesced_request_count_bucket: "0",
      refresh_successor_pending: false,
      refresh_configured_indexing_mode: "automatic",
      refresh_daemon_trigger_kind: "periodic_reconciliation",
      refresh_reconciliation_demand: "exhaustive",
      refresh_processed_sessions_bucket: "2-5",
      refresh_processed_messages_bucket: "21-100",
      refresh_processed_tool_calls_bucket: "6-20",
      refresh_processed_bytes_bucket: "1mb-10mb",
    });
    expect(rows?.[0]?.properties).not.toHaveProperty("refresh_discovery_duration_bucket");
    for (const key of SIDECAR_KEYS) {
      expect(rows?.[1]?.properties).not.toHaveProperty(key);
    }
  });

  test.each([
    [
      "sidecar on a CLI refresh",
      providerRefreshEvent({
        properties: {
          ...(providerRefreshEvent().properties as Record<string, unknown>),
          refresh_successor_pending: false,
        },
      }),
      "invalid_refresh_terminal_health_surface",
    ],
    [
      "sidecar on another event",
      operationEvent({
        properties: {
          ...(operationEvent().properties as Record<string, unknown>),
          refresh_successor_pending: false,
        },
      }),
      "unknown_operation_property",
    ],
    [
      "sidecar on a runtime observation",
      runtimeEvent({
        properties: {
          ...(runtimeEvent().properties as Record<string, unknown>),
          refresh_successor_pending: false,
        },
      }),
      "unknown_daemon_runtime_property",
    ],
    [
      "optional field without the required anchor",
      daemonRefreshEvent({ refresh_discovery_duration_bucket: "lt_1s" }),
      "incomplete_refresh_terminal_health",
    ],
    [
      "abandoned receipt field",
      daemonRefreshEvent({ refresh_execution_duration_bucket: "lt_1s" }),
      "unknown_provider_refresh_property",
    ],
    [
      "unknown duration bucket",
      daemonRefreshEvent({
        refresh_commit_duration_bucket: "lt_50ms",
        refresh_successor_pending: false,
      }),
      "invalid_refresh_commit_duration_bucket",
    ],
    [
      "unknown count bucket",
      daemonRefreshEvent({
        refresh_coalesced_request_count_bucket: "2",
        refresh_successor_pending: false,
      }),
      "invalid_refresh_coalesced_request_count_bucket",
    ],
    [
      "malformed successor flag",
      daemonRefreshEvent({ refresh_successor_pending: "false" }),
      "invalid_refresh_successor_pending",
    ],
    [
      "unknown configured mode",
      daemonRefreshEvent({
        refresh_configured_indexing_mode: "sometimes",
        refresh_successor_pending: false,
      }),
      "invalid_refresh_configured_indexing_mode",
    ],
    [
      "unknown daemon trigger kind",
      daemonRefreshEvent({
        refresh_daemon_trigger_kind: "debounce",
        refresh_successor_pending: false,
      }),
      "invalid_refresh_daemon_trigger_kind",
    ],
    [
      "invalid processed byte bucket",
      daemonRefreshEvent({
        refresh_processed_bytes_bucket: "1gb+",
        refresh_successor_pending: false,
      }),
      "invalid_refresh_processed_bytes_bucket",
    ],
    [
      "pending successor without remaining work",
      daemonRefreshEvent({
        work_remaining: false,
        refresh_successor_pending: true,
      }),
      "inconsistent_refresh_terminal_health",
    ],
  ])("rejects %s", async (_name, event, code) => {
    const harness = workerHarness();
    const response = await harness.worker.fetch(
      jsonRequest("/functions/v1/telemetry", v1Batch([event])),
      ENV,
    );

    expect(response.status).toBe(422);
    expect(await response.json()).toEqual({ error: code });
    expect(harness.insertTelemetryRows).not.toHaveBeenCalled();
  });

  test("retained generation is failure-only and daemon subtype requires the daemon trigger", async () => {
    const harness = workerHarness();
    const successWithRetention = daemonRefreshEvent({
      refresh_retained_previous_generation: true,
      refresh_successor_pending: false,
    });
    const wrongTrigger = daemonRefreshEvent({
      trigger: "search",
      refresh_daemon_trigger_kind: "daemon_watch",
      refresh_successor_pending: false,
    });
    for (const [event, code] of [
      [successWithRetention, "inconsistent_refresh_retained_generation"],
      [wrongTrigger, "inconsistent_refresh_daemon_trigger_kind"],
    ] as const) {
      const response = await harness.worker.fetch(
        jsonRequest("/functions/v1/telemetry", v1Batch([event])),
        ENV,
      );
      expect(response.status).toBe(422);
      expect(await response.json()).toEqual({ error: code });
    }

    const failed = daemonRefreshEvent({
      change: "no_op",
      refresh_result: "failure",
      core_result: "failure",
      failure_scope: "system",
      failure_type: "system",
      refresh_retained_previous_generation: true,
      refresh_successor_pending: false,
    }, { outcome: "failure" });
    const response = await harness.worker.fetch(
      jsonRequest("/functions/v1/telemetry", v1Batch([failed])),
      ENV,
    );
    expect(response.status, await response.text()).toBe(204);
  });

  test("admits daemon-owned setup receipts without inventing a daemon subtype", async () => {
    const harness = workerHarness();
    const setup = daemonRefreshEvent({
      trigger: "setup",
      refresh_configured_indexing_mode: "automatic",
      refresh_reconciliation_demand: "exhaustive",
      refresh_processed_messages_bucket: "21-100",
      refresh_processed_bytes_bucket: "1mb-10mb",
      refresh_successor_pending: false,
    });
    const response = await harness.worker.fetch(
      jsonRequest("/functions/v1/telemetry", v1Batch([setup])),
      ENV,
    );

    expect(response.status, await response.text()).toBe(204);
    const message = (await harness.queueMessages())[0];
    if (message?.kind !== "telemetry_row") throw new Error("expected_row");
    const row = message.row;
    expect(row?.activity_class).toBe("setup");
    expect(row?.properties).not.toHaveProperty("refresh_daemon_trigger_kind");
  });
});
