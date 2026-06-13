import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { deriveBrowserStreamToken } from "../api/browserStreamAuth";
import type {
  Session,
  SessionHeadSnapshot,
  SessionSnapshotSummary,
  Task,
  WorkspaceActiveHeadBatch,
  WorkspaceActiveSnapshot,
  WorkspaceActiveTaskSummary,
} from "@ctx/types";
import { waitForCondition } from "../testUtils/waitForCondition";
import { getUiDiagnostics, resetUiDiagnosticsForTests } from "./diagnosticsChannel";
import {
  getActiveProjectionFixture,
  getSessionGapSeedFixture,
} from "../testdata/projectionEquivalenceFixtures";
import { buildWorkbenchThreadViewModel } from "../pages/SessionPage.workbenchViewModel";
import type { WorkspaceActiveSnapshotPatch } from "./workspaceActiveSnapshotProtocol";
import {
  readWorkspaceEventReceivedAt,
  readWorkspaceEventStreamSource,
} from "./workspaceEventTelemetry";
import { ctxUiRemoteIncident20260508 } from "./__fixtures__/ctxUiRemoteIncident20260508";

vi.mock("../api/client", () => {
  const idToString = (id: string | null | undefined): string => {
    if (id === null || id === undefined) return "";
    if (typeof id !== "string") {
      throw new Error("Expected id to be a string");
    }
    return id;
  };
  return {
    idToString,
    authToken: vi.fn(() => null),
    getDaemonConnectionReadiness: vi.fn((connection: { baseUrl?: string | null; authToken?: string | null }) => {
      const hasBaseUrl = Boolean(connection.baseUrl);
      const hasAuthToken = Boolean(connection.authToken);
      return {
        hasBaseUrl,
        hasAuthToken,
        isReady: hasBaseUrl && hasAuthToken,
        missing: !hasBaseUrl ? "base" : !hasAuthToken ? "auth" : null,
      };
    }),
    getDaemonClientConfig: vi.fn(() => ({
      baseUrl: "http://localhost:4399",
      wsBaseUrl: "ws://localhost:4399",
      authToken: null,
      runId: null,
    })),
    subscribeDaemonConfig: vi.fn(() => () => {}),
    getDaemonConnection: vi.fn(() => ({
      baseUrl: "http://localhost:4399",
      wsBaseUrl: "ws://localhost:4399",
      authToken: null,
      runId: null,
      source: "desktop",
    })),
    syncDesktopDaemonConnectionFromBridge: vi.fn(async () => ({
      config: {
        baseUrl: "http://localhost:4399",
        wsBaseUrl: "ws://localhost:4399",
        authToken: null,
        runId: null,
      },
      info: null,
      synced: false,
      error: null,
    })),
    getSessionHead: vi.fn(async () => null),
    getWorkspaceActiveHeads: vi.fn(async () => ({
      workspace_id: "ws-1",
      snapshot_rev: 0,
      heads: [],
    })),
    getWorkspaceActiveSnapshot: vi.fn(),
    listWorkspaceArchivedTaskSummaries: vi.fn(async () => ({
      workspace_id: "ws-1",
      archived_rev: 0,
      tasks: [],
      total_archived: 0,
      next_cursor: null,
    })),
    recordClientCounterMetric: vi.fn(),
    recordClientGaugeMetric: vi.fn(),
    recordClientHistogramMetric: vi.fn(),
    recordSemanticTelemetryEvent: vi.fn(),
  };
});

vi.mock("./uiStateStore", () => ({
  loadWorkspaceActiveSnapshotV1: vi.fn(async () => null),
  saveWorkspaceActiveSnapshotV1: vi.fn(async () => {}),
}));

type MockWs = {
  readyState: number;
  send: ReturnType<typeof vi.fn>;
};

type StoreInternals = {
  handleStreamMessage: (raw: string) => Promise<void>;
  ws?: MockWs;
  connectStream: () => Promise<void>;
  openWebSocket: (url: string) => Promise<void>;
  scheduleReconnect: () => void;
  applySessionSummaryDelta: (delta: unknown) => boolean;
  flushSubscriptions: (reason?: string) => void;
};

type MockDaemonClientConfig = {
  baseUrl: string | null;
  wsBaseUrl: string | null;
  authToken: string | null;
  runId: string | null;
};

const asStoreInternals = (store: object): StoreInternals =>
  store as unknown as StoreInternals;

const asRecord = (value: unknown): Record<string, unknown> =>
  value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};

const mkTask = (taskId: string, workspaceId: string, now: string): Task => ({
  id: taskId,
  workspace_id: workspaceId,
  title: "Active task",
  status: "running",
  created_at: now,
  updated_at: now,
});

const mkSession = (sessionId: string, taskId: string, workspaceId: string, now: string): Session => ({
  id: sessionId,
  task_id: taskId,
  workspace_id: workspaceId,
  worktree_id: "wt-1",
  provider_id: "fake",
  model_id: "fake-model",
  title: "Session",
  agent_role: "assistant",
  status: "active",
  created_at: now,
  updated_at: now,
});

const mkSummary = (session: Session, now: string): SessionSnapshotSummary => ({
  session,
  last_message_at: now,
  last_message_preview: "hello",
  last_event_seq: 0,
  state_rev: 0,
  activity: { is_working: false },
  unread: false,
});

const mkHead = (session: Session): SessionHeadSnapshot => ({
  session,
  turns: [],
  events: [],
  messages: [],
  last_event_seq: 0,
  state_rev: 0,
  activity: { is_working: false },
  has_more_turns: false,
  history_cursor: null,
  has_more_history: false,
});

const mkActiveSummary = (
  task: Task,
  summary: SessionSnapshotSummary,
  head: SessionHeadSnapshot,
  now: string,
): WorkspaceActiveTaskSummary => ({
  task,
  primary_session: summary,
  primary_session_head: head,
  sessions: [summary],
  sort_at: now,
});

type IncidentReplayEvent = {
  lane: "foreground" | "workspace";
  eventType: string;
};

const expandCtxUiIncidentReplayEvents = (): IncidentReplayEvent[] => {
  const events: IncidentReplayEvent[] = [];
  for (const segment of ctxUiRemoteIncident20260508.rle) {
    const [laneCode, eventType, rawCount] = segment.split(":");
    const count = Number.parseInt(rawCount ?? "", 10);
    if ((laneCode !== "f" && laneCode !== "w") || !eventType || !Number.isFinite(count) || count <= 0) {
      throw new Error(`Invalid ctx-ui incident replay segment: ${segment}`);
    }
    const lane = laneCode === "f" ? "foreground" : "workspace";
    for (let index = 0; index < count; index += 1) {
      events.push({ lane, eventType });
    }
  }
  return events;
};

const openWsState = (globalThis.WebSocket as unknown as { OPEN?: number } | undefined)?.OPEN ?? 1;

const mkOpenWs = (): MockWs => ({
  readyState: openWsState,
  send: vi.fn(),
});

describe("WorkspaceActiveSnapshotStore", () => {
  beforeEach(() => {
    vi.useRealTimers();
  });

  afterEach(() => {
    vi.clearAllMocks();
    resetUiDiagnosticsForTests();
    vi.useRealTimers();
  });

  it("hydrates from stream snapshot without HTTP", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");
    const { getWorkspaceActiveHeads, getWorkspaceActiveSnapshot } = await import("../api/client");

    const now = new Date().toISOString();
    const task = mkTask("task-1", "ws-1", now);
    const session = mkSession("session-1", "task-1", "ws-1", now);
    const summary = mkSummary(session, now);
    const head = mkHead(session);

    const activeSummary = mkActiveSummary(task, summary, head, now);
    const activeSnapshot: WorkspaceActiveSnapshot = {
      workspace_id: "ws-1",
      snapshot_rev: 2,
      archived_rev: 0,
      active: { total_count: 1, tasks: [activeSummary] },
    };
    const activeHeads: WorkspaceActiveHeadBatch = {
      workspace_id: "ws-1",
      snapshot_rev: 2,
      heads: [head],
    };

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "snapshot",
        rev: 2,
        active_snapshot: activeSnapshot,
        active_heads: activeHeads,
      }),
    );

    await waitForCondition(() => store.getSnapshot().initialized);

    expect(getWorkspaceActiveSnapshot).not.toHaveBeenCalled();
    expect(getWorkspaceActiveHeads).not.toHaveBeenCalled();

    const snapshot = store.getSnapshot();
    expect(snapshot.activeIds).toEqual(["task-1"]);
    expect(store.getSessionHeadSnapshot("session-1")?.last_event_seq).toBe(0);
  });

  it("retains a primary session head from snapshot summaries without a separate active head batch", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const now = new Date().toISOString();
    const task = mkTask("task-1", "ws-1", now);
    const session = mkSession("session-1", "task-1", "ws-1", now);
    const summary = mkSummary(session, now);
    const head = mkHead(session);

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "snapshot",
        rev: 2,
        active_snapshot: {
          workspace_id: "ws-1",
          snapshot_rev: 2,
          archived_rev: 0,
          active: { total_count: 1, tasks: [mkActiveSummary(task, summary, head, now)] },
        },
      }),
    );

    await waitForCondition(() => store.getSnapshot().initialized);

    expect(store.getSessionHeadSnapshot(session.id)?.session.id).toBe(session.id);
    expect(store.getSnapshot().tasksById[task.id]?.primarySessionHead?.session.id).toBe(session.id);
  });

  it("does not treat replay session event revs as workspace snapshot progress or resets", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const now = new Date().toISOString();
    const task = mkTask("task-1", "ws-1", now);
    const session = mkSession("session-1", "task-1", "ws-1", now);
    const summary = mkSummary(session, now);
    const head = mkHead(session);
    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    const ws = mkOpenWs();
    asStoreInternals(store).ws = ws;

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "snapshot",
        rev: 5,
        active_snapshot: {
          workspace_id: "ws-1",
          snapshot_rev: 5,
          archived_rev: 0,
          active: { total_count: 1, tasks: [mkActiveSummary(task, summary, head, now)] },
        },
      }),
    );
    await waitForCondition(() => store.getSnapshot().initialized);
    store.setSubscribedSessions([{ sessionId: session.id, replay: { kind: "resume", afterSeq: 1 } }]);
    ws.send.mockClear();

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 6,
        stream_source: "replay",
        event: {
          type: "session_head_delta",
          workspace_id: "ws-1",
          snapshot_rev: 1,
          delta: {
            session_id: session.id,
            last_event_seq: 1,
            projection_rev: 1,
            state_rev: 1,
          },
        },
      }),
    );

    expect(ws.send).not.toHaveBeenCalled();

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 7,
        stream_source: "replay",
        event: {
          type: "session_head_delta",
          workspace_id: "ws-1",
          snapshot_rev: 9,
          delta: {
            session_id: session.id,
            last_event_seq: 2,
            projection_rev: 2,
            state_rev: 2,
          },
        },
      }),
    );

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 8,
        event: { type: "ready", workspace_id: "ws-1", snapshot_rev: 6 },
      }),
    );

    expect(ws.send).not.toHaveBeenCalled();
    store.destroy();
  });

  it("does not treat replay head batch revs as workspace snapshot progress", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const now = new Date().toISOString();
    const task = mkTask("task-1", "ws-1", now);
    const session = mkSession("session-1", "task-1", "ws-1", now);
    const summary = mkSummary(session, now);
    const head = mkHead(session);
    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    const ws = mkOpenWs();
    asStoreInternals(store).ws = ws;

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "snapshot",
        rev: 5,
        active_snapshot: {
          workspace_id: "ws-1",
          snapshot_rev: 5,
          archived_rev: 0,
          active: { total_count: 1, tasks: [mkActiveSummary(task, summary, head, now)] },
        },
      }),
    );
    await waitForCondition(() => store.getSnapshot().initialized);
    store.setSubscribedSessions([{ sessionId: session.id, replay: { kind: "resume", afterSeq: 1 } }]);
    ws.send.mockClear();

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "heads_batch",
        rev: 6,
        stream_source: "replay",
        snapshot_rev: 9,
        deltas: [],
      }),
    );

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 7,
        event: { type: "ready", workspace_id: "ws-1", snapshot_rev: 6 },
      }),
    );

    expect(ws.send).not.toHaveBeenCalled();
    store.destroy();
  });

  it("ignores lower live snapshot revs without resetting active subscriptions", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const now = new Date().toISOString();
    const task = mkTask("task-1", "ws-1", now);
    const session = mkSession("session-1", "task-1", "ws-1", now);
    const summary = mkSummary(session, now);
    const head = mkHead(session);
    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    const ws = mkOpenWs();
    asStoreInternals(store).ws = ws;

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "snapshot",
        rev: 10,
        active_snapshot: {
          workspace_id: "ws-1",
          snapshot_rev: 10,
          archived_rev: 0,
          active: { total_count: 1, tasks: [mkActiveSummary(task, summary, head, now)] },
        },
      }),
    );
    await waitForCondition(() => store.getSnapshot().initialized);
    store.setSubscribedSessions([{ sessionId: session.id, replay: { kind: "resume", afterSeq: 1 } }]);
    ws.send.mockClear();

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "heads_batch",
        rev: 11,
        snapshot_rev: 7,
        deltas: [],
      }),
    );
    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 12,
        event: { type: "ready", workspace_id: "ws-1", snapshot_rev: 8 },
      }),
    );

    const internalState = (store as unknown as {
      state: { getSnapshotRev: () => number };
    }).state;
    expect(internalState.getSnapshotRev()).toBe(10);
    expect(ws.send).not.toHaveBeenCalled();
    store.destroy();
  });

  it("requests snapshot on reset_required", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");
    const { getWorkspaceActiveSnapshot } = await import("../api/client");

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    const ws = mkOpenWs();
    asStoreInternals(store).ws = ws;
    await asStoreInternals(store).handleStreamMessage(JSON.stringify({ type: "reset_required", latest_rev: 5 }));

    expect(getWorkspaceActiveSnapshot).not.toHaveBeenCalled();
    expect(ws.send).toHaveBeenCalled();
    store.destroy();
  });

  it("resubscribes on session_gap", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    const ws = mkOpenWs();
    asStoreInternals(store).ws = ws;
    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 1,
        event: {
          type: "session_gap",
          workspace_id: "ws-1",
          snapshot_rev: 1,
          session_id: "session-1",
          after_seq: 5,
        },
      }),
    );

    expect(ws.send).not.toHaveBeenCalled();
  });

  it("does not resubscribe on paired seed-following session_gap", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    const ws = mkOpenWs();
    const events: Array<{ type: string; session_id?: string }> = [];
    asStoreInternals(store).ws = ws;
    const unsubscribe = store.subscribeEvents((event) => {
      if (event.type === "session_gap") {
        events.push({ type: event.type, session_id: event.session_id });
      }
    });

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 1,
        event: {
          type: "session_gap",
          workspace_id: "ws-1",
          snapshot_rev: 1,
          session_id: "session-1",
          after_seq: 5,
          reason: "replay_limit_exceeded",
          seed_follows: true,
        },
      }),
    );

    expect(events).toEqual([{ type: "session_gap", session_id: "session-1" }]);
    expect(ws.send).not.toHaveBeenCalled();
    unsubscribe();
    store.destroy();
  });

  it("marks live sessions recovering when the workspace stream sequence has a gap", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    const ws = mkOpenWs();
    const events: Array<{ type: string; session_id?: string; reason?: string }> = [];
    asStoreInternals(store).ws = ws;
    const unsubscribe = store.subscribeEvents((event) => {
      if (event.type === "session_gap") {
        events.push({
          type: event.type,
          session_id: event.session_id,
          reason: event.reason ?? undefined,
        });
      }
    });

    store.setForegroundSessionId?.("session-1");
    store.setSubscribedSessions([
      { sessionId: "session-1", replay: { kind: "resume", afterSeq: 5 } },
      { sessionId: "session-2", replay: { kind: "auto" } },
    ]);
    ws.send.mockClear();

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 1,
        event: { type: "ready", workspace_id: "ws-1", snapshot_rev: 1 },
      }),
    );
    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 3,
        event: { type: "ready", workspace_id: "ws-1", snapshot_rev: 3 },
      }),
    );

    expect(events).toEqual([
      { type: "session_gap", session_id: "session-1", reason: "stream_seq_gap" },
      { type: "session_gap", session_id: "session-2", reason: "stream_seq_gap" },
    ]);
    expect(ws.send).not.toHaveBeenCalled();
    unsubscribe();
    store.destroy();
  });

  it("flushes subscribe messages when replay mode changes to reset", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    const ws = mkOpenWs();
    asStoreInternals(store).ws = ws;

    store.setSubscribedSessions([{ sessionId: "session-1", replay: { kind: "resume", afterSeq: 5 } }]);
    expect(ws.send).toHaveBeenCalledTimes(1);

    ws.send.mockClear();
    store.setSubscribedSessions([{ sessionId: "session-1", replay: { kind: "reset" } }]);

    expect(ws.send).toHaveBeenCalledTimes(1);
    const payload = JSON.parse(String(ws.send.mock.calls[0]?.[0] ?? "{}"));
    expect(payload.sessions).toEqual([
      { session_id: "session-1", intent: "replay", replay: { mode: "reset" } },
    ]);
    expect(payload.include_active_heads).toBe(false);
  });

  it("flushes subscribe messages when a pending session gains a resume cursor", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    const ws = mkOpenWs();
    asStoreInternals(store).ws = ws;

    store.setSubscribedSessions([{ sessionId: "session-1", replay: { kind: "auto" } }]);
    expect(ws.send).toHaveBeenCalledTimes(1);

    ws.send.mockClear();
    store.setSubscribedSessions([
      { sessionId: "session-1", replay: { kind: "resume", afterSeq: 3, afterProjectionRev: 7 } },
    ]);

    expect(ws.send).toHaveBeenCalledTimes(1);
    const payload = JSON.parse(String(ws.send.mock.calls[0]?.[0] ?? "{}"));
    expect(payload.include_active_heads).toBe(false);
    expect(payload.sessions).toEqual([
      {
        session_id: "session-1",
        intent: "replay",
        replay: { mode: "resume", after_seq: 3, after_projection_rev: 7 },
      },
    ]);
    store.destroy();
  });

  it("does not interrupt replay when a subscribed session relaxes from resume to auto", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    const ws = mkOpenWs();
    asStoreInternals(store).ws = ws;

    store.setSubscribedSessions([{ sessionId: "session-1", replay: { kind: "resume", afterSeq: 5 } }]);
    expect(ws.send).toHaveBeenCalledTimes(1);

    ws.send.mockClear();
    store.setSubscribedSessions([{ sessionId: "session-1", replay: { kind: "auto" } }]);

    expect(ws.send).not.toHaveBeenCalled();
    store.destroy();
  });

  it("requests active heads when subscribed session ids change", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    const ws = mkOpenWs();
    asStoreInternals(store).ws = ws;

    store.setSubscribedSessions([{ sessionId: "session-1", replay: { kind: "auto" } }]);

    expect(ws.send).toHaveBeenCalledTimes(1);
    const payload = JSON.parse(String(ws.send.mock.calls[0]?.[0] ?? "{}"));
    expect(payload.include_active_heads).toBe(true);
    store.destroy();
  });

  it("includes the foreground session in subscribe messages", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    const ws = mkOpenWs();
    asStoreInternals(store).ws = ws;

    store.setForegroundSessionId?.("session-foreground");

    expect(ws.send).toHaveBeenCalledTimes(1);
    const payload = JSON.parse(String(ws.send.mock.calls[0]?.[0] ?? "{}"));
    expect(payload.foreground_session_id).toBe("session-foreground");
    expect(payload.include_active_heads).toBe(false);
    store.destroy();
  });

  it("promotes head-only subscriptions to replay only for the foreground session", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    const ws = mkOpenWs();
    asStoreInternals(store).ws = ws;

    store.setSubscribedSessions([
      { sessionId: "session-1", intent: "head", replay: { kind: "resume", afterSeq: 5 } },
      { sessionId: "session-2", intent: "head", replay: { kind: "resume", afterSeq: 7 } },
    ]);

    expect(ws.send).toHaveBeenCalledTimes(1);
    let payload = JSON.parse(String(ws.send.mock.calls[0]?.[0] ?? "{}"));
    expect(payload.include_active_heads).toBe(true);
    expect(payload.sessions).toEqual([
      {
        session_id: "session-1",
        intent: "head",
        replay: { mode: "resume", after_seq: 5 },
      },
      {
        session_id: "session-2",
        intent: "head",
        replay: { mode: "resume", after_seq: 7 },
      },
    ]);

    ws.send.mockClear();
    store.setForegroundSessionId?.("session-2");

    expect(ws.send).toHaveBeenCalledTimes(1);
    payload = JSON.parse(String(ws.send.mock.calls[0]?.[0] ?? "{}"));
    expect(payload.include_active_heads).toBe(false);
    expect(payload.foreground_session_id).toBe("session-2");
    expect(payload.sessions).toEqual([
      {
        session_id: "session-1",
        intent: "head",
        replay: { mode: "resume", after_seq: 5 },
      },
      {
        session_id: "session-2",
        intent: "replay",
        replay: { mode: "resume", after_seq: 7 },
      },
    ]);

    ws.send.mockClear();
    store.setSubscribedSessions([
      { sessionId: "session-1", intent: "head", replay: { kind: "resume", afterSeq: 8 } },
      { sessionId: "session-2", intent: "head", replay: { kind: "resume", afterSeq: 9 } },
    ]);
    expect(ws.send).not.toHaveBeenCalled();
    store.destroy();
  });

  it("keeps a large active startup subscription at the optimal request count", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    const ws = mkOpenWs();
    asStoreInternals(store).ws = ws;
    const sessions = Array.from({ length: 29 }, (_, index) => ({
      sessionId: `session-${index + 1}`,
      intent: "head" as const,
      replay: { kind: "resume" as const, afterSeq: 100 + index, afterProjectionRev: 200 + index },
    }));

    store.setSubscribedSessions(sessions);

    expect(ws.send).toHaveBeenCalledTimes(1);
    let payload = JSON.parse(String(ws.send.mock.calls[0]?.[0] ?? "{}"));
    expect(payload.include_active_heads).toBe(true);
    expect(payload.sessions).toHaveLength(29);
    expect(payload.sessions.every((session: { intent?: string }) => session.intent === "head")).toBe(true);

    ws.send.mockClear();
    asStoreInternals(store).flushSubscriptions("active_task_upsert");
    asStoreInternals(store).flushSubscriptions("session_gap");
    asStoreInternals(store).flushSubscriptions("stream_seq_gap");
    store.setSubscribedSessions(
      sessions.map((session, index) => ({
        ...session,
        replay: {
          kind: "resume" as const,
          afterSeq: 300 + index,
          afterProjectionRev: 400 + index,
        },
      })),
    );
    expect(ws.send).not.toHaveBeenCalled();

    store.setForegroundSessionId?.("session-18");

    expect(ws.send).toHaveBeenCalledTimes(1);
    payload = JSON.parse(String(ws.send.mock.calls[0]?.[0] ?? "{}"));
    expect(payload.include_active_heads).toBe(false);
    expect(payload.foreground_session_id).toBe("session-18");
    expect(payload.sessions.filter((session: { intent?: string }) => session.intent === "replay")).toEqual([
      expect.objectContaining({ session_id: "session-18" }),
    ]);

    ws.send.mockClear();
    asStoreInternals(store).flushSubscriptions("foreground_session");
    asStoreInternals(store).flushSubscriptions("active_task_upsert");
    expect(ws.send).not.toHaveBeenCalled();
    store.destroy();
  });

  it("resubscribes with active heads after an active task upsert", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    const ws = mkOpenWs();
    asStoreInternals(store).ws = ws;
    const now = new Date().toISOString();
    const task = mkTask("task-1", "ws-1", now);
    const session = mkSession("session-1", "task-1", "ws-1", now);
    const summary = mkSummary(session, now);
    const head = mkHead(session);

    store.setSubscribedSessions([{ sessionId: session.id, replay: { kind: "auto" } }]);
    store.setForegroundSessionId?.(session.id);
    ws.send.mockClear();

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 1,
        event: {
          type: "active_task_upsert",
          workspace_id: "ws-1",
          snapshot_rev: 1,
          task: mkActiveSummary(task, summary, head, now),
        },
      }),
    );

    expect(ws.send).not.toHaveBeenCalled();
    store.destroy();
  });

  it("flushes worker patches immediately for foreground terminal session deltas", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    vi.useFakeTimers();
    const patches: Array<{ events: Array<{ type: string }> }> = [];
    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", {
      disableWorker: true,
      onPatch: (patch) => patches.push({ events: patch.events.map((event) => ({ type: event.type })) }),
    });

    store.setSubscribedSessions([{ sessionId: "session-1", replay: { kind: "auto" } }]);
    store.setForegroundSessionId?.("session-1");
    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 1,
        event: {
          type: "session_head_delta",
          workspace_id: "ws-1",
          snapshot_rev: 1,
          delta: {
            session_id: "session-1",
            last_event_seq: 1,
            projection_rev: 1,
            state_rev: 1,
            event: {
              seq: 1,
              id: "event-1",
              session_id: "session-1",
              turn_id: "turn-1",
              event_type: "assistant_complete",
              payload_json: { full_content: "final" },
              created_at: "2026-03-09T00:00:01.000Z",
            },
            message: {
              id: "message-1",
              session_id: "session-1",
              task_id: "task-1",
              turn_id: "turn-1",
              role: "assistant",
              content: "final",
              delivery: "immediate",
              created_at: "2026-03-09T00:00:01.000Z",
            },
          },
        },
      }),
    );

    expect(patches).toHaveLength(1);
    expect(patches[0]?.events.map((event) => event.type)).toEqual(["session_head_delta"]);
    store.destroy();
  });

  it("preserves stream metadata across worker patch delivery", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    vi.useFakeTimers();
    const patches: WorkspaceActiveSnapshotPatch[] = [];
    const workerStore = new WorkspaceActiveSnapshotStoreImpl("ws-1", {
      disableWorker: true,
      onPatch: (patch) => patches.push(patch),
    });

    workerStore.setSubscribedSessions([{ sessionId: "session-1", replay: { kind: "auto" } }]);
    workerStore.setForegroundSessionId?.("session-1");
    await asStoreInternals(workerStore).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 1,
        stream_source: "replay",
        event: {
          type: "session_head_delta",
          workspace_id: "ws-1",
          snapshot_rev: 1,
          delta: {
            session_id: "session-1",
            last_event_seq: 1,
            projection_rev: 1,
            state_rev: 1,
            emitted_at_ms: Date.now(),
            event: {
              seq: 1,
              id: "event-1",
              session_id: "session-1",
              turn_id: "turn-1",
              event_type: "assistant_complete",
              payload_json: { full_content: "final" },
              created_at: "2026-03-09T00:00:01.000Z",
            },
            message: {
              id: "message-1",
              session_id: "session-1",
              task_id: "task-1",
              turn_id: "turn-1",
              role: "assistant",
              content: "final",
              delivery: "immediate",
              created_at: "2026-03-09T00:00:01.000Z",
            },
          },
        },
      }),
    );

    expect(patches).toHaveLength(1);
    expect(patches[0]?.eventStreamSources).toEqual(["replay"]);
    expect(typeof patches[0]?.eventReceivedAtMs?.[0]).toBe("number");

    const mainStore = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    const observed: Array<{ receivedAtMs: number | null; streamSource: string | null }> = [];
    const unsubscribe = mainStore.subscribeEvents((event) => {
      observed.push({
        receivedAtMs: readWorkspaceEventReceivedAt(event),
        streamSource: readWorkspaceEventStreamSource(event),
      });
    });
    const clonedPatch = JSON.parse(JSON.stringify(patches[0])) as WorkspaceActiveSnapshotPatch;
    mainStore.applyWorkerPatch(clonedPatch);

    expect(observed).toHaveLength(1);
    expect(observed[0]?.streamSource).toBe("replay");
    expect(typeof observed[0]?.receivedAtMs).toBe("number");
    unsubscribe();
    workerStore.destroy();
    mainStore.destroy();
  });

  it("flushes worker patches immediately for the current subscription when foreground state is stale", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    vi.useFakeTimers();
    const patches: Array<{ events: Array<{ type: string }> }> = [];
    const telemetry: Array<{ lane: string; eventType: string }> = [];
    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", {
      disableWorker: true,
      onPatch: (patch) => patches.push({ events: patch.events.map((event) => ({ type: event.type })) }),
      onStreamTelemetry: (sample) => telemetry.push({ lane: sample.lane, eventType: sample.eventType }),
    });

    store.setSubscribedSessions([
      { sessionId: "session-current", replay: { kind: "auto" } },
    ]);
    store.setForegroundSessionId?.("session-stale");
    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 1,
        event: {
          type: "session_head_delta",
          workspace_id: "ws-1",
          snapshot_rev: 1,
          delta: {
            session_id: "session-current",
            last_event_seq: 1,
            projection_rev: 1,
            state_rev: 1,
            emitted_at_ms: Date.now(),
            event: {
              seq: 1,
              id: "event-1",
              session_id: "session-current",
              turn_id: "turn-1",
              event_type: "assistant_complete",
              payload_json: { full_content: "final" },
              created_at: "2026-03-09T00:00:01.000Z",
            },
            message: {
              id: "message-1",
              session_id: "session-current",
              task_id: "task-1",
              turn_id: "turn-1",
              role: "assistant",
              content: "final",
              delivery: "immediate",
              created_at: "2026-03-09T00:00:01.000Z",
            },
          },
        },
      }),
    );

    expect(telemetry.at(-1)).toEqual({
      lane: "foreground",
      eventType: "session_head_delta",
    });
    expect(patches).toHaveLength(1);
    expect(patches[0]?.events.map((event) => event.type)).toEqual(["session_head_delta"]);
    store.destroy();
  });

  it("preempts queued background session-head worker events when foreground terminal progress arrives", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    vi.useFakeTimers();
    const patches: Array<{ sessionIds: string[]; upsertIds: string[] }> = [];
    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", {
      disableWorker: true,
      onPatch: (patch) =>
        patches.push({
          sessionIds: patch.events.map((event) =>
            event.type === "session_head_delta" ? String(event.delta.session_id) : "",
          ),
          upsertIds: Object.keys(patch.sessionHeadUpserts ?? {}),
        }),
    });

    store.setSubscribedSessions([
      { sessionId: "session-foreground", replay: { kind: "auto" } },
      { sessionId: "session-background", replay: { kind: "auto" } },
    ]);
    store.setForegroundSessionId?.("session-foreground");

    const now = "2026-03-09T00:00:00.000Z";
    const seedHead = async (sessionId: string, seq: number) => {
      await asStoreInternals(store).handleStreamMessage(
        JSON.stringify({
          type: "event",
          rev: seq,
          event: {
            type: "session_head_seed",
            workspace_id: "ws-1",
            snapshot_rev: seq,
            head: mkHead(mkSession(sessionId, "task-1", "ws-1", now)),
          },
        }),
      );
    };

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 0,
        event: {
          type: "ready",
          workspace_id: "ws-1",
          snapshot_rev: 0,
          archived_rev: 0,
        },
      }),
    );
    await seedHead("session-background", 1);
    await seedHead("session-foreground", 2);
    vi.advanceTimersByTime(60);
    expect(patches.length).toBeGreaterThan(0);
    patches.length = 0;

    const sendTerminalDelta = async (sessionId: string, seq: number) => {
      await asStoreInternals(store).handleStreamMessage(
        JSON.stringify({
          type: "event",
          rev: seq,
          event: {
            type: "session_head_delta",
            workspace_id: "ws-1",
            snapshot_rev: seq,
            delta: {
              session_id: sessionId,
              last_event_seq: seq,
              projection_rev: seq,
              state_rev: seq,
              event: {
                seq,
                id: `event-${seq}`,
                session_id: sessionId,
                turn_id: `turn-${seq}`,
                event_type: "assistant_complete",
                payload_json: { full_content: `final ${seq}` },
                created_at: "2026-03-09T00:00:01.000Z",
              },
              message: {
                id: `message-${seq}`,
                session_id: sessionId,
                task_id: "task-1",
                turn_id: `turn-${seq}`,
                role: "assistant",
                content: `final ${seq}`,
                delivery: "immediate",
                created_at: "2026-03-09T00:00:01.000Z",
              },
            },
          },
        }),
      );
    };

    await sendTerminalDelta("session-background", 3);
    expect(patches).toHaveLength(0);

    await sendTerminalDelta("session-foreground", 4);

    expect(patches).toHaveLength(1);
    expect(patches[0]?.sessionIds).toEqual(["session-foreground"]);
    expect(patches[0]?.upsertIds).not.toContain("session-background");

    vi.advanceTimersByTime(60);
    expect(patches).toHaveLength(2);
    expect(patches[1]?.sessionIds).toEqual(["session-background"]);
    expect(patches[1]?.upsertIds).not.toContain("session-foreground");
    store.destroy();
  });

  it("keeps terminal worker patches batched for subscribed non-foreground sessions", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    vi.useFakeTimers();
    const patches: Array<{ events: Array<{ type: string }> }> = [];
    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", {
      disableWorker: true,
      onPatch: (patch) => patches.push({ events: patch.events.map((event) => ({ type: event.type })) }),
    });

    store.setSubscribedSessions([
      { sessionId: "session-1", replay: { kind: "auto" } },
      { sessionId: "session-2", replay: { kind: "auto" } },
    ]);
    store.setForegroundSessionId?.("session-1");
    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 1,
        event: {
          type: "session_head_delta",
          workspace_id: "ws-1",
          snapshot_rev: 1,
          delta: {
            session_id: "session-2",
            last_event_seq: 1,
            projection_rev: 1,
            state_rev: 1,
            event: {
              seq: 1,
              id: "event-1",
              session_id: "session-2",
              turn_id: "turn-1",
              event_type: "assistant_complete",
              payload_json: { full_content: "final" },
              created_at: "2026-03-09T00:00:01.000Z",
            },
            message: {
              id: "message-1",
              session_id: "session-2",
              task_id: "task-1",
              turn_id: "turn-1",
              role: "assistant",
              content: "final",
              delivery: "immediate",
              created_at: "2026-03-09T00:00:01.000Z",
            },
          },
        },
      }),
    );

    expect(patches).toHaveLength(0);
    vi.advanceTimersByTime(60);
    expect(patches).toHaveLength(1);
    expect(patches[0]?.events.map((event) => event.type)).toEqual(["session_head_delta"]);
    store.destroy();
  });

  it("keeps partial-only worker patches batched for subscribed sessions", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    vi.useFakeTimers();
    const patches: Array<{ events: Array<{ type: string }> }> = [];
    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", {
      disableWorker: true,
      onPatch: (patch) => patches.push({ events: patch.events.map((event) => ({ type: event.type })) }),
    });

    store.setSubscribedSessions([{ sessionId: "session-1", replay: { kind: "auto" } }]);
    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 1,
        event: {
          type: "session_head_delta",
          workspace_id: "ws-1",
          snapshot_rev: 1,
          delta: {
            session_id: "session-1",
            last_event_seq: 1,
            projection_rev: 1,
            state_rev: 1,
            event: {
              seq: 1,
              id: "event-1",
              session_id: "session-1",
              turn_id: "turn-1",
              event_type: "assistant_chunk",
              payload_json: { content_fragment: "partial" },
              created_at: "2026-03-09T00:00:01.000Z",
            },
          },
        },
      }),
    );

    expect(patches).toHaveLength(0);
    vi.advanceTimersByTime(50);
    expect(patches).toHaveLength(1);
    expect(patches[0]?.events.map((event) => event.type)).toEqual(["session_head_delta"]);
    store.destroy();
  });

  it("keeps the desktop publish path bounded under the ctx-ui May 8 incident event mix", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    vi.useFakeTimers();
    const now = "2026-05-08T15:00:00.000Z";
    const task = mkTask("ctx-ui-task", "ws-1", now);
    const foregroundSession = mkSession("ctx-ui-foreground-session", "ctx-ui-task", "ws-1", now);
    const workspaceSession = mkSession("ctx-ui-workspace-session", "ctx-ui-task", "ws-1", now);
    const foregroundSummary = mkSummary(foregroundSession, now);
    const workspaceSummary = mkSummary(workspaceSession, now);
    const foregroundHead = mkHead(foregroundSession);
    const activeSnapshot: WorkspaceActiveSnapshot = {
      workspace_id: "ws-1",
      snapshot_rev: 1,
      archived_rev: 0,
      active: {
        total_count: 1,
        tasks: [
          {
            ...mkActiveSummary(task, foregroundSummary, foregroundHead, now),
            sessions: [foregroundSummary, workspaceSummary],
          },
        ],
      },
    };

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", {
      disableCache: true,
      disableWorker: true,
    });
    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "snapshot",
        rev: 1,
        active_snapshot: activeSnapshot,
      }),
    );
    store.setForegroundSessionId?.(foregroundSession.id);
    await waitForCondition(() => store.getSnapshot().initialized);

    let publishCount = 0;
    let foregroundDeltaEvents = 0;
    const unsubscribeSnapshot = store.subscribe(() => {
      publishCount += 1;
    });
    const unsubscribeEvents = store.subscribeEvents((event) => {
      if (
        event.type === "session_head_delta" &&
        event.delta.session_id === foregroundSession.id
      ) {
        foregroundDeltaEvents += 1;
      }
    });

    let rev = 1;
    let foregroundSeq = 0;
    let workspaceSeq = 0;
    let omittedVcsEvents = 0;
    let taskRev = 0;
    const replayEvents = expandCtxUiIncidentReplayEvents();
    expect(replayEvents).toHaveLength(ctxUiRemoteIncident20260508.eventCount);

    const sendEvent = async (event: object) => {
      rev += 1;
      await asStoreInternals(store).handleStreamMessage(
        JSON.stringify({
          type: "event",
          rev,
          event: {
            workspace_id: "ws-1",
            snapshot_rev: rev,
            ...event,
          },
        }),
      );
    };

    for (const replayEvent of replayEvents) {
      switch (replayEvent.eventType) {
        case "worktree_vcs_snapshot": {
          omittedVcsEvents += 1;
          break;
        }
        case "session_head_delta": {
          const session = replayEvent.lane === "foreground" ? foregroundSession : workspaceSession;
          const seq = replayEvent.lane === "foreground" ? ++foregroundSeq : ++workspaceSeq;
          await sendEvent({
            type: "session_head_delta",
            delta: {
              session_id: session.id,
              last_event_seq: seq,
              projection_rev: seq,
              state_rev: seq,
              emitted_at_ms: Date.now(),
              activity: { is_working: true, last_turn_status: "running" },
            },
          });
          break;
        }
        case "session_summary_delta": {
          const session = replayEvent.lane === "foreground" ? foregroundSession : workspaceSession;
          const seq = replayEvent.lane === "foreground" ? foregroundSeq : workspaceSeq;
          await sendEvent({
            type: "session_summary_delta",
            delta: {
              session_id: session.id,
              task_id: task.id,
              activity: { is_working: true, last_turn_status: "running" },
              last_message_at: now,
              last_message_preview: `${replayEvent.lane} preview ${seq}`,
              last_event_seq: seq,
              state_rev: seq,
              emitted_at_ms: Date.now(),
            },
          });
          break;
        }
        case "task_delta": {
          taskRev += 1;
          await sendEvent({
            type: "task_delta",
            delta: {
              kind: "updated",
              task: {
                ...task,
                title: `Active task ${taskRev}`,
                updated_at: `2026-05-08T15:${String(taskRev % 60).padStart(2, "0")}:00.000Z`,
              },
            },
          });
          break;
        }
        case "session_gap":
          await sendEvent({
            type: "session_gap",
            session_id: replayEvent.lane === "foreground" ? foregroundSession.id : workspaceSession.id,
            after_seq: replayEvent.lane === "foreground" ? foregroundSeq : workspaceSeq,
          });
          break;
        case "session_head_seed":
          await sendEvent({
            type: "session_head_seed",
            head: {
              ...mkHead(replayEvent.lane === "foreground" ? foregroundSession : workspaceSession),
              last_event_seq: replayEvent.lane === "foreground" ? foregroundSeq : workspaceSeq,
            },
          });
          break;
        case "active_task_upsert":
          await sendEvent({
            type: "active_task_upsert",
            task: mkActiveSummary(task, foregroundSummary, foregroundHead, now),
          });
          break;
        case "active_task_delete":
          await sendEvent({ type: "active_task_delete", task_id: "unused-task" });
          break;
        case "archived_task_upsert":
          await sendEvent({
            type: "archived_task_upsert",
            archived_rev: rev,
            task: {
              task: { ...task, id: "archived-task", archived_at: now },
              sessions: [],
              sort_at: now,
            },
          });
          break;
        case "archived_task_delete":
          await sendEvent({ type: "archived_task_delete", archived_rev: rev, task_id: "archived-task" });
          break;
        case "session_removed":
          await sendEvent({ type: "session_removed", session_id: "unused-session" });
          break;
        case "ready":
          await sendEvent({ type: "ready" });
          break;
        default:
          throw new Error(`Unhandled ctx-ui incident event type: ${replayEvent.eventType}`);
      }
    }

    expect(foregroundDeltaEvents).toBe(
      ctxUiRemoteIncident20260508.counts["foreground/session_head_delta"],
    );
    expect(ctxUiRemoteIncident20260508.counts["workspace/worktree_vcs_snapshot"]).toBeGreaterThan(
      10_000,
    );
    expect(omittedVcsEvents).toBe(ctxUiRemoteIncident20260508.counts["workspace/worktree_vcs_snapshot"]);
    expect(publishCount).toBeLessThan(500);

    await vi.runOnlyPendingTimersAsync();
    expect(publishCount).toBeLessThan(510);
    unsubscribeEvents();
    unsubscribeSnapshot();
    store.destroy();
  });

  it("does not flush subscribe messages when only the resume cursor advances", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    const ws = mkOpenWs();
    asStoreInternals(store).ws = ws;

    store.setSubscribedSessions([
      { sessionId: "session-1", replay: { kind: "resume", afterSeq: 5, afterProjectionRev: 1 } },
    ]);
    expect(ws.send).toHaveBeenCalledTimes(1);

    ws.send.mockClear();
    store.setSubscribedSessions([
      { sessionId: "session-1", replay: { kind: "resume", afterSeq: 6, afterProjectionRev: 1 } },
    ]);
    store.setSubscribedSessions([
      { sessionId: "session-1", replay: { kind: "resume", afterSeq: 8, afterProjectionRev: 2 } },
    ]);

    expect(ws.send).not.toHaveBeenCalled();

    asStoreInternals(store).flushSubscriptions("ws_open");
    expect(ws.send).toHaveBeenCalledTimes(1);
    const reconnectPayload = JSON.parse(String(ws.send.mock.calls[0]?.[0] ?? "{}"));
    expect(reconnectPayload.include_active_heads).toBe(true);
    expect(reconnectPayload.sessions).toEqual([
      {
        session_id: "session-1",
        intent: "replay",
        replay: { mode: "resume", after_seq: 8, after_projection_rev: 2 },
      },
    ]);
  });

  it("keeps cache and render projections aligned with the shared active fixture", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const fixture = getActiveProjectionFixture();
    const store = new WorkspaceActiveSnapshotStoreImpl(fixture.workspaceId, { disableWorker: true });

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "snapshot",
        rev: 4,
        active_snapshot: fixture.activeSnapshot,
        active_heads: fixture.activeHeads,
      }),
    );

    await waitForCondition(() => store.getSnapshot().initialized);

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 4,
        event: {
          type: "session_head_delta",
          workspace_id: fixture.workspaceId,
          snapshot_rev: 4,
          delta: fixture.partialDelta,
        },
      }),
    );

    const snapshot = store.getSnapshot();
    expect(snapshot.activeIds).toEqual([fixture.task.id]);

    const summary = snapshot.tasksById[fixture.task.id]?.sessions[0];
    expect(summary?.last_event_seq).toBe(fixture.expected.summaryLastEventSeq);

    const head = store.getSessionHeadSnapshot(fixture.session.id);
    expect(head?.last_event_seq).toBe(fixture.expected.headLastEventSeq);
    expect(head?.head_window?.event_limit).toBe(0);
    expect(head?.events ?? []).toEqual([]);
    expect((head?.events ?? []).some((event) => event.event_type === "assistant_chunk")).toBe(false);

    const view = buildWorkbenchThreadViewModel(
      head?.turns ?? [],
      head?.messages ?? [],
      fixture.toolsByTurnId,
      head?.events ?? [],
    );
    const items = view.groups[0]?.items ?? [];
    expect(items.map((item) => item.kind)).toEqual(fixture.expected.renderItemKinds);
    expect(items.find((item) => item.kind === "assistant")).toMatchObject({
      kind: "assistant",
      content: fixture.expected.assistantContent,
    });
    expect(items.find((item) => item.kind === "tool")).toMatchObject({
      kind: "tool",
      tool_call_id: fixture.expected.toolCallId,
    });
  }, 15_000);

  it("rehydrates the seeded head from the shared gap fixture", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const fixture = getSessionGapSeedFixture();
    const store = new WorkspaceActiveSnapshotStoreImpl(fixture.workspaceId, { disableWorker: true });

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "snapshot",
        rev: 4,
        active_snapshot: fixture.activeSnapshot,
        active_heads: fixture.activeHeads,
      }),
    );

    await waitForCondition(() => store.getSnapshot().initialized);

    const ws = mkOpenWs();
    asStoreInternals(store).ws = ws;

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 5,
        event: fixture.gapEvent,
      }),
    );

    expect(ws.send).not.toHaveBeenCalled();

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 5,
        event: fixture.seedEvent,
      }),
    );

    const head = store.getSessionHeadSnapshot(fixture.session.id);
    expect(head?.last_event_seq).toBe(fixture.expected.headLastEventSeq);
    expect(head?.messages?.[1]?.content).toBe("Seeded response after gap.");
  });

  it("uses one canonical websocket url per connect cycle", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");
    const { getDaemonClientConfig } = await import("../api/client");
    vi.spyOn(Date, "now").mockReturnValueOnce(1_700_000_000_000);
    vi.mocked(getDaemonClientConfig).mockReturnValue({
      baseUrl: "http://daemon.local",
      wsBaseUrl: "ws://daemon.local",
      authToken: "token-1",
      runId: null,
    });
    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    const internals = asStoreInternals(store);
    const openSpy = vi.spyOn(internals, "openWebSocket").mockResolvedValueOnce(undefined);
    const reconnectSpy = vi.spyOn(internals, "scheduleReconnect").mockImplementation(() => {});
    const expectedExpiresAt = Math.floor((1_700_000_000_000 + 5 * 60 * 1000) / 1000);
    const expectedToken = await deriveBrowserStreamToken("token-1", {
      kind: "workspace_active_snapshot",
      workspaceId: "ws-1",
    }, expectedExpiresAt);

    await internals.connectStream();

    expect(openSpy).toHaveBeenCalledTimes(1);
    expect(openSpy).toHaveBeenCalledWith(
      `ws://daemon.local/api/workspaces/ws-1/active_snapshot/stream?expires_at=${expectedExpiresAt}&token=${expectedToken}`,
    );
    expect(reconnectSpy).not.toHaveBeenCalled();
    const diagnostics = getUiDiagnostics().filter((event) => event.code === "workspace.stream_connect_failed");
    expect(diagnostics).toHaveLength(0);
  });

  it("emits a single connect-failed diagnostic when canonical websocket connect fails", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");
    const { getDaemonClientConfig } = await import("../api/client");
    vi.mocked(getDaemonClientConfig).mockReturnValue({
      baseUrl: "http://daemon.local",
      wsBaseUrl: "ws://daemon.local",
      authToken: null,
      runId: null,
    });
    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    const internals = asStoreInternals(store);
    vi.spyOn(internals, "openWebSocket").mockRejectedValueOnce(new Error("workspace active snapshot ws timeout"));
    const reconnectSpy = vi.spyOn(internals, "scheduleReconnect").mockImplementation(() => {});

    await internals.connectStream();

    expect(reconnectSpy).toHaveBeenCalledTimes(1);
    const diagnostics = getUiDiagnostics().filter((event) => event.code === "workspace.stream_connect_failed");
    expect(diagnostics).toHaveLength(1);
    const context = asRecord(diagnostics[0]?.context);
    expect(context.url).toBe("ws://daemon.local/api/workspaces/ws-1/active_snapshot/stream");
    expect(String(context.error ?? "")).toContain("timeout");
  });

  it("applies session_head_seed events to the session head cache", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const now = new Date().toISOString();
    const task = mkTask("task-seed", "ws-1", now);
    const session = mkSession("session-seed", "task-seed", "ws-1", now);
    const summary = mkSummary(session, now);
    const head = mkHead(session);
    const activeSnapshot: WorkspaceActiveSnapshot = {
      workspace_id: "ws-1",
      snapshot_rev: 1,
      archived_rev: 0,
      active: { total_count: 1, tasks: [mkActiveSummary(task, summary, head, now)] },
    };

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "snapshot",
        rev: 1,
        active_snapshot: activeSnapshot,
      }),
    );

    const seededHead: SessionHeadSnapshot = {
      ...head,
      turns: [
        {
          turn_id: "turn-seed",
          session_id: "session-seed",
          run_id: null,
          user_message_id: "user-seed",
          status: "completed",
          start_seq: 1,
          end_seq: 2,
          started_at: now,
          updated_at: now,
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
      last_event_seq: 3,
      messages: [
        {
          id: "msg-seed",
          session_id: "session-seed",
          task_id: "task-seed",
          turn_id: "turn-seed",
          role: "assistant",
          content: "seeded",
          delivery: "immediate",
          created_at: now,
        },
      ],
    };

    const seenSeedEvents: string[] = [];
    const unsubscribe = store.subscribeEvents((event) => {
      seenSeedEvents.push(event.type);
    });

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 2,
        event: {
          type: "session_head_seed",
          workspace_id: "ws-1",
          snapshot_rev: 1,
          head: seededHead,
        },
      }),
    );

    expect(store.getSessionHeadSnapshot("session-seed")?.last_event_seq).toBe(3);
    expect(store.getSessionHeadSnapshot("session-seed")?.messages).toHaveLength(1);
    expect(seenSeedEvents).toContain("session_head_seed");
    unsubscribe();
  });

  it("requests snapshot on stream seq gap", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");
    const { getWorkspaceActiveSnapshot } = await import("../api/client");

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    const ws = mkOpenWs();
    asStoreInternals(store).ws = ws;
    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 1,
        event: { type: "ready", workspace_id: "ws-1", snapshot_rev: 1, archived_rev: 0 },
      }),
    );
    expect(getWorkspaceActiveSnapshot).not.toHaveBeenCalled();
    expect(ws.send).not.toHaveBeenCalled();

    vi.mocked(getWorkspaceActiveSnapshot).mockClear();
    ws.send.mockClear();
    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 3,
        event: { type: "ready", workspace_id: "ws-1", snapshot_rev: 3, archived_rev: 0 },
      }),
    );
    expect(getWorkspaceActiveSnapshot).not.toHaveBeenCalled();
    expect(ws.send).not.toHaveBeenCalled();
    store.destroy();
  });

  it("applies task_delta to remove archived tasks from the active list", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const now = "2024-01-01T00:00:00.000Z";
    const later = "2024-01-01T01:00:00.000Z";
    const archivedAt = "2024-01-02T00:00:00.000Z";

    const taskA = mkTask("task-1", "ws-1", now);
    const taskB = mkTask("task-2", "ws-1", later);
    const sessionA = mkSession("session-1", "task-1", "ws-1", now);
    const sessionB = mkSession("session-2", "task-2", "ws-1", later);
    const summaryA = mkSummary(sessionA, now);
    const summaryB = mkSummary(sessionB, later);
    const headA = mkHead(sessionA);
    const headB = mkHead(sessionB);

    const activeSnapshot: WorkspaceActiveSnapshot = {
      workspace_id: "ws-1",
      snapshot_rev: 1,
      archived_rev: 0,
      active: {
        total_count: 2,
        tasks: [mkActiveSummary(taskA, summaryA, headA, now), mkActiveSummary(taskB, summaryB, headB, later)],
      },
    };

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "snapshot",
        rev: 1,
        active_snapshot: activeSnapshot,
      }),
    );

    await waitForCondition(() => store.getSnapshot().initialized);

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 2,
        event: {
          type: "task_delta",
          workspace_id: "ws-1",
          snapshot_rev: 2,
          delta: {
            kind: "archived",
            task: {
              ...taskA,
              archived_at: archivedAt,
              updated_at: archivedAt,
            },
          },
        },
      }),
    );

    const snapshot = store.getSnapshot();
    expect(snapshot.activeIds).toEqual(["task-2"]);
    expect(snapshot.archivedIds).toEqual([]);
    expect(snapshot.totalActive).toBe(1);
    expect(snapshot.totalArchived).toBe(0);
  });

  it("keeps an unarchived task visible after archived_task_delete arrives", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const now = "2024-01-01T00:00:00.000Z";
    const archivedAt = "2024-01-02T00:00:00.000Z";

    const task = mkTask("task-1", "ws-1", now);
    const session = mkSession("session-1", "task-1", "ws-1", now);
    const summary = mkSummary(session, now);
    const head = mkHead(session);

    const activeSnapshot: WorkspaceActiveSnapshot = {
      workspace_id: "ws-1",
      snapshot_rev: 1,
      archived_rev: 0,
      active: {
        total_count: 1,
        tasks: [mkActiveSummary(task, summary, head, now)],
      },
    };

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "snapshot",
        rev: 1,
        active_snapshot: activeSnapshot,
      }),
    );

    await waitForCondition(() => store.getSnapshot().initialized);

    const archivedTask = {
      ...task,
      archived_at: archivedAt,
      updated_at: archivedAt,
    };

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 2,
        event: {
          type: "task_delta",
          workspace_id: "ws-1",
          snapshot_rev: 2,
          delta: {
            kind: "archived",
            task: archivedTask,
          },
        },
      }),
    );

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 3,
        event: {
          type: "archived_task_upsert",
          workspace_id: "ws-1",
          archived_rev: 1,
          task: {
            task: archivedTask,
            sessions: [session],
            sort_at: archivedAt,
          },
        },
      }),
    );

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 4,
        event: {
          type: "task_delta",
          workspace_id: "ws-1",
          snapshot_rev: 3,
          delta: {
            kind: "unarchived",
            task,
          },
        },
      }),
    );

    const afterStreamOnlyUnarchive = store.getSnapshot();
    expect(afterStreamOnlyUnarchive.activeIds).toEqual([task.id]);
    expect(afterStreamOnlyUnarchive.archivedIds).toEqual([]);
    expect(afterStreamOnlyUnarchive.totalActive).toBe(1);
    expect(afterStreamOnlyUnarchive.totalArchived).toBe(0);

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 5,
        event: {
          type: "active_task_upsert",
          workspace_id: "ws-1",
          snapshot_rev: 4,
          task: mkActiveSummary(task, summary, head, now),
        },
      }),
    );

    const publishedSnapshots: Array<{ archivedRev: number; totalArchived: number }> = [];
    const unsubscribe = store.subscribe(() => {
      const snapshot = store.getSnapshot();
      publishedSnapshots.push({
        archivedRev: snapshot.archivedRev,
        totalArchived: snapshot.totalArchived,
      });
    });

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 6,
        event: {
          type: "archived_task_delete",
          workspace_id: "ws-1",
          archived_rev: 2,
          task_id: task.id,
        },
      }),
    );
    unsubscribe();

    const snapshot = store.getSnapshot();
    expect(snapshot.activeIds).toEqual([task.id]);
    expect(snapshot.archivedIds).toEqual([]);
    expect(snapshot.tasksById[task.id]?.task.archived_at ?? null).toBeNull();
    expect(snapshot.totalActive).toBe(1);
    expect(snapshot.totalArchived).toBe(0);
    expect(snapshot.archivedRev).toBe(2);
    expect(publishedSnapshots).toEqual([{ archivedRev: 2, totalArchived: 0 }]);
  });

  it("merges tool_summaries from session_head_delta events", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const now = "2024-01-01T00:00:00.000Z";
    const task = mkTask("task-1", "ws-1", now);
    const session = mkSession("session-1", "task-1", "ws-1", now);
    const summary = mkSummary(session, now);

    const turn: SessionHeadSnapshot["turns"][number] = {
      turn_id: "turn-1",
      session_id: session.id,
      run_id: null,
      user_message_id: null,
      status: "completed",
      start_seq: 1,
      end_seq: 2,
      started_at: now,
      updated_at: now,
      assistant_partial: null,
      thought_partial: null,
      metrics_json: null,
      tool_total: 1,
      tool_pending: 0,
      tool_running: 0,
      tool_completed: 1,
      tool_failed: 0,
    };

    const head: SessionHeadSnapshot = {
      ...mkHead(session),
      turns: [turn],
      tool_summaries: [],
    };

    const activeSummary = mkActiveSummary(task, summary, head, now);
    const activeSnapshot: WorkspaceActiveSnapshot = {
      workspace_id: "ws-1",
      snapshot_rev: 1,
      archived_rev: 0,
      active: { total_count: 1, tasks: [activeSummary] },
    };
    const activeHeads: WorkspaceActiveHeadBatch = {
      workspace_id: "ws-1",
      snapshot_rev: 1,
      heads: [head],
    };

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "snapshot",
        rev: 1,
        active_snapshot: activeSnapshot,
        active_heads: activeHeads,
      }),
    );
    await waitForCondition(() => store.getSnapshot().initialized);

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 2,
        event: {
          type: "session_head_delta",
          workspace_id: "ws-1",
          snapshot_rev: 2,
          delta: {
            session_id: session.id,
            last_event_seq: 2,
            state_rev: 0,
            tool_summaries: [
              {
                session_id: session.id,
                tool_call_id: "call-1",
                turn_id: "turn-1",
                tool_kind: "tool",
                title: "Tool",
                status: "completed",
                input_preview: null,
                output_preview: null,
                created_at: now,
                updated_at: now,
              },
            ],
          },
        },
      }),
    );

  const updated = store.getSessionHeadSnapshot(session.id);
  expect(updated?.tool_summaries?.length ?? 0).toBe(1);
  expect(updated?.tool_summaries?.[0]?.tool_call_id).toBe("call-1");
  });

  it("does not synthesize head activity from summary-only state when the first delta arrives", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const now = "2024-01-01T00:00:00.000Z";
    const task = mkTask("task-1", "ws-1", now);
    const session = mkSession("session-1", "task-1", "ws-1", now);
    const summary: SessionSnapshotSummary = {
      ...mkSummary(session, now),
      activity: { is_working: true, last_turn_status: "running" },
      last_event_seq: 4,
      state_rev: 4,
    };

    const activeSnapshot: WorkspaceActiveSnapshot = {
      workspace_id: "ws-1",
      snapshot_rev: 1,
      archived_rev: 0,
      active: {
        total_count: 1,
        tasks: [
          {
            task,
            primary_session: summary,
            sessions: [summary],
            sort_at: now,
          } as WorkspaceActiveTaskSummary,
        ],
      },
    };

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "snapshot",
        rev: 1,
        active_snapshot: activeSnapshot,
      }),
    );

    await waitForCondition(() => store.getSnapshot().initialized);

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 2,
        event: {
          type: "session_head_delta",
          workspace_id: "ws-1",
          snapshot_rev: 2,
          delta: {
            session_id: session.id,
            last_event_seq: 5,
            state_rev: 5,
            turn: {
              turn_id: "turn-1",
              session_id: session.id,
              run_id: null,
              user_message_id: "user-1",
              status: "completed",
              start_seq: 1,
              end_seq: 2,
              started_at: now,
              updated_at: now,
              assistant_partial: null,
              thought_partial: null,
              metrics_json: null,
              tool_total: 0,
              tool_pending: 0,
              tool_running: 0,
              tool_completed: 0,
              tool_failed: 0,
            },
            message: {
              id: "m-1",
              session_id: session.id,
              task_id: task.id,
              turn_id: "turn-1",
              role: "assistant",
              content: "from delta",
              delivery: "immediate",
              created_at: now,
            },
          },
        },
      }),
    );

    const seededHead = store.getSessionHeadSnapshot(session.id);
    expect(seededHead?.activity).toBeUndefined();
    expect(seededHead?.last_event_seq).toBe(5);
    expect(seededHead?.messages?.[0]?.content).toBe("from delta");
  });

  it("keeps session_head_delta transcript tails bounded in the workspace head cache", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const now = "2024-01-01T00:00:00.000Z";
    const task = mkTask("task-1", "ws-1", now);
    const session = mkSession("session-1", "task-1", "ws-1", now);
    const summary: SessionSnapshotSummary = {
      ...mkSummary(session, now),
      last_event_seq: 0,
      projection_rev: 0,
      state_rev: 0,
    };

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "snapshot",
        rev: 1,
        active_snapshot: {
          workspace_id: "ws-1",
          snapshot_rev: 1,
          archived_rev: 0,
          active: {
            total_count: 1,
            tasks: [
              {
                task,
                primary_session: summary,
                sessions: [summary],
                sort_at: now,
              } as WorkspaceActiveTaskSummary,
            ],
          },
        },
      }),
    );

    await waitForCondition(() => store.getSnapshot().initialized);

    for (let index = 0; index < 8; index += 1) {
      const seq = index + 1;
      const createdAt = `2024-01-01T00:00:${String(seq).padStart(2, "0")}.000Z`;
      await asStoreInternals(store).handleStreamMessage(
        JSON.stringify({
          type: "event",
          rev: seq + 1,
          event: {
            type: "session_head_delta",
            workspace_id: "ws-1",
            snapshot_rev: seq + 1,
            delta: {
              session_id: session.id,
              last_event_seq: seq,
              projection_rev: seq,
              state_rev: seq,
              turn: {
                turn_id: `turn-${seq}`,
                session_id: session.id,
                run_id: null,
                user_message_id: `user-${seq}`,
                status: "completed",
                start_seq: seq,
                end_seq: seq + 1,
                started_at: createdAt,
                updated_at: createdAt,
                assistant_partial: null,
                thought_partial: null,
                metrics_json: null,
                tool_total: 0,
                tool_pending: 0,
                tool_running: 0,
                tool_completed: 0,
                tool_failed: 0,
              },
              message: {
                id: `message-${seq}`,
                session_id: session.id,
                task_id: task.id,
                turn_id: `turn-${seq}`,
                role: seq % 2 === 0 ? "assistant" : "user",
                content: `message-${seq}`,
                delivery: "immediate",
                created_at: createdAt,
                updated_at: createdAt,
              },
              event: {
                seq,
                id: `event-${seq}`,
                session_id: session.id,
                turn_id: `turn-${seq}`,
                event_type: "done",
                payload_json: { seq },
                created_at: createdAt,
              },
            },
          },
        }),
      );
    }

    const boundedHead = store.getSessionHeadSnapshot(session.id);
    expect(boundedHead?.turns.map((turn) => turn.turn_id)).toEqual([
      "turn-4",
      "turn-5",
      "turn-6",
      "turn-7",
      "turn-8",
    ]);
    expect(boundedHead?.messages.map((message) => message.turn_id)).toEqual([
      "turn-4",
      "turn-5",
      "turn-6",
      "turn-7",
      "turn-8",
    ]);
    expect(boundedHead?.events).toEqual([]);
    expect(boundedHead?.head_window?.turn_limit).toBe(5);
    expect(boundedHead?.head_window?.event_limit).toBe(0);
    expect(boundedHead?.head_window?.truncated).toBe(true);
  });

  it("evicts unsubscribed non-primary session heads even when the task summary still lists them", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const now = "2024-01-01T00:00:00.000Z";
    const task = mkTask("task-1", "ws-1", now);
    const primarySession = mkSession("session-primary", "task-1", "ws-1", now);
    const warmSessionA = mkSession("session-warm-a", "task-1", "ws-1", now);
    const warmSessionB = mkSession("session-warm-b", "task-1", "ws-1", now);
    const primarySummary = mkSummary(primarySession, now);
    const warmSummaryA = mkSummary(warmSessionA, now);
    const warmSummaryB = mkSummary(warmSessionB, now);

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "snapshot",
        rev: 1,
        active_snapshot: {
          workspace_id: "ws-1",
          snapshot_rev: 1,
          archived_rev: 0,
          active: {
            total_count: 1,
            tasks: [
              {
                task,
                primary_session: primarySummary,
                primary_session_head: mkHead(primarySession),
                sessions: [primarySummary, warmSummaryA, warmSummaryB],
                sort_at: now,
              } as WorkspaceActiveTaskSummary,
            ],
          },
        },
      }),
    );

    await waitForCondition(() => store.getSnapshot().initialized);

    store.setSubscribedSessions([
      { sessionId: warmSessionA.id, replay: { kind: "auto" } },
      { sessionId: warmSessionB.id, replay: { kind: "auto" } },
    ]);

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 2,
        event: {
          type: "session_head_seed",
          workspace_id: "ws-1",
          snapshot_rev: 1,
          head: {
            ...mkHead(warmSessionA),
            turns: [
              {
                turn_id: "turn-warm-a",
                session_id: warmSessionA.id,
                run_id: null,
                user_message_id: "user-warm-a",
                status: "completed",
                start_seq: 1,
                end_seq: 2,
                started_at: now,
                updated_at: now,
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
            last_event_seq: 1,
            messages: [
              {
                id: "msg-warm-a",
                session_id: warmSessionA.id,
                task_id: task.id,
                turn_id: "turn-warm-a",
                role: "assistant",
                content: "warm-a",
                delivery: "immediate",
                created_at: now,
              },
            ],
          },
        },
      }),
    );
    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 3,
        event: {
          type: "session_head_seed",
          workspace_id: "ws-1",
          snapshot_rev: 1,
          head: {
            ...mkHead(warmSessionB),
            turns: [
              {
                turn_id: "turn-warm-b",
                session_id: warmSessionB.id,
                run_id: null,
                user_message_id: "user-warm-b",
                status: "completed",
                start_seq: 1,
                end_seq: 2,
                started_at: now,
                updated_at: now,
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
            last_event_seq: 1,
            messages: [
              {
                id: "msg-warm-b",
                session_id: warmSessionB.id,
                task_id: task.id,
                turn_id: "turn-warm-b",
                role: "assistant",
                content: "warm-b",
                delivery: "immediate",
                created_at: now,
              },
            ],
          },
        },
      }),
    );

    expect(store.getSessionHeadSnapshot(warmSessionA.id)?.messages[0]?.content).toBe("warm-a");
    expect(store.getSessionHeadSnapshot(warmSessionB.id)?.messages[0]?.content).toBe("warm-b");

    store.setSubscribedSessions([{ sessionId: warmSessionA.id, replay: { kind: "auto" } }]);

    expect(store.getSessionHeadSnapshot(warmSessionA.id)).not.toBeNull();
    expect(store.getSessionHeadSnapshot(warmSessionB.id)).toBeNull();

    store.setSubscribedSessions([]);

    expect(store.getSessionHeadSnapshot(warmSessionA.id)).toBeNull();

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 4,
        event: {
          type: "session_head_seed",
          workspace_id: "ws-1",
          snapshot_rev: 1,
          head: {
            ...mkHead(warmSessionA),
            turns: [
              {
                turn_id: "turn-warm-a-late",
                session_id: warmSessionA.id,
                run_id: null,
                user_message_id: "user-warm-a-late",
                status: "completed",
                start_seq: 2,
                end_seq: 3,
                started_at: now,
                updated_at: now,
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
            last_event_seq: 2,
            messages: [
              {
                id: "msg-warm-a-late",
                session_id: warmSessionA.id,
                task_id: task.id,
                turn_id: "turn-warm-a-late",
                role: "assistant",
                content: "late-warm-a",
                delivery: "immediate",
                created_at: now,
              },
            ],
          },
        },
      }),
    );

    expect(store.getSessionHeadSnapshot(warmSessionA.id)).toBeNull();
  });

  it("keeps retained non-primary heads across live and cached snapshot resets", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const now = "2024-01-01T00:00:00.000Z";
    const task = mkTask("task-1", "ws-1", now);
    const primarySession = mkSession("session-primary", "task-1", "ws-1", now);
    const retainedSession = mkSession("session-retained", "task-1", "ws-1", now);
    const primarySummary = mkSummary(primarySession, now);
    const retainedSummary = mkSummary(retainedSession, now);

    const liveTask = {
      task,
      primary_session: primarySummary,
      primary_session_head: mkHead(primarySession),
      sessions: [primarySummary, retainedSummary],
      sort_at: now,
    } as WorkspaceActiveTaskSummary;

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "snapshot",
        rev: 1,
        active_snapshot: {
          workspace_id: "ws-1",
          snapshot_rev: 1,
          archived_rev: 0,
          active: { total_count: 1, tasks: [liveTask] },
        },
      }),
    );

    await waitForCondition(() => store.getSnapshot().initialized);
    store.setSubscribedSessions([{ sessionId: retainedSession.id, replay: { kind: "auto" } }]);

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 2,
        event: {
          type: "session_head_seed",
          workspace_id: "ws-1",
          snapshot_rev: 1,
          head: {
            ...mkHead(retainedSession),
            turns: [
              {
                turn_id: "turn-retained",
                session_id: retainedSession.id,
                run_id: null,
                user_message_id: "user-retained",
                status: "completed",
                start_seq: 1,
                end_seq: 2,
                started_at: now,
                updated_at: now,
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
            last_event_seq: 1,
            messages: [
              {
                id: "msg-retained",
                session_id: retainedSession.id,
                task_id: task.id,
                turn_id: "turn-retained",
                role: "assistant",
                content: "retained-hot-head",
                delivery: "immediate",
                created_at: now,
              },
            ],
          },
        },
      }),
    );

    expect(store.getSessionHeadSnapshot(retainedSession.id)?.messages[0]?.content).toBe(
      "retained-hot-head",
    );

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "snapshot",
        rev: 3,
        active_snapshot: {
          workspace_id: "ws-1",
          snapshot_rev: 3,
          archived_rev: 0,
          active: { total_count: 1, tasks: [liveTask] },
        },
      }),
    );

    expect(store.getSessionHeadSnapshot(retainedSession.id)?.messages[0]?.content).toBe(
      "retained-hot-head",
    );

    store.seedCachedSnapshot({
      v: 1,
      workspaceId: "ws-1",
      snapshotRev: 4,
      archivedRev: 0,
      worktreeVcsSnapshots: [],
      active: {
        totalCount: 1,
        tasks: [{ ...liveTask, primary_session_head: liveTask.primary_session_head ?? null }],
      },
      updatedAtMs: Date.now(),
    });

    expect(store.getSessionHeadSnapshot(retainedSession.id)?.messages[0]?.content).toBe(
      "retained-hot-head",
    );

    const updatedRetainedSummary = {
      ...retainedSummary,
      last_event_seq: 5,
      projection_rev: 5,
      state_rev: 5,
    };
    const updatedLiveTask = {
      ...liveTask,
      sessions: [primarySummary, updatedRetainedSummary],
    } as WorkspaceActiveTaskSummary;

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "snapshot",
        rev: 5,
        active_snapshot: {
          workspace_id: "ws-1",
          snapshot_rev: 5,
          archived_rev: 0,
          active: { total_count: 1, tasks: [updatedLiveTask] },
        },
      }),
    );

    expect(store.getSessionHeadSnapshot(retainedSession.id)).toBeNull();

    store.seedCachedSnapshot({
      v: 1,
      workspaceId: "ws-1",
      snapshotRev: 6,
      archivedRev: 0,
      worktreeVcsSnapshots: [],
      active: {
        totalCount: 1,
        tasks: [{ ...updatedLiveTask, primary_session_head: updatedLiveTask.primary_session_head ?? null }],
      },
      updatedAtMs: Date.now(),
    });

    expect(store.getSessionHeadSnapshot(retainedSession.id)).toBeNull();
  });

  it("does not restore retained heads after the session disappears from refreshed summaries", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const now = "2024-01-01T00:00:00.000Z";
    const task = mkTask("task-1", "ws-1", now);
    const primarySession = mkSession("session-primary", "task-1", "ws-1", now);
    const retainedSession = mkSession("session-retained", "task-1", "ws-1", now);
    const primarySummary = mkSummary(primarySession, now);
    const retainedSummary = mkSummary(retainedSession, now);

    const liveTask = {
      task,
      primary_session: primarySummary,
      primary_session_head: mkHead(primarySession),
      sessions: [primarySummary, retainedSummary],
      sort_at: now,
    } as WorkspaceActiveTaskSummary;
    const prunedTask = {
      ...liveTask,
      sessions: [primarySummary],
    } as WorkspaceActiveTaskSummary;

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "snapshot",
        rev: 1,
        active_snapshot: {
          workspace_id: "ws-1",
          snapshot_rev: 1,
          archived_rev: 0,
          active: { total_count: 1, tasks: [liveTask] },
        },
      }),
    );

    await waitForCondition(() => store.getSnapshot().initialized);
    store.setSubscribedSessions([{ sessionId: retainedSession.id, replay: { kind: "auto" } }]);

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 2,
        event: {
          type: "session_head_seed",
          workspace_id: "ws-1",
          snapshot_rev: 1,
          head: {
            ...mkHead(retainedSession),
            last_event_seq: 1,
          },
        },
      }),
    );

    expect(store.getSessionHeadSnapshot(retainedSession.id)?.session.id).toBe(retainedSession.id);

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "snapshot",
        rev: 3,
        active_snapshot: {
          workspace_id: "ws-1",
          snapshot_rev: 3,
          archived_rev: 0,
          active: { total_count: 1, tasks: [prunedTask] },
        },
      }),
    );

    expect(store.getSessionHeadSnapshot(retainedSession.id)).toBeNull();

    store.seedCachedSnapshot({
      v: 1,
      workspaceId: "ws-1",
      snapshotRev: 4,
      archivedRev: 0,
      worktreeVcsSnapshots: [],
      active: {
        totalCount: 1,
        tasks: [{ ...prunedTask, primary_session_head: prunedTask.primary_session_head ?? null }],
      },
      updatedAtMs: Date.now(),
    });

    expect(store.getSessionHeadSnapshot(retainedSession.id)).toBeNull();
  });

  it("evicts retained non-primary heads when the task is removed incrementally", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const now = "2024-01-01T00:00:00.000Z";
    const archivedAt = "2024-01-02T00:00:00.000Z";
    const task = mkTask("task-1", "ws-1", now);
    const primarySession = mkSession("session-primary", "task-1", "ws-1", now);
    const retainedSession = mkSession("session-retained", "task-1", "ws-1", now);
    const primarySummary = mkSummary(primarySession, now);
    const retainedSummary = mkSummary(retainedSession, now);

    const liveTask = {
      task,
      primary_session: primarySummary,
      primary_session_head: mkHead(primarySession),
      sessions: [primarySummary, retainedSummary],
      sort_at: now,
    } as WorkspaceActiveTaskSummary;

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "snapshot",
        rev: 1,
        active_snapshot: {
          workspace_id: "ws-1",
          snapshot_rev: 1,
          archived_rev: 0,
          active: { total_count: 1, tasks: [liveTask] },
        },
      }),
    );

    await waitForCondition(() => store.getSnapshot().initialized);
    store.setSubscribedSessions([{ sessionId: retainedSession.id, replay: { kind: "auto" } }]);

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 2,
        event: {
          type: "session_head_seed",
          workspace_id: "ws-1",
          snapshot_rev: 1,
          head: {
            ...mkHead(retainedSession),
            last_event_seq: 1,
          },
        },
      }),
    );

    expect(store.getSessionHeadSnapshot(retainedSession.id)?.session.id).toBe(retainedSession.id);

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 3,
        event: {
          type: "task_delta",
          workspace_id: "ws-1",
          snapshot_rev: 1,
          delta: {
            kind: "archived",
            task: {
              ...task,
              archived_at: archivedAt,
              updated_at: archivedAt,
            },
          },
        },
      }),
    );

    expect(store.getSessionHeadSnapshot(retainedSession.id)).toBeNull();
  });

  it("ignores stale worker head upserts and late seeds after a retained session has been evicted", async () => {
    const { WorkspaceActiveSnapshotStoreState } = await import("./workspaceActiveSnapshot/storeState");

    const now = "2024-01-01T00:00:00.000Z";
    const archivedAt = "2024-01-02T00:00:00.000Z";
    const task = mkTask("task-1", "ws-1", now);
    const primarySession = mkSession("session-primary", "task-1", "ws-1", now);
    const retainedSession = mkSession("session-retained", "task-1", "ws-1", now);
    const primarySummary = mkSummary(primarySession, now);
    const retainedSummary = mkSummary(retainedSession, now);
    const state = new WorkspaceActiveSnapshotStoreState("ws-1");

    state.applyWorkspaceSnapshot({
      workspace_id: "ws-1",
      snapshot_rev: 1,
      archived_rev: 0,
      active: {
        total_count: 1,
        tasks: [{
          task,
          primary_session: primarySummary,
          primary_session_head: mkHead(primarySession),
          sessions: [primarySummary, retainedSummary],
          sort_at: now,
        } as WorkspaceActiveTaskSummary],
      },
    });
    state.setRetainedLiveSessionIds([retainedSession.id]);
    state.applySessionHeadSeed({
      ...mkHead(retainedSession),
      last_event_seq: 1,
    });

    expect(state.getSessionHeadSnapshot(retainedSession.id)?.session.id).toBe(retainedSession.id);

    state.applyTaskDelta({
      type: "task_delta",
      workspace_id: "ws-1",
      snapshot_rev: 1,
      delta: {
        kind: "archived",
        task: {
          ...task,
          archived_at: archivedAt,
          updated_at: archivedAt,
        },
      },
    });

    expect(state.getSessionHeadSnapshot(retainedSession.id)).toBeNull();

    state.applySessionHeadSeed({
      ...mkHead(retainedSession),
      last_event_seq: 2,
    });

    expect(state.getSessionHeadSnapshot(retainedSession.id)).toBeNull();

    state.applyWorkerPatch({
      events: [],
      snapshotRev: 1,
      archivedRev: 0,
      activeSessionIds: [],
      persist: false,
      sessionHeadUpserts: {
        [retainedSession.id]: {
          ...mkHead(retainedSession),
          last_event_seq: 2,
        },
      },
    } satisfies WorkspaceActiveSnapshotPatch);

    expect(state.getSessionHeadSnapshot(retainedSession.id)).toBeNull();
  });

  it("filters full worker snapshot replacements through current retention rules", async () => {
    const { WorkspaceActiveSnapshotStoreState } = await import("./workspaceActiveSnapshot/storeState");

    const now = "2024-01-01T00:00:00.000Z";
    const task = mkTask("task-1", "ws-1", now);
    const primarySession = mkSession("session-primary", "task-1", "ws-1", now);
    const retainedSession = mkSession("session-retained", "task-1", "ws-1", now);
    const primarySummary = mkSummary(primarySession, now);
    const retainedSummary = mkSummary(retainedSession, now);
    const state = new WorkspaceActiveSnapshotStoreState("ws-1");

    state.applyWorkspaceSnapshot({
      workspace_id: "ws-1",
      snapshot_rev: 1,
      archived_rev: 0,
      active: {
        total_count: 1,
        tasks: [{
          task,
          primary_session: primarySummary,
          primary_session_head: mkHead(primarySession),
          sessions: [primarySummary, retainedSummary],
          sort_at: now,
        } as WorkspaceActiveTaskSummary],
      },
    });
    state.setRetainedLiveSessionIds([retainedSession.id]);
    state.applySessionHeadSeed({
      ...mkHead(retainedSession),
      last_event_seq: 1,
    });

    expect(state.getSessionHeadSnapshot(retainedSession.id)?.session.id).toBe(retainedSession.id);

    state.setRetainedLiveSessionIds([]);
    expect(state.getSessionHeadSnapshot(retainedSession.id)).toBeNull();

    state.applyWorkerPatch({
      snapshot: state.getSnapshot(),
      events: [],
      snapshotRev: state.getSnapshotRev(),
      archivedRev: state.getArchivedRev(),
      activeSessionIds: state.getActiveSessionIds(),
      persist: false,
      sessionHeadUpserts: {
        [retainedSession.id]: {
          ...mkHead(retainedSession),
          last_event_seq: 2,
        },
      },
    } satisfies WorkspaceActiveSnapshotPatch);

    expect(state.getSessionHeadSnapshot(retainedSession.id)).toBeNull();
  });

  it("reprojects restored retained primary heads onto task summaries across live and cached resets", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const now = "2024-01-01T00:00:00.000Z";
    const task = mkTask("task-1", "ws-1", now);
    const session = mkSession("session-primary", "task-1", "ws-1", now);
    const summary = mkSummary(session, now);
    const head: SessionHeadSnapshot = {
      ...mkHead(session),
      messages: [
        {
          id: "msg-primary",
          session_id: session.id,
          task_id: task.id,
          turn_id: "turn-primary",
          role: "assistant",
          content: "primary-head",
          delivery: "immediate",
          created_at: now,
        },
      ],
      last_event_seq: 1,
    };

    const initialTask = mkActiveSummary(task, summary, head, now);
    const resetTask = {
      ...initialTask,
      primary_session_head: null,
    } as WorkspaceActiveTaskSummary;

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "snapshot",
        rev: 1,
        active_snapshot: {
          workspace_id: "ws-1",
          snapshot_rev: 1,
          archived_rev: 0,
          active: { total_count: 1, tasks: [initialTask] },
        },
      }),
    );

    await waitForCondition(() => store.getSnapshot().initialized);
    expect(store.getSessionHeadSnapshot(session.id)?.session.id).toBe(session.id);
    store.setSubscribedSessions([{ sessionId: session.id, replay: { kind: "auto" } }]);

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "snapshot",
        rev: 2,
        active_snapshot: {
          workspace_id: "ws-1",
          snapshot_rev: 2,
          archived_rev: 0,
          active: { total_count: 1, tasks: [resetTask] },
        },
      }),
    );

    expect(store.getSessionHeadSnapshot(session.id)?.session.id).toBe(session.id);
    expect(store.getSnapshot().tasksById[task.id]?.primarySessionHead?.session.id).toBe(session.id);

    store.seedCachedSnapshot({
      v: 1,
      workspaceId: "ws-1",
      snapshotRev: 3,
      archivedRev: 0,
      worktreeVcsSnapshots: [],
      active: {
        totalCount: 1,
        tasks: [{ ...resetTask, primary_session_head: resetTask.primary_session_head ?? null }],
      },
      updatedAtMs: Date.now(),
    });

    expect(store.getSessionHeadSnapshot(session.id)?.session.id).toBe(session.id);
    expect(store.getSnapshot().tasksById[task.id]?.primarySessionHead?.session.id).toBe(session.id);
  });

  it("applies session_summary_delta preview metadata and canonical activity when revision advances", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const now = "2024-01-01T00:00:00.000Z";
    const later = "2024-01-01T02:00:00.000Z";
    const task = mkTask("task-1", "ws-1", now);
    const session = mkSession("session-1", "task-1", "ws-1", now);
    const summary = mkSummary(session, now);
    const head = mkHead(session);

    const activeSnapshot: WorkspaceActiveSnapshot = {
      workspace_id: "ws-1",
      snapshot_rev: 1,
      archived_rev: 0,
      active: {
        total_count: 1,
        tasks: [mkActiveSummary(task, summary, head, now)],
      },
    };

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "snapshot",
        rev: 1,
        active_snapshot: activeSnapshot,
      }),
    );

    await waitForCondition(() => store.getSnapshot().initialized);

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 2,
        event: {
          type: "session_summary_delta",
          workspace_id: "ws-1",
          snapshot_rev: 2,
          delta: {
            session_id: "session-1",
            task_id: "task-1",
            activity: { is_working: true, last_turn_status: "running" },
            last_message_at: later,
            last_message_preview: "updated preview",
            last_event_seq: 5,
            state_rev: 2,
          },
        },
      }),
    );

    const snapshot = store.getSnapshot();
    const updated = snapshot.tasksById["task-1"].sessions[0];
    expect(updated.activity?.is_working).toBe(true);
    expect(updated.activity?.last_turn_status ?? null).toBe("running");
    expect(updated.last_message_at).toBe(later);
    expect(updated.last_message_preview).toBe("updated preview");
    expect(updated.last_event_seq).toBe(5);
    expect(updated.state_rev).toBe(2);
  });

  it("does not regress canonical activity when session_summary_delta activity is older", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const now = "2024-01-01T00:00:00.000Z";
    const task = mkTask("task-1", "ws-1", now);
    const session = mkSession("session-1", "task-1", "ws-1", now);
    const summary = {
      ...mkSummary(session, now),
      last_event_seq: 10,
      projection_rev: 10,
      state_rev: 10,
      activity: { is_working: true, last_turn_status: "running" as const },
    };
    const head = mkHead(session);

    const activeSnapshot: WorkspaceActiveSnapshot = {
      workspace_id: "ws-1",
      snapshot_rev: 1,
      archived_rev: 0,
      active: {
        total_count: 1,
        tasks: [mkActiveSummary(task, summary, head, now)],
      },
    };

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "snapshot",
        rev: 1,
        active_snapshot: activeSnapshot,
      }),
    );

    await waitForCondition(() => store.getSnapshot().initialized);

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 2,
        event: {
          type: "session_summary_delta",
          workspace_id: "ws-1",
          snapshot_rev: 2,
          delta: {
            session_id: "session-1",
            task_id: "task-1",
            activity: { is_working: false, last_turn_status: "completed" },
            last_event_seq: 8,
            projection_rev: 8,
            state_rev: 8,
          },
        },
      }),
    );

    const updated = store.getSnapshot().tasksById["task-1"].sessions[0];
    expect(updated.activity?.is_working).toBe(true);
    expect(updated.activity?.last_turn_status ?? null).toBe("running");
  });

  it("does not regress monotonic session summary fields when applying session_summary_delta", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const now = "2024-01-01T00:00:00.000Z";
    const later = "2024-01-01T02:00:00.000Z";
    const task = mkTask("task-1", "ws-1", now);
    const session = mkSession("session-1", "task-1", "ws-1", now);
    const head = mkHead(session);
    const seededSummary = { ...mkSummary(session, now), last_message_at: later, last_event_seq: 10, state_rev: 5 };

    const activeSnapshot: WorkspaceActiveSnapshot = {
      workspace_id: "ws-1",
      snapshot_rev: 1,
      archived_rev: 0,
      active: {
        total_count: 1,
        tasks: [mkActiveSummary(task, seededSummary, head, now)],
      },
    };

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "snapshot",
        rev: 1,
        active_snapshot: activeSnapshot,
      }),
    );

    await waitForCondition(() => store.getSnapshot().initialized);

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "event",
        rev: 2,
        event: {
          type: "session_summary_delta",
          workspace_id: "ws-1",
          snapshot_rev: 2,
          delta: {
            session_id: "session-1",
            task_id: "task-1",
            activity: { is_working: false, last_turn_status: "completed" },
            last_message_at: now,
            last_event_seq: 3,
            state_rev: 2,
          },
        },
      }),
    );

    const updated = store.getSnapshot().tasksById["task-1"].sessions[0];
    expect(updated.last_message_at).toBe(later);
    expect(updated.last_event_seq).toBe(10);
    expect(updated.state_rev).toBe(5);
    expect(updated.activity?.is_working).toBe(false);
    expect(updated.activity?.last_turn_status ?? null).toBe(null);
  });

  it("drops malformed primary_session_head when the summary has a durable newer cursor", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const now = "2024-01-01T00:00:00.000Z";
    const task = mkTask("task-1", "ws-1", now);
    const session = mkSession("session-1", "task-1", "ws-1", now);
    const summary = {
      ...mkSummary(session, now),
      last_event_seq: 10,
      projection_rev: 7,
      state_rev: 7,
      activity: { is_working: true, last_turn_status: "running" as const },
    };
    const malformedHead: SessionHeadSnapshot = {
      ...mkHead(session),
      last_event_seq: -7,
      projection_rev: 7,
      state_rev: 7,
      activity: { is_working: true, last_turn_status: "running" },
    };

    const activeSnapshot: WorkspaceActiveSnapshot = {
      workspace_id: "ws-1",
      snapshot_rev: 1,
      archived_rev: 0,
      active: {
        total_count: 1,
        tasks: [mkActiveSummary(task, summary, malformedHead, now)],
      },
    };

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "snapshot",
        rev: 1,
        active_snapshot: activeSnapshot,
      }),
    );

    await waitForCondition(() => store.getSnapshot().initialized);

    const snapshot = store.getSnapshot();
    expect(snapshot.tasksById["task-1"]?.sessions[0]?.last_event_seq).toBe(10);
    expect(snapshot.tasksById["task-1"]?.primarySessionHead).toBeNull();
    expect(store.getSessionHeadSnapshot("session-1")).toBeNull();
  });

  it("keeps a reconnect session head when the event cursor matches but the projection cursor trails", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const now = "2024-01-01T00:00:00.000Z";
    const task = mkTask("task-1", "ws-1", now);
    const session = mkSession("session-1", "task-1", "ws-1", now);
    const summary = {
      ...mkSummary(session, now),
      last_event_seq: 10,
      projection_rev: 7,
      state_rev: 7,
      activity: { is_working: true, last_turn_status: "running" as const },
    };
    const reconnectHead: SessionHeadSnapshot = {
      ...mkHead(session),
      turns: [
        {
          turn_id: "turn-1",
          session_id: "session-1",
          run_id: null,
          user_message_id: "user-1",
          status: "completed",
          start_seq: 1,
          end_seq: 2,
          started_at: now,
          updated_at: now,
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
      last_event_seq: 10,
      projection_rev: 6,
      state_rev: 7,
      activity: { is_working: true, last_turn_status: "running" },
      messages: [
        {
          id: "msg-1",
          session_id: "session-1",
          task_id: "task-1",
          turn_id: "turn-1",
          turn_sequence: 1,
          order_seq: 1,
          role: "assistant",
          content: "recovered message",
          delivery: "immediate",
          created_at: now,
        },
      ],
    };

    const activeSnapshot: WorkspaceActiveSnapshot = {
      workspace_id: "ws-1",
      snapshot_rev: 1,
      archived_rev: 0,
      active: {
        total_count: 1,
        tasks: [mkActiveSummary(task, summary, reconnectHead, now)],
      },
    };

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "snapshot",
        rev: 1,
        active_snapshot: activeSnapshot,
      }),
    );

    await waitForCondition(() => store.getSnapshot().initialized);

    const snapshot = store.getSnapshot();
    expect(snapshot.tasksById["task-1"]?.primarySessionHead?.last_event_seq).toBe(10);
    expect(snapshot.tasksById["task-1"]?.primarySessionHead?.projection_rev).toBe(6);
    expect(
      snapshot.tasksById["task-1"]?.primarySessionHead?.messages.map((message) => message.content),
    ).toEqual(["recovered message"]);
  });

  it("replaces a cached session head when the event cursor advances even if the projection cursor trails", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const now = "2024-01-01T00:00:00.000Z";
    const task = mkTask("task-1", "ws-1", now);
    const session = mkSession("session-1", "task-1", "ws-1", now);
    const initialSummary = {
      ...mkSummary(session, now),
      last_event_seq: 10,
      projection_rev: 7,
      state_rev: 7,
      activity: { is_working: true, last_turn_status: "running" as const },
    };
    const initialHead: SessionHeadSnapshot = {
      ...mkHead(session),
      last_event_seq: 10,
      projection_rev: 7,
      state_rev: 7,
      activity: { is_working: true, last_turn_status: "running" },
    };

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "snapshot",
        rev: 1,
        active_snapshot: {
          workspace_id: "ws-1",
          snapshot_rev: 1,
          archived_rev: 0,
          active: {
            total_count: 1,
            tasks: [mkActiveSummary(task, initialSummary, initialHead, now)],
          },
        },
        active_heads: {
          workspace_id: "ws-1",
          snapshot_rev: 1,
          heads: [initialHead],
        },
      }),
    );

    await waitForCondition(() => store.getSnapshot().initialized);

    const repairedHead: SessionHeadSnapshot = {
      ...initialHead,
      turns: [
        {
          turn_id: "turn-1",
          session_id: "session-1",
          run_id: null,
          user_message_id: "user-1",
          status: "completed",
          start_seq: 1,
          end_seq: 2,
          started_at: now,
          updated_at: now,
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
      last_event_seq: 11,
      projection_rev: 6,
      state_rev: 7,
      activity: { is_working: false, last_turn_status: "completed" },
      messages: [
        {
          id: "msg-repaired",
          session_id: "session-1",
          task_id: "task-1",
          turn_id: "turn-1",
          turn_sequence: 1,
          order_seq: 1,
          role: "assistant",
          content: "repaired transcript",
          delivery: "immediate",
          created_at: now,
        },
      ],
    };
    const repairedSummary = {
      ...initialSummary,
      last_event_seq: 11,
      activity: { is_working: false, last_turn_status: "completed" as const },
    };

    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "snapshot",
        rev: 2,
        active_snapshot: {
          workspace_id: "ws-1",
          snapshot_rev: 2,
          archived_rev: 0,
          active: {
            total_count: 1,
            tasks: [mkActiveSummary(task, repairedSummary, repairedHead, now)],
          },
        },
        active_heads: {
          workspace_id: "ws-1",
          snapshot_rev: 2,
          heads: [repairedHead],
        },
      }),
    );

    const snapshot = store.getSnapshot();
    expect(snapshot.tasksById["task-1"]?.primarySessionHead?.last_event_seq).toBe(11);
    expect(snapshot.tasksById["task-1"]?.primarySessionHead?.projection_rev).toBe(6);
    expect(snapshot.tasksById["task-1"]?.primarySessionHead?.activity?.last_turn_status).toBe(
      "completed",
    );
    expect(store.getSessionHeadSnapshot("session-1")?.messages.map((message) => message.content)).toEqual([
      "repaired transcript",
    ]);
  });

  it("treats no-op session_summary_delta as no change", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const now = "2024-01-01T00:00:00.000Z";
    const later = "2024-01-01T02:00:00.000Z";
    const task = mkTask("task-1", "ws-1", now);
    const session = mkSession("session-1", "task-1", "ws-1", now);
    const head = mkHead(session);
    const seededSummary = {
      ...mkSummary(session, now),
      last_message_at: later,
      last_message_preview: "hello",
      last_event_seq: 10,
      state_rev: 5,
      activity: { is_working: false, last_turn_status: "completed" as const },
    };

    const activeSnapshot: WorkspaceActiveSnapshot = {
      workspace_id: "ws-1",
      snapshot_rev: 1,
      archived_rev: 0,
      active: {
        total_count: 1,
        tasks: [mkActiveSummary(task, seededSummary, head, now)],
      },
    };

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    await asStoreInternals(store).handleStreamMessage(
      JSON.stringify({
        type: "snapshot",
        rev: 1,
        active_snapshot: activeSnapshot,
      }),
    );

    await waitForCondition(() => store.getSnapshot().initialized);

    const changed = asStoreInternals(store).applySessionSummaryDelta({
      type: "session_summary_delta",
      workspace_id: "ws-1",
      snapshot_rev: 2,
      delta: {
        session_id: "session-1",
        task_id: "task-1",
        // older / identical fields that should not cause any update
        last_message_at: now,
        last_message_preview: "hello",
        last_event_seq: 1,
        state_rev: 2,
      },
    });
    expect(changed).toBe(false);

    const updated = store.getSnapshot().tasksById["task-1"].sessions[0];
    expect(updated.last_message_at).toBe(later);
    expect(updated.last_message_preview).toBe("hello");
    expect(updated.last_event_seq).toBe(10);
    expect(updated.state_rev).toBe(5);
    expect(updated.activity?.is_working).toBe(false);
    expect(updated.activity?.last_turn_status).toBe("completed");
  });

  it("rehydrates desktop connection before worker connection resolution when base URL is missing", async () => {
    const { getDaemonClientConfig, syncDesktopDaemonConnectionFromBridge } = await import("../api/client");
    const { resolveWorkerConnectionState } = await import("./workspaceActiveSnapshot/workerConnection");

    const previousTauri = (globalThis as typeof globalThis & { __TAURI__?: unknown }).__TAURI__;

    try {
      (globalThis as typeof globalThis & { __TAURI__?: unknown }).__TAURI__ = {};
      let currentConfig: MockDaemonClientConfig = {
        baseUrl: null,
        wsBaseUrl: null,
        authToken: null,
        runId: null,
      };
      vi.mocked(getDaemonClientConfig).mockImplementation(() => currentConfig);
      vi.mocked(syncDesktopDaemonConnectionFromBridge).mockImplementation(async () => {
        currentConfig = {
          baseUrl: "http://daemon.local",
          wsBaseUrl: "ws://daemon.local",
          authToken: "token-1",
          runId: null,
        };
        return {
          config: {
            baseUrl: null,
            wsBaseUrl: null,
            authToken: null,
            runId: null,
          },
          info: {
            kind: "local",
            base_url: "http://daemon.local",
            browser_query_secret: "token-1",
          },
          synced: true,
          error: null,
        };
      });

      const connection = await resolveWorkerConnectionState({
        workspaceId: "ws-1",
        phase: "worker_init",
        authTokenOverride: null,
        wsBaseUrlOverride: null,
      });

      expect(connection).toMatchObject({
        baseUrl: "http://daemon.local",
        wsBaseUrl: "ws://daemon.local",
        authToken: "token-1",
        runId: null,
      });
      expect(syncDesktopDaemonConnectionFromBridge).toHaveBeenCalledTimes(1);
      const diagnostics = getUiDiagnostics().filter((event) => event.code === "workspace.worker_desktop_bridge_missing_base");
      expect(diagnostics).toHaveLength(0);
    } finally {
      if (previousTauri === undefined) {
        delete (globalThis as typeof globalThis & { __TAURI__?: unknown }).__TAURI__;
      } else {
        (globalThis as typeof globalThis & { __TAURI__?: unknown }).__TAURI__ = previousTauri;
      }
    }
  });

  it("rehydrates desktop auth before worker connection resolution when only a persisted base is available", async () => {
    const { getDaemonClientConfig, syncDesktopDaemonConnectionFromBridge } = await import("../api/client");
    const { resolveWorkerConnectionState } = await import("./workspaceActiveSnapshot/workerConnection");

    const previousTauri = (globalThis as typeof globalThis & { __TAURI__?: unknown }).__TAURI__;

    try {
      (globalThis as typeof globalThis & { __TAURI__?: unknown }).__TAURI__ = {};
      let currentConfig: MockDaemonClientConfig = {
        baseUrl: "http://daemon.local",
        wsBaseUrl: "ws://daemon.local",
        authToken: null,
        runId: null,
      };
      vi.mocked(getDaemonClientConfig).mockImplementation(() => currentConfig);
      vi.mocked(syncDesktopDaemonConnectionFromBridge).mockImplementation(async () => {
        currentConfig = {
          baseUrl: "http://daemon.local",
          wsBaseUrl: "ws://daemon.local",
          authToken: "token-1",
          runId: null,
        };
        return {
          config: {
            baseUrl: "http://daemon.local",
            wsBaseUrl: "ws://daemon.local",
            authToken: null,
            runId: null,
          },
          info: {
            kind: "local",
            base_url: "http://daemon.local",
            browser_query_secret: "token-1",
          },
          synced: true,
          error: null,
        };
      });

      const connection = await resolveWorkerConnectionState({
        workspaceId: "ws-1",
        phase: "worker_init",
        authTokenOverride: null,
        wsBaseUrlOverride: null,
      });

      expect(connection).toMatchObject({
        baseUrl: "http://daemon.local",
        wsBaseUrl: "ws://daemon.local",
        authToken: "token-1",
        runId: null,
      });
      expect(syncDesktopDaemonConnectionFromBridge).toHaveBeenCalledTimes(1);
    } finally {
      if (previousTauri === undefined) {
        delete (globalThis as typeof globalThis & { __TAURI__?: unknown }).__TAURI__;
      } else {
        (globalThis as typeof globalThis & { __TAURI__?: unknown }).__TAURI__ = previousTauri;
      }
    }
  });

  it("emits a desktop bridge invariant diagnostic when worker base URL stays missing", async () => {
    const { getDaemonClientConfig, syncDesktopDaemonConnectionFromBridge } = await import("../api/client");
    const { resolveWorkerConnectionState } = await import("./workspaceActiveSnapshot/workerConnection");

    const previousTauri = (globalThis as typeof globalThis & { __TAURI__?: unknown }).__TAURI__;

    try {
      (globalThis as typeof globalThis & { __TAURI__?: unknown }).__TAURI__ = {};
      vi.mocked(getDaemonClientConfig).mockReturnValue({
        baseUrl: null,
        wsBaseUrl: null,
        authToken: null,
        runId: null,
      });
      vi.mocked(syncDesktopDaemonConnectionFromBridge).mockResolvedValue({
        config: {
          baseUrl: null,
          wsBaseUrl: null,
          authToken: null,
          runId: null,
        },
        info: {
          kind: "local",
          base_url: null,
          browser_query_secret: null,
        },
        synced: true,
        error: null,
      });

      await resolveWorkerConnectionState({
        workspaceId: "ws-1",
        phase: "worker_init",
        authTokenOverride: null,
        wsBaseUrlOverride: null,
      });
      const diagnostics = getUiDiagnostics().filter((event) => event.code === "workspace.worker_desktop_bridge_missing_base");
      expect(diagnostics).toHaveLength(1);
      expect(asRecord(diagnostics[0]?.context).phase).toBe("worker_init");
    } finally {
      if (previousTauri === undefined) {
        delete (globalThis as typeof globalThis & { __TAURI__?: unknown }).__TAURI__;
      } else {
        (globalThis as typeof globalThis & { __TAURI__?: unknown }).__TAURI__ = previousTauri;
      }
    }
  });

  it("emits a desktop bridge invariant diagnostic when worker auth stays missing", async () => {
    const { getDaemonClientConfig, syncDesktopDaemonConnectionFromBridge } = await import("../api/client");
    const { resolveWorkerConnectionState } = await import("./workspaceActiveSnapshot/workerConnection");

    const previousTauri = (globalThis as typeof globalThis & { __TAURI__?: unknown }).__TAURI__;

    try {
      (globalThis as typeof globalThis & { __TAURI__?: unknown }).__TAURI__ = {};
      vi.mocked(getDaemonClientConfig).mockReturnValue({
        baseUrl: "http://daemon.local",
        wsBaseUrl: "ws://daemon.local",
        authToken: null,
        runId: null,
      });
      vi.mocked(syncDesktopDaemonConnectionFromBridge).mockResolvedValue({
        config: {
          baseUrl: "http://daemon.local",
          wsBaseUrl: "ws://daemon.local",
          authToken: null,
          runId: null,
        },
        info: {
          kind: "local",
          base_url: "http://daemon.local",
          browser_query_secret: null,
        },
        synced: true,
        error: null,
      });

      const connection = await resolveWorkerConnectionState({
        workspaceId: "ws-1",
        phase: "worker_init",
        authTokenOverride: null,
        wsBaseUrlOverride: null,
      });

      expect(connection).toMatchObject({
        baseUrl: "http://daemon.local",
        wsBaseUrl: "ws://daemon.local",
        authToken: null,
        runId: null,
      });
      await waitForCondition(
        () => getUiDiagnostics().some((event) => event.code === "workspace.worker_desktop_bridge_missing_auth"),
      );
      expect(syncDesktopDaemonConnectionFromBridge).toHaveBeenCalledTimes(1);
    } finally {
      if (previousTauri === undefined) {
        delete (globalThis as typeof globalThis & { __TAURI__?: unknown }).__TAURI__;
      } else {
        (globalThis as typeof globalThis & { __TAURI__?: unknown }).__TAURI__ = previousTauri;
      }
    }
  });

  it("resolves desktop auth after a canonical config update fills a persisted base", async () => {
    const {
      getDaemonClientConfig,
      syncDesktopDaemonConnectionFromBridge,
    } = await import("../api/client");
    const { resolveWorkerConnectionState } = await import("./workspaceActiveSnapshot/workerConnection");

    type DaemonConfig = {
      baseUrl: string | null;
      wsBaseUrl: string | null;
      authToken: string | null;
      runId: string | null;
    };

    const previousTauri = (globalThis as typeof globalThis & { __TAURI__?: unknown }).__TAURI__;
    let daemonConfig: DaemonConfig = {
      baseUrl: "http://daemon.local",
      wsBaseUrl: "ws://daemon.local",
      authToken: null,
      runId: null,
    };

    try {
      (globalThis as typeof globalThis & { __TAURI__?: unknown }).__TAURI__ = {};
      vi.mocked(getDaemonClientConfig).mockImplementation(() => daemonConfig);
      vi.mocked(syncDesktopDaemonConnectionFromBridge).mockResolvedValue({
        config: daemonConfig,
        info: {
          kind: "local",
          base_url: "http://daemon.local",
          browser_query_secret: null,
        },
        synced: true,
        error: null,
      });

      const missingConnection = await resolveWorkerConnectionState({
        workspaceId: "ws-1",
        phase: "worker_init",
        authTokenOverride: null,
        wsBaseUrlOverride: null,
      });

      expect(missingConnection.authToken).toBeNull();

      daemonConfig = {
        baseUrl: "http://daemon.local",
        wsBaseUrl: "ws://daemon.local",
        authToken: "token-1",
        runId: "run-1",
      };

      const readyConnection = await resolveWorkerConnectionState({
        workspaceId: "ws-1",
        phase: "worker_update_auth",
        authTokenOverride: null,
        wsBaseUrlOverride: null,
      });

      expect(readyConnection).toMatchObject({
        baseUrl: "http://daemon.local",
        wsBaseUrl: "ws://daemon.local",
        authToken: "token-1",
        runId: "run-1",
      });
      expect(syncDesktopDaemonConnectionFromBridge).toHaveBeenCalledTimes(1);
    } finally {
      if (previousTauri === undefined) {
        delete (globalThis as typeof globalThis & { __TAURI__?: unknown }).__TAURI__;
      } else {
        (globalThis as typeof globalThis & { __TAURI__?: unknown }).__TAURI__ = previousTauri;
      }
    }
  });

  it("keeps the snapshot worker stopped in desktop mode even when Worker exists", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");
    const { getDaemonClientConfig } = await import("../api/client");

    const previousTauri = (globalThis as typeof globalThis & { __TAURI__?: unknown }).__TAURI__;
    const previousWorker = globalThis.Worker;

    class WorkerMock {
      static instances: WorkerMock[] = [];
      onmessage: ((event: MessageEvent<unknown>) => void) | null = null;
      postMessage = vi.fn();
      terminate = vi.fn();
      constructor(..._args: unknown[]) {
        WorkerMock.instances.push(this);
      }
    }

    try {
      (globalThis as typeof globalThis & { __TAURI__?: unknown }).__TAURI__ = {};
      vi.stubGlobal("Worker", WorkerMock as unknown as typeof Worker);
      vi.mocked(getDaemonClientConfig).mockReturnValue({
        baseUrl: "http://daemon.local",
        wsBaseUrl: "ws://daemon.local",
        authToken: "token-1",
        runId: "run-1",
      });

      const store = new WorkspaceActiveSnapshotStoreImpl("ws-1");
      store.init();
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(WorkerMock.instances).toHaveLength(0);
      store.destroy();
    } finally {
      if (previousTauri === undefined) {
        delete (globalThis as typeof globalThis & { __TAURI__?: unknown }).__TAURI__;
      } else {
        (globalThis as typeof globalThis & { __TAURI__?: unknown }).__TAURI__ = previousTauri;
      }
      if (previousWorker) {
        vi.stubGlobal("Worker", previousWorker);
      } else {
        Reflect.deleteProperty(globalThis as unknown as Record<string, unknown>, "Worker");
      }
    }
  });

  it("pushes later canonical base and token rotations into an already running worker", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");
    const {
      getDaemonClientConfig,
      subscribeDaemonConfig,
      syncDesktopDaemonConnectionFromBridge,
    } = await import("../api/client");

    type DaemonConfig = {
      baseUrl: string | null;
      wsBaseUrl: string | null;
      authToken: string | null;
      runId: string | null;
    };

    const previousWorker = globalThis.Worker;
    let daemonConfig: DaemonConfig = {
      baseUrl: "http://daemon.old",
      wsBaseUrl: "ws://daemon.old",
      authToken: "token-old",
      runId: "run-old",
    };
    let configListener: unknown = null;
    const emitConfigListener = (config: DaemonConfig) => {
      if (typeof configListener !== "function") {
        throw new Error("Expected subscribeDaemonConfig listener to be registered.");
      }
      (configListener as (value: DaemonConfig) => void)(config);
    };

    class WorkerMock {
      static instances: WorkerMock[] = [];
      onmessage: ((event: MessageEvent<unknown>) => void) | null = null;
      postMessage = vi.fn();
      terminate = vi.fn();
      constructor(..._args: unknown[]) {
        WorkerMock.instances.push(this);
      }
    }

    try {
      vi.stubGlobal("Worker", WorkerMock as unknown as typeof Worker);
      vi.mocked(getDaemonClientConfig).mockImplementation(() => daemonConfig);
      vi.mocked(subscribeDaemonConfig).mockImplementation((listener) => {
        configListener = listener as (config: DaemonConfig) => void;
        return () => {
          configListener = null;
        };
      });

      const store = new WorkspaceActiveSnapshotStoreImpl("ws-1");
      store.init();

      await waitForCondition(() => WorkerMock.instances.length === 1);
      const initCall = WorkerMock.instances[0]?.postMessage.mock.calls.find(
        ([msg]) => asRecord(msg).type === "init",
      );
      expect(initCall).toBeTruthy();
      expect(asRecord(initCall?.[0])).toMatchObject({
        baseUrl: "http://daemon.old",
        wsBaseUrl: "ws://daemon.old",
        authToken: "token-old",
        runId: "run-old",
      });

      daemonConfig = {
        baseUrl: "http://daemon.new",
        wsBaseUrl: "ws://daemon.new",
        authToken: "token-new",
        runId: "run-new",
      };
      emitConfigListener(daemonConfig);

      await waitForCondition(() => {
        const updateCalls = WorkerMock.instances[0]?.postMessage.mock.calls.filter(
          ([msg]) => asRecord(msg).type === "update_auth",
        );
        return Boolean(updateCalls && updateCalls.length === 1);
      });
      const updateCalls = WorkerMock.instances[0]?.postMessage.mock.calls.filter(
        ([msg]) => asRecord(msg).type === "update_auth",
      );
      expect(updateCalls).toHaveLength(1);
      expect(asRecord(updateCalls?.[0]?.[0])).toMatchObject({
        baseUrl: "http://daemon.new",
        wsBaseUrl: "ws://daemon.new",
        authToken: "token-new",
        runId: "run-new",
      });
      expect(syncDesktopDaemonConnectionFromBridge).not.toHaveBeenCalled();
      store.destroy();
    } finally {
      if (previousWorker) {
        vi.stubGlobal("Worker", previousWorker);
      } else {
        Reflect.deleteProperty(globalThis as unknown as Record<string, unknown>, "Worker");
      }
    }
  });

  it("drops stale worker auth updates when canonical desktop state resolves out of order", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");
    const { getDaemonClientConfig, syncDesktopDaemonConnectionFromBridge } = await import("../api/client");

    type BridgeSyncResult = {
      config: {
        baseUrl: string | null;
        wsBaseUrl: string | null;
        authToken: string | null;
        runId: string | null;
      };
      info: {
        kind: "none" | "local" | "ssh";
        intent: "auto_local_bootstrap" | "explicit_local" | "explicit_remote" | "explicit_disconnected";
        local_auto_bootstrap_allowed: boolean;
        base_url: string | null;
        browser_query_secret: string | null;
      } | null;
      synced: boolean;
      error: string | null;
    };

    const previousTauri = (globalThis as typeof globalThis & { __TAURI__?: unknown }).__TAURI__;
    let resolveFirstSync!: (value: BridgeSyncResult) => void;
    const firstSyncPromise = new Promise<BridgeSyncResult>((resolve) => {
      resolveFirstSync = resolve;
    });
    const syncedResult: BridgeSyncResult = {
      config: {
        baseUrl: "http://daemon.local",
        wsBaseUrl: "ws://daemon.local",
        authToken: "bridge-token",
        runId: null,
      },
      info: {
        kind: "local",
        intent: "explicit_local",
        local_auto_bootstrap_allowed: true,
        base_url: "http://daemon.local",
        browser_query_secret: "bridge-token",
      },
      synced: true,
      error: null,
    };

    try {
      (globalThis as typeof globalThis & { __TAURI__?: unknown }).__TAURI__ = {};
      let currentConfig: MockDaemonClientConfig = {
        baseUrl: null,
        wsBaseUrl: null,
        authToken: null,
        runId: null,
      };
      vi.mocked(getDaemonClientConfig).mockImplementation(() => currentConfig);
      vi.mocked(syncDesktopDaemonConnectionFromBridge)
        .mockImplementationOnce(async () => firstSyncPromise)
        .mockImplementation(async () => {
          currentConfig = { ...syncedResult.config };
          return syncedResult;
        });

      const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
      const postMessage = vi.fn();
      (store as unknown as { worker: Worker | null }).worker = {
        postMessage,
        terminate: vi.fn(),
      } as unknown as Worker;

      store.updateAuthConfig({
        authToken: "token-old",
        wsBaseUrl: null,
        baseUrl: null,
        runId: "run-old",
      });
      store.updateAuthConfig({
        authToken: "token-new",
        wsBaseUrl: null,
        baseUrl: null,
        runId: "run-new",
      });

      await Promise.resolve();
      expect(postMessage).not.toHaveBeenCalled();
      currentConfig = { ...syncedResult.config };
      resolveFirstSync(syncedResult);

      await waitForCondition(() => {
        const updateCalls = postMessage.mock.calls.filter(([msg]) => asRecord(msg).type === "update_auth");
        return updateCalls.length === 1;
      });
      const updateCalls = postMessage.mock.calls.filter(([msg]) => asRecord(msg).type === "update_auth");
      expect(updateCalls).toHaveLength(1);
      expect(syncDesktopDaemonConnectionFromBridge).toHaveBeenCalledTimes(1);
      const update = asRecord(updateCalls[0]?.[0]);
      expect(update.authToken).toBe("token-new");
      expect(update.wsBaseUrl).toBe("ws://daemon.local");
      expect(update.baseUrl).toBe("http://daemon.local");
      expect(update.runId).toBe("run-new");
      expect(typeof update.connectionSeq).toBe("number");
      store.destroy();
    } finally {
      if (previousTauri === undefined) {
        delete (globalThis as typeof globalThis & { __TAURI__?: unknown }).__TAURI__;
      } else {
        (globalThis as typeof globalThis & { __TAURI__?: unknown }).__TAURI__ = previousTauri;
      }
    }
  });

  it("emits archived load diagnostics with root cause details", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");
    const { listWorkspaceArchivedTaskSummaries } = await import("../api/client");

    vi.mocked(listWorkspaceArchivedTaskSummaries).mockRejectedValueOnce(new Error("archived fetch boom"));
    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", { disableWorker: true });
    store.ensureArchivedLoaded();

    await waitForCondition(() => store.getSnapshot().fetchState.archived === "error");
    const diagnostics = getUiDiagnostics().filter((event) => event.code === "workspace.archived_load_failed");
    expect(diagnostics).toHaveLength(1);
    expect(String(asRecord(diagnostics[0]?.context).error ?? "")).toContain("archived fetch boom");
    store.destroy();
  });
});
