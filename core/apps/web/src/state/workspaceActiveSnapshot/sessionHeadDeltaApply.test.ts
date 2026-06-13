import { describe, expect, it } from "vitest";
import type { Message, Session, SessionHeadDelta, SessionHeadSnapshot, SessionTurn } from "@ctx/types";
import { applySessionHeadDeltaToSnapshot } from "./sessionHeadDeltaApply";

const now = "2026-04-23T16:00:00.000Z";

const mkSession = (): Session => ({
  id: "session-1",
  task_id: "task-1",
  workspace_id: "ws-1",
  worktree_id: "wt-1",
  provider_id: "fake",
  model_id: "fake-model",
  title: "Session",
  agent_role: "assistant",
  status: "active",
  created_at: now,
  updated_at: now,
});

const mkHead = (session: Session): SessionHeadSnapshot => ({
  session,
  turns: [],
  tool_summaries: [],
  messages: [],
  events: [],
  last_event_seq: 0,
  projection_rev: 0,
  state_rev: 0,
  activity: { is_working: false, last_turn_status: null },
  has_more_turns: false,
  history_cursor: null,
  has_more_history: false,
});

const mkMessageDelta = (): SessionHeadDelta => {
  const message: Message = {
    id: "message-1",
    session_id: "session-1",
    task_id: "task-1",
    turn_id: "turn-1",
    role: "assistant",
    content: "hello",
    created_at: now,
    delivery: "immediate",
    order_seq: 10,
    turn_sequence: 10,
  };
  return {
    session_id: "session-1",
    last_event_seq: 10,
    projection_rev: 10,
    state_rev: 10,
    message,
  };
};

const mkRunningTurnDelta = (): SessionHeadDelta => {
  const turn: SessionTurn = {
    turn_id: "turn-1",
    session_id: "session-1",
    run_id: null,
    user_message_id: "message-0",
    status: "running",
    start_seq: 10,
    end_seq: null,
    started_at: now,
    updated_at: now,
    assistant_partial: null,
    thought_partial: null,
    metrics_json: null,
    tool_total: 1,
    tool_pending: 0,
    tool_running: 1,
    tool_completed: 0,
    tool_failed: 0,
  };
  return {
    session_id: "session-1",
    last_event_seq: 11,
    projection_rev: 11,
    state_rev: 11,
    turn,
  };
};

describe("applySessionHeadDeltaToSnapshot", () => {
  it("does not pin a synthetic message-only turn to completed when a later turn delta arrives", () => {
    const session = mkSession();
    const sessionHeadsById = new Map<string, SessionHeadSnapshot>([
      [session.id, mkHead(session)],
    ]);

    expect(
      applySessionHeadDeltaToSnapshot({
        delta: mkMessageDelta(),
        tasks: new Map(),
        sessionHeadsById,
      }),
    ).toBe(true);

    const placeholderTurn = sessionHeadsById.get(session.id)?.turns?.[0];
    expect(placeholderTurn?.turn_id).toBe("turn-1");
    expect(placeholderTurn?.status).toBe("queued");

    expect(
      applySessionHeadDeltaToSnapshot({
        delta: mkRunningTurnDelta(),
        tasks: new Map(),
        sessionHeadsById,
      }),
    ).toBe(true);

    const head = sessionHeadsById.get(session.id);
    expect(head?.turns).toHaveLength(1);
    expect(head?.turns[0]?.status).toBe("running");
    expect(head?.messages).toHaveLength(1);
    expect(head?.messages[0]?.turn_id).toBe("turn-1");
  });
});
