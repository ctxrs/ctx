import { beforeEach, describe, expect, it, vi } from "vitest";
import { handleSessionReplicaFreshnessEvent } from "./sessionReplicaBridge";

const getDaemonClientConfigMock = vi.hoisted(() => vi.fn(() => ({
  baseUrl: "http://127.0.0.1:4399",
  wsBaseUrl: "ws://127.0.0.1:4399",
  authToken: "browser-secret",
  runId: null,
})));
const subscribeDaemonConfigMock = vi.hoisted(() => vi.fn(() => () => {}));
const noteGapRecoveryStartedMock = vi.hoisted(() => vi.fn());
const noteFinalDeltaReceivedMock = vi.hoisted(() => vi.fn());
const noteSessionReplicaApplyLagMock = vi.hoisted(() => vi.fn());
const noteSessionReplicaApplyDurationMock = vi.hoisted(() => vi.fn());
const noteSessionReplicaEventAgeMock = vi.hoisted(() => vi.fn());
const noteStaleHeadDeltaDroppedMock = vi.hoisted(() => vi.fn());

vi.mock("../utils/desktop", () => ({
  isDesktopApp: () => false,
}));

vi.mock("../api/client", () => ({
  getDaemonClientConfig: getDaemonClientConfigMock,
  subscribeDaemonConfig: subscribeDaemonConfigMock,
  getSessionHead: vi.fn(),
  getSessionSnapshot: vi.fn(),
  getSessionState: vi.fn(),
}));

vi.mock("./foregroundFreshnessTelemetry", () => ({
  noteFinalDeltaReceived: noteFinalDeltaReceivedMock,
  noteGapRecoveryFinished: vi.fn(),
  noteGapRecoveryStarted: noteGapRecoveryStartedMock,
  noteGapRepairMismatch: vi.fn(),
  noteProjectionOrSeqRegression: vi.fn(),
  noteSessionReplicaApplyDuration: noteSessionReplicaApplyDurationMock,
  noteSessionReplicaApplyLag: noteSessionReplicaApplyLagMock,
  noteSessionReplicaEventAge: noteSessionReplicaEventAgeMock,
  noteStaleHeadDeltaDropped: noteStaleHeadDeltaDroppedMock,
}));

describe("SessionReplicaBridge", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("records replica freshness telemetry through the main-thread handler", () => {
    handleSessionReplicaFreshnessEvent({
      type: "gap_recovery_started",
      sessionId: "session-1",
      reason: "session_gap",
      lane: "workspace",
    });
    handleSessionReplicaFreshnessEvent({
      type: "final_delta_received",
      sessionId: "session-1",
      turnId: "turn-1",
      emittedAtMs: 12,
      lastEventSeq: 3,
    });

    expect(noteGapRecoveryStartedMock).toHaveBeenCalledWith("session-1", "session_gap", "workspace");
    expect(noteFinalDeltaReceivedMock).toHaveBeenCalledWith({
      sessionId: "session-1",
      turnId: "turn-1",
      emittedAtMs: 12,
      lastEventSeq: 3,
    });
  });

  it("records replica apply age telemetry from freshness events", () => {
    vi.spyOn(performance, "now").mockReturnValue(100);

    handleSessionReplicaFreshnessEvent({
      type: "replica_delta_applied",
      sessionId: "session-1",
      emittedAtMs: (performance.timeOrigin ?? 0) + 40,
      receivedAtMs: (performance.timeOrigin ?? 0) + 70,
      streamSource: "replay",
      lastEventSeq: 3,
      eventType: "session_head_delta",
    });

    expect(noteSessionReplicaEventAgeMock).toHaveBeenCalledWith(60, {
      session_id: "session-1",
      last_event_seq: 3,
      event_type: "session_head_delta",
      stream_source: "replay",
    });
    expect(noteSessionReplicaApplyLagMock).toHaveBeenCalledWith(30, {
      session_id: "session-1",
      last_event_seq: 3,
      event_type: "session_head_delta",
      stream_source: "replay",
      lag_source: "received_at",
    });
  });

  it("records dropped stale head deltas separately from hard regressions", () => {
    handleSessionReplicaFreshnessEvent({
      type: "stale_head_delta_dropped",
      sessionId: "session-1",
      dimension: "last_event_seq",
      incoming: 2,
      existing: 7,
    });

    expect(noteStaleHeadDeltaDroppedMock).toHaveBeenCalledWith(
      "session-1",
      "last_event_seq",
      2,
      7,
    );
  });
});
