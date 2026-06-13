import { describe, expect, it } from "vitest";
import { publish } from "./snapshotProjection";
import { createInternalEntry, type SessionSupervisorSnapshot } from "./entryState";

describe("sessionSupervisor snapshotProjection", () => {
  it("projects support-owned tool state into threadProjection", () => {
    const entry = createInternalEntry("session-1", { transientSeqStart: 1, warmTtlMs: 60_000 });
    entry.turns = [{
      turn_id: "turn-1",
      session_id: "session-1",
      run_id: null,
      user_message_id: "message-1",
      status: "running",
      start_seq: 1,
      end_seq: null,
      started_at: "2026-04-08T12:00:00.000Z",
      updated_at: "2026-04-08T12:00:00.000Z",
      assistant_partial: "",
      thought_partial: "",
      metrics_json: null,
      tool_total: 1,
      tool_pending: 0,
      tool_running: 1,
      tool_completed: 0,
      tool_failed: 0,
    }];
    entry.messages = [];
    entry.events = [];
    entry.turnsRev = 2;
    entry.messagesRev = 3;
    entry.eventsRev = 4;
    entry.assistantStreamingRev = 5;
    entry.projectionRev = 6;
    entry.support.stateLoaded = true;
    entry.support.toolSummariesReady = true;
    entry.support.turnToolsByTurnId = {
      "turn-1": [{
        session_id: "session-1",
        tool_call_id: "tool-1",
        turn_id: "turn-1",
        tool_kind: "execute",
        provider_tool_name: "Bash",
        title: "Run pwd",
        subtitle: null,
        status: "running",
        input_json: { command: "pwd" },
        output_text: null,
        order_seq: 1,
        input_truncated: null,
        input_original_bytes: null,
        output_truncated: null,
        output_original_bytes: null,
        first_event_seq: 1,
        created_at: "2026-04-08T12:00:00.000Z",
        updated_at: "2026-04-08T12:00:00.000Z",
      }],
    };

    const host = {
      maxCachedSessions: 10,
      listeners: new Set<() => void>(),
      snapshot: { connection: "idle", sessions: {} } as SessionSupervisorSnapshot,
      entries: new Map([[entry.sessionId, entry]]),
    };

    publish.call(host);

    const projected = host.snapshot.sessions["session-1"]?.threadProjection;
    expect(projected?.loaded).toBe(true);
    expect(projected?.toolSummariesReady).toBe(true);
    expect(projected?.toolsByTurnId).toBe(entry.support.turnToolsByTurnId);
    expect(projected?.toolsByTurnId["turn-1"]?.[0]?.tool_call_id).toBe("tool-1");
    expect(projected?.projectionRev).toBe(6);
  });

  it("reuses the published session entry when transcript-facing inputs are unchanged", () => {
    const entry = createInternalEntry("session-1", { transientSeqStart: 1, warmTtlMs: 60_000 });
    entry.support.stateLoaded = true;
    entry.turns = [];
    entry.messages = [];
    entry.events = [];

    const host = {
      maxCachedSessions: 10,
      listeners: new Set<() => void>(),
      snapshot: { connection: "idle", sessions: {} } as SessionSupervisorSnapshot,
      entries: new Map([[entry.sessionId, entry]]),
    };

    publish.call(host);
    const firstPublished = host.snapshot.sessions["session-1"];

    publish.call(host);
    const secondPublished = host.snapshot.sessions["session-1"];

    expect(secondPublished).toBe(firstPublished);
    expect(secondPublished?.threadProjection).toBe(firstPublished?.threadProjection);
  });

  it("rebuilds threadProjection when transcript arrays mutate in place without revision bumps", () => {
    const entry = createInternalEntry("session-1", { transientSeqStart: 1, warmTtlMs: 60_000 });
    entry.support.stateLoaded = true;
    entry.turns = [{
      turn_id: "turn-1",
      session_id: "session-1",
      run_id: null,
      user_message_id: "message-1",
      status: "completed",
      start_seq: 1,
      end_seq: 2,
      started_at: "2026-04-08T12:00:00.000Z",
      updated_at: "2026-04-08T12:00:01.000Z",
      assistant_partial: "",
      thought_partial: "",
      metrics_json: null,
      tool_total: 0,
      tool_pending: 0,
      tool_running: 0,
      tool_completed: 0,
      tool_failed: 0,
    }];
    entry.messages = [{
      id: "message-1",
      session_id: "session-1",
      task_id: "task-1",
      turn_id: "turn-1",
      turn_sequence: 1,
      role: "assistant",
      content: "old content",
      created_at: "2026-04-08T12:00:01.000Z",
      delivery: "immediate",
      attachments: [],
    }];
    entry.events = [];
    entry.turnsRev = 2;
    entry.messagesRev = 3;
    entry.eventsRev = 4;
    entry.assistantStreamingRev = 5;
    entry.projectionRev = 6;

    const host = {
      maxCachedSessions: 10,
      listeners: new Set<() => void>(),
      snapshot: { connection: "idle", sessions: {} } as SessionSupervisorSnapshot,
      entries: new Map([[entry.sessionId, entry]]),
    };

    publish.call(host);
    const firstPublished = host.snapshot.sessions["session-1"];

    entry.turns.push({
      turn_id: "turn-2",
      session_id: "session-1",
      run_id: null,
      user_message_id: "message-2",
      status: "completed",
      start_seq: 3,
      end_seq: 4,
      started_at: "2026-04-08T12:00:02.000Z",
      updated_at: "2026-04-08T12:00:03.000Z",
      assistant_partial: "",
      thought_partial: "",
      metrics_json: null,
      tool_total: 0,
      tool_pending: 0,
      tool_running: 0,
      tool_completed: 0,
      tool_failed: 0,
    });
    entry.messages[0] = {
      ...entry.messages[0]!,
      content: "new content",
    };
    entry.messages.push({
      id: "message-2",
      session_id: "session-1",
      task_id: "task-1",
      turn_id: "turn-2",
      turn_sequence: 2,
      role: "assistant",
      content: "second content",
      created_at: "2026-04-08T12:00:03.000Z",
      delivery: "immediate",
      attachments: [],
    });

    publish.call(host);
    const secondPublished = host.snapshot.sessions["session-1"];
    const secondThreadProjection = secondPublished?.threadProjection;

    expect(secondPublished).not.toBe(firstPublished);
    expect(secondThreadProjection).not.toBe(firstPublished?.threadProjection);
    expect(secondThreadProjection).toBeDefined();
    if (!secondThreadProjection) {
      throw new Error("threadProjection should be defined after publish");
    }
    expect(secondThreadProjection.turns).toHaveLength(2);
    expect(secondThreadProjection.messages).toHaveLength(2);
    expect(secondThreadProjection.messages[0]?.content).toBe("new content");
    expect(secondThreadProjection.messages[1]?.content).toBe("second content");
  });

  it("skips listener notification when publish is a true no-op", () => {
    const entry = createInternalEntry("session-1", { transientSeqStart: 1, warmTtlMs: 60_000 });
    entry.support.stateLoaded = true;
    entry.turns = [];
    entry.messages = [];
    entry.events = [];

    let notifications = 0;
    const host = {
      maxCachedSessions: 10,
      listeners: new Set<() => void>([() => {
        notifications += 1;
      }]),
      snapshot: { connection: "idle", sessions: {} } as SessionSupervisorSnapshot,
      entries: new Map([[entry.sessionId, entry]]),
    };

    publish.call(host);
    expect(notifications).toBe(1);

    publish.call(host);
    expect(notifications).toBe(1);
  });

  it("keeps artifactsLoading false while cached artifacts refresh in the background", () => {
    const entry = createInternalEntry("session-1", { transientSeqStart: 1, warmTtlMs: 60_000 });
    entry.support.stateLoaded = false;
    entry.support.stateLoading = true;
    entry.support.artifacts = [{
      id: "artifact-1",
      session_id: "session-1",
      task_id: "task-1",
      workspace_id: "workspace-1",
      worktree_id: "worktree-1",
      name: "artifact.txt",
      mime_type: "text/plain",
      bytes: 12,
      absolute_path: "/tmp/artifact.txt",
      missing: false,
      created_at: "2026-04-08T12:00:00.000Z",
    }];

    const host = {
      maxCachedSessions: 10,
      listeners: new Set<() => void>(),
      snapshot: { connection: "idle", sessions: {} } as SessionSupervisorSnapshot,
      entries: new Map([[entry.sessionId, entry]]),
    };

    publish.call(host);

    const projected = host.snapshot.sessions["session-1"];
    expect(projected?.artifacts).toHaveLength(1);
    expect(projected?.artifactsLoading).toBe(false);
    expect(projected?.stateLoading).toBe(true);
    expect(projected?.stateLoaded).toBe(false);
  });
});
