import { describe, expect, it } from "vitest";
import type { SessionHeadSnapshot } from "../../api/client";
import { createInternalEntry } from "./entryState";
import { shouldSkipBoundedActiveSnapshotSeed } from "./config";

const makeBoundedHead = (sessionId: string): SessionHeadSnapshot => ({
  session: {
    id: sessionId,
    task_id: "task-1",
    workspace_id: "workspace-1",
    worktree_id: "worktree-1",
    provider_id: "codex",
    model_id: "gpt-5",
    title: "Session",
    agent_role: "implementer",
    status: "active",
    created_at: "2026-03-09T00:00:00.000Z",
    updated_at: "2026-03-09T00:00:00.000Z",
  },
  turns: [
    {
      turn_id: "turn-1",
      session_id: sessionId,
      run_id: null,
      user_message_id: "message-1",
      status: "completed",
      start_seq: 1,
      end_seq: 2,
      started_at: "2026-03-09T00:00:00.000Z",
      updated_at: "2026-03-09T00:00:00.000Z",
      assistant_partial: null,
      thought_partial: null,
      metrics_json: null,
      tool_total: 0,
      tool_pending: 0,
      tool_running: 0,
      tool_completed: 0,
      tool_failed: 0,
    },
  ],
  messages: [
    {
      id: "message-1",
      session_id: sessionId,
      task_id: "task-1",
      turn_id: "turn-1",
      role: "assistant",
      content: "hello",
      delivery: "immediate",
      created_at: "2026-03-09T00:00:00.000Z",
    },
  ],
  events: [],
  last_event_seq: 2,
  projection_rev: 2,
  state_rev: 2,
  has_more_turns: false,
  has_more_history: false,
  history_cursor: null,
  head_window: {
    turn_limit: 5,
    message_limit: 50,
    event_limit: 800,
    byte_limit: 200_000,
    turn_count: 1,
    message_count: 1,
    event_count: 0,
    bytes: 256,
    truncated: true,
  },
});

describe("session supervisor config", () => {
  it("does not let bounded active heads seed sessions already marked recovering by load state", () => {
    const entry = createInternalEntry("session-1", {
      transientSeqStart: 1,
      warmTtlMs: 1_000,
    });
    entry.loadState = "recovering";
    entry.freshness = "bootstrap";

    expect(shouldSkipBoundedActiveSnapshotSeed(entry, makeBoundedHead("session-1"))).toBe(true);
  });

  it("allows visible bounded heads to seed fresh empty bootstrap sessions", () => {
    const entry = createInternalEntry("session-1", {
      transientSeqStart: 1,
      warmTtlMs: 1_000,
    });

    expect(shouldSkipBoundedActiveSnapshotSeed(entry, makeBoundedHead("session-1"))).toBe(false);
  });
});
