import type { SessionEvent, SessionHeadSnapshot } from "../../api/client";
import type { WorkspaceActiveSnapshotState } from "../workspaceActiveSnapshotStore";
import { applyReplicaPatches } from "./replicaPatchApply";
import type { ConnectionStatus, InternalEntry, SessionLoadState, SessionMode } from "./entryState";
import type { SessionReplicaCommand, SessionReplicaPatch } from "../sessionReplicaProtocol";
import type { SessionSubscriptionCursor } from "../sessionSubscription";
import {
  buildThoughtCacheKey,
  isFinalThoughtEvent,
  normalizeFinalThoughtPayload,
  readThoughtFullContent,
} from "./thoughtProjection";
import { reconcileOptimisticOverlay } from "./optimisticOverlay";
import {
  emitSubscribedSessions,
  markOpenSessionsRecovering,
  refreshSubscriptions,
} from "./subscriptions";
import type {
  SessionSupervisorSubscribedSessionIdsSink,
  SessionSupervisorWorkspaceSnapshotState,
} from "./workspaceInputs";
import { syncActiveSnapshot as syncWorkspaceAuthorityActiveSnapshot } from "./workspaceAuthority";
import { seedReplicaFromActiveSnapshot } from "./activeSnapshotSeed";
import { dedupeIds, sameIdList } from "./cachePolicy";
import { emitUiDiagnostic } from "../diagnosticsChannel";
import { resolveSessionMode, shouldFailPendingSessionOpen } from "./sessionMode";

type SessionSupervisorCoreLike = {
  workspaceSnapshotState: SessionSupervisorWorkspaceSnapshotState;
  workspaceSessionHeadsById: Map<string, SessionHeadSnapshot>;
  workspaceActivePrimarySessionIds: string[];
  entries: Map<string, InternalEntry>;
  activeTaskSessionIds: string[];
  warmSessionIds: string[];
  subscribedSessionIds: string[];
  subscribedSessionIdsSink: SessionSupervisorSubscribedSessionIdsSink;
  ensureEntry(sessionId: string): InternalEntry;
  resolveSessionMode(sessionId: string, entry?: InternalEntry, explicitMode?: SessionMode): SessionMode | null;
  resetEntryProjectionForReplace(entry: InternalEntry, opts?: { skipPublish?: boolean }): void;
  setSessionLoadState(entry: InternalEntry, next: SessionLoadState): void;
  setFatalError(entry: InternalEntry, message: string): void;
  applyAcpMetaFromEvents(entry: InternalEntry, events: SessionEvent[]): boolean;
  applyGitStatusSnapshotFromEvents(entry: InternalEntry, events: SessionEvent[]): boolean;
  syncStateCache(entry: InternalEntry): void;
  clearSupportLoadError(entry: InternalEntry, key: "state" | "subagentInvocations"): void;
  adoptLoadedSubagentInvocationsRevision(entry: InternalEntry, stateRev: number): void;
  ensureProviderOptions(entry: InternalEntry): Promise<void>;
  ensureSubagentInvocations(entry: InternalEntry, opts?: { force?: boolean }): Promise<void>;
  syncSupportLoadsForOpenSession(entry: InternalEntry): void;
  bumpTurnsRev(entry: InternalEntry): void;
  ensureThoughtCache(entry: InternalEntry): Promise<void>;
  persistThoughtCache(entry: InternalEntry): Promise<void>;
  resolveRequestedStateRev(entry: InternalEntry): number | undefined;
  invalidateSupportLoadsWithoutAuthoritativeRevision(entry: InternalEntry): void;
  shouldFailPendingSessionOpen(): boolean;
  mapConnection(connection: WorkspaceActiveSnapshotState["connection"]): ConnectionStatus;
  setConnection(next: ConnectionStatus): void;
  clearTaskThoughts(taskId: string): Promise<void>;
  publish(): void;
  buildSubscribedSessions(): SessionSubscriptionCursor[];
  replicaDispatch(cmd: SessionReplicaCommand): void;
  refreshSubscriptions(opts?: { emitIfUnchanged?: boolean }): void;
};

export const applyReplicaPatchesToSupervisor = (
  supervisor: SessionSupervisorCoreLike,
  patches: SessionReplicaPatch[],
): void => {
  const { changed, subscriptionCursorsChanged } = applyReplicaPatches(
    {
      workspaceSnapshotState: supervisor.workspaceSnapshotState,
      getEntry: (sessionId) => supervisor.entries.get(sessionId),
      ensureEntry: (sessionId) => supervisor.ensureEntry(sessionId),
      resolveSessionMode: (sessionId, entry, explicitMode) =>
        supervisor.resolveSessionMode(sessionId, entry, explicitMode),
      resetEntryProjectionForReplace: (entry, opts) => supervisor.resetEntryProjectionForReplace(entry, opts),
      setSessionLoadState: (entry, next) => supervisor.setSessionLoadState(entry, next),
      setFatalError: (entry, message) => supervisor.setFatalError(entry, message),
      applyAcpMetaFromEvents: (entry, events) => supervisor.applyAcpMetaFromEvents(entry, events),
      applyGitStatusSnapshotFromEvents: (entry, events) =>
        supervisor.applyGitStatusSnapshotFromEvents(entry, events),
      syncStateCache: (entry) => supervisor.syncStateCache(entry),
      clearSupportLoadError: (entry, key) => supervisor.clearSupportLoadError(entry, key),
      adoptLoadedSubagentInvocationsRevision: (entry, stateRev) =>
        supervisor.adoptLoadedSubagentInvocationsRevision(entry, stateRev),
      ensureProviderOptions: (entry) => supervisor.ensureProviderOptions(entry),
      ensureSubagentInvocations: (entry, opts) => supervisor.ensureSubagentInvocations(entry, opts),
      syncSupportLoadsForOpenSession: (entry) => supervisor.syncSupportLoadsForOpenSession(entry),
      bumpTurnsRev: (entry) => supervisor.bumpTurnsRev(entry),
    },
    patches,
  );
  for (const patch of patches) {
    if (patch.op === "evict") continue;
    const sessionId = String(patch.sessionId || "").trim();
    if (!sessionId) continue;
    const entry = supervisor.entries.get(sessionId);
    if (!entry) continue;
    if (patch.data.session) {
      void supervisor.ensureThoughtCache(entry);
    }
    if (Array.isArray(patch.data.events) && patch.data.events.length > 0) {
      let thoughtChanged = false;
      for (const event of patch.data.events) {
        if (!isFinalThoughtEvent(event)) continue;
        const key = buildThoughtCacheKey(event);
        if (!key) continue;
        const payload = normalizeFinalThoughtPayload(event.payload_json ?? {});
        if (!readThoughtFullContent(payload)) continue;
        const normalizedEvent: SessionEvent = {
          ...event,
          payload_json: payload,
        };
        const existing = entry.thoughtCacheByKey[key];
        if (existing && existing.event.seq === normalizedEvent.seq) continue;
        entry.thoughtCacheByKey = {
          ...entry.thoughtCacheByKey,
          [key]: {
            key,
            event: normalizedEvent,
            updatedAtMs: Date.now(),
          },
        };
        entry.thoughtCacheDirty = true;
        thoughtChanged = true;
      }
      if (thoughtChanged) {
        void supervisor.persistThoughtCache(entry);
      }
    }
    reconcileOptimisticOverlay(entry);
  }
  if (changed) {
    supervisor.publish();
  }
  if (subscriptionCursorsChanged) {
    emitSupervisorSubscribedSessions(supervisor);
  }
};

export const setSupervisorActiveTaskSessionIds = (
  supervisor: SessionSupervisorCoreLike,
  sessionIds: string[],
): void => {
  const next = dedupeIds(sessionIds);
  if (sameIdList(next, supervisor.activeTaskSessionIds)) return;
  const previous = supervisor.activeTaskSessionIds;
  supervisor.activeTaskSessionIds = next;
  for (const sessionId of next) {
    if (previous.includes(sessionId)) continue;
    const entry = supervisor.ensureEntry(sessionId);
    seedReplicaFromActiveSnapshot(
      {
        workspaceSnapshotState: supervisor.workspaceSnapshotState,
        workspaceSessionHeadsById: supervisor.workspaceSessionHeadsById,
        dispatchSeedHead: (cmd) => supervisor.replicaDispatch(cmd),
      },
      sessionId,
      entry,
      { allowRecoveringRefresh: true, allowRepairReplace: true },
    );
  }
  supervisor.refreshSubscriptions({ emitIfUnchanged: true });
};

export const setSupervisorWarmSessionIds = (
  supervisor: SessionSupervisorCoreLike,
  sessionIds: string[],
): void => {
  const next = dedupeIds(sessionIds);
  if (sameIdList(next, supervisor.warmSessionIds)) return;
  supervisor.warmSessionIds = next;
  for (const sessionId of next) {
    const entry = supervisor.ensureEntry(sessionId);
    seedReplicaFromActiveSnapshot(
      {
        workspaceSnapshotState: supervisor.workspaceSnapshotState,
        workspaceSessionHeadsById: supervisor.workspaceSessionHeadsById,
        dispatchSeedHead: (cmd) => supervisor.replicaDispatch(cmd),
      },
      sessionId,
      entry,
      { allowRecoveringRefresh: true, allowRepairReplace: true },
    );
  }
  supervisor.refreshSubscriptions({ emitIfUnchanged: true });
};

export const resolveSupervisorSessionMode = (
  supervisor: SessionSupervisorCoreLike,
  sessionId: string,
  entry?: InternalEntry,
  explicitMode?: SessionMode,
): SessionMode | null => {
  return resolveSessionMode.call(supervisor as never, sessionId, entry, explicitMode);
};

export const shouldFailPendingSupervisorSessionOpen = (
  supervisor: SessionSupervisorCoreLike,
): boolean => shouldFailPendingSessionOpen(supervisor.workspaceSnapshotState);

export const setSupervisorSessionLoadState = (
  entry: InternalEntry,
  next: SessionLoadState,
): void => {
  if (next === "recovering" && entry.recoverySubscriptionPolicy === undefined) {
    entry.recoverySubscriptionPolicy = "reset";
  } else if (next !== "recovering" && entry.freshness !== "recovering") {
    entry.recoverySubscriptionPolicy = undefined;
  }
  if (entry.loadState === next) return;
  entry.loadState = next;
};

export const bumpSupervisorTurnsRev = (entry: InternalEntry): void => {
  entry.turnsRev += 1;
};

export const bumpSupervisorMessagesRev = (entry: InternalEntry): void => {
  entry.messagesRev += 1;
};

export const bumpSupervisorEventsRev = (entry: InternalEntry): void => {
  entry.eventsRev += 1;
};

export const setSupervisorFatalError = (entry: InternalEntry, message: string): void => {
  emitUiDiagnostic({
    source: "session_supervisor",
    code: "session.load_fatal",
    severity: "error",
    fatal: true,
    message,
    context: {
      sessionId: entry.sessionId,
      mode: entry.mode ?? null,
    },
  });
  entry.error = message;
  setSupervisorSessionLoadState(entry, "fatal");
};

export const createSessionLifecycleHost = (supervisor: SessionSupervisorCoreLike) => {
  return {
    entries: supervisor.entries,
    getWorkspaceSnapshotState: () => supervisor.workspaceSnapshotState,
    getWorkspaceSessionHeadsById: () => supervisor.workspaceSessionHeadsById,
    getActiveTaskSessionIds: () => supervisor.activeTaskSessionIds,
    setActiveTaskSessionIds: (sessionIds: string[]) => {
      supervisor.activeTaskSessionIds = sessionIds;
    },
    getWarmSessionIds: () => supervisor.warmSessionIds,
    setWarmSessionIds: (sessionIds: string[]) => {
      supervisor.warmSessionIds = sessionIds;
    },
    getSubscribedSessionIds: () => supervisor.subscribedSessionIds,
    setSubscribedSessionIds: (sessionIds: string[]) => {
      supervisor.subscribedSessionIds = sessionIds;
    },
    ensureEntry: (sessionId: string) => supervisor.ensureEntry(sessionId),
    resolveRequestedStateRev: (entry: InternalEntry) => supervisor.resolveRequestedStateRev(entry),
    invalidateSupportLoadsWithoutAuthoritativeRevision: (entry: InternalEntry) =>
      supervisor.invalidateSupportLoadsWithoutAuthoritativeRevision(entry),
    setSessionLoadState: (entry: InternalEntry, next: SessionLoadState) =>
      supervisor.setSessionLoadState(entry, next),
    setFatalError: (entry: InternalEntry, message: string) => supervisor.setFatalError(entry, message),
    syncSupportLoadsForOpenSession: (entry: InternalEntry) => supervisor.syncSupportLoadsForOpenSession(entry),
    resolveSessionMode: (sessionId: string, entry?: InternalEntry, explicitMode?: SessionMode) =>
      supervisor.resolveSessionMode(sessionId, entry, explicitMode),
    shouldFailPendingSessionOpen: () => supervisor.shouldFailPendingSessionOpen(),
    refreshSubscriptions: (opts?: { emitIfUnchanged?: boolean }) => refreshSupervisorSubscriptions(supervisor, opts),
    publish: () => supervisor.publish(),
    replicaDispatch: (cmd: SessionReplicaCommand) => supervisor.replicaDispatch(cmd),
  };
};

export const createWorkspaceAuthorityHost = (supervisor: SessionSupervisorCoreLike) => {
  return {
    getWorkspaceSnapshotState: () => supervisor.workspaceSnapshotState,
    setWorkspaceSnapshotState: (state: SessionSupervisorWorkspaceSnapshotState) => {
      supervisor.workspaceSnapshotState = state;
    },
    getWorkspaceSessionHeadsById: () => supervisor.workspaceSessionHeadsById,
    setWorkspaceSessionHeadsById: (heads: Map<string, SessionHeadSnapshot>) => {
      supervisor.workspaceSessionHeadsById = heads;
    },
    getWorkspaceActivePrimarySessionIds: () => supervisor.workspaceActivePrimarySessionIds,
    setWorkspaceActivePrimarySessionIds: (sessionIds: string[]) => {
      supervisor.workspaceActivePrimarySessionIds = sessionIds;
    },
    getActiveTaskSessionIds: () => supervisor.activeTaskSessionIds,
    getWarmSessionIds: () => supervisor.warmSessionIds,
    mapConnection: (connection: WorkspaceActiveSnapshotState["connection"]) =>
      supervisor.mapConnection(connection),
    setConnection: (next: ReturnType<typeof supervisor.mapConnection>) => supervisor.setConnection(next),
    syncActiveSnapshot: (state: WorkspaceActiveSnapshotState) => syncSupervisorActiveSnapshot(supervisor, state),
    markOpenSessionsRecovering: () => markOpenSessionsRecoveringForSupervisor(supervisor),
    rehydrateRecoveringOpenSessions: () => rehydrateRecoveringOpenSessions(supervisor),
    refreshSubscriptions: (opts?: { emitIfUnchanged?: boolean }) => refreshSupervisorSubscriptions(supervisor, opts),
    emitSubscribedSessions: () => emitSupervisorSubscribedSessions(supervisor),
    clearTaskThoughts: (taskId: string) => supervisor.clearTaskThoughts(taskId),
    publish: () => supervisor.publish(),
    syncSupportLoadsForOpenSession: (entry: InternalEntry) => supervisor.syncSupportLoadsForOpenSession(entry),
    replicaDispatch: (cmd: SessionReplicaCommand) => supervisor.replicaDispatch(cmd),
    entries: supervisor.entries,
    ensureEntry: (sessionId: string) => supervisor.ensureEntry(sessionId),
    setSessionLoadState: (entry: InternalEntry, next: SessionLoadState) =>
      supervisor.setSessionLoadState(entry, next),
  };
};

export const createWorkspaceActiveSyncHost = (supervisor: SessionSupervisorCoreLike) => {
  return {
    ensureEntry: (sessionId: string) => supervisor.ensureEntry(sessionId),
    getWorkspaceSessionHeadsById: () => supervisor.workspaceSessionHeadsById,
    replicaDispatch: (cmd: SessionReplicaCommand) => supervisor.replicaDispatch(cmd),
  };
};

export const markOpenSessionsRecoveringForSupervisor = (supervisor: SessionSupervisorCoreLike): void => {
  markOpenSessionsRecovering({
    entries: supervisor.entries,
    emitSubscribedSessions: () => emitSupervisorSubscribedSessions(supervisor),
    publish: () => supervisor.publish(),
  });
};

export const rehydrateRecoveringOpenSessions = (supervisor: SessionSupervisorCoreLike): void => {
  let changed = false;
  for (const entry of supervisor.entries.values()) {
    if (entry.refCount <= 0) continue;
    if (entry.loadState !== "recovering" && entry.freshness !== "recovering") continue;
    entry.error = undefined;
    entry.updatedAtMs = Date.now();
    changed = true;
    supervisor.replicaDispatch({
      type: "hydrate_session_head",
      sessionId: entry.sessionId,
      force: true,
      silent: true,
    });
  }
  if (changed) {
    supervisor.publish();
  }
};

export const refreshSupervisorSubscriptions = (
  supervisor: SessionSupervisorCoreLike,
  opts?: { emitIfUnchanged?: boolean },
): void => {
  refreshSubscriptions({
    entries: supervisor.entries,
    activeTaskSessionIds: supervisor.activeTaskSessionIds,
    workspaceActivePrimarySessionIds: supervisor.workspaceActivePrimarySessionIds,
    warmSessionIds: supervisor.warmSessionIds,
    subscribedSessionIds: supervisor.subscribedSessionIds,
    setSubscribedSessionIds: (next) => {
      supervisor.subscribedSessionIds = next;
    },
    emitSubscribedSessions: () => emitSupervisorSubscribedSessions(supervisor),
    ensureEntry: (sessionId) => supervisor.ensureEntry(sessionId),
    publish: () => supervisor.publish(),
  }, opts);
};

export const emitSupervisorSubscribedSessions = (supervisor: SessionSupervisorCoreLike): void => {
  emitSubscribedSessions(supervisor.subscribedSessionIdsSink, supervisor.buildSubscribedSessions());
};

export const syncSupervisorActiveSnapshot = (
  supervisor: SessionSupervisorCoreLike,
  state: WorkspaceActiveSnapshotState,
): void => {
  syncWorkspaceAuthorityActiveSnapshot(createWorkspaceActiveSyncHost(supervisor), state);
};
