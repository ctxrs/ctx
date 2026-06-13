import { describe, expect, it } from "vitest";
import type { Message, Session, SessionEvent, SessionHeadSnapshot, SessionTurn } from "../api/client";
import {
  shouldPreserveExistingTranscriptWindow,
  shouldRepairSessionHeadReplace,
} from "./sessionHeadRepair";

const mkSession = (sessionId: string): Session => ({
  id: sessionId,
  task_id: "task-1",
  workspace_id: "ws-1",
  worktree_id: "wt-1",
  provider_id: "fake",
  model_id: "fake-model",
  title: "Session",
  agent_role: "assistant",
  status: "active",
});

const mkTurn = ({
  sessionId,
  turnId,
  status,
  startSeq,
}: {
  sessionId: string;
  turnId: string;
  status: SessionTurn["status"];
  startSeq: number;
}): SessionTurn => ({
  turn_id: turnId,
  session_id: sessionId,
  run_id: null,
  user_message_id: `${turnId}-user`,
  status,
  start_seq: startSeq,
  end_seq: status === "completed" ? startSeq + 1 : null,
  started_at: new Date(Date.UTC(2026, 2, 9, 0, 0, startSeq)).toISOString(),
  updated_at: new Date(Date.UTC(2026, 2, 9, 0, 0, startSeq + 1)).toISOString(),
  assistant_partial: null,
  thought_partial: null,
  metrics_json: null,
  tool_total: 0,
  tool_pending: 0,
  tool_running: 0,
  tool_completed: 0,
  tool_failed: 0,
});

const mkMessage = ({
  sessionId,
  turnId,
  id,
  role,
}: {
  sessionId: string;
  turnId: string;
  id: string;
  role: Message["role"];
}): Message => ({
  id,
  session_id: sessionId,
  task_id: "task-1",
  turn_id: turnId,
  role,
  content: id,
  delivery: "immediate",
  created_at: "2026-03-09T00:00:00.000Z",
});

const mkBoundedHead = ({
  sessionId,
  turns,
  messages,
  lastEventSeq,
  projectionRev,
  activity,
}: {
  sessionId: string;
  turns: SessionTurn[];
  messages: Message[];
  lastEventSeq: number;
  projectionRev: number;
  activity?: SessionHeadSnapshot["activity"];
}): SessionHeadSnapshot => ({
  session: mkSession(sessionId),
  turns,
  events: [] as SessionEvent[],
  messages,
  activity,
  last_event_seq: lastEventSeq,
  projection_rev: projectionRev,
  state_rev: projectionRev,
  has_more_turns: false,
  has_more_history: false,
  history_cursor: null,
  head_window: {
    turn_limit: 5,
    message_limit: 50,
    event_limit: 800,
    byte_limit: 200_000,
    turn_count: turns.length,
    message_count: messages.length,
    event_count: 0,
    bytes: 256,
    truncated: true,
  },
});

describe("sessionHeadRepair", () => {
  it("does not repair bounded heads that are covered and do not advance authority", () => {
    const sessionId = "session-covered-bounded-head";
    const runningTurn = mkTurn({ sessionId, turnId: "turn-1", status: "running", startSeq: 1 });
    const userMessage = mkMessage({ sessionId, turnId: "turn-1", id: "m-user-1", role: "user" });
    const head = mkBoundedHead({
      sessionId,
      turns: [runningTurn],
      messages: [userMessage],
      lastEventSeq: 4,
      projectionRev: 7,
      activity: { is_working: true, last_turn_status: "running" },
    });

    expect(
      shouldRepairSessionHeadReplace(
        {
          turnsHydrated: true,
          turns: [runningTurn],
          messages: [userMessage],
          freshness: "replica",
          loadState: "live",
          lastEventSeq: 4,
          projectionRev: 7,
          activity: { is_working: true, last_turn_status: "running" },
        },
        head,
      ),
    ).toBe(false);
  });

  it("repairs bounded heads that omit non-terminal turns from the current entry", () => {
    const sessionId = "session-omitted-running-turn";
    const runningTurn = mkTurn({ sessionId, turnId: "turn-running", status: "running", startSeq: 1 });
    const completedTurn = mkTurn({ sessionId, turnId: "turn-complete", status: "completed", startSeq: 3 });
    const assistantMessage = mkMessage({
      sessionId,
      turnId: "turn-complete",
      id: "m-assistant-1",
      role: "assistant",
    });
    const head = mkBoundedHead({
      sessionId,
      turns: [completedTurn],
      messages: [assistantMessage],
      lastEventSeq: 4,
      projectionRev: 7,
      activity: { is_working: false, last_turn_status: "completed" },
    });

    expect(
      shouldRepairSessionHeadReplace(
        {
          turnsHydrated: true,
          turns: [runningTurn, completedTurn],
          messages: [assistantMessage],
          freshness: "replica",
          loadState: "live",
          lastEventSeq: 4,
          projectionRev: 7,
          activity: { is_working: false, last_turn_status: "completed" },
        },
        head,
      ),
    ).toBe(true);
  });

  it("preserves existing transcript when a bounded head window is disjoint", () => {
    const sessionId = "session-disjoint-bounded-head";
    const previousTurn = mkTurn({ sessionId, turnId: "turn-old", status: "completed", startSeq: 1 });
    const previousMessage = mkMessage({
      sessionId,
      turnId: "turn-old",
      id: "m-old",
      role: "assistant",
    });
    const incomingTurn = mkTurn({ sessionId, turnId: "turn-new", status: "completed", startSeq: 9 });
    const incomingMessage = mkMessage({
      sessionId,
      turnId: "turn-new",
      id: "m-new",
      role: "user",
    });
    const head = mkBoundedHead({
      sessionId,
      turns: [incomingTurn],
      messages: [incomingMessage],
      lastEventSeq: 12,
      projectionRev: 12,
    });

    expect(
      shouldPreserveExistingTranscriptWindow(
        {
          turnsHydrated: true,
          turns: [previousTurn],
          messages: [previousMessage],
          freshness: "replica",
          loadState: "live",
          lastEventSeq: 8,
          projectionRev: 8,
          activity: { is_working: false, last_turn_status: "completed" },
        },
        head,
      ),
    ).toBe(true);
  });
});
