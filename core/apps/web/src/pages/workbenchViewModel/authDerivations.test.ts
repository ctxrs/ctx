import { describe, expect, it } from "vitest";
import type { SessionEvent, SessionTurn } from "../../api/client";
import { deriveAuthUi, deriveSessionError, extractErrorMessage } from "./authDerivations";

describe("extractErrorMessage", () => {
  it("returns plain-string payload errors", () => {
    expect(extractErrorMessage(" provider runtime crashed ")).toBe("provider runtime crashed");
  });

  it("keeps object message + details formatting", () => {
    expect(extractErrorMessage({ message: "Provider failed", details: "Timed out" })).toBe(
      "Provider failed\nDetails: Timed out",
    );
  });
});

const makeFailedTurn = (turnId: string): SessionTurn => ({
  turn_id: turnId,
  session_id: "session-1",
  run_id: "run-1",
  user_message_id: "message-1",
  status: "failed",
  start_seq: 1,
  end_seq: 2,
  started_at: "2026-05-06T18:00:00.000Z",
  updated_at: "2026-05-06T18:00:01.000Z",
  assistant_partial: null,
  thought_partial: null,
  metrics_json: null,
  tool_total: 0,
  tool_pending: 0,
  tool_running: 0,
  tool_completed: 0,
  tool_failed: 0,
});

const makeEvent = (
  eventType: SessionEvent["event_type"],
  turnId: string,
  payload: Record<string, unknown>,
): SessionEvent => ({
  seq: 2,
  id: `${eventType}-event`,
  session_id: "session-1",
  run_id: "run-1",
  turn_id: turnId,
  event_type: eventType,
  payload_json: payload,
  created_at: "2026-05-06T18:00:01.000Z",
});

describe("deriveSessionError", () => {
  it("surfaces failed turn_finished messages when the matching error event is absent", () => {
    const turnId = "turn-1";
    const error = deriveSessionError(
      [makeFailedTurn(turnId)],
      [
        makeEvent("turn_finished", turnId, {
          status: "failed",
          message: "You've hit your usage limit. Try again at May 12th, 2026 3:50 AM.",
          kind: "usageLimitExceeded",
          provider: "codex",
        }),
      ],
    );

    expect(error).toEqual({
      message:
        "You've hit your usage limit. Try again at May 12th, 2026 3:50 AM.\nDetails: usageLimitExceeded",
      provider: "codex",
    });
  });

  it("keeps the generic harness error only when no failure payload is available", () => {
    expect(deriveSessionError([makeFailedTurn("turn-1")], [])).toEqual({
      message: "Harness error.",
    });
  });
});

describe("deriveAuthUi", () => {
  it("treats normalized auth_error notices as failed auth", () => {
    expect(
      deriveAuthUi([
        makeEvent("notice", "turn-1", {
          kind: "auth_error",
          provider: "codex",
          message: "Run codex login.",
        }),
      ]),
    ).toEqual({
      status: "failed",
      provider: "codex",
      message: "Run codex login.",
      methods: [],
    });
  });

  it("treats normalized auth success notices as authenticated", () => {
    expect(
      deriveAuthUi([
        makeEvent("notice", "turn-1", {
          kind: "authenticated",
          provider: "codex",
          message: "authenticated",
        }),
      ]),
    ).toEqual({
      status: "authenticated",
      provider: "codex",
      message: undefined,
      methods: [],
    });
  });
});
