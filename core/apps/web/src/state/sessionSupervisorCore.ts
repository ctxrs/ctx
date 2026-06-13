import {
  type GitStatusSummary,
  type Message,
  type ProviderOptions,
  type Session,
  type SessionHeadSnapshot,
  type SessionState,
  type SessionTurn,
  type SubagentInvocation,
} from "../api/client";
import type { WorkspaceActiveSnapshotState } from "./workspaceActiveSnapshotStore";
import { type PersistedTaskThoughtsV1 } from "./uiStateStore";
import { SessionReplicaBridge } from "./sessionReplicaBridge";
import type {
  SessionReplicaCommand,
  SessionReplicaPatch,
} from "./sessionReplicaProtocol";
import type { SessionSubscriptionCursor } from "./sessionSubscription";
import {
  createInternalEntry,
  type InternalEntry,
  type OpenOptions,
  type SessionCacheEntry,
  type SessionLoadState,
  type SessionMode,
  type SessionSupportLoadErrorKey,
  type SessionSupervisorSnapshot,
} from "./sessionSupervisor/entryState";
import {
  mergeEvents,
  mergeMessages,
  mergeTurns,
  ensureTurnFromEvent,
  applyEventToTurns,
} from "./sessionSupervisor/eventProjection";
import {
  applyState,
  applyToolSummaries,
  persistHead,
  resetEntryProjectionForReplace,
  syncStateCache,
} from "./sessionSupervisor/headProjection";
import {
  evictIfNeeded,
  mapConnection,
  publish,
  setConnection,
} from "./sessionSupervisor/snapshotProjection";
import {
  applyAcpMeta,
  applyAcpMetaFromEvents,
  applyGitStatusSnapshotFromEvents,
  ensureProviderOptions,
  ensureState,
  ensureSubagentInvocations,
  resolveRequestedStateRev,
} from "./sessionSupervisor/hydration";
import {
  clearTaskThoughts,
  ensureThoughtCache,
  overlayThoughtCacheOnEvents,
  overlayThoughtCacheOnTurns,
  persistThoughtCache,
  resolveEntryWorkspaceOwnerScope,
  resolveWorkspaceOwnerScope,
} from "./sessionSupervisor/thoughtCache";
import {
  reconcileActivityFromTurns,
} from "./sessionSupervisor/cachePolicy";
import {
  addOptimisticQueueRemovalId,
  removeOptimisticQueuedMessage,
  removeOptimisticQueueRemovalId,
  removeOptimisticThreadMessage,
  upsertOptimisticQueuedMessage,
  upsertOptimisticThreadMessage,
} from "./sessionSupervisor/optimisticOverlay";
import {
  applyReplicaPatchesToSupervisor,
  createSessionLifecycleHost,
  createWorkspaceActiveSyncHost,
  createWorkspaceAuthorityHost,
  emitSupervisorSubscribedSessions,
  markOpenSessionsRecoveringForSupervisor,
  bumpSupervisorEventsRev,
  bumpSupervisorMessagesRev,
  bumpSupervisorTurnsRev,
  resolveSupervisorSessionMode,
  setSupervisorFatalError,
  setSupervisorSessionLoadState,
  setSupervisorActiveTaskSessionIds,
  setSupervisorWarmSessionIds,
  shouldFailPendingSupervisorSessionOpen,
  refreshSupervisorSubscriptions,
  rehydrateRecoveringOpenSessions,
  syncSupervisorActiveSnapshot,
} from "./sessionSupervisor/coreSupport";
import {
  setSupervisorMessages,
  setSupervisorSession,
  setSupervisorSessionActivity,
  setSupervisorTurns,
} from "./sessionSupervisor/manualMutations";
import {
  buildSubscribedSessions,
} from "./sessionSupervisor/subscriptions";
import type { SessionActivityState } from "@ctx/types";
import {
  beginSessionOpen as beginSessionLifecycleOpen,
  closeSession as closeSessionLifecycle,
  commitSessionOpenMode as commitSessionLifecycleOpenMode,
  dropSessionEntry as dropSessionLifecycleEntry,
  failPendingSessionOpen as failPendingLifecycleOpen,
  openSession as openSessionLifecycle,
  refreshSession as refreshSessionLifecycle,
} from "./sessionSupervisor/sessionLifecycle";
import {
  EVENT_BUFFER_LIMIT,
  HEAD_LIMIT,
  MAX_CACHED_SESSIONS,
  TURN_PAGE_LIMIT,
  WARM_TTL_MS,
} from "./sessionSupervisor/config";
import { loadMoreTurnsForEntry, loadTurnToolsForEntry } from "./sessionSupervisor/historySupport";
import {
  adoptLoadedSubagentInvocationsRevision,
  clearSupportLoadError,
  invalidateSupportLoadsWithoutAuthoritativeRevision,
  setSupportLoadError,
  syncSupportLoadsForOpenSession,
} from "./sessionSupervisor/supportLoads";
import type {
  SessionSupervisorSubscribedSessionIdsSink,
  SessionSupervisorWorkspaceEvent,
  SessionSupervisorWorkspaceSessionHeads,
  SessionSupervisorWorkspaceSnapshotState,
} from "./sessionSupervisor/workspaceInputs";
import {
  ingestWorkspaceEvent as ingestWorkspaceAuthorityEvent,
  setWorkspaceSessionHeads as setWorkspaceAuthoritySessionHeads,
  setWorkspaceSnapshotState as setWorkspaceAuthoritySnapshotState,
  upsertWorkspaceSessionHead as upsertWorkspaceAuthoritySessionHead,
} from "./sessionSupervisor/workspaceAuthority";

export type {
  SessionCacheEntry,
  SessionLoadState,
  SessionMode,
  SessionSupportLoadErrorKey,
  SessionSupervisorSnapshot,
} from "./sessionSupervisor/entryState";

// The daemon serializes transient events with `seq: null` (see Rust `SessionEvent` Serialize).
// We assign a stable synthetic seq in a negative JS-safe range so sorting never scrambles
// streaming partials (assistant chunks), and these events never look durable (seq >= 0).
const TRANSIENT_SEQ_START = -4503599627370496; // -(2 ** 52)

export class SessionSupervisor {
  eventBufferLimit = EVENT_BUFFER_LIMIT;
  maxCachedSessions = MAX_CACHED_SESSIONS;
  listeners = new Set<() => void>();
  snapshot: SessionSupervisorSnapshot = { connection: "idle", sessions: {} };
  entries = new Map<string, InternalEntry>();
  private replica: SessionReplicaBridge;
  private activeTaskSessionIds: string[] = [];
  private warmSessionIds: string[] = [];
  private subscribedSessionIds: string[] = [];
  private subscribedSessionIdsSink: SessionSupervisorSubscribedSessionIdsSink = null;
  providerOptionsCache = new Map<string, ProviderOptions>();
  providerOptionsInFlight = new Map<string, Promise<ProviderOptions | undefined>>();
  taskThoughtCache = new Map<string, PersistedTaskThoughtsV1>();
  taskThoughtCacheLoading = new Map<string, Promise<PersistedTaskThoughtsV1>>();
  stateCacheBySessionId = new Map<string, { state: SessionState; stateRev?: number }>();
  stateRequestsInFlight = new Map<string, Promise<void>>();
  subagentInvocationsCacheBySessionId = new Map<
    string,
    { invocations: SubagentInvocation[]; stateRev: number }
  >();
  subagentInvocationsRequestsInFlight = new Map<string, Promise<void>>();
  workspaceSnapshotState: SessionSupervisorWorkspaceSnapshotState = null;
  workspaceSessionHeadsById = new Map<string, SessionHeadSnapshot>();
  private workspaceActivePrimarySessionIds: string[] = [];
  replicaDispatch = (cmd: SessionReplicaCommand) => this.replica.dispatch(cmd);
  applyAcpMeta = applyAcpMeta;
  applyAcpMetaFromEvents = applyAcpMetaFromEvents;
  applyGitStatusSnapshotFromEvents = applyGitStatusSnapshotFromEvents;
  ensureProviderOptions = ensureProviderOptions;
  ensureState = ensureState;
  resolveRequestedStateRev = resolveRequestedStateRev;
  ensureSubagentInvocations = ensureSubagentInvocations;
  resolveWorkspaceOwnerScope = resolveWorkspaceOwnerScope;
  resolveEntryWorkspaceOwnerScope = resolveEntryWorkspaceOwnerScope;
  ensureThoughtCache = ensureThoughtCache;
  persistThoughtCache = persistThoughtCache;
  clearTaskThoughts = clearTaskThoughts;
  overlayThoughtCacheOnEvents = overlayThoughtCacheOnEvents;
  overlayThoughtCacheOnTurns = overlayThoughtCacheOnTurns;
  mergeTurns = mergeTurns;
  mergeMessages = mergeMessages;
  mergeEvents = mergeEvents;
  ensureTurnFromEvent = ensureTurnFromEvent;
  applyEventToTurns = applyEventToTurns;
  applyToolSummaries = applyToolSummaries;
  applyState = applyState;
  syncStateCache = syncStateCache;
  persistHead = persistHead;
  resetEntryProjectionForReplace = resetEntryProjectionForReplace;
  evictIfNeeded = evictIfNeeded;
  mapConnection = mapConnection;
  publish = publish;
  setConnection = setConnection;
  syncSupportLoadsForOpenSession = (entry: InternalEntry) =>
    syncSupportLoadsForOpenSession(entry, {
      resolveRequestedStateRev: (nextEntry) => this.resolveRequestedStateRev(nextEntry),
      ensureState: (nextEntry) => this.ensureState(nextEntry),
      ensureSubagentInvocations: (nextEntry) => this.ensureSubagentInvocations(nextEntry),
    });
  private invalidateSupportLoadsWithoutAuthoritativeRevision = (entry: InternalEntry) =>
    invalidateSupportLoadsWithoutAuthoritativeRevision(entry, {
      resolveRequestedStateRev: (nextEntry) => this.resolveRequestedStateRev(nextEntry),
      subagentInvocationsCacheBySessionId: this.subagentInvocationsCacheBySessionId,
      invalidateStateRequest: (nextEntry) => {
        nextEntry.support.stateFetchToken += 1;
        nextEntry.support.stateLoading = false;
        this.stateRequestsInFlight.delete(nextEntry.sessionId);
      },
      invalidateSubagentInvocationsRequest: (nextEntry) => {
        nextEntry.support.subagentInvocationsFetchToken += 1;
        nextEntry.support.subagentInvocationsLoading = false;
        this.subagentInvocationsRequestsInFlight.delete(nextEntry.sessionId);
      },
    });
  adoptLoadedSubagentInvocationsRevision = (entry: InternalEntry, stateRev: number) =>
    adoptLoadedSubagentInvocationsRevision(
      entry,
      stateRev,
      this.subagentInvocationsCacheBySessionId,
    );
  clearSupportLoadError = clearSupportLoadError;
  setSupportLoadError = setSupportLoadError;

  constructor() {
    this.replica = new SessionReplicaBridge(this.handleReplicaPatches, {
      eventBufferLimit: EVENT_BUFFER_LIMIT,
      headLimit: HEAD_LIMIT,
    });
  }

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  getSnapshot = (): SessionSupervisorSnapshot => this.snapshot;

  setSubscribedSessionIdsSink = (sink: SessionSupervisorSubscribedSessionIdsSink) => {
    this.subscribedSessionIdsSink = sink;
    this.emitSubscribedSessions();
  };

  setWorkspaceSnapshotState = (state: SessionSupervisorWorkspaceSnapshotState) => {
    setWorkspaceAuthoritySnapshotState(this.createWorkspaceAuthorityHost(), state);
  };

  setWorkspaceSessionHeads = (heads: SessionSupervisorWorkspaceSessionHeads) => {
    setWorkspaceAuthoritySessionHeads(this.createWorkspaceAuthorityHost(), heads);
  };

  upsertWorkspaceSessionHead = (sessionId: string, head: SessionHeadSnapshot) => {
    upsertWorkspaceAuthoritySessionHead(this.createWorkspaceAuthorityHost(), sessionId, head);
  };

  handleWorkspaceEvent = (evt: SessionSupervisorWorkspaceEvent) => {
    ingestWorkspaceAuthorityEvent(this.createWorkspaceAuthorityHost(), evt);
  };

  beginSessionOpen = (sessionId: string, opts?: OpenOptions) => {
    beginSessionLifecycleOpen(this.createSessionLifecycleHost(), sessionId, opts);
  };
  commitSessionOpenMode = (sessionId: string, mode: SessionMode, opts?: OpenOptions) => {
    commitSessionLifecycleOpenMode(this.createSessionLifecycleHost(), sessionId, mode, opts);
  };

  failPendingSessionOpen = (sessionId: string, message?: string) => {
    failPendingLifecycleOpen(this.createSessionLifecycleHost(), sessionId, message);
  };
  openSession = (sessionId: string, opts?: OpenOptions) => {
    return openSessionLifecycle(this.createSessionLifecycleHost(), sessionId, opts);
  };
  closeSession = (sessionId: string, opts?: OpenOptions) => {
    closeSessionLifecycle(this.createSessionLifecycleHost(), sessionId, opts);
  };
  refreshSession = (sessionId: string, opts?: OpenOptions) => {
    refreshSessionLifecycle(this.createSessionLifecycleHost(), sessionId, opts);
  };

  loadSessionState = (sessionId: string, opts?: { force?: boolean }) => {
    const id = String(sessionId || "").trim();
    if (!id) return;
    const entry = this.ensureEntry(id);
    void this.ensureState(entry, {
      ...opts,
      allowEntryStateRevFallback: entry.refCount <= 0,
    });
  };
  loadSubagentInvocations = (sessionId: string, opts?: { force?: boolean }) => {
    const id = String(sessionId || "").trim();
    if (!id) return;
    const entry = this.ensureEntry(id);
    void this.ensureSubagentInvocations(entry, {
      ...opts,
      allowEntryStateRevFallback: entry.refCount <= 0,
    });
  };
  refreshQueue = (sessionId: string) => {
    this.replica.dispatch({ type: "refresh_session", sessionId });
  };
  getSubscribedSessionIds = (): string[] => this.subscribedSessionIds.slice();

  private buildSubscribedSessions(): SessionSubscriptionCursor[] {
    return buildSubscribedSessions(
      this.subscribedSessionIds,
      this.entries,
      this.workspaceSessionHeadsById,
      this.activeTaskSessionIds,
      this.workspaceActivePrimarySessionIds,
      this.warmSessionIds,
    );
  }

  setActiveTaskSessionIds = (sessionIds: string[]) => {
    setSupervisorActiveTaskSessionIds(this as never, sessionIds);
  };
  setWarmSessionIds = (sessionIds: string[]) => {
    setSupervisorWarmSessionIds(this as never, sessionIds);
  };
  setSession = (session: Session) => {
    setSupervisorSession(this as never, session);
  };
  setSessionActivity = (sessionId: string, activity: SessionActivityState | null) => {
    setSupervisorSessionActivity(this as never, sessionId, activity);
  };
  setMessages = (sessionId: string, messages: Message[], opts?: { replace?: boolean }) => {
    setSupervisorMessages(this as never, sessionId, messages, opts);
  };

  setTurns = (sessionId: string, turns: SessionTurn[], opts?: { replace?: boolean }) => {
    setSupervisorTurns(this as never, sessionId, turns, opts);
  };

  upsertOptimisticThreadMessage = (sessionId: string, message: Message) => {
    const id = String(sessionId || "").trim();
    if (!id) return;
    const entry = this.ensureEntry(id);
    if (!upsertOptimisticThreadMessage(entry, message)) return;
    this.publish();
  };

  removeOptimisticThreadMessage = (sessionId: string, messageId: string) => {
    const id = String(sessionId || "").trim();
    if (!id) return;
    const entry = this.ensureEntry(id);
    if (!removeOptimisticThreadMessage(entry, messageId)) return;
    this.publish();
  };

  upsertOptimisticQueuedMessage = (sessionId: string, message: Message) => {
    const id = String(sessionId || "").trim();
    if (!id) return;
    const entry = this.ensureEntry(id);
    if (!upsertOptimisticQueuedMessage(entry, message)) return;
    this.publish();
  };

  removeOptimisticQueuedMessage = (sessionId: string, messageId: string) => {
    const id = String(sessionId || "").trim();
    if (!id) return;
    const entry = this.ensureEntry(id);
    if (!removeOptimisticQueuedMessage(entry, messageId)) return;
    this.publish();
  };

  addOptimisticQueueRemovalId = (sessionId: string, messageId: string) => {
    const id = String(sessionId || "").trim();
    if (!id) return;
    const entry = this.ensureEntry(id);
    if (!addOptimisticQueueRemovalId(entry, messageId)) return;
    this.publish();
  };

  removeOptimisticQueueRemovalId = (sessionId: string, messageId: string) => {
    const id = String(sessionId || "").trim();
    if (!id) return;
    const entry = this.ensureEntry(id);
    if (!removeOptimisticQueueRemovalId(entry, messageId)) return;
    this.publish();
  };

  setError = (sessionId: string, error: string | null) => {
    const id = String(sessionId || "").trim();
    if (!id) return;
    const entry = this.ensureEntry(id);
    if (error) {
      this.setFatalError(entry, error);
    } else {
      entry.error = undefined;
      this.setSessionLoadState(entry, "live");
    }
    entry.updatedAtMs = Date.now();
    this.publish();
  };

  dropSessionEntry = (sessionId: string) => {
    dropSessionLifecycleEntry(this.createSessionLifecycleHost(), sessionId);
  };

  setDiff = (sessionId: string, diff: string) => {
    const entry = this.ensureEntry(sessionId);
    entry.support.diff = diff;
    entry.updatedAtMs = Date.now();
    this.publish();
  };

  setGitStatusSummary = (sessionId: string, summary: GitStatusSummary | null) => {
    const entry = this.ensureEntry(sessionId);
    entry.support.gitStatusSummary = summary;
    entry.updatedAtMs = Date.now();
    this.publish();
  };

  async loadMoreTurns(sessionId: string): Promise<number | null> {
    const entry = this.entries.get(String(sessionId));
    if (!entry) return null;
    return loadMoreTurnsForEntry({
      sessionId,
      entry,
      turnPageLimit: TURN_PAGE_LIMIT,
      resolveEntryWorkspaceOwnerScope: (nextEntry) => this.resolveEntryWorkspaceOwnerScope(nextEntry),
      mergeTurns: (nextEntry, turns) => this.mergeTurns(nextEntry, turns),
      normalizeActivity: (nextEntry) => {
        const normalizedActivity = reconcileActivityFromTurns(nextEntry.activity, nextEntry.turns);
        if (normalizedActivity !== nextEntry.activity) {
          nextEntry.activity = normalizedActivity;
        }
      },
      mergeMessages: (nextEntry, messages) => this.mergeMessages(nextEntry, messages),
      publish: () => this.publish(),
      persistHead: (nextEntry) => this.persistHead(nextEntry),
    });
  }

  async loadTurnTools(sessionId: string, turnId: string) {
    const entry = this.entries.get(String(sessionId));
    if (!entry) return;
    return loadTurnToolsForEntry({
      sessionId,
      turnId,
      entry,
      publish: () => this.publish(),
    });
  }

  private handleReplicaPatches = (patches: SessionReplicaPatch[]) => {
    applyReplicaPatchesToSupervisor(this as never, patches);
  };

  private ensureEntry(sessionId: string): InternalEntry {
    const existing = this.entries.get(sessionId);
    if (existing) return existing;
    const entry = createInternalEntry(sessionId, {
      transientSeqStart: TRANSIENT_SEQ_START,
      warmTtlMs: WARM_TTL_MS,
    });
    this.entries.set(sessionId, entry);
    return entry;
  }

  private createSessionLifecycleHost() {
    return createSessionLifecycleHost(this as never);
  }

  private createWorkspaceAuthorityHost() {
    return createWorkspaceAuthorityHost(this as never);
  }

  private createWorkspaceActiveSyncHost() {
    return createWorkspaceActiveSyncHost(this as never);
  }

  resolveSessionMode(
    sessionId: string,
    entry?: InternalEntry,
    explicitMode?: SessionMode,
  ): SessionMode | null {
    return resolveSupervisorSessionMode(this as never, sessionId, entry, explicitMode);
  }

  private shouldFailPendingSessionOpen() {
    return shouldFailPendingSupervisorSessionOpen(this as never);
  }

  setSessionLoadState(entry: InternalEntry, next: SessionLoadState) {
    setSupervisorSessionLoadState(entry, next);
  }

  bumpTurnsRev(entry: InternalEntry) {
    bumpSupervisorTurnsRev(entry);
  }

  bumpMessagesRev(entry: InternalEntry) {
    bumpSupervisorMessagesRev(entry);
  }

  bumpEventsRev(entry: InternalEntry) {
    bumpSupervisorEventsRev(entry);
  }

  private setFatalError(entry: InternalEntry, message: string) {
    setSupervisorFatalError(entry, message);
  }

  private markOpenSessionsRecovering() {
    markOpenSessionsRecoveringForSupervisor(this as never);
  }

  private rehydrateRecoveringOpenSessions() {
    rehydrateRecoveringOpenSessions(this as never);
  }

  private refreshSubscriptions(opts?: { emitIfUnchanged?: boolean }) {
    refreshSupervisorSubscriptions(this as never, opts);
  }

  private emitSubscribedSessions() {
    emitSupervisorSubscribedSessions(this as never);
  }

  onEvictSession = (sessionId: string) => {
    this.replicaDispatch({ type: "drop_session", sessionId });
  };

  private syncActiveSnapshot(state: WorkspaceActiveSnapshotState) {
    syncSupervisorActiveSnapshot(this as never, state);
  }
}
