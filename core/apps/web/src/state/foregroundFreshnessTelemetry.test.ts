import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const clientMocks = vi.hoisted(() => ({
  recordClientCounterMetric: vi.fn(),
  recordClientGaugeMetric: vi.fn(),
  recordClientHistogramMetric: vi.fn(),
}));

const diagnosticMocks = vi.hoisted(() => ({
  emitUiDiagnostic: vi.fn(),
}));

const analyticsMocks = vi.hoisted(() => ({
  trackForegroundBacklogObserved: vi.fn(),
  trackForegroundFreshnessSlaMissed: vi.fn(),
  trackForegroundGapRecoveryObserved: vi.fn(),
  trackFreshnessRecovered: vi.fn(),
  trackRendererBacklogSample: vi.fn(),
  trackRendererBacklogSpike: vi.fn(),
}));

vi.mock("../api/client", () => ({
  recordClientCounterMetric: clientMocks.recordClientCounterMetric,
  recordClientGaugeMetric: clientMocks.recordClientGaugeMetric,
  recordClientHistogramMetric: clientMocks.recordClientHistogramMetric,
}));

vi.mock("./diagnosticsChannel", () => ({
  emitUiDiagnostic: diagnosticMocks.emitUiDiagnostic,
}));

vi.mock("../utils/analytics", () => ({
  trackForegroundBacklogObserved: analyticsMocks.trackForegroundBacklogObserved,
  trackForegroundFreshnessSlaMissed: analyticsMocks.trackForegroundFreshnessSlaMissed,
  trackForegroundGapRecoveryObserved: analyticsMocks.trackForegroundGapRecoveryObserved,
  trackFreshnessRecovered: analyticsMocks.trackFreshnessRecovered,
  trackRendererBacklogSample: analyticsMocks.trackRendererBacklogSample,
  trackRendererBacklogSpike: analyticsMocks.trackRendererBacklogSpike,
}));

import {
  noteClientReceiveLag,
  noteGapRecoveryFinished,
  noteGapRecoveryStarted,
  noteGapRepairMismatch,
  noteFinalDeltaReceived,
  noteFinalVisible,
  noteInterruptClicked,
  noteInterruptPendingVisible,
  noteQueueAgeSample,
  noteLateChunkAfterTerminal,
  noteNavThreadActivityMismatch,
  noteProjectionOrSeqRegression,
  noteSessionSwitchFirstPaint,
  noteSessionSwitchStarted,
  noteSessionReplicaApplyDuration,
  noteSessionReplicaEventAge,
  noteSessionReplicaApplyLag,
  noteStaleHeadDeltaDropped,
  noteWorkspaceEventAge,
  noteWorkspaceStreamEventObserved,
  noteSwitchStaleVisible,
  resetForegroundFreshnessTelemetryForTests,
} from "./foregroundFreshnessTelemetry";

describe("foregroundFreshnessTelemetry", () => {
  beforeEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
    vi.spyOn(performance, "now").mockReturnValue(0);
    clientMocks.recordClientCounterMetric.mockReset();
    clientMocks.recordClientGaugeMetric.mockReset();
    clientMocks.recordClientHistogramMetric.mockReset();
    diagnosticMocks.emitUiDiagnostic.mockReset();
    analyticsMocks.trackForegroundBacklogObserved.mockReset();
    analyticsMocks.trackForegroundFreshnessSlaMissed.mockReset();
    analyticsMocks.trackForegroundGapRecoveryObserved.mockReset();
    analyticsMocks.trackFreshnessRecovered.mockReset();
    analyticsMocks.trackRendererBacklogSample.mockReset();
    analyticsMocks.trackRendererBacklogSpike.mockReset();
    resetForegroundFreshnessTelemetryForTests();
  });

  afterEach(() => {
    vi.useRealTimers();
    resetForegroundFreshnessTelemetryForTests();
  });

  it("records session switch first paint latency", () => {
    vi.spyOn(performance, "now").mockReturnValue(10);
    noteSessionSwitchStarted("session-old", "session-new");
    vi.spyOn(performance, "now").mockReturnValue(65);
    noteSessionSwitchFirstPaint("session-new");

    expect(clientMocks.recordClientHistogramMetric).toHaveBeenCalledWith(
      "workbench.switch_to_first_paint_ms",
      "ms",
      55,
      { phase: "first_paint" },
    );
    expect(diagnosticMocks.emitUiDiagnostic).not.toHaveBeenCalled();
    expect(analyticsMocks.trackForegroundFreshnessSlaMissed).not.toHaveBeenCalled();
  });

  it("records final visibility freshness and warns when the telemetry SLA is breached", () => {
    const timeOrigin = performance.timeOrigin ?? 0;
    vi.spyOn(performance, "now").mockReturnValue(100);
    noteFinalDeltaReceived({
      sessionId: "session-1",
      turnId: "turn-1",
      emittedAtMs: timeOrigin + 60,
      lastEventSeq: 42,
    });

    vi.spyOn(performance, "now").mockReturnValue(220);
    noteFinalVisible("session-1", ["turn-1"]);

    expect(clientMocks.recordClientHistogramMetric).toHaveBeenCalledWith(
      "workbench.final_ws_to_dom_ms",
      "ms",
      120,
      undefined,
    );
    expect(clientMocks.recordClientHistogramMetric).toHaveBeenCalledWith(
      "workbench.final_ingress_to_dom_ms",
      "ms",
      160,
      undefined,
    );
    expect(diagnosticMocks.emitUiDiagnostic).toHaveBeenCalledWith(
      expect.objectContaining({
        code: "final.ws_to_dom_sla_missed",
        severity: "warning",
      }),
    );
    expect(diagnosticMocks.emitUiDiagnostic).toHaveBeenCalledWith(
      expect.objectContaining({
        code: "final.ingress_to_dom_sla_missed",
        severity: "warning",
      }),
    );
    expect(analyticsMocks.trackForegroundFreshnessSlaMissed).toHaveBeenCalledWith({
      metric: "workbench.final_ws_to_dom_ms",
      surface: "final_delivery",
      bucket: "slight",
    });
    expect(analyticsMocks.trackForegroundFreshnessSlaMissed).toHaveBeenCalledWith({
      metric: "workbench.final_ingress_to_dom_ms",
      surface: "final_delivery",
      bucket: "slight",
    });
  });

  it("escalates final visibility diagnostics to error only past the hard user-visible budget", () => {
    const timeOrigin = performance.timeOrigin ?? 0;
    vi.spyOn(performance, "now").mockReturnValue(100);
    noteFinalDeltaReceived({
      sessionId: "session-1",
      turnId: "turn-1",
      emittedAtMs: timeOrigin + 100,
      lastEventSeq: 42,
    });

    vi.spyOn(performance, "now").mockReturnValue(10_250);
    noteFinalVisible("session-1", ["turn-1"]);

    expect(diagnosticMocks.emitUiDiagnostic).toHaveBeenCalledWith(
      expect.objectContaining({
        code: "final.ws_to_dom_sla_missed",
        severity: "error",
      }),
    );
    expect(diagnosticMocks.emitUiDiagnostic).toHaveBeenCalledWith(
      expect.objectContaining({
        code: "final.ingress_to_dom_sla_missed",
        severity: "error",
      }),
    );
  });

  it("records interrupt click to pending latency", () => {
    vi.spyOn(performance, "now").mockReturnValue(50);
    noteInterruptClicked("session-1", "thread_header");
    vi.spyOn(performance, "now").mockReturnValue(70);
    noteInterruptPendingVisible("session-1");

    expect(clientMocks.recordClientHistogramMetric).toHaveBeenCalledWith(
      "workbench.interrupt_click_to_pending_ms",
      "ms",
      20,
      { source: "thread_header" },
    );
    expect(diagnosticMocks.emitUiDiagnostic).not.toHaveBeenCalled();
  });

  it("records client receive lag with lane and stream source labels", () => {
    noteClientReceiveLag("foreground", 125, {
      stream_source: "live",
      event_type: "session_head_delta",
    });

    expect(clientMocks.recordClientHistogramMetric).toHaveBeenCalledWith(
      "workbench.client_receive_lag_ms",
      "ms",
      125,
      { lane: "foreground", stream_source: "live" },
    );
    expect(diagnosticMocks.emitUiDiagnostic).not.toHaveBeenCalled();
  });

  it("keeps foreground receive lag diagnostics as warnings below the hard remote-soak budget", () => {
    noteClientReceiveLag("foreground", 12_000, {
      stream_source: "live",
      event_type: "session_head_delta",
    });

    expect(diagnosticMocks.emitUiDiagnostic).toHaveBeenCalledWith(
      expect.objectContaining({
        code: "client_receive_lag.foreground.sla_missed",
        severity: "warning",
      }),
    );
  });

  it("escalates foreground receive lag diagnostics at the hard remote-soak budget", () => {
    noteClientReceiveLag("foreground", 20_000, {
      stream_source: "live",
      event_type: "session_head_delta",
    });

    expect(diagnosticMocks.emitUiDiagnostic).toHaveBeenCalledWith(
      expect.objectContaining({
        code: "client_receive_lag.foreground.sla_missed",
        severity: "error",
      }),
    );
  });

  it("records workspace event age without treating it as live receive lag", () => {
    noteWorkspaceEventAge("foreground", 1250, {
      stream_source: "replay",
      event_type: "session_head_delta",
    });

    expect(clientMocks.recordClientHistogramMetric).toHaveBeenCalledWith(
      "workbench.workspace_event_age_ms",
      "ms",
      1250,
      { lane: "foreground", stream_source: "replay", event_type: "session_head_delta" },
    );
    expect(diagnosticMocks.emitUiDiagnostic).not.toHaveBeenCalled();
  });

  it("records replica apply lag separately from event age and apply duration", () => {
    noteSessionReplicaApplyLag(80, {
      lag_source: "received_at",
      event_type: "session_head_delta",
    });
    noteSessionReplicaEventAge(1800, {
      stream_source: "replay",
      event_type: "session_head_delta",
    });
    noteSessionReplicaApplyDuration(12, {
      patch_count: 2,
      op: "append",
    });

    expect(clientMocks.recordClientHistogramMetric).toHaveBeenCalledWith(
      "workbench.session_replica_apply_lag_ms",
      "ms",
      80,
      { op: "session_head_delta", lag_source: "received_at" },
    );
    expect(clientMocks.recordClientHistogramMetric).toHaveBeenCalledWith(
      "workbench.session_replica_event_age_ms",
      "ms",
      1800,
      { op: "session_head_delta", stream_source: "replay" },
    );
    expect(clientMocks.recordClientHistogramMetric).toHaveBeenCalledWith(
      "workbench.session_replica_apply_duration_ms",
      "ms",
      12,
      { patch_count: "2", op: "append" },
    );
  });

  it("keeps load-level replica lag diagnostics as warnings below the hard freshness budget", () => {
    noteSessionReplicaApplyLag(2045, {
      lag_source: "received_at",
      event_type: "session_delta",
    });

    expect(diagnosticMocks.emitUiDiagnostic).toHaveBeenCalledWith(
      expect.objectContaining({
        code: "session_replica.apply_lag_sla_missed",
        severity: "warning",
      }),
    );
  });

  it("records workspace stream event counts by lane and event type", () => {
    noteWorkspaceStreamEventObserved("foreground", "session_head_delta");

    expect(clientMocks.recordClientCounterMetric).toHaveBeenCalledWith(
      "workbench.workspace_stream_event_count",
      { lane: "foreground", event_type: "session_head_delta" },
    );
  });

  it("records gap recovery timeout once the recovery budget is exceeded", () => {
    vi.useFakeTimers();
    noteGapRecoveryStarted("session-1", "session_gap");

    vi.advanceTimersByTime(1000);

    expect(clientMocks.recordClientCounterMetric).toHaveBeenCalledWith(
      "workbench.foreground_gap_recovery_timeout_count",
    );
    expect(analyticsMocks.trackForegroundGapRecoveryObserved).toHaveBeenCalledWith({
      result: "timeout",
    });
    expect(analyticsMocks.trackForegroundFreshnessSlaMissed).toHaveBeenCalledWith({
      metric: "workbench.foreground_gap_recovery_timeout_count",
      surface: "gap_recovery",
      bucket: "severe",
    });
    expect(diagnosticMocks.emitUiDiagnostic).toHaveBeenCalledWith(
      expect.objectContaining({
        code: "foreground_gap_recovery.timeout",
        severity: "error",
      }),
    );

    noteGapRecoveryFinished("session-1");
  });

  it("keeps workspace gap recovery timeout metrics out of the foreground lane", () => {
    vi.useFakeTimers();
    noteGapRecoveryStarted("session-warm", "session_gap", "workspace");

    vi.advanceTimersByTime(1000);

    expect(clientMocks.recordClientCounterMetric).toHaveBeenCalledWith(
      "workbench.workspace_gap_recovery_timeout_count",
    );
    expect(clientMocks.recordClientCounterMetric).not.toHaveBeenCalledWith(
      "workbench.foreground_gap_recovery_timeout_count",
    );
    expect(analyticsMocks.trackForegroundGapRecoveryObserved).not.toHaveBeenCalledWith({
      result: "timeout",
    });

    vi.spyOn(performance, "now").mockReturnValue(1200);
    noteGapRecoveryFinished("session-warm");

    expect(clientMocks.recordClientHistogramMetric).toHaveBeenCalledWith(
      "workbench.workspace_gap_recovery_ms",
      "ms",
      expect.any(Number),
    );
  });

  it("records invariant counters once per dedupe key", () => {
    noteLateChunkAfterTerminal("turn-1");
    noteLateChunkAfterTerminal("turn-1");
    noteProjectionOrSeqRegression("session-1", "last_event_seq", 3, 7);
    noteStaleHeadDeltaDropped("session-1", "last_event_seq", 3, 7);
    noteGapRepairMismatch("session-1", 9, 5);
    noteSwitchStaleVisible("task-1", "session-old", "session-new");
    noteNavThreadActivityMismatch("task-1", "session-new", true, false);

    expect(clientMocks.recordClientCounterMetric).toHaveBeenCalledWith(
      "workbench.late_chunk_after_terminal_count",
      undefined,
    );
    expect(clientMocks.recordClientCounterMetric).toHaveBeenCalledWith(
      "workbench.projection_or_seq_regression_count",
      { dimension: "last_event_seq" },
    );
    expect(clientMocks.recordClientCounterMetric).toHaveBeenCalledWith(
      "workbench.stale_head_delta_dropped_count",
      { dimension: "last_event_seq" },
    );
    expect(clientMocks.recordClientCounterMetric).toHaveBeenCalledWith(
      "workbench.gap_repair_mismatch_count",
      undefined,
    );
    expect(clientMocks.recordClientCounterMetric).toHaveBeenCalledWith(
      "workbench.switch_stale_visible_count",
      undefined,
    );
    expect(clientMocks.recordClientCounterMetric).toHaveBeenCalledWith(
      "workbench.nav_thread_activity_mismatch_count",
      undefined,
    );
    expect(clientMocks.recordClientCounterMetric).toHaveBeenCalledTimes(6);
  });

  it("dedupes degraded backlog observations within the same severity bucket and bounds gauge sampling", () => {
    vi.spyOn(performance, "now").mockReturnValue(0);
    noteQueueAgeSample("workspace", 300, { source: "worker_patch" });

    vi.spyOn(performance, "now").mockReturnValue(100);
    noteQueueAgeSample("workspace", 320, { source: "worker_patch" });

    vi.spyOn(performance, "now").mockReturnValue(1200);
    noteQueueAgeSample("workspace", 340, { source: "worker_patch" });

    vi.spyOn(performance, "now").mockReturnValue(1300);
    noteQueueAgeSample("workspace", 1200, { source: "worker_patch" });

    vi.spyOn(performance, "now").mockReturnValue(2000);
    noteQueueAgeSample("workspace", 40, { source: "worker_patch" });

    expect(clientMocks.recordClientGaugeMetric).toHaveBeenCalledTimes(2);
    expect(analyticsMocks.trackRendererBacklogSample).toHaveBeenCalledTimes(2);
    expect(analyticsMocks.trackRendererBacklogSpike).toHaveBeenCalledTimes(1);
    expect(analyticsMocks.trackForegroundBacklogObserved).toHaveBeenCalledTimes(2);
    expect(analyticsMocks.trackForegroundBacklogObserved).toHaveBeenNthCalledWith(1, {
      lane: "workspace",
      bucket: "over_250ms",
    });
    expect(analyticsMocks.trackForegroundBacklogObserved).toHaveBeenNthCalledWith(2, {
      lane: "workspace",
      bucket: "over_1000ms",
    });
    expect(analyticsMocks.trackFreshnessRecovered).toHaveBeenCalledWith({
      lane: "workspace",
      source: "worker_patch",
      degradedForMs: 2000,
    });
  });
});
