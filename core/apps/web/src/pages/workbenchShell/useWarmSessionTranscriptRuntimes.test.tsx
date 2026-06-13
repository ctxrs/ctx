import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Session } from "../../api/client";
import type { WorkbenchListItem } from "../sessionView";
import type { SessionCacheEntry, SessionSupervisorSnapshot } from "../../state/sessionSupervisor";
import type { WorkspaceActiveSnapshotState } from "../../state/workspaceActiveSnapshotStore";
import type { SessionThreadProjection } from "../../state/sessionThreadProjection/types";
import {
  buildSessionPretextRuntimeLayoutKey,
  getOrCreateSessionPretextRuntime,
  createDefaultSessionTranscriptUiState,
  primeSessionPretextRuntime,
  readSessionPretextRuntimePreparedState,
  resetSessionPretextRuntimeCache,
} from "../sessionThread/pretextSessionRuntimeCache";
import {
  noteSessionTranscriptWarmVerbosity,
  noteSessionTranscriptWarmViewport,
} from "../sessionThread/sessionTranscriptWarmState";
import { useWarmSessionTranscriptRuntimes } from "./useWarmSessionTranscriptRuntimes";

const primeWarmWorkbenchThreadViewModelMock = vi.hoisted(() => vi.fn());
const pruneWarmWorkbenchThreadViewModelCacheMock = vi.hoisted(() => vi.fn());

vi.mock("../workbenchThreadViewModelWarmCache", () => ({
  primeWarmWorkbenchThreadViewModel: (...args: unknown[]) =>
    primeWarmWorkbenchThreadViewModelMock(...args),
  pruneWarmWorkbenchThreadViewModelCache: (...args: unknown[]) =>
    pruneWarmWorkbenchThreadViewModelCacheMock(...args),
}));

const now = "2026-03-18T00:00:00.000Z";

const toolGroupItem: WorkbenchListItem = {
  kind: "tool_group",
  id: "tool-group-1",
  turn_id: "turn-1",
  created_at: now,
  updated_at: now,
  tool_total: 0,
  tool_pending: 0,
  tool_running: 0,
  tool_completed: 0,
  tool_failed: 0,
  tools: [],
  thought: "",
};

const baseSession: Session = {
  id: "session-1",
  task_id: "task-1",
  workspace_id: "workspace-1",
  worktree_id: "worktree-1",
  provider_id: "codex",
  model_id: "gpt-5",
  title: "Conversation",
  agent_role: "primary",
  status: "active",
  created_at: now,
  updated_at: now,
};

const threadProjection: SessionThreadProjection = {
  loaded: true,
  turns: [],
  turnsStamp: "0:0",
  assistantStreamingByTurnId: {},
  assistantStreamingStamp: "0",
  messages: [],
  messagesStamp: "0:0",
  events: [],
  eventsStamp: "0:0:0",
  toolsByTurnId: {},
  toolSummariesReady: true,
  projectionRev: 1,
};

function buildEntry(overrides?: Partial<SessionCacheEntry>): SessionCacheEntry {
  return {
    sessionId: "session-1",
    loadState: "live",
    freshness: "authoritative",
    session: baseSession,
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
    queue: [],
    loading: false,
    subscribed: true,
    updatedAtMs: 0,
    threadProjection,
    fetching: {
      head: false,
      history: false,
    },
    ...overrides,
  };
}

function buildSessionSnapshot(entry: SessionCacheEntry): SessionSupervisorSnapshot {
  return {
    connection: "connected",
    sessions: {
      [entry.sessionId]: entry,
    },
  };
}

function buildSessionSnapshotMap(entries: SessionCacheEntry[]): SessionSupervisorSnapshot {
  return {
    connection: "connected",
    sessions: Object.fromEntries(entries.map((entry) => [entry.sessionId, entry])),
  };
}

function makeWorkspaceSnapshot(sessionIds: string | string[]): WorkspaceActiveSnapshotState {
  const normalizedSessionIds = Array.isArray(sessionIds) ? sessionIds : [sessionIds];
  return {
    workspaceId: "workspace-1",
    initialized: true,
    liveSnapshotApplied: true,
    connection: "connected",
    tasksById: Object.fromEntries(
      normalizedSessionIds.map((sessionId, index) => [
        `task-${index + 1}`,
        {
          id: `task-${index + 1}`,
          task: {
            id: `task-${index + 1}`,
            workspace_id: "workspace-1",
            title: `Task ${index + 1}`,
            status: "running",
            created_at: now,
            updated_at: now,
            last_activity_at: now,
            archived_at: null,
            assistant_seen_at: null,
            last_assistant_message_at: now,
            primary_session_id: sessionId,
          },
          sessions: [],
          primarySessionId: sessionId,
          primarySessionHead: null,
          sort_at: now,
          sortAtMs: Date.parse(now),
        },
      ]),
    ),
    activeIds: normalizedSessionIds.map((_, index) => `task-${index + 1}`),
    archivedIds: [],
    totalActive: normalizedSessionIds.length,
    totalArchived: 0,
    archivedRev: 0,
    fetchState: { active: "idle", archived: "idle" },
    hasMoreActive: false,
    hasMoreArchived: false,
    archivedLoaded: false,
  };
}

describe("useWarmSessionTranscriptRuntimes", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    resetSessionPretextRuntimeCache();
    primeWarmWorkbenchThreadViewModelMock.mockReset();
    pruneWarmWorkbenchThreadViewModelCacheMock.mockReset();
    noteSessionTranscriptWarmViewport({ width: 900, height: 300 });
    noteSessionTranscriptWarmVerbosity("default");
    primeWarmWorkbenchThreadViewModelMock.mockReturnValue({
      sourceKey: "source-1",
      layoutKey: "verbosity:default",
      warmKey: "warm-1",
      projectionRevision: 1,
      view: {
        groups: [],
        debugEvents: [],
      },
      listItems: [toolGroupItem],
      groupRanges: new Map(),
      turnsLen: 0,
      messagesLen: 0,
      eventsLen: 0,
      caches: {
        messagesByTurnId: new Map(),
        eventsByTurnId: new Map(),
      },
    });
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("refreshes prepared runtime layout when only ui state changes for a previously opened session", () => {
    const sessionId = "session-1";
    const initialUiState = {
      ...createDefaultSessionTranscriptUiState("default", []),
      expandedTurnDetailsById: { "turn-1": true },
    };
    const runtime = primeSessionPretextRuntime({
      sessionId,
      listItems: [toolGroupItem],
      uiState: initialUiState,
      viewportWidth: 900,
      viewportHeight: 300,
    });
    const initialHeight = readSessionPretextRuntimePreparedState(runtime).snapshot.totalHeight;

    const sessionSnap = buildSessionSnapshot(
      buildEntry({
        turnToolsLoading: ["turn-1"],
      }),
    );
    const workspaceSnapshot = makeWorkspaceSnapshot(sessionId);

    renderHook(() =>
      useWarmSessionTranscriptRuntimes({
        workspaceSnapshot,
        sessionSnap,
        activeSessionId: null,
      }),
    );

    act(() => {
      vi.advanceTimersByTime(20);
    });

    const warmedRuntime = getOrCreateSessionPretextRuntime(sessionId);
    const warmedPreparedState = readSessionPretextRuntimePreparedState(warmedRuntime);

    expect(primeWarmWorkbenchThreadViewModelMock).toHaveBeenCalledTimes(1);
    expect(pruneWarmWorkbenchThreadViewModelCacheMock).toHaveBeenCalledWith([sessionId]);
    expect(warmedRuntime.uiState.turnToolsLoading).toEqual(["turn-1"]);
    expect(warmedPreparedState.sourceKey).toBe("warm-1");
    expect(warmedPreparedState.layoutKey).toBe(
      buildSessionPretextRuntimeLayoutKey({
        uiState: warmedRuntime.uiState,
        listItems: [toolGroupItem],
      }),
    );
    expect(warmedPreparedState.snapshot.totalHeight).toBeGreaterThan(initialHeight);
  });

  it("does not replan warmed items on a no-op warm cycle", () => {
    const sessionId = "session-1";
    const initialUiState = {
      ...createDefaultSessionTranscriptUiState("default", []),
      expandedTurnDetailsById: { "turn-1": true },
    };
    const runtime = primeSessionPretextRuntime({
      sessionId,
      listItems: [toolGroupItem],
      uiState: initialUiState,
      viewportWidth: 900,
      viewportHeight: 300,
    });

    const initialSessionSnap = buildSessionSnapshot(
      buildEntry({
        turnToolsLoading: ["turn-1"],
      }),
    );
    const initialWorkspaceSnapshot = makeWorkspaceSnapshot(sessionId);

    const { rerender } = renderHook(
      (props: {
        workspaceSnapshot: WorkspaceActiveSnapshotState;
        sessionSnap: SessionSupervisorSnapshot;
        activeSessionId: string | null;
      }) => useWarmSessionTranscriptRuntimes(props),
      {
        initialProps: {
          workspaceSnapshot: initialWorkspaceSnapshot,
          sessionSnap: initialSessionSnap,
          activeSessionId: null,
        },
      },
    );

    act(() => {
      vi.advanceTimersByTime(20);
    });

    const warmedRuntime = getOrCreateSessionPretextRuntime(sessionId);
    const replaceItemsSpy = vi.spyOn(warmedRuntime.core, "replaceItems");
    const previousUiState = warmedRuntime.uiState;

    rerender({
      workspaceSnapshot: makeWorkspaceSnapshot(sessionId),
      sessionSnap: buildSessionSnapshot(
        buildEntry({
          turnToolsLoading: ["turn-1"],
        }),
      ),
      activeSessionId: null,
    });

    act(() => {
      vi.advanceTimersByTime(20);
    });

    expect(replaceItemsSpy).not.toHaveBeenCalled();
    expect(getOrCreateSessionPretextRuntime(sessionId).uiState).toBe(previousUiState);
  });

  it("limits background warming to the prefetch target budget while retaining the foreground runtime", () => {
    const foregroundSessionId = "session-foreground";
    const warmSessionIds = Array.from({ length: 12 }, (_, index) => `session-${index + 1}`);
    const expectedWarmSessionIds = warmSessionIds.slice(0, 8);
    const entries = [foregroundSessionId, ...warmSessionIds].map((sessionId) =>
      buildEntry({
        sessionId,
        session: {
          ...baseSession,
          id: sessionId,
          task_id: `task-${sessionId}`,
        },
      }),
    );

    renderHook(() =>
      useWarmSessionTranscriptRuntimes({
        workspaceSnapshot: makeWorkspaceSnapshot([foregroundSessionId, ...warmSessionIds]),
        sessionSnap: buildSessionSnapshotMap(entries),
        activeSessionId: foregroundSessionId,
      }),
    );

    act(() => {
      vi.advanceTimersByTime(200);
    });

    expect(pruneWarmWorkbenchThreadViewModelCacheMock).toHaveBeenCalledWith([
      foregroundSessionId,
      ...expectedWarmSessionIds,
    ]);
    expect(primeWarmWorkbenchThreadViewModelMock).toHaveBeenCalledTimes(8);
    expect(primeWarmWorkbenchThreadViewModelMock.mock.calls.map(([arg]) => (arg as { sessionId: string }).sessionId)).toEqual(
      expectedWarmSessionIds,
    );
  });

  it("suppresses background warming while retaining only the foreground runtime", () => {
    const foregroundSessionId = "session-foreground";
    const warmSessionIds = ["session-1", "session-2", "session-3"];
    const entries = [foregroundSessionId, ...warmSessionIds].map((sessionId) =>
      buildEntry({
        sessionId,
        session: {
          ...baseSession,
          id: sessionId,
          task_id: `task-${sessionId}`,
        },
      }),
    );

    renderHook(() =>
      useWarmSessionTranscriptRuntimes({
        workspaceSnapshot: makeWorkspaceSnapshot([foregroundSessionId, ...warmSessionIds]),
        sessionSnap: buildSessionSnapshotMap(entries),
        activeSessionId: foregroundSessionId,
        suppressWarmSessions: true,
      }),
    );

    act(() => {
      vi.advanceTimersByTime(200);
    });

    expect(pruneWarmWorkbenchThreadViewModelCacheMock).toHaveBeenCalledWith([foregroundSessionId]);
    expect(primeWarmWorkbenchThreadViewModelMock).not.toHaveBeenCalled();
  });
});
