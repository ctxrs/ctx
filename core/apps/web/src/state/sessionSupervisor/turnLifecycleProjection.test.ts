import { describe, expect, it } from "vitest";
import type { SessionEvent, SessionTurn } from "../../api/client";
import {
  isTerminalTurnStatus,
  mergeOrderedTurnStatus,
  resolveTurnFailureFromLifecycleEvent,
  resolveTurnStatusFromLifecycleEvent,
} from "./turnLifecycleProjection";

const buildEvent = (
  eventType: SessionEvent["event_type"],
  payloadJson: SessionEvent["payload_json"] = {},
): SessionEvent => ({
  seq: 1,
  id: `event-${eventType}`,
  session_id: "session-1",
  turn_id: "turn-1",
  event_type: eventType,
  payload_json: payloadJson,
  created_at: "2024-01-01T00:00:00.000Z",
});

describe("turnLifecycleProjection", () => {
  it("recognizes terminal statuses", () => {
    expect(isTerminalTurnStatus("completed")).toBe(true);
    expect(isTerminalTurnStatus("failed")).toBe(true);
    expect(isTerminalTurnStatus("interrupted")).toBe(true);
    expect(isTerminalTurnStatus("running")).toBe(false);
  });

  it("keeps terminal statuses sticky except failed to interrupted", () => {
    expect(mergeOrderedTurnStatus("completed", "running")).toBe("completed");
    expect(mergeOrderedTurnStatus("failed", "interrupted")).toBe("interrupted");
    expect(mergeOrderedTurnStatus("interrupted", "failed")).toBe("interrupted");
  });

  it("defaults turn_finished to completed when no payload status is present", () => {
    expect(resolveTurnStatusFromLifecycleEvent("running", buildEvent("turn_finished"))).toBe("completed");
  });

  it("preserves failed and interrupted on payload-less turn_finished", () => {
    expect(resolveTurnStatusFromLifecycleEvent("failed", buildEvent("turn_finished"))).toBe("failed");
    expect(resolveTurnStatusFromLifecycleEvent("interrupted", buildEvent("turn_finished"))).toBe("interrupted");
  });

  it("uses explicit payload status overrides on turn_finished", () => {
    const event = buildEvent("turn_finished", { status: "failed" });
    expect(resolveTurnStatusFromLifecycleEvent("running", event)).toBe("failed");
  });

  it("treats turn_finished status error as failed", () => {
    const event = buildEvent("turn_finished", { status: "error" });
    expect(resolveTurnStatusFromLifecycleEvent("running", event)).toBe("failed");
  });

  it("maps lifecycle events to the expected next statuses", () => {
    const cases: Array<[SessionTurn["status"] | undefined, SessionEvent["event_type"], SessionTurn["status"]]> = [
      [undefined, "turn_queued", "queued"],
      ["queued", "turn_started", "running"],
      ["running", "turn_interrupted", "interrupted"],
      ["running", "done", "completed"],
    ];

    for (const [previousStatus, eventType, expectedStatus] of cases) {
      expect(resolveTurnStatusFromLifecycleEvent(previousStatus, buildEvent(eventType))).toBe(expectedStatus);
    }
  });

  it("extracts failure details from failed turn_finished", () => {
    const failure = resolveTurnFailureFromLifecycleEvent(
      buildEvent("turn_finished", {
        status: "failed",
        message: "provider failed",
        details: { exit_code: 1 },
        kind: "provider_protocol_violation",
        providerId: "codex",
      }),
    );

    expect(failure).toEqual({
      message: "provider failed",
      details: { exit_code: 1 },
      kind: "provider_protocol_violation",
      reason: undefined,
      provider: undefined,
      provider_id: "codex",
    });
  });

  it("preserves scalar and array failure details from failed turn_finished", () => {
    expect(
      resolveTurnFailureFromLifecycleEvent(
        buildEvent("turn_finished", {
          status: "failed",
          message: "provider failed",
          details: "stderr tail",
        }),
      )?.details,
    ).toBe("stderr tail");

    expect(
      resolveTurnFailureFromLifecycleEvent(
        buildEvent("turn_finished", {
          status: "failed",
          message: "provider failed",
          details: ["line one", "line two"],
        }),
      )?.details,
    ).toEqual(["line one", "line two"]);
  });
});
