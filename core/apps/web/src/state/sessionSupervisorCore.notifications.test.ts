import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Message, Session, SessionEvent, SessionHead, SessionTurn } from "../api/client";
import type { SessionReplicaPatch } from "./sessionReplicaProtocol";
import { SessionSupervisor, type SessionCacheEntry } from "./sessionSupervisorCore";
import {
  applyHead,
  type SessionSupervisorHeadProjectionHost,
} from "./sessionSupervisor/headProjection";
import type { InternalEntry } from "./sessionSupervisor/entryState";
import {
  applyReplicaPatches,
  type SessionSupervisorReplicaPatchHost,
} from "./sessionSupervisor/replicaPatchApply";

const sendDesktopNotification = vi.hoisted(() => vi.fn());
const isAppInForeground = vi.hoisted(() => vi.fn());
const getClientSettingsState = vi.hoisted(() => vi.fn());
const ensureThoughtCacheMock = vi.hoisted(() => vi.fn(async () => {}));

vi.mock("../utils/desktopNotifications", () => ({
  sendDesktopNotification,
}));

vi.mock("../utils/windowFocus", () => ({
  isAppInForeground,
}));

vi.mock("./clientSettings", () => ({
  getClientSettingsState,
}));

vi.mock("./sessionSupervisor/thoughtCache", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./sessionSupervisor/thoughtCache")>();
  return {
    ...actual,
    ensureThoughtCache: ensureThoughtCacheMock,
  };
});

const baseIso = "2024-01-01T00:00:00.000Z";

type SessionSupervisorInternals = {
  ensureEntry: (sessionId: string) => SessionCacheEntry;
  ensureTurnFromEvent: (entry: SessionCacheEntry, event: SessionEvent) => boolean;
  applyEventToTurns: (entry: SessionCacheEntry, event: SessionEvent) => boolean;
};

type AppendPatchData = Omit<Extract<SessionReplicaPatch, { op: "append" }>["data"], "appendMode">;
type ReplacePatchData = Extract<SessionReplicaPatch, { op: "replace" }>["data"];

const asSupervisorInternals = (supervisor: SessionSupervisor): SessionSupervisorInternals =>
  supervisor as unknown as SessionSupervisorInternals;

const asReplicaPatchHost = (supervisor: SessionSupervisor): SessionSupervisorReplicaPatchHost =>
  supervisor as unknown as SessionSupervisorReplicaPatchHost;

const asHeadHost = (supervisor: SessionSupervisor): SessionSupervisorHeadProjectionHost =>
  supervisor as unknown as SessionSupervisorHeadProjectionHost;

const asInternalEntry = (entry: SessionCacheEntry): InternalEntry =>
  entry as unknown as InternalEntry;

const makeWorkspaceSnapshot = (taskTitle: string) => ({
  workspaceId: "workspace-1",
  initialized: true,
  liveSnapshotApplied: true,
  connection: "connected" as const,
  tasksById: {
    "task-1": {
      id: "task-1",
      task: {
        id: "task-1",
        workspace_id: "workspace-1",
        title: taskTitle,
        status: "active",
        primary_session_id: "session-1",
        primary_worktree_id: "worktree-1",
        created_at: baseIso,
        updated_at: baseIso,
      },
      sessions: [],
      sortAtMs: Date.parse(baseIso),
    },
  },
  activeIds: ["task-1"],
  archivedIds: [],
  totalActive: 1,
  totalArchived: 0,
  archivedRev: 0,
  fetchState: {
    active: "idle" as const,
    archived: "idle" as const,
  },
  hasMoreActive: false,
  hasMoreArchived: false,
  archivedLoaded: true,
});

const buildTurn = (
  turnId: string,
  status: SessionTurn["status"],
  overrides?: Partial<SessionTurn>,
): SessionTurn => ({
  turn_id: turnId,
  session_id: "session-1",
  run_id: null,
  user_message_id: null,
  status,
  start_seq: 1,
  end_seq: status === "running" || status === "queued" ? null : 2,
  started_at: baseIso,
  updated_at: status === "running" || status === "queued" ? baseIso : "2024-01-01T00:00:10.000Z",
  assistant_partial: "",
  thought_partial: "",
  metrics_json: null,
  tool_total: 0,
  tool_pending: 0,
  tool_running: 0,
  tool_completed: 0,
  tool_failed: 0,
  ...overrides,
});

const applyAppendPatch = (
  supervisor: SessionSupervisor,
  data: AppendPatchData,
  appendMode: Extract<SessionReplicaPatch, { op: "append" }>["data"]["appendMode"] = "stream_delta",
) => {
  applyReplicaPatches(asReplicaPatchHost(supervisor), [{
    op: "append",
    sessionId: "session-1",
    data: {
      appendMode,
      ...data,
    },
  }]);
};

const applyReplacePatch = (
  supervisor: SessionSupervisor,
  data: ReplacePatchData,
) => {
  applyReplicaPatches(asReplicaPatchHost(supervisor), [{
    op: "replace",
    sessionId: "session-1",
    data,
  }]);
};

const setupSupervisorWithSession = (sessionOverrides?: Partial<Session>) => {
  const supervisor = new SessionSupervisor();
  const internals = asSupervisorInternals(supervisor);
  const entry = internals.ensureEntry("session-1");
  const session: Session = {
    id: "session-1",
    task_id: "task-1",
    workspace_id: "workspace-1",
    worktree_id: "worktree-1",
    provider_id: "codex",
    model_id: "gpt-5",
    agent_role: "implementer",
    status: "active",
    title: "Demo session",
    created_at: baseIso,
    ...sessionOverrides,
  };
  entry.session = session;
  return { supervisor, entry, internals };
};

const setupLegacyTurnEntry = () => {
  const { supervisor, entry, internals } = setupSupervisorWithSession();
  const startEvent: SessionEvent = {
    seq: 1,
    id: "ev-1",
    session_id: "session-1",
    turn_id: "turn-1",
    event_type: "turn_started",
    payload_json: {},
    created_at: baseIso,
  };
  internals.ensureTurnFromEvent(entry, startEvent);
  return { supervisor, entry, internals };
};

describe("SessionSupervisor turn notifications", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    getClientSettingsState.mockReturnValue({
      loaded: true,
      settings: {
        v: 3,
        desktopNotifications: {
          turnCompleted: true,
          turnFailed: true,
          badgeUnreadCount: true,
        },
        telemetry: {
          clientEnabled: true,
        },
      },
    });
    isAppInForeground.mockReturnValue(false);
  });

  it("sends a notification when a live stream_delta completes while unfocused", () => {
    const { supervisor } = setupSupervisorWithSession();
    supervisor.setWorkspaceSnapshotState(makeWorkspaceSnapshot("Implement desktop notifications"));

    applyAppendPatch(supervisor, {
      turns: [buildTurn("turn-1", "completed")],
      messages: [{
        id: "message-1",
        session_id: "session-1",
        task_id: "task-1",
        turn_id: "turn-1",
        role: "assistant",
        content: "Added Dock badge aggregation across open workspaces.",
        delivery: "immediate",
        created_at: baseIso,
      } satisfies Message],
      events: [],
      turnsRev: 1,
      messagesRev: 1,
      eventsRev: 1,
    });

    expect(sendDesktopNotification).toHaveBeenCalledWith({
      kind: "turn_completed",
      title: "Implement desktop notifications",
      body: "Added Dock badge aggregation across open workspaces.",
      workspaceId: "workspace-1",
      taskId: "task-1",
      sessionId: "session-1",
    });
  });

  it("skips live notifications when the app is focused", () => {
    isAppInForeground.mockReturnValue(true);
    const { supervisor } = setupSupervisorWithSession();
    supervisor.setWorkspaceSnapshotState(makeWorkspaceSnapshot("Implement desktop notifications"));

    applyAppendPatch(supervisor, {
      turns: [buildTurn("turn-1", "completed")],
      events: [],
      messages: [],
      turnsRev: 1,
      eventsRev: 1,
      messagesRev: 1,
    });

    expect(sendDesktopNotification).not.toHaveBeenCalled();
  });

  it("skips live notifications when the setting is disabled", () => {
    getClientSettingsState.mockReturnValue({
      loaded: true,
      settings: {
        v: 3,
        desktopNotifications: {
          turnCompleted: false,
          turnFailed: true,
          badgeUnreadCount: true,
        },
        telemetry: {
          clientEnabled: true,
        },
      },
    });
    const { supervisor } = setupSupervisorWithSession();
    supervisor.setWorkspaceSnapshotState(makeWorkspaceSnapshot("Implement desktop notifications"));

    applyAppendPatch(supervisor, {
      turns: [buildTurn("turn-1", "completed")],
      events: [],
      messages: [],
      turnsRev: 1,
      eventsRev: 1,
      messagesRev: 1,
    });

    expect(sendDesktopNotification).not.toHaveBeenCalled();
  });

  it("uses the turn failure message when a live failed turn has no assistant message", () => {
    const { supervisor } = setupSupervisorWithSession();
    supervisor.setWorkspaceSnapshotState(makeWorkspaceSnapshot("Fix login race"));

    applyAppendPatch(supervisor, {
      turns: [buildTurn("turn-1", "failed", {
        failure: {
          message: "OAuth token has expired. Please reconnect.",
        },
      })],
      events: [],
      messages: [],
      turnsRev: 1,
      eventsRev: 1,
      messagesRev: 1,
    });

    expect(sendDesktopNotification).toHaveBeenCalledWith({
      kind: "turn_failed",
      title: "Fix login race",
      body: "OAuth token has expired. Please reconnect.",
      workspaceId: "workspace-1",
      taskId: "task-1",
      sessionId: "session-1",
    });
  });

  it("does not notify for head_refresh append patches", () => {
    const { supervisor } = setupSupervisorWithSession();
    supervisor.setWorkspaceSnapshotState(makeWorkspaceSnapshot("Fix login race"));

    applyAppendPatch(supervisor, {
      turns: [buildTurn("turn-1", "completed")],
      events: [],
      messages: [{
        id: "message-1",
        session_id: "session-1",
        task_id: "task-1",
        turn_id: "turn-1",
        role: "assistant",
        content: "This should stay silent.",
        delivery: "immediate",
        created_at: baseIso,
      }],
      turnsRev: 1,
      eventsRev: 1,
      messagesRev: 1,
    }, "head_refresh");

    expect(sendDesktopNotification).not.toHaveBeenCalled();
  });

  it("does not notify for bootstrap replace patches", () => {
    const { supervisor } = setupSupervisorWithSession();
    supervisor.setWorkspaceSnapshotState(makeWorkspaceSnapshot("Fix login race"));

    applyReplacePatch(supervisor, {
      replaceMode: "bootstrap_seed",
      turns: [buildTurn("turn-1", "completed")],
      events: [],
      messages: [{
        id: "message-1",
        session_id: "session-1",
        task_id: "task-1",
        turn_id: "turn-1",
        role: "assistant",
        content: "This should stay silent.",
        delivery: "immediate",
        created_at: baseIso,
      }],
      turnsRev: 1,
      eventsRev: 1,
      messagesRev: 1,
    });

    expect(sendDesktopNotification).not.toHaveBeenCalled();
  });

  it("does not notify for repair replace patches", () => {
    const { supervisor } = setupSupervisorWithSession();
    supervisor.setWorkspaceSnapshotState(makeWorkspaceSnapshot("Fix login race"));

    applyReplacePatch(supervisor, {
      replaceMode: "repair_replace",
      turns: [buildTurn("turn-1", "completed")],
      events: [],
      messages: [{
        id: "message-1",
        session_id: "session-1",
        task_id: "task-1",
        turn_id: "turn-1",
        role: "assistant",
        content: "This should stay silent.",
        delivery: "immediate",
        created_at: baseIso,
      }],
      turnsRev: 1,
      eventsRev: 1,
      messagesRev: 1,
    });

    expect(sendDesktopNotification).not.toHaveBeenCalled();
  });

  it("does not notify during authoritative head hydration", () => {
    const { supervisor, entry } = setupSupervisorWithSession();
    supervisor.setWorkspaceSnapshotState(makeWorkspaceSnapshot("Fix login race"));
    const head: SessionHead = {
      session: entry.session as Session,
      turns: [buildTurn("turn-1", "completed")],
      events: [],
      messages: [{
        id: "message-1",
        session_id: "session-1",
        task_id: "task-1",
        turn_id: "turn-1",
        role: "assistant",
        content: "This should stay silent.",
        delivery: "immediate",
        created_at: baseIso,
      }],
      last_event_seq: 2,
      has_more_turns: false,
    };

    applyHead.call(asHeadHost(supervisor), asInternalEntry(entry), head, {
      freshness: "authoritative",
    });

    expect(sendDesktopNotification).not.toHaveBeenCalled();
  });

  it("does not notify for subagent stream_delta completions", () => {
    const { supervisor } = setupSupervisorWithSession({
      parent_session_id: "session-root",
      relationship: "sub_agent",
    });
    supervisor.setWorkspaceSnapshotState(makeWorkspaceSnapshot("Fix login race"));

    applyAppendPatch(supervisor, {
      turns: [buildTurn("turn-1", "completed")],
      events: [],
      messages: [],
      turnsRev: 1,
      eventsRev: 1,
      messagesRev: 1,
    });

    expect(sendDesktopNotification).not.toHaveBeenCalled();
  });

  it("direct legacy event projection does not notify", () => {
    const { supervisor, entry, internals } = setupLegacyTurnEntry();
    supervisor.setWorkspaceSnapshotState(makeWorkspaceSnapshot("Fix login race"));
    const finishEvent: SessionEvent = {
      seq: 2,
      id: "ev-2",
      session_id: "session-1",
      turn_id: "turn-1",
      event_type: "turn_finished",
      payload_json: {},
      created_at: "2024-01-01T00:00:10.000Z",
    };

    const changed = internals.applyEventToTurns(entry, finishEvent);

    expect(changed).toBe(true);
    expect(sendDesktopNotification).not.toHaveBeenCalled();
  });
});
