import { describe, expect, it, vi } from "vitest";
import type { Message, Session, SessionHeadSnapshot, SessionTurn } from "@ctx/types";
import type { InternalEntry } from "./entryState";
import {
  ingestWorkspaceEvent,
  syncActiveSnapshot,
  upsertWorkspaceSessionHead,
} from "./workspaceAuthority";
import type { SessionSupervisorWorkspaceEvent } from "./workspaceInputs";
import { markWorkspaceEventStreamSource } from "../workspaceEventTelemetry";

const now = "2026-05-05T00:00:00.000Z";

const session: Session = {
  id: "session-1",
  task_id: "task-1",
  workspace_id: "workspace-1",
  worktree_id: "worktree-1",
  provider_id: "fake",
  model_id: "fake-model",
  title: "Session",
  agent_role: "assistant",
  status: "active",
  created_at: now,
  updated_at: now,
};

const userMessage: Message = {
  id: "message-user",
  session_id: "session-1",
  task_id: "task-1",
  turn_id: "turn-1",
  role: "user",
  content: "hello",
  delivery: "immediate",
  created_at: now,
};

const assistantMessage: Message = {
  id: "message-assistant",
  session_id: "session-1",
  task_id: "task-1",
  turn_id: "turn-1",
  role: "assistant",
  content: "done: hello",
  delivery: "immediate",
  created_at: now,
};

const runningTurn: SessionTurn = {
  turn_id: "turn-1",
  session_id: "session-1",
  run_id: null,
  user_message_id: "message-user",
  status: "running",
  start_seq: 1,
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

const completedTurn: SessionTurn = {
  ...runningTurn,
  status: "completed",
  end_seq: 8,
  tool_running: 0,
  tool_completed: 1,
};

describe("workspaceAuthority", () => {
  const makeStaleEntry = (): InternalEntry =>
    ({
      sessionId: "session-1",
      loadState: "live",
      freshness: "authoritative",
      turnsHydrated: true,
      turns: [runningTurn],
      messages: [userMessage],
      events: [],
      lastEventSeq: 3,
      activity: { is_working: true, last_turn_status: "running" },
    }) as unknown as InternalEntry;

  const completedHead: SessionHeadSnapshot = {
    session,
    turns: [completedTurn],
    messages: [userMessage, assistantMessage],
    events: [],
    activity: { is_working: false, last_turn_status: "completed" },
    last_event_seq: 8,
    has_more_turns: false,
    has_more_history: false,
    history_cursor: null,
  };

  const makeDeltaEvent = (sessionId = "session-1"): SessionSupervisorWorkspaceEvent =>
    ({
      type: "session_head_delta",
      workspace_id: "workspace-1",
      snapshot_rev: 1,
      delta: {
        session_id: sessionId,
        last_event_seq: 8,
        projection_rev: 8,
        state_rev: 8,
        turn: { ...completedTurn, session_id: sessionId },
        message: { ...assistantMessage, session_id: sessionId },
        activity: { is_working: false, last_turn_status: "completed" },
      },
    }) as unknown as SessionSupervisorWorkspaceEvent;

  const makeGapEvent = (sessionId = "session-1"): SessionSupervisorWorkspaceEvent =>
    ({
      type: "session_gap",
      workspace_id: "workspace-1",
      snapshot_rev: 1,
      session_id: sessionId,
      after_seq: 8,
      reason: "stream_seq_gap",
    }) as unknown as SessionSupervisorWorkspaceEvent;

  const makeIngestHost = ({
    entries = new Map<string, InternalEntry>(),
    activeTaskSessionIds = [],
    workspaceActivePrimarySessionIds = [],
    warmSessionIds = [],
    replicaDispatch = vi.fn(),
  }: {
    entries?: Map<string, InternalEntry>;
    activeTaskSessionIds?: string[];
    workspaceActivePrimarySessionIds?: string[];
    warmSessionIds?: string[];
    replicaDispatch?: ReturnType<typeof vi.fn>;
  } = {}) =>
    ({
      entries,
      getActiveTaskSessionIds: () => activeTaskSessionIds,
      getWorkspaceActivePrimarySessionIds: () => workspaceActivePrimarySessionIds,
      getWarmSessionIds: () => warmSessionIds,
      replicaDispatch,
      publish: vi.fn(),
      emitSubscribedSessions: vi.fn(),
      clearTaskThoughts: vi.fn(async () => {}),
      setSessionLoadState: vi.fn(),
    }) as unknown as Parameters<typeof ingestWorkspaceEvent>[0];

  it("repair-replaces an open stale foreground entry with a newer authoritative head", () => {
    let heads = new Map<string, SessionHeadSnapshot>();
    const entry = makeStaleEntry();
    const replicaDispatch = vi.fn();
    const host = {
      getWorkspaceSessionHeadsById: () => heads,
      setWorkspaceSessionHeadsById: (next: Map<string, SessionHeadSnapshot>) => {
        heads = next;
      },
      entries: new Map([["session-1", entry]]),
      replicaDispatch,
      syncSupportLoadsForOpenSession: vi.fn(),
    } as unknown as Parameters<typeof upsertWorkspaceSessionHead>[0];
    upsertWorkspaceSessionHead(host, "session-1", completedHead);

    expect(replicaDispatch).toHaveBeenCalledWith({
      type: "seed_head",
      sessionId: "session-1",
      head: completedHead,
      mode: "repair_replace",
    });
  });

  it("repair-replaces with a bounded but complete head window", () => {
    let heads = new Map<string, SessionHeadSnapshot>();
    const entry = makeStaleEntry();
    const replicaDispatch = vi.fn();
    const completeBoundedHead: SessionHeadSnapshot = {
      ...completedHead,
      head_window: {
        turn_limit: 60,
        message_limit: 200,
        event_limit: 200,
        byte_limit: 256000,
        turn_count: 1,
        message_count: 2,
        event_count: 6,
        bytes: 4679,
        truncated: false,
      },
    };
    const host = {
      getWorkspaceSessionHeadsById: () => heads,
      setWorkspaceSessionHeadsById: (next: Map<string, SessionHeadSnapshot>) => {
        heads = next;
      },
      entries: new Map([["session-1", entry]]),
      replicaDispatch,
      syncSupportLoadsForOpenSession: vi.fn(),
    } as unknown as Parameters<typeof upsertWorkspaceSessionHead>[0];

    upsertWorkspaceSessionHead(host, "session-1", completeBoundedHead);

    expect(replicaDispatch).toHaveBeenCalledWith({
      type: "seed_head",
      sessionId: "session-1",
      head: completeBoundedHead,
      mode: "repair_replace",
    });
  });

  it("repair-replaces complete heads that still advertise older pageable history", () => {
    let heads = new Map<string, SessionHeadSnapshot>();
    const entry = makeStaleEntry();
    const replicaDispatch = vi.fn();
    const completeHeadWithHistory: SessionHeadSnapshot = {
      ...completedHead,
      has_more_history: true,
      history_cursor: 1,
      head_window: {
        turn_limit: 60,
        message_limit: 200,
        event_limit: 200,
        byte_limit: 256000,
        turn_count: 1,
        message_count: 2,
        event_count: 6,
        bytes: 4679,
        truncated: false,
      },
    };
    const host = {
      getWorkspaceSessionHeadsById: () => heads,
      setWorkspaceSessionHeadsById: (next: Map<string, SessionHeadSnapshot>) => {
        heads = next;
      },
      entries: new Map([["session-1", entry]]),
      replicaDispatch,
      syncSupportLoadsForOpenSession: vi.fn(),
    } as unknown as Parameters<typeof upsertWorkspaceSessionHead>[0];

    upsertWorkspaceSessionHead(host, "session-1", completeHeadWithHistory);

    expect(replicaDispatch).toHaveBeenCalledWith({
      type: "seed_head",
      sessionId: "session-1",
      head: completeHeadWithHistory,
      mode: "repair_replace",
    });
  });

  it("repair-replaces with an overlapping partial head window that advances the foreground tail", () => {
    let heads = new Map<string, SessionHeadSnapshot>();
    const entry = makeStaleEntry();
    const replicaDispatch = vi.fn();
    const partialHead: SessionHeadSnapshot = {
      ...completedHead,
      has_more_turns: true,
      has_more_history: true,
      head_window: {
        turn_limit: 60,
        message_limit: 200,
        event_limit: 200,
        byte_limit: 256000,
        turn_count: 60,
        message_count: 200,
        event_count: 200,
        bytes: 256000,
        truncated: true,
      },
    };
    const host = {
      getWorkspaceSessionHeadsById: () => heads,
      setWorkspaceSessionHeadsById: (next: Map<string, SessionHeadSnapshot>) => {
        heads = next;
      },
      entries: new Map([["session-1", entry]]),
      replicaDispatch,
      syncSupportLoadsForOpenSession: vi.fn(),
    } as unknown as Parameters<typeof upsertWorkspaceSessionHead>[0];

    upsertWorkspaceSessionHead(host, "session-1", partialHead);

    expect(replicaDispatch).toHaveBeenCalledWith({
      type: "seed_head",
      sessionId: "session-1",
      head: partialHead,
      mode: "repair_replace",
    });
  });

  it("does not repair-replace with a disjoint partial head window", () => {
    let heads = new Map<string, SessionHeadSnapshot>();
    const entry = makeStaleEntry();
    const replicaDispatch = vi.fn();
    const partialHead: SessionHeadSnapshot = {
      ...completedHead,
      turns: [{ ...completedTurn, turn_id: "turn-2", user_message_id: "message-user-2" }],
      messages: [
        { ...userMessage, id: "message-user-2", turn_id: "turn-2", content: "new user" },
        { ...assistantMessage, id: "message-assistant-2", turn_id: "turn-2", content: "new assistant" },
      ],
      has_more_turns: true,
      has_more_history: true,
      head_window: {
        turn_limit: 60,
        message_limit: 200,
        event_limit: 200,
        byte_limit: 256000,
        turn_count: 60,
        message_count: 200,
        event_count: 200,
        bytes: 256000,
        truncated: true,
      },
    };
    const host = {
      getWorkspaceSessionHeadsById: () => heads,
      setWorkspaceSessionHeadsById: (next: Map<string, SessionHeadSnapshot>) => {
        heads = next;
      },
      entries: new Map([["session-1", entry]]),
      replicaDispatch,
      syncSupportLoadsForOpenSession: vi.fn(),
    } as unknown as Parameters<typeof upsertWorkspaceSessionHead>[0];

    upsertWorkspaceSessionHead(host, "session-1", partialHead);

    expect(replicaDispatch).not.toHaveBeenCalled();
  });

  it("does not forward unretained active stream deltas to the session replica", () => {
    const replicaDispatch = vi.fn();
    const host = makeIngestHost({ replicaDispatch });

    ingestWorkspaceEvent(host, makeDeltaEvent("session-background"));

    expect(replicaDispatch).not.toHaveBeenCalled();
  });

  it("does not spend transcript replica work on auto-subscribed active-primary sessions until retained", () => {
    const replicaDispatch = vi.fn();
    const event = makeDeltaEvent("session-active-primary");
    const host = makeIngestHost({
      workspaceActivePrimarySessionIds: ["session-active-primary"],
      replicaDispatch,
    });

    ingestWorkspaceEvent(host, event);

    expect(replicaDispatch).not.toHaveBeenCalled();
  });

  it("forwards retained foreground stream deltas to the session replica", () => {
    const replicaDispatch = vi.fn();
    const event = makeDeltaEvent("session-foreground");
    markWorkspaceEventStreamSource(event, "replay");
    const host = makeIngestHost({
      activeTaskSessionIds: ["session-foreground"],
      replicaDispatch,
    });

    ingestWorkspaceEvent(host, event);

    expect(replicaDispatch).toHaveBeenCalledWith({
      type: "workspace_event",
      event,
      lane: "foreground",
      receivedAtMs: null,
      streamSource: "replay",
    });
  });

  it("does not forward live warm stream deltas to the transcript replica", () => {
    const replicaDispatch = vi.fn();
    const event = makeDeltaEvent("session-warm");
    markWorkspaceEventStreamSource(event, "live");
    const host = makeIngestHost({
      warmSessionIds: ["session-warm"],
      replicaDispatch,
    });

    ingestWorkspaceEvent(host, event);

    expect(replicaDispatch).not.toHaveBeenCalled();
  });

  it("keeps replay warm stream deltas on the workspace replica lane", () => {
    const replicaDispatch = vi.fn();
    const event = makeDeltaEvent("session-warm");
    markWorkspaceEventStreamSource(event, "replay");
    const host = makeIngestHost({
      warmSessionIds: ["session-warm"],
      replicaDispatch,
    });

    ingestWorkspaceEvent(host, event);

    expect(replicaDispatch).toHaveBeenCalledWith({
      type: "workspace_event",
      event,
      lane: "workspace",
      receivedAtMs: null,
      streamSource: "replay",
    });
  });

  it("routes replay session_gap recovery for the retained foreground session to the workspace lane", () => {
    const replicaDispatch = vi.fn();
    const event = makeGapEvent("session-foreground");
    markWorkspaceEventStreamSource(event, "replay");
    const host = makeIngestHost({
      entries: new Map([["session-foreground", makeStaleEntry()]]),
      activeTaskSessionIds: ["session-foreground"],
      replicaDispatch,
    });

    ingestWorkspaceEvent(host, event);

    expect(replicaDispatch).toHaveBeenCalledWith({
      type: "workspace_event",
      event,
      lane: "workspace",
      receivedAtMs: null,
      streamSource: "replay",
    });
  });

  it("keeps live session_gap recovery for the retained foreground session on the foreground lane", () => {
    const replicaDispatch = vi.fn();
    const event = makeGapEvent("session-foreground");
    markWorkspaceEventStreamSource(event, "live");
    const host = makeIngestHost({
      entries: new Map([["session-foreground", makeStaleEntry()]]),
      activeTaskSessionIds: ["session-foreground"],
      replicaDispatch,
    });

    ingestWorkspaceEvent(host, event);

    expect(replicaDispatch).toHaveBeenCalledWith({
      type: "workspace_event",
      event,
      lane: "foreground",
      receivedAtMs: null,
      streamSource: "live",
    });
  });

  it("repair-replaces stale entries during active snapshot sync", () => {
    const entry = makeStaleEntry();
    const replicaDispatch = vi.fn();
    const host = {
      getWorkspaceSessionHeadsById: () => new Map([["session-1", completedHead]]),
      ensureEntry: () => entry,
      replicaDispatch,
    } as unknown as Parameters<typeof syncActiveSnapshot>[0];

    syncActiveSnapshot(host, {
      workspaceId: "workspace-1",
      initialized: true,
      liveSnapshotApplied: true,
      connection: "connected",
      tasksById: {
        "task-1": {
          id: "task-1",
          task: {
            id: "task-1",
            workspace_id: "workspace-1",
            title: "Task",
            status: "running",
            created_at: now,
            updated_at: now,
            last_activity_at: now,
            archived_at: null,
            assistant_seen_at: null,
            last_assistant_message_at: now,
            primary_session_id: "session-1",
          },
          sessions: [],
          primarySessionId: "session-1",
          primarySessionHead: null,
          sort_at: now,
          sortAtMs: Date.parse(now),
        },
      },
      activeIds: ["task-1"],
      archivedIds: [],
      totalActive: 1,
      totalArchived: 0,
      archivedRev: 0,
      fetchState: { active: "idle", archived: "idle" },
      hasMoreActive: false,
      hasMoreArchived: false,
      archivedLoaded: false,
    });

    expect(replicaDispatch).toHaveBeenCalledWith({
      type: "seed_head",
      sessionId: "session-1",
      head: completedHead,
      mode: "repair_replace",
    });
  });
});
