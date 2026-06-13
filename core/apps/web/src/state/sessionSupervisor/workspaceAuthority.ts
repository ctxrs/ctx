import { idToString, type SessionHeadSnapshot } from "../../api/client";
import type { WorkspaceActiveSnapshotState } from "../workspaceActiveSnapshotStore";
import { collectWorkspaceActivePrimarySessionIds } from "../workspaceActiveSnapshot/projection";
import type { SessionReplicaCommand, SessionReplicaHeadSeedMode } from "../sessionReplicaProtocol";
import { canRepairFromPartialSessionHead } from "../sessionHeadRepair";
import {
  readWorkspaceEventReceivedAt,
  readWorkspaceEventStreamSource,
} from "../workspaceEventTelemetry";
import { classifyActiveSnapshotSeedMode } from "./activeSnapshotSeed";
import type { ConnectionStatus, InternalEntry } from "./entryState";
import { sameIdList } from "./cachePolicy";
import { applySessionActivityUpdate } from "./subscriptions";
import type {
  SessionSupervisorWorkspaceEvent,
  SessionSupervisorWorkspaceSessionHeads,
  SessionSupervisorWorkspaceSnapshotState,
} from "./workspaceInputs";

type SessionSupervisorWorkspaceAuthorityHost = {
  getWorkspaceSnapshotState(): SessionSupervisorWorkspaceSnapshotState;
  setWorkspaceSnapshotState(state: SessionSupervisorWorkspaceSnapshotState): void;
  getWorkspaceSessionHeadsById(): Map<string, SessionHeadSnapshot>;
  setWorkspaceSessionHeadsById(heads: Map<string, SessionHeadSnapshot>): void;
  getWorkspaceActivePrimarySessionIds(): string[];
  setWorkspaceActivePrimarySessionIds(sessionIds: string[]): void;
  getActiveTaskSessionIds(): string[];
  getWarmSessionIds(): string[];
  mapConnection(connection: WorkspaceActiveSnapshotState["connection"]): ConnectionStatus;
  setConnection(next: ConnectionStatus): void;
  syncActiveSnapshot(state: WorkspaceActiveSnapshotState): void;
  markOpenSessionsRecovering(): void;
  rehydrateRecoveringOpenSessions(): void;
  refreshSubscriptions(opts?: { emitIfUnchanged?: boolean }): void;
  emitSubscribedSessions(): void;
  clearTaskThoughts(taskId: string): Promise<void>;
  publish(): void;
  syncSupportLoadsForOpenSession(entry: InternalEntry): void;
  replicaDispatch(cmd: SessionReplicaCommand): void;
  entries: Map<string, InternalEntry>;
  ensureEntry(sessionId: string): InternalEntry;
  setSessionLoadState(entry: InternalEntry, next: InternalEntry["loadState"]): void;
};

const canApplyWorkspaceSessionHeadMode = (
  entry: InternalEntry,
  mode: SessionReplicaHeadSeedMode,
  head: SessionHeadSnapshot,
): boolean => {
  if (mode !== "repair_replace") return true;
  return canRepairFromPartialSessionHead(entry, head);
};

const shouldApplyWorkspaceSessionHeadMode = (
  mode: SessionReplicaHeadSeedMode,
  recovering: boolean,
): boolean =>
  mode === "bootstrap_seed" || mode === "repair_replace" || recovering;

const workspaceReplicaEventSessionId = (evt: SessionSupervisorWorkspaceEvent): string => {
  switch (evt.type) {
    case "session_head_delta":
      return idToString(evt.delta.session_id);
    case "session_head_seed":
      return idToString(evt.head.session.id);
    case "session_gap":
      return idToString(evt.session_id);
    default:
      return "";
  }
};

const sessionGapSeedFollows = (evt: SessionSupervisorWorkspaceEvent): boolean =>
  evt.type === "session_gap" && (evt as { seed_follows?: unknown }).seed_follows === true;

const workspaceReplicaLane = (
  host: SessionSupervisorWorkspaceAuthorityHost,
  evt: SessionSupervisorWorkspaceEvent,
  sessionId: string,
  streamSource: ReturnType<typeof readWorkspaceEventStreamSource>,
): "foreground" | "workspace" => {
  if (evt.type === "session_gap" && streamSource === "replay") {
    return "workspace";
  }
  return isForegroundReplicaSession(host, sessionId) ? "foreground" : "workspace";
};

const isForegroundReplicaSession = (
  host: SessionSupervisorWorkspaceAuthorityHost,
  sessionId: string,
): boolean => host.getActiveTaskSessionIds().includes(sessionId);

const isRetainedReplicaSession = (
  host: SessionSupervisorWorkspaceAuthorityHost,
  sessionId: string,
  streamSource: ReturnType<typeof readWorkspaceEventStreamSource>,
): boolean => {
  const entry = host.entries.get(sessionId);
  if ((entry?.refCount ?? 0) > 0) return true;
  if (isForegroundReplicaSession(host, sessionId)) return true;
  if (streamSource === "live") return false;
  return host.getWarmSessionIds().includes(sessionId);
};

export const setWorkspaceSnapshotState = (
  host: SessionSupervisorWorkspaceAuthorityHost,
  state: SessionSupervisorWorkspaceSnapshotState,
) => {
  host.setWorkspaceSnapshotState(state);
  if (!state) {
    host.setWorkspaceActivePrimarySessionIds([]);
    host.setConnection("disconnected");
    return;
  }
  const nextWorkspaceActivePrimarySessionIds = collectWorkspaceActivePrimarySessionIds(state);
  const activePrimaryMembershipChanged = !sameIdList(
    nextWorkspaceActivePrimarySessionIds,
    host.getWorkspaceActivePrimarySessionIds(),
  );
  host.setWorkspaceActivePrimarySessionIds(nextWorkspaceActivePrimarySessionIds);
  const next = host.mapConnection(state.connection);
  host.setConnection(next);
  if (next !== "connected") {
    host.syncActiveSnapshot(state);
    host.markOpenSessionsRecovering();
  } else {
    host.rehydrateRecoveringOpenSessions();
    host.syncActiveSnapshot(state);
  }
  host.refreshSubscriptions({ emitIfUnchanged: activePrimaryMembershipChanged });
};

export const setWorkspaceSessionHeads = (
  host: SessionSupervisorWorkspaceAuthorityHost,
  heads: SessionSupervisorWorkspaceSessionHeads,
) => {
  host.setWorkspaceSessionHeadsById(new Map(Object.entries(heads)));
  for (const [sessionId, head] of host.getWorkspaceSessionHeadsById().entries()) {
    const entry = host.entries.get(sessionId);
    if (!entry) continue;
    const recovering = entry.freshness === "recovering" || entry.loadState === "recovering";
    const mode = classifyActiveSnapshotSeedMode(entry, head, { allowRecoveringRefresh: true });
    if (
      !mode ||
      !canApplyWorkspaceSessionHeadMode(entry, mode, head) ||
      !shouldApplyWorkspaceSessionHeadMode(mode, recovering)
    ) {
      continue;
    }
    host.replicaDispatch({ type: "seed_head", sessionId, head, mode });
  }
  for (const entry of host.entries.values()) {
    host.syncSupportLoadsForOpenSession(entry);
  }
  host.emitSubscribedSessions();
};

export const upsertWorkspaceSessionHead = (
  host: SessionSupervisorWorkspaceAuthorityHost,
  sessionId: string,
  head: SessionHeadSnapshot,
) => {
  const normalizedSessionId = idToString(sessionId);
  if (!normalizedSessionId) return;
  const nextHeads = new Map(host.getWorkspaceSessionHeadsById());
  nextHeads.set(normalizedSessionId, head);
  host.setWorkspaceSessionHeadsById(nextHeads);
  const entry = host.entries.get(normalizedSessionId);
  if (entry) {
    const recovering = entry.freshness === "recovering" || entry.loadState === "recovering";
    const mode = classifyActiveSnapshotSeedMode(entry, head, { allowRecoveringRefresh: true });
    const canApply = Boolean(mode && canApplyWorkspaceSessionHeadMode(entry, mode, head));
    const shouldApply = Boolean(mode && shouldApplyWorkspaceSessionHeadMode(mode, recovering));
    if (
      mode &&
      canApply &&
      shouldApply
    ) {
      host.replicaDispatch({ type: "seed_head", sessionId: normalizedSessionId, head, mode });
    }
    host.syncSupportLoadsForOpenSession(entry);
  }
};

export const ingestWorkspaceEvent = (
  host: SessionSupervisorWorkspaceAuthorityHost,
  evt: SessionSupervisorWorkspaceEvent,
) => {
  let changed = false;
  let subscriptionCursorsChanged = false;
  if (evt.type === "archived_task_upsert") {
    const taskId = idToString(evt.task?.task?.id);
    if (taskId) {
      void host.clearTaskThoughts(taskId);
    }
  } else if (evt.type === "archived_task_delete") {
    const taskId = idToString(evt.task_id);
    if (taskId) {
      void host.clearTaskThoughts(taskId);
    }
  } else if (evt.type === "session_gap") {
    const sessionId = idToString(evt.session_id);
    if (sessionId) {
      const seedFollows = sessionGapSeedFollows(evt);
      const entry = host.entries.get(sessionId);
      if (entry) {
        entry.recoverySubscriptionPolicy = seedFollows ? "preserve" : "reset";
        host.setSessionLoadState(entry, "recovering");
        entry.error = undefined;
        entry.turnsHydrated = false;
        entry.updatedAtMs = Date.now();
        changed = true;
        if (entry.subscribed && !seedFollows) {
          subscriptionCursorsChanged = true;
        }
      }
    }
  } else if (evt.type === "session_summary_delta") {
    const sessionId = idToString(evt.delta.session_id);
    if (sessionId) {
      const result = applySessionActivityUpdate(host.entries, sessionId, evt.delta.activity, {
        lastEventSeq: evt.delta.last_event_seq,
        projectionRev: evt.delta.projection_rev,
        stateRev: evt.delta.state_rev,
      });
      changed = result.changed || changed;
      subscriptionCursorsChanged = result.subscriptionCursorChanged || subscriptionCursorsChanged;
    }
  } else if (evt.type === "session_summary") {
    const sessionId = idToString(evt.summary.session.id);
    if (sessionId) {
      const result = applySessionActivityUpdate(host.entries, sessionId, evt.summary.activity, {
        lastEventSeq: evt.summary.last_event_seq,
        projectionRev: evt.summary.projection_rev,
        stateRev: evt.summary.state_rev,
      });
      changed = result.changed || changed;
      subscriptionCursorsChanged = result.subscriptionCursorChanged || subscriptionCursorsChanged;
    }
  }
  if (changed) {
    host.publish();
  }
  if (subscriptionCursorsChanged) {
    host.emitSubscribedSessions();
  }
  // The workspace stream carries all active-task deltas, but the session replica
  // should only spend transcript work on sessions the workbench is retaining.
  const replicaSessionId = workspaceReplicaEventSessionId(evt);
  const streamSource = readWorkspaceEventStreamSource(evt);
  if (!replicaSessionId || !isRetainedReplicaSession(host, replicaSessionId, streamSource)) {
    return;
  }
  host.replicaDispatch({
    type: "workspace_event",
    event: evt,
    lane: workspaceReplicaLane(host, evt, replicaSessionId, streamSource),
    receivedAtMs: readWorkspaceEventReceivedAt(evt),
    streamSource,
  });
};

export const syncActiveSnapshot = (
  host: Pick<
    SessionSupervisorWorkspaceAuthorityHost,
    "ensureEntry" | "getWorkspaceSessionHeadsById" | "replicaDispatch"
  >,
  state: WorkspaceActiveSnapshotState,
) => {
  for (const taskId of state.activeIds) {
    const item = state.tasksById[taskId];
    const sessionId =
      idToString(item?.primarySessionId ?? "") ||
      idToString(item?.task.primary_session_id ?? "") ||
      idToString(item?.primarySessionHead?.session?.id ?? "");
    if (!sessionId) continue;
    const head = host.getWorkspaceSessionHeadsById().get(sessionId) ?? item?.primarySessionHead;
    if (!head) continue;
    const entry = host.ensureEntry(sessionId);
    const mode = classifyActiveSnapshotSeedMode(entry, head, { allowRecoveringRefresh: true });
    const recovering = entry.freshness === "recovering" || entry.loadState === "recovering";
    if (!mode) continue;
    if (!canApplyWorkspaceSessionHeadMode(entry, mode, head)) continue;
    if (!shouldApplyWorkspaceSessionHeadMode(mode, recovering)) continue;
    host.replicaDispatch({ type: "seed_head", sessionId, head, mode });
  }
};
