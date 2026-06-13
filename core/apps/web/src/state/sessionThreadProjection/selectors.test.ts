import { describe, expect, it } from "vitest";
import type { Message, SessionTurn } from "../../api/client";
import type { SessionCacheEntry } from "../sessionSupervisor/entryState";
import { buildSessionThreadProjectionFromSnapshot } from "./applySnapshot";
import { selectSessionQueuePanelMessages, selectSessionThreadProjection } from "./selectors";

function buildMessage(overrides: Partial<Message> = {}): Message {
  return {
    id: overrides.id ?? "message-1",
    session_id: overrides.session_id ?? "session-1",
    task_id: overrides.task_id ?? "task-1",
    turn_id: overrides.turn_id ?? "turn-1",
    role: overrides.role ?? "user",
    content: overrides.content ?? "hello",
    delivery: overrides.delivery ?? "immediate",
    created_at: overrides.created_at ?? "2026-03-19T12:00:00.000Z",
    ...overrides,
  } as Message;
}

function buildTurn(overrides: Partial<SessionTurn> = {}): SessionTurn {
  return {
    turn_id: overrides.turn_id ?? "turn-1",
    session_id: overrides.session_id ?? "session-1",
    run_id: overrides.run_id ?? null,
    user_message_id: overrides.user_message_id ?? "message-1",
    status: overrides.status ?? "queued",
    start_seq: overrides.start_seq ?? null,
    end_seq: overrides.end_seq ?? null,
    started_at: overrides.started_at ?? "2026-03-19T12:00:00.000Z",
    updated_at: overrides.updated_at ?? "2026-03-19T12:00:00.000Z",
    assistant_partial: overrides.assistant_partial ?? "",
    thought_partial: overrides.thought_partial ?? "",
    metrics_json: overrides.metrics_json ?? null,
    tool_total: overrides.tool_total ?? 0,
    tool_pending: overrides.tool_pending ?? 0,
    tool_running: overrides.tool_running ?? 0,
    tool_completed: overrides.tool_completed ?? 0,
    tool_failed: overrides.tool_failed ?? 0,
    ...overrides,
  } as SessionTurn;
}

function buildEntry(overrides: Partial<SessionCacheEntry> = {}): SessionCacheEntry {
  const entry: SessionCacheEntry = {
    sessionId: "session-1",
    loadState: "live",
    freshness: "authoritative",
    turns: [],
    turnToolsByTurnId: {},
    turnToolsLoading: [],
    toolSummaries: [],
    toolSummariesReady: true,
    hasMoreTurns: false,
    events: [],
    messages: [],
    artifacts: [],
    artifactsLoading: false,
    subagentInvocations: [],
    subagentInvocationsLoading: false,
    stateLoaded: true,
    stateLoading: false,
    loadErrors: {},
    queue: [],
    loading: false,
    subscribed: false,
    updatedAtMs: 0,
    ...overrides,
  };
  return {
    ...entry,
    threadProjection: overrides.threadProjection ?? buildSessionThreadProjectionFromSnapshot(entry),
  };
}

describe("sessionThreadProjection selectors", () => {
  it("applies optimistic thread overlay on top of the canonical projection", () => {
    const optimisticMessage = buildMessage({
      id: "message-opt",
      turn_id: "turn-opt",
      content: "optimistic hello",
    });
    const projection = selectSessionThreadProjection(
      buildEntry({
        projectionRev: 7,
        optimisticThreadMessages: [optimisticMessage],
        overlayRev: 3,
      }),
    );

    expect(projection.projectionRev).toBe(10);
    expect(projection.messages.map((message) => String(message.id))).toEqual(["message-opt"]);
    expect(projection.turns.map((turn) => String(turn.turn_id))).toEqual(["turn-opt"]);
    expect(projection.turns[0]?.status).toBe("running");
  });

  it("builds the queue panel from supervisor-owned optimistic queue state", () => {
    const queuedMessage = buildMessage({
      id: "queued-1",
      delivery: "queued",
    });
    const optimisticQueuedMessage = buildMessage({
      id: "queued-2",
      turn_id: "turn-2",
      delivery: "queued",
      content: "optimistic queued",
    });
    const queue = selectSessionQueuePanelMessages(
      buildEntry({
        queue: [queuedMessage],
        optimisticQueuedMessages: [optimisticQueuedMessage],
        optimisticQueueRemovalIds: ["queued-1"],
      }),
      [buildTurn({ turn_id: "turn-1", user_message_id: "queued-1", status: "queued" })],
    );

    expect(queue.map((message) => String(message.id))).toEqual(["queued-2"]);
  });
});
