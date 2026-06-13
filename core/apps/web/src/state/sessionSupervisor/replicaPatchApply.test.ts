import { describe, expect, it, vi } from "vitest";
import { applyReplicaPatches } from "./replicaPatchApply";
import { applyToolSummaries } from "./headProjection";
import { createInternalEntry } from "./entryState";
import type { SessionSupervisorReplicaPatchHost } from "./replicaPatchApply";
import type { SessionSupervisorHeadProjectionHost } from "./headProjection";
import type { Message, SessionTurn } from "../../api/client";

function createReplicaHost(entry: ReturnType<typeof createInternalEntry>): SessionSupervisorReplicaPatchHost {
  return {
    workspaceSnapshotState: null,
    getEntry: () => entry,
    ensureEntry: () => entry,
    resolveSessionMode: () => entry.mode ?? null,
    resetEntryProjectionForReplace: () => undefined,
    setSessionLoadState: () => undefined,
    setFatalError: () => undefined,
    applyAcpMetaFromEvents: () => false,
    applyGitStatusSnapshotFromEvents: () => false,
    syncStateCache: () => undefined,
    clearSupportLoadError: () => undefined,
    adoptLoadedSubagentInvocationsRevision: () => undefined,
    ensureProviderOptions: async () => undefined,
    ensureSubagentInvocations: async () => undefined,
    syncSupportLoadsForOpenSession: () => undefined,
    bumpTurnsRev: () => undefined,
  };
}

function createHeadHost(): SessionSupervisorHeadProjectionHost {
  return {
    workspaceSnapshotState: null,
    workspaceSessionHeadsById: new Map(),
    stateCacheBySessionId: new Map(),
    publish: vi.fn(),
    mergeTurns: () => undefined,
    mergeEvents: () => undefined,
    mergeMessages: () => undefined,
    applyAcpMeta: () => false,
    applyAcpMetaFromEvents: () => false,
    applyGitStatusSnapshotFromEvents: () => false,
    ensureProviderOptions: async () => undefined,
    resolveSessionMode: () => "active",
    setSessionLoadState: () => undefined,
    syncSupportLoadsForOpenSession: () => undefined,
    ensureThoughtCache: async () => undefined,
    adoptLoadedSubagentInvocationsRevision: () => undefined,
    clearSupportLoadError: () => undefined,
    bumpTurnsRev: () => undefined,
    bumpMessagesRev: () => undefined,
    bumpEventsRev: () => undefined,
  };
}

describe("replicaPatchApply", () => {
  it("does not mark the entry changed for no-op canonical transcript patches", () => {
    const entry = createInternalEntry("session-1", { transientSeqStart: 1, warmTtlMs: 60_000 });
    entry.turnsHydrated = true;
    entry.turns = [];
    entry.messages = [];
    entry.events = [];
    entry.assistantStreamingByTurnId = {};
    entry.turnsRev = 2;
    entry.messagesRev = 3;
    entry.eventsRev = 4;
    entry.assistantStreamingRev = 5;

    const host = createReplicaHost(entry);
    const result = applyReplicaPatches(host, [{
      sessionId: "session-1",
      op: "append",
      data: {
        appendMode: "metadata_update",
        turns: entry.turns,
        messages: entry.messages,
        events: entry.events,
        assistantStreamingByTurnId: entry.assistantStreamingByTurnId,
      },
    }]);

    expect(result.changed).toBe(false);
    expect(entry.turnsRev).toBe(2);
    expect(entry.messagesRev).toBe(3);
    expect(entry.eventsRev).toBe(4);
    expect(entry.assistantStreamingRev).toBe(5);
  });

  it("drops overlay-only stream patches when no supervisor entry exists", () => {
    const fallbackEntry = createInternalEntry("session-1", { transientSeqStart: 1, warmTtlMs: 60_000 });
    const ensureEntry = vi.fn(() => fallbackEntry);
    const host: SessionSupervisorReplicaPatchHost = {
      ...createReplicaHost(fallbackEntry),
      getEntry: () => undefined,
      ensureEntry,
    };

    const result = applyReplicaPatches(host, [{
      sessionId: "session-1",
      op: "append",
      data: {
        appendMode: "stream_delta",
        assistantStreamingByTurnId: {
          "turn-1": {
            content: "partial",
            providerMessageId: null,
            orderSeq: 2,
          },
        },
        assistantStreamingRev: 1,
      },
    }]);

    expect(result).toEqual({ changed: false, subscriptionCursorsChanged: false });
    expect(ensureEntry).not.toHaveBeenCalled();
  });

  it("merges live stream delta transcript arrays into the existing canonical window", () => {
    const entry = createInternalEntry("session-1", { transientSeqStart: 1, warmTtlMs: 60_000 });
    entry.turnsHydrated = true;
    entry.turns = [{
      turn_id: "turn-1",
      session_id: "session-1",
      run_id: "run-1",
      user_message_id: "message-1",
      status: "running",
      start_seq: 1,
      end_seq: null,
      started_at: "2026-04-29T00:00:00.000Z",
      updated_at: "2026-04-29T00:00:00.000Z",
      assistant_partial: null,
      thought_partial: null,
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
      role: "user",
      content: "hello",
      delivery: "immediate",
      created_at: "2026-04-29T00:00:00.000Z",
    }];
    entry.events = [{
      seq: 1,
      id: "event-1",
      session_id: "session-1",
      run_id: "run-1",
      turn_id: "turn-1",
      event_type: "turn_started",
      payload_json: {},
      created_at: "2026-04-29T00:00:00.000Z",
    }];

    const host = createReplicaHost(entry);
    const result = applyReplicaPatches(host, [{
      sessionId: "session-1",
      op: "append",
      data: {
        appendMode: "stream_delta",
        freshness: "authoritative",
        lastEventSeq: 2,
        turns: [{
          ...entry.turns[0]!,
          status: "completed",
          end_seq: 2,
          updated_at: "2026-04-29T00:00:01.000Z",
        }],
        messages: [{
          id: "message-2",
          session_id: "session-1",
          task_id: "task-1",
          turn_id: "turn-1",
          role: "assistant",
          content: "done",
          delivery: "immediate",
          created_at: "2026-04-29T00:00:01.000Z",
        }],
        events: [{
          seq: 2,
          id: "event-2",
          session_id: "session-1",
          run_id: "run-1",
          turn_id: "turn-1",
          event_type: "assistant_message_inserted",
          payload_json: { message_id: "message-2" },
          created_at: "2026-04-29T00:00:01.000Z",
        }],
      },
    }]);

    expect(result.changed).toBe(true);
    expect(entry.turns.map((turn) => [turn.turn_id, turn.status])).toEqual([["turn-1", "completed"]]);
    expect(entry.messages.map((message) => message.id)).toEqual(["message-1", "message-2"]);
    expect(entry.events.map((event) => event.id)).toEqual(["event-1", "event-2"]);
    expect(entry.lastEventSeq).toBe(2);
  });

  it("lets live stream delta turn updates clear non-cumulative tool counters", () => {
    const entry = createInternalEntry("session-1", { transientSeqStart: 1, warmTtlMs: 60_000 });
    entry.turnsHydrated = true;
    entry.turns = [{
      turn_id: "turn-1",
      session_id: "session-1",
      run_id: "run-1",
      user_message_id: "message-1",
      status: "running",
      start_seq: 1,
      end_seq: null,
      started_at: "2026-04-29T00:00:00.000Z",
      updated_at: "2026-04-29T00:00:00.000Z",
      assistant_partial: null,
      thought_partial: null,
      metrics_json: null,
      tool_total: 1,
      tool_pending: 1,
      tool_running: 1,
      tool_completed: 0,
      tool_failed: 0,
    }];

    const host = createReplicaHost(entry);
    const result = applyReplicaPatches(host, [{
      sessionId: "session-1",
      op: "append",
      data: {
        appendMode: "stream_delta",
        freshness: "authoritative",
        lastEventSeq: 2,
        turns: [{
          ...entry.turns[0]!,
          updated_at: "2026-04-29T00:00:01.000Z",
          tool_pending: 0,
          tool_running: 0,
          tool_completed: 1,
        }],
      },
    }]);

    expect(result.changed).toBe(true);
    expect(entry.turns[0]?.tool_total).toBe(1);
    expect(entry.turns[0]?.tool_pending).toBe(0);
    expect(entry.turns[0]?.tool_running).toBe(0);
    expect(entry.turns[0]?.tool_completed).toBe(1);
  });

  it("ignores stale stream delta patches that would regress an authoritative entry", () => {
    const entry = createInternalEntry("session-1", { transientSeqStart: 1, warmTtlMs: 60_000 });
    entry.freshness = "replica";
    entry.lastEventSeq = 8;
    entry.projectionRev = 8;
    entry.turnsHydrated = true;
    entry.turns = [{
      turn_id: "turn-1",
      session_id: "session-1",
      run_id: "run-1",
      user_message_id: "message-1",
      status: "completed",
      start_seq: 1,
      end_seq: 8,
      started_at: "2026-04-29T00:00:00.000Z",
      updated_at: "2026-04-29T00:00:08.000Z",
      assistant_partial: null,
      thought_partial: null,
      metrics_json: null,
      tool_total: 1,
      tool_pending: 0,
      tool_running: 0,
      tool_completed: 1,
      tool_failed: 0,
    }];
    entry.turnsRev = 4;

    const host = createReplicaHost(entry);
    const result = applyReplicaPatches(host, [{
      sessionId: "session-1",
      op: "append",
      data: {
        appendMode: "stream_delta",
        freshness: "authoritative",
        lastEventSeq: 3,
        projectionRev: 3,
        turns: [{
          ...entry.turns[0]!,
          status: "running",
          end_seq: null,
          updated_at: "2026-04-29T00:00:03.000Z",
          tool_pending: 1,
          tool_running: 1,
          tool_completed: 0,
        }],
      },
    }]);

    expect(result).toEqual({ changed: false, subscriptionCursorsChanged: false });
    expect(entry.lastEventSeq).toBe(8);
    expect(entry.projectionRev).toBe(8);
    expect(entry.turnsRev).toBe(4);
    expect(entry.turns[0]?.status).toBe("completed");
    expect(entry.turns[0]?.tool_pending).toBe(0);
    expect(entry.turns[0]?.tool_running).toBe(0);
    expect(entry.turns[0]?.tool_completed).toBe(1);
  });

  it("applies equal-version authoritative replacements when they repair stale live transcript state", () => {
    const entry = createInternalEntry("session-1", { transientSeqStart: 1, warmTtlMs: 60_000 });
    entry.freshness = "replica";
    entry.lastEventSeq = 8;
    entry.projectionRev = 8;
    entry.turnsHydrated = true;
    entry.activity = { is_working: true, last_turn_status: "running" };
    entry.turns = [{
      turn_id: "turn-1",
      session_id: "session-1",
      run_id: "run-1",
      user_message_id: "message-1",
      status: "running",
      start_seq: 1,
      end_seq: null,
      started_at: "2026-04-29T00:00:00.000Z",
      updated_at: "2026-04-29T00:00:03.000Z",
      assistant_partial: null,
      thought_partial: null,
      metrics_json: null,
      tool_total: 1,
      tool_pending: 1,
      tool_running: 1,
      tool_completed: 0,
      tool_failed: 0,
    }];
    entry.messages = [{
      id: "message-1",
      session_id: "session-1",
      task_id: "task-1",
      turn_id: "turn-1",
      role: "user",
      content: "hello",
      delivery: "immediate",
      created_at: "2026-04-29T00:00:00.000Z",
    }];

    const host = createReplicaHost(entry);
    const result = applyReplicaPatches(host, [{
      sessionId: "session-1",
      op: "replace",
      data: {
        replaceMode: "authoritative_replace",
        freshness: "authoritative",
        lastEventSeq: 8,
        projectionRev: 8,
        activity: { is_working: false, last_turn_status: "completed" },
        turns: [{
          ...entry.turns[0]!,
          status: "completed",
          end_seq: 8,
          updated_at: "2026-04-29T00:00:08.000Z",
          tool_pending: 0,
          tool_running: 0,
          tool_completed: 1,
        }],
        messages: [
          ...entry.messages,
          {
            id: "message-2",
            session_id: "session-1",
            task_id: "task-1",
            turn_id: "turn-1",
            role: "assistant",
            content: "done: hello",
            delivery: "immediate",
            created_at: "2026-04-29T00:00:08.000Z",
          },
        ],
        events: [{
          seq: 8,
          id: "event-turn-finished",
          session_id: "session-1",
          run_id: "run-1",
          turn_id: "turn-1",
          event_type: "turn_finished",
          payload_json: { status: "completed" },
          created_at: "2026-04-29T00:00:08.000Z",
        }],
      },
    }]);

    expect(result.changed).toBe(true);
    expect(entry.lastEventSeq).toBe(8);
    expect(entry.projectionRev).toBe(8);
    expect(entry.activity?.is_working).toBe(false);
    expect(entry.turns[0]?.status).toBe("completed");
    expect(entry.turns[0]?.tool_pending).toBe(0);
    expect(entry.turns[0]?.tool_running).toBe(0);
    expect(entry.turns[0]?.tool_completed).toBe(1);
    expect(entry.messages.map((message) => message.content)).toEqual(["hello", "done: hello"]);
  });

  it("applies equal-seq authoritative repairs with older projection cursors without regressing cursors", () => {
    const entry = createInternalEntry("session-1", { transientSeqStart: 1, warmTtlMs: 60_000 });
    entry.freshness = "replica";
    entry.lastEventSeq = 8;
    entry.projectionRev = 9;
    entry.turnsHydrated = true;
    entry.loading = true;
    entry.activity = { is_working: true, last_turn_status: "running" };
    entry.turns = [{
      turn_id: "turn-1",
      session_id: "session-1",
      run_id: "run-1",
      user_message_id: "message-1",
      status: "running",
      start_seq: 1,
      end_seq: null,
      started_at: "2026-04-29T00:00:00.000Z",
      updated_at: "2026-04-29T00:00:03.000Z",
      assistant_partial: null,
      thought_partial: null,
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
      role: "user",
      content: "hello",
      delivery: "immediate",
      created_at: "2026-04-29T00:00:00.000Z",
    }];

    const host = createReplicaHost(entry);
    const result = applyReplicaPatches(host, [{
      sessionId: "session-1",
      op: "replace",
      data: {
        replaceMode: "authoritative_replace",
        freshness: "authoritative",
        lastEventSeq: 8,
        projectionRev: 7,
        loading: false,
        activity: { is_working: false, last_turn_status: "completed" },
        turns: [{ ...entry.turns[0]!, status: "completed", end_seq: 8 }],
        messages: [{
          id: "message-2",
          session_id: "session-1",
          task_id: "task-1",
          turn_id: "turn-1",
          role: "assistant",
          content: "done: hello",
          delivery: "immediate",
          created_at: "2026-04-29T00:00:08.000Z",
        }],
      },
    }]);

    expect(result.changed).toBe(true);
    expect(entry.lastEventSeq).toBe(8);
    expect(entry.projectionRev).toBe(9);
    expect(entry.loading).toBe(false);
    expect(entry.messages.map((message) => message.content)).toEqual(["hello", "done: hello"]);
    expect(entry.activity?.is_working).toBe(false);
  });

  it("applies live stream delta message removals without a full message window", () => {
    const entry = createInternalEntry("session-1", { transientSeqStart: 1, warmTtlMs: 60_000 });
    entry.messages = [{
      id: "message-1",
      session_id: "session-1",
      task_id: "task-1",
      turn_id: "turn-1",
      role: "user",
      content: "queued",
      delivery: "queued",
      created_at: "2026-04-29T00:00:00.000Z",
    }];
    entry.queue = entry.messages;

    const host = createReplicaHost(entry);
    const result = applyReplicaPatches(host, [{
      sessionId: "session-1",
      op: "append",
      data: {
        appendMode: "stream_delta",
        freshness: "authoritative",
        lastEventSeq: 2,
        removedMessageIds: ["message-1"],
        messagesRev: 2,
      },
    }]);

    expect(result.changed).toBe(true);
    expect(entry.messages).toEqual([]);
    expect(entry.queue).toEqual([]);
    expect(entry.messagesRev).toBe(2);
  });

  it("does not re-anchor tombstoned messages from combined live stream deltas", () => {
    const entry = createInternalEntry("session-1", { transientSeqStart: 1, warmTtlMs: 60_000 });
    entry.turnsHydrated = true;
    entry.turns = [{
      turn_id: "turn-1",
      session_id: "session-1",
      run_id: "run-1",
      user_message_id: "message-1",
      status: "running",
      start_seq: 1,
      end_seq: null,
      started_at: "2026-04-29T00:00:00.000Z",
      updated_at: "2026-04-29T00:00:00.000Z",
      assistant_partial: null,
      thought_partial: null,
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
      role: "user",
      content: "queued",
      delivery: "queued",
      created_at: "2026-04-29T00:00:00.000Z",
    }];
    entry.queue = entry.messages;

    const host = createReplicaHost(entry);
    const result = applyReplicaPatches(host, [{
      sessionId: "session-1",
      op: "append",
      data: {
        appendMode: "stream_delta",
        freshness: "authoritative",
        lastEventSeq: 2,
        removedMessageIds: ["message-1"],
        turns: [{
          ...entry.turns[0]!,
          status: "completed",
          end_seq: 2,
          updated_at: "2026-04-29T00:00:01.000Z",
        }],
        messagesRev: 2,
      },
    }]);

    expect(result.changed).toBe(true);
    expect(entry.messages).toEqual([]);
    expect(entry.queue).toEqual([]);
    expect(entry.turns[0]?.status).toBe("completed");
  });

  it("does not republish duplicate tool summaries", () => {
    const entry = createInternalEntry("session-1", { transientSeqStart: 1, warmTtlMs: 60_000 });
    const summaries = [{
      session_id: "session-1",
      tool_call_id: "tool-1",
      turn_id: "turn-1",
      tool_kind: "execute",
      provider_tool_name: "Bash",
      title: "Run pwd",
      subtitle: null,
      status: "running",
      input_preview: { command: "pwd" },
      order_seq: 1,
      input_truncated: null,
      input_original_bytes: null,
      output_truncated: null,
      output_original_bytes: null,
      first_event_seq: 1,
      created_at: "2026-04-08T12:00:00.000Z",
      updated_at: "2026-04-08T12:00:00.000Z",
    }];
    entry.toolSummaries = summaries;
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
        summary_only: true,
      } as typeof entry.support.turnToolsByTurnId[string][number] & { summary_only: boolean }],
    };
    entry.support.turnToolsHydratedByTurnId = { "turn-1": false };

    const host = createHeadHost();
    applyToolSummaries.call(host, entry, summaries);

    expect(host.publish).not.toHaveBeenCalled();
  });

  it("preserves a prior local user-message anchor when a replace patch points the turn at a missing user message", () => {
    const entry = createInternalEntry("session-1", { transientSeqStart: 1, warmTtlMs: 60_000 });
    entry.turnsHydrated = true;
    entry.turns = [{
      turn_id: "turn-1",
      session_id: "session-1",
      run_id: null,
      user_message_id: "message-local-user",
      status: "running",
      start_seq: 1,
      end_seq: null,
      started_at: "2026-04-14T00:00:00.000Z",
      updated_at: "2026-04-14T00:00:00.000Z",
      assistant_partial: null,
      thought_partial: null,
      metrics_json: null,
      tool_total: 0,
      tool_pending: 0,
      tool_running: 0,
      tool_completed: 0,
      tool_failed: 0,
    }];
    entry.messages = [{
      id: "message-local-user",
      session_id: "session-1",
      task_id: "task-1",
      turn_id: "turn-1",
      role: "user",
      content: "optimistic first message",
      delivery: "immediate",
      created_at: "2026-04-14T00:00:00.000Z",
    }];
    entry.messagesRev = 1;
    entry.turnsRev = 1;

    const host = createReplicaHost(entry);
    const result = applyReplicaPatches(host, [{
      sessionId: "session-1",
      op: "replace",
      data: {
        freshness: "authoritative",
        turns: [{
          ...entry.turns[0]!,
          user_message_id: "message-server-missing",
          status: "completed",
          end_seq: 2,
          updated_at: "2026-04-14T00:00:05.000Z",
        }],
        messages: [{
          id: "message-assistant",
          session_id: "session-1",
          task_id: "task-1",
          turn_id: "turn-1",
          role: "assistant",
          content: "done: optimistic first message",
          delivery: "immediate",
          created_at: "2026-04-14T00:00:05.000Z",
        }],
        events: [],
        turnsHydrated: true,
        loading: false,
      },
    }]);

    expect(result.changed).toBe(true);
    expect(entry.turns[0]?.user_message_id).toBe("message-local-user");
    expect(entry.messages.map((message) => message.id)).toEqual([
      "message-local-user",
      "message-assistant",
    ]);
  });

  it("preserves terminal transcript history when a compact repair replace is disjoint", () => {
    const entry = createInternalEntry("session-1", { transientSeqStart: 1, warmTtlMs: 60_000 });
    const oldTurn: SessionTurn = {
      turn_id: "turn-old",
      session_id: "session-1",
      run_id: null,
      user_message_id: "message-old",
      status: "completed",
      start_seq: 1,
      end_seq: 2,
      started_at: "2026-04-14T00:00:00.000Z",
      updated_at: "2026-04-14T00:00:01.000Z",
      assistant_partial: null,
      thought_partial: null,
      metrics_json: null,
      tool_total: 0,
      tool_pending: 0,
      tool_running: 0,
      tool_completed: 0,
      tool_failed: 0,
    };
    const oldMessage: Message = {
      id: "message-old",
      session_id: "session-1",
      task_id: "task-1",
      turn_id: "turn-old",
      role: "assistant",
      content: "older visible transcript",
      delivery: "immediate",
      created_at: "2026-04-14T00:00:01.000Z",
    };
    const newTurn: SessionTurn = {
      ...oldTurn,
      turn_id: "turn-new",
      user_message_id: "message-new",
      start_seq: 20,
      end_seq: 21,
      started_at: "2026-04-14T00:00:20.000Z",
      updated_at: "2026-04-14T00:00:21.000Z",
    };
    const newMessage: Message = {
      ...oldMessage,
      id: "message-new",
      turn_id: "turn-new",
      role: "user",
      content: "new compact head message",
      created_at: "2026-04-14T00:00:20.000Z",
    };
    entry.turnsHydrated = true;
    entry.turns = [oldTurn];
    entry.messages = [oldMessage];
    entry.events = [];

    const resetEntryProjectionForReplace = vi.fn();
    const host = {
      ...createReplicaHost(entry),
      resetEntryProjectionForReplace,
    };

    const result = applyReplicaPatches(host, [{
      sessionId: "session-1",
      op: "replace",
      data: {
        replaceMode: "repair_replace",
        freshness: "authoritative",
        turns: [newTurn],
        messages: [newMessage],
        events: [],
        turnsHydrated: true,
        loading: false,
        lastEventSeq: 21,
        projectionRev: 21,
        headWindow: {
          turn_limit: 1,
          message_limit: 1,
          event_limit: 40,
          byte_limit: 4096,
          turn_count: 1,
          message_count: 1,
          event_count: 0,
          bytes: 512,
          truncated: true,
        },
      },
    }]);

    expect(result.changed).toBe(true);
    expect(resetEntryProjectionForReplace).not.toHaveBeenCalled();
    expect(entry.messages.map((message) => message.id)).toEqual(["message-old", "message-new"]);
    expect(entry.turns.map((turn) => turn.turn_id)).toEqual(["turn-old", "turn-new"]);
  });
});
