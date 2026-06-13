import type { SessionHeadSnapshot } from "../../api/client";
import type { SessionReplicaCommand } from "../sessionReplicaProtocol";
import { seedReplicaFromActiveSnapshot } from "./activeSnapshotSeed";
import { resolveReplicaReadyLoadState } from "./authorityPolicy";
import type { InternalEntry, OpenOptions, SessionMode, SessionLoadState } from "./entryState";
import { WARM_TTL_MS, isReplicaAuthority } from "./config";
import { shouldFetchSessionState, shouldFetchSubagentInvocations } from "./supportLoads";
import type {
  SessionSupervisorWorkspaceSessionHeads,
  SessionSupervisorWorkspaceSnapshotState,
} from "./workspaceInputs";

type SessionSupervisorLifecycleHost = {
  entries: Map<string, InternalEntry>;
  getWorkspaceSnapshotState(): SessionSupervisorWorkspaceSnapshotState;
  getWorkspaceSessionHeadsById():
    | Map<string, SessionHeadSnapshot>
    | SessionSupervisorWorkspaceSessionHeads;
  getActiveTaskSessionIds(): string[];
  setActiveTaskSessionIds(sessionIds: string[]): void;
  getWarmSessionIds(): string[];
  setWarmSessionIds(sessionIds: string[]): void;
  getSubscribedSessionIds(): string[];
  setSubscribedSessionIds(sessionIds: string[]): void;
  ensureEntry(sessionId: string): InternalEntry;
  invalidateSupportLoadsWithoutAuthoritativeRevision(entry: InternalEntry): void;
  resolveRequestedStateRev(entry: InternalEntry): number | undefined;
  setSessionLoadState(entry: InternalEntry, next: SessionLoadState): void;
  setFatalError(entry: InternalEntry, message: string): void;
  syncSupportLoadsForOpenSession(entry: InternalEntry): void;
  resolveSessionMode(
    sessionId: string,
    entry?: InternalEntry,
    explicitMode?: SessionMode,
  ): SessionMode | null;
  shouldFailPendingSessionOpen(): boolean;
  refreshSubscriptions(opts?: { emitIfUnchanged?: boolean }): void;
  publish(): void;
  replicaDispatch(cmd: SessionReplicaCommand): void;
};

const beginSessionOpenEntry = (
  host: SessionSupervisorLifecycleHost,
  sessionId: string,
  opts?: OpenOptions,
): InternalEntry | null => {
  const id = String(sessionId ?? "").trim();
  if (!id) return null;
  const entry = host.ensureEntry(id);
  const reopeningSession = entry.refCount === 0;
  entry.refCount += 1;
  entry.warmUntilMs = Date.now() + WARM_TTL_MS;
  if (opts?.mode) {
    entry.mode = opts.mode;
  }
  if (entry.error) {
    entry.error = undefined;
  }
  if (reopeningSession) {
    host.invalidateSupportLoadsWithoutAuthoritativeRevision(entry);
    const requestedStateRev = host.resolveRequestedStateRev(entry);
    if (shouldFetchSessionState({ ...entry.support, stateRev: requestedStateRev })) {
      entry.support.stateAutoLoadKey = undefined;
    }
    if (shouldFetchSubagentInvocations(entry.support, requestedStateRev)) {
      entry.support.subagentAutoLoadKey = undefined;
    }
  }
  host.setSessionLoadState(entry, "pending_hydration");
  return entry;
};

const openSessionWithMode = (
  host: SessionSupervisorLifecycleHost,
  sessionId: string,
  entry: InternalEntry,
  mode: SessionMode,
  opts?: OpenOptions,
) => {
  entry.mode = mode;
  const workspaceSessionHeadsById = host.getWorkspaceSessionHeadsById();
  const seededHead =
    mode === "active"
      ? seedReplicaFromActiveSnapshot(
          {
            workspaceSnapshotState: host.getWorkspaceSnapshotState(),
            workspaceSessionHeadsById:
              workspaceSessionHeadsById instanceof Map
                ? workspaceSessionHeadsById
                : new Map(Object.entries(workspaceSessionHeadsById)),
            dispatchSeedHead: (cmd) => host.replicaDispatch(cmd),
          },
          sessionId,
          entry,
        )
      : false;
  const shouldSkipCache =
    (entry.turnsHydrated ||
      entry.messages.length > 0 ||
      entry.events.length > 0 ||
      typeof entry.lastEventSeq === "number") ||
    entry.freshness !== "bootstrap";
  host.replicaDispatch({
    type: "open_session",
    sessionId,
    force: opts?.force,
    silent: opts?.silent,
    skipCache: shouldSkipCache,
    skipBoundedBootstrapCache: mode === "active",
    forceHydrate: entry.freshness === "recovering" || entry.loadState === "recovering",
    hydrateIfNeeded:
      mode === "archived" ||
      !isReplicaAuthority(entry.freshness) ||
      entry.loadState === "recovering",
  });
  if (mode === "archived") {
    host.setSessionLoadState(entry, "pending_hydration");
    host.syncSupportLoadsForOpenSession(entry);
    return;
  }
  const hasVisibleTranscriptData =
    seededHead || entry.turnsHydrated || entry.messages.length > 0 || entry.events.length > 0;
  if (hasVisibleTranscriptData) {
    host.setSessionLoadState(entry, resolveReplicaReadyLoadState(entry));
    host.syncSupportLoadsForOpenSession(entry);
    return;
  }
  host.setSessionLoadState(entry, "pending_hydration");
  host.syncSupportLoadsForOpenSession(entry);
};

export const beginSessionOpen = (
  host: SessionSupervisorLifecycleHost,
  sessionId: string,
  opts?: OpenOptions,
) => {
  const entry = beginSessionOpenEntry(host, sessionId, opts);
  if (!entry) return;
  host.refreshSubscriptions();
  host.publish();
};

export const commitSessionOpenMode = (
  host: SessionSupervisorLifecycleHost,
  sessionId: string,
  mode: SessionMode,
  opts?: OpenOptions,
) => {
  const id = String(sessionId ?? "").trim();
  if (!id) return;
  const entry = host.entries.get(id);
  if (!entry || entry.refCount <= 0) return;
  openSessionWithMode(host, id, entry, mode, opts);
  host.refreshSubscriptions();
  host.publish();
};

export const failPendingSessionOpen = (
  host: SessionSupervisorLifecycleHost,
  sessionId: string,
  message?: string,
) => {
  const id = String(sessionId ?? "").trim();
  if (!id) return;
  const entry = host.entries.get(id);
  if (!entry || entry.refCount <= 0) return;
  host.setFatalError(entry, message ?? `Session not found in workspace snapshot: ${id}`);
  entry.updatedAtMs = Date.now();
  host.refreshSubscriptions();
  host.publish();
};

export const openSession = (
  host: SessionSupervisorLifecycleHost,
  sessionId: string,
  opts?: OpenOptions,
): (() => void) => {
  const id = String(sessionId ?? "").trim();
  if (!id) return () => {};
  const entry = beginSessionOpenEntry(host, id, opts);
  if (!entry) return () => closeSession(host, id);
  const mode = host.resolveSessionMode(id, entry, opts?.mode);
  if (mode) {
    openSessionWithMode(host, id, entry, mode, opts);
  } else if (host.shouldFailPendingSessionOpen()) {
    host.setFatalError(entry, `Session not found in workspace snapshot: ${id}`);
    entry.updatedAtMs = Date.now();
  }
  host.refreshSubscriptions();
  host.publish();
  return () => closeSession(host, id);
};

export const closeSession = (
  host: SessionSupervisorLifecycleHost,
  sessionId: string,
  _opts?: OpenOptions,
) => {
  const entry = host.entries.get(sessionId);
  if (!entry) return;
  entry.refCount = Math.max(0, entry.refCount - 1);
  entry.warmUntilMs = Date.now() + WARM_TTL_MS;
  host.replicaDispatch({ type: "close_session", sessionId });
  host.refreshSubscriptions();
  host.publish();
};

export const refreshSession = (
  host: SessionSupervisorLifecycleHost,
  sessionId: string,
  opts?: OpenOptions,
) => {
  const id = String(sessionId ?? "").trim();
  if (!id) return;
  const entry = host.entries.get(id);
  if (!entry) return;
  const mode = host.resolveSessionMode(id, entry, opts?.mode);
  if (!mode) {
    if (host.shouldFailPendingSessionOpen()) {
      failPendingSessionOpen(host, id);
    }
    return;
  }
  host.replicaDispatch({
    type: "refresh_session",
    sessionId: id,
  });
  if (mode === "archived" || !isReplicaAuthority(entry.freshness)) {
    host.setSessionLoadState(entry, "pending_hydration");
  }
};

export const dropSessionEntry = (
  host: SessionSupervisorLifecycleHost,
  sessionId: string,
) => {
  const id = String(sessionId ?? "").trim();
  if (!id) return;
  if (!host.entries.has(id)) return;
  host.entries.delete(id);
  host.replicaDispatch({ type: "drop_session", sessionId: id });
  host.setActiveTaskSessionIds(host.getActiveTaskSessionIds().filter((entryId) => entryId !== id));
  host.setWarmSessionIds(host.getWarmSessionIds().filter((entryId) => entryId !== id));
  host.setSubscribedSessionIds(host.getSubscribedSessionIds().filter((entryId) => entryId !== id));
  host.refreshSubscriptions();
  host.publish();
};
