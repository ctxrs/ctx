import { describe, expect, it } from "vitest";
import type { SessionEvent, SessionTurn } from "../api/client";
import {
  applyReplicaTranscriptEvent,
  type SessionReplicaTranscriptEntry,
} from "./sessionReplicaTranscript";

const mkTurn = (status: SessionTurn["status"]): SessionTurn => ({
  turn_id: "turn-1",
  session_id: "session-1",
  run_id: "run-1",
  user_message_id: "message-1",
  status,
  start_seq: 1,
  end_seq: null,
  started_at: new Date(1).toISOString(),
  updated_at: new Date(1).toISOString(),
  assistant_partial: "",
  thought_partial: "",
  metrics_json: null,
  tool_total: 0,
  tool_pending: 0,
  tool_running: 0,
  tool_completed: 0,
  tool_failed: 0,
});

const mkEvent = (
  seq: number,
  event_type: SessionEvent["event_type"],
  payload_json: Record<string, unknown>,
): SessionEvent => ({
  seq,
  id: `event-${seq}`,
  session_id: "session-1",
  run_id: "run-1",
  turn_id: "turn-1",
  event_type,
  payload_json,
  created_at: new Date(seq).toISOString(),
});

const mkEntry = (): SessionReplicaTranscriptEntry => ({
  sessionId: "session-1",
  turns: [mkTurn("running")],
  turnsRev: 0,
  assistantStreamingByTurnId: {},
  assistantStreamingRev: 0,
  messages: [],
  messagesRev: 0,
  events: [],
  eventsRev: 0,
  toolSummaries: [],
  nextTransientSeq: -1,
  startedTurnIds: new Set(["turn-1"]),
  toolStatusByKey: new Map(),
  toolIdsByTurn: new Map(),
});

describe("sessionReplicaTranscript", () => {
  it("keeps assistant chunks in streaming overlay without mutating durable turn structure", () => {
    const entry = mkEntry();
    const beforeUpdatedAt = entry.turns[0]?.updated_at;

    applyReplicaTranscriptEvent(
      entry,
      mkEvent(2, "assistant_chunk", {
        content_fragment: "Hello",
        order_seq: 2,
      }),
    );

    expect(entry.assistantStreamingByTurnId["turn-1"]?.content).toBe("Hello");
    expect(entry.assistantStreamingRev).toBe(1);
    expect(entry.turnsRev).toBe(0);
    expect(entry.turns[0]?.updated_at).toBe(beforeUpdatedAt);
    expect(entry.turns).toHaveLength(1);
  });

  it("does not synthesize durable turns from assistant chunks alone", () => {
    const entry = mkEntry();
    entry.turns = [];
    entry.startedTurnIds = new Set();

    applyReplicaTranscriptEvent(
      entry,
      mkEvent(2, "assistant_chunk", {
        content_fragment: "Hello",
        order_seq: 2,
      }),
    );

    expect(entry.assistantStreamingByTurnId["turn-1"]).toEqual({
      content: "Hello",
      providerMessageId: null,
      orderSeq: 2,
    });
    expect(entry.assistantStreamingRev).toBe(1);
    expect(entry.turns).toEqual([]);
    expect(entry.turnsRev).toBe(0);
    expect(entry.startedTurnIds.size).toBe(0);
  });

  it("promotes failed turns to interrupted when turn_interrupted follows cancel fallout", () => {
    const entry = mkEntry();

    applyReplicaTranscriptEvent(entry, mkEvent(2, "turn_finished", { status: "failed", message: "cancelled" }));
    expect(entry.turns[0]?.status).toBe("failed");

    applyReplicaTranscriptEvent(entry, mkEvent(3, "turn_interrupted", { reason: "user_interrupt" }));
    expect(entry.turns[0]?.status).toBe("interrupted");
  });

  it("clears running tool counters when a lifecycle event terminalizes the turn", () => {
    const entry = mkEntry();
    entry.turns[0] = {
      ...entry.turns[0]!,
      tool_total: 1,
      tool_pending: 0,
      tool_running: 1,
    };

    applyReplicaTranscriptEvent(entry, mkEvent(2, "turn_finished", { status: "failed" }));

    expect(entry.turns[0]?.status).toBe("failed");
    expect(entry.turns[0]?.tool_total).toBe(1);
    expect(entry.turns[0]?.tool_pending).toBe(0);
    expect(entry.turns[0]?.tool_running).toBe(0);
  });
});
