import {
  getProviderOptions,
  getSessionState,
  idToString,
  listSessionSubagentInvocations,
  type GitStatusSummary,
  type ProviderOptions,
  type Session,
  type SessionEvent,
  type SessionHeadSnapshot,
  type SessionState,
  type SubagentInvocation,
} from "../../api/client";
import { findWorkspaceSessionHead } from "../workspaceActiveSnapshot/projection";
import { saveSessionAcpMetaV1 } from "../uiStateStore";
import { normalizeGitStatusSummaryInput } from "./gitStatusNormalization";
import {
  shouldFetchSessionState,
  shouldFetchSubagentInvocations,
} from "./supportLoads";
import { asRecord } from "./eventHydration";
import {
  extractAcpMetaFromEvent,
  hasModelList,
  mergeAcpMetaIntoSharedProviderOptions,
  readAcpCurrentModelId,
  type AcpMeta,
} from "./acpMeta";
import { updateProvidersBootstrap } from "../providersBootstrapStore";
import type { SessionSupervisorWorkspaceSnapshotState } from "./workspaceInputs";
import type { InternalEntry } from "./entryState";

type SessionSupportLoadErrorKey = "state" | "subagentInvocations";

export type SessionSupervisorHydrationEntry = InternalEntry;

export type SessionSupervisorHydrationHost = {
  providerOptionsCache: Map<string, ProviderOptions>;
  providerOptionsInFlight: Map<string, Promise<ProviderOptions | undefined>>;
  stateCacheBySessionId: Map<string, { state: SessionState; stateRev?: number }>;
  stateRequestsInFlight: Map<string, Promise<void>>;
  subagentInvocationsCacheBySessionId: Map<
    string,
    { invocations: SubagentInvocation[]; stateRev: number }
  >;
  subagentInvocationsRequestsInFlight: Map<string, Promise<void>>;
  workspaceSnapshotState: SessionSupervisorWorkspaceSnapshotState;
  workspaceSessionHeadsById: Map<string, SessionHeadSnapshot>;
  entries: Map<string, SessionSupervisorHydrationEntry>;
  publish(): void;
  applyState(
    entry: SessionSupervisorHydrationEntry,
    state: SessionState | null,
    stateRev?: number,
  ): void;
  clearSupportLoadError(
    entry: SessionSupervisorHydrationEntry,
    key: SessionSupportLoadErrorKey,
  ): void;
  setSupportLoadError(
    entry: SessionSupervisorHydrationEntry,
    key: SessionSupportLoadErrorKey,
    value: unknown,
  ): void;
  ensureState(entry: SessionSupervisorHydrationEntry, opts?: SupportLoadOpts): Promise<void>;
  ensureSubagentInvocations(
    entry: SessionSupervisorHydrationEntry,
    opts?: SupportLoadOpts,
  ): Promise<void>;
  resolveRequestedStateRev(entry: SessionSupervisorHydrationEntry): number | undefined;
  syncSupportLoadsForOpenSession(entry: SessionSupervisorHydrationEntry): void;
};

const providerOptionsKey = (session: Session): string =>
  `${idToString(session.workspace_id)}:${session.provider_id}`;

export function applyAcpMeta(
  entry: SessionSupervisorHydrationEntry,
  meta: AcpMeta,
  opts?: { persist?: boolean; syncSharedProviderCatalog?: boolean },
): boolean {
  const nextModels = meta.models ?? entry.acpModels;
  const nextModes = meta.modes ?? entry.acpModes;
  const nextCurrent =
    meta.currentModelId ?? readAcpCurrentModelId(nextModels) ?? entry.acpCurrentModelId;
  const nextCommands = meta.commands ?? entry.acpCommands;
  const nextSlashCommands = meta.slashCommands ?? entry.acpSlashCommands;
  const modelsChanged = JSON.stringify(nextModels ?? null) !== JSON.stringify(entry.acpModels ?? null);
  const modesChanged = JSON.stringify(nextModes ?? null) !== JSON.stringify(entry.acpModes ?? null);
  const currentChanged = nextCurrent !== entry.acpCurrentModelId;
  const commandsChanged = JSON.stringify(nextCommands ?? null) !== JSON.stringify(entry.acpCommands ?? null);
  const slashCommandsChanged =
    JSON.stringify(nextSlashCommands ?? null) !== JSON.stringify(entry.acpSlashCommands ?? null);
  if (!modelsChanged && !modesChanged && !currentChanged && !commandsChanged && !slashCommandsChanged) {
    return false;
  }

  entry.acpModels = nextModels;
  entry.acpModes = nextModes;
  entry.acpCurrentModelId = nextCurrent;
  entry.acpCommands = nextCommands;
  entry.acpSlashCommands = nextSlashCommands;
  entry.acpMetaUpdatedAtMs = Date.now();
  if (opts?.syncSharedProviderCatalog !== false) {
    syncSharedProviderCatalogFromAcpMeta(entry, meta);
  }
  if (opts?.persist !== false && (nextModels || nextModes || nextCurrent || nextCommands || nextSlashCommands)) {
    void saveSessionAcpMetaV1(entry.sessionId, {
      models: nextModels,
      modes: nextModes,
      currentModelId: nextCurrent,
      commands: nextCommands,
      slashCommands: nextSlashCommands,
    });
  }
  return true;
}

function syncSharedProviderCatalogFromAcpMeta(
  entry: SessionSupervisorHydrationEntry,
  meta: AcpMeta,
): void {
  const session = entry.session;
  if (!session) return;
  if (!meta.models && !meta.modes) return;
  const workspaceId = idToString(session.workspace_id);
  if (!workspaceId) return;

  updateProvidersBootstrap(workspaceId, (current) => {
    const existing = current.provider_options[session.provider_id];
    const nextProviderOptions = mergeAcpMetaIntoSharedProviderOptions(
      existing,
      meta,
      session.provider_id,
      workspaceId,
    );
    if (!nextProviderOptions || nextProviderOptions === existing) {
      return current;
    }

    return {
      ...current,
      provider_options: {
        ...current.provider_options,
        [session.provider_id]: nextProviderOptions,
      },
    };
  });
}

export function applyAcpMetaFromEvents(
  this: SessionSupervisorHydrationHost,
  entry: SessionSupervisorHydrationEntry,
  events: SessionEvent[],
): boolean {
  for (let i = events.length - 1; i >= 0; i -= 1) {
    const meta = extractAcpMetaFromEvent(events[i]);
    if (meta) {
      return applyAcpMeta(entry, meta);
    }
  }
  return false;
}

export function applyGitStatusSnapshotFromEvents(
  entry: SessionSupervisorHydrationEntry,
  events: SessionEvent[],
): boolean {
  for (let i = events.length - 1; i >= 0; i -= 1) {
    const event = events[i];
    if (String(event.event_type) !== "notice") continue;
    const payload = event.payload_json;
    if (payload?.kind !== "git_status_snapshot") continue;
    const partial = normalizeGitStatusSummaryInput(payload.summary, payload.entries);
    if (Object.keys(partial).length === 0) return false;
    entry.support.gitStatusSummary = { ...(entry.support.gitStatusSummary ?? {}), ...partial };
    return true;
  }
  return false;
}

const seedAcpMetaFromProviderOptions = (
  entry: SessionSupervisorHydrationEntry,
  opts?: ProviderOptions,
): boolean => {
  if (!opts?.models && !opts?.modes) return false;
  return applyAcpMeta(entry, {
    models: opts.models,
    modes: opts.modes,
    currentModelId: readAcpCurrentModelId(opts.models),
  }, { syncSharedProviderCatalog: false });
};

export async function ensureProviderOptions(
  this: SessionSupervisorHydrationHost,
  entry: SessionSupervisorHydrationEntry,
) {
  if (entry.acpModels && hasModelList(entry.acpModels)) return;
  const session = entry.session;
  if (!session) return;
  const key = providerOptionsKey(session);
  const cached = this.providerOptionsCache.get(key);
  if (cached) {
    if (seedAcpMetaFromProviderOptions(entry, cached)) {
      entry.updatedAtMs = Date.now();
      this.publish();
    }
    return;
  }
  const existing = this.providerOptionsInFlight.get(key);
  if (existing) {
    const opts = await existing.catch(() => undefined);
    if (opts && seedAcpMetaFromProviderOptions(entry, opts)) {
      entry.updatedAtMs = Date.now();
      this.publish();
    }
    return;
  }
  const workspaceId = idToString(session.workspace_id);
  if (!workspaceId) return;
  const request = getProviderOptions(workspaceId, session.provider_id)
    .then((opts) => {
      this.providerOptionsCache.set(key, opts);
      return opts;
    })
    .catch(() => undefined)
    .finally(() => {
      if (this.providerOptionsInFlight.get(key) === request) {
        this.providerOptionsInFlight.delete(key);
      }
    });
  this.providerOptionsInFlight.set(key, request);
  const opts = await request;
  if (opts && seedAcpMetaFromProviderOptions(entry, opts)) {
    entry.updatedAtMs = Date.now();
    this.publish();
  }
}

export function resolveRequestedStateRev(
  this: SessionSupervisorHydrationHost,
  entry: SessionSupervisorHydrationEntry,
): number | undefined {
  const workspaceConnection = this.workspaceSnapshotState?.connection;
  if (
    workspaceConnection &&
    workspaceConnection !== "connected" &&
    (entry.freshness === "authoritative" || entry.freshness === "replica")
  ) {
    return undefined;
  }
  if (entry.freshness !== "authoritative" && entry.freshness !== "replica") return undefined;
  if (typeof entry.stateRev === "number") return entry.stateRev;
  const head = findWorkspaceSessionHead(
    this.workspaceSnapshotState,
    this.workspaceSessionHeadsById,
    entry.sessionId,
  );
  if (!head) return undefined;
  const headRecord = asRecord(head);
  const headStateRev = headRecord?.state_rev ?? headRecord?.stateRev;
  return typeof headStateRev === "number" ? headStateRev : undefined;
}

type SupportLoadOpts = {
  force?: boolean;
  allowEntryStateRevFallback?: boolean;
};

function resolveSupportRequestedStateRev(
  host: SessionSupervisorHydrationHost,
  entry: SessionSupervisorHydrationEntry,
  opts?: SupportLoadOpts,
): number | undefined {
  const authoritativeRequestedStateRev = host.resolveRequestedStateRev(entry);
  if (typeof authoritativeRequestedStateRev === "number") {
    return authoritativeRequestedStateRev;
  }
  const workspaceConnection = host.workspaceSnapshotState?.connection;
  const canUseEntryStateRevFallback =
    opts?.allowEntryStateRevFallback &&
    typeof entry.stateRev === "number" &&
    workspaceConnection !== "disconnected" &&
    workspaceConnection !== "connecting";
  if (canUseEntryStateRevFallback) {
    return entry.stateRev;
  }
  return undefined;
}

function isRequestedStateRevStale(
  requestRev: number | undefined,
  liveRequestedStateRev: number | undefined,
): boolean {
  if (typeof requestRev === "number" && typeof liveRequestedStateRev === "number") {
    return liveRequestedStateRev !== requestRev;
  }
  if (typeof requestRev !== "number" && typeof liveRequestedStateRev === "number") {
    return true;
  }
  return false;
}

export async function ensureState(
  this: SessionSupervisorHydrationHost,
  entry: SessionSupervisorHydrationEntry,
  opts?: SupportLoadOpts,
) {
  const cached = this.stateCacheBySessionId.get(entry.sessionId);
  const requestedStateRev = resolveSupportRequestedStateRev(this, entry, opts);
  const cachedOrAppliedRev =
    typeof cached?.stateRev === "number" ? cached.stateRev : entry.support.stateAppliedRev;
  const cacheMatchesRequestedRev =
    typeof requestedStateRev === "number" &&
    typeof cachedOrAppliedRev === "number" &&
    cachedOrAppliedRev >= requestedStateRev;
  if (!opts?.force && cached && cacheMatchesRequestedRev) {
    this.applyState(entry, cached.state, cached.stateRev ?? requestedStateRev);
    entry.updatedAtMs = Date.now();
    this.publish();
    return;
  }
  if (this.stateRequestsInFlight.has(entry.sessionId)) {
    entry.support.stateLoading = true;
    entry.updatedAtMs = Date.now();
    this.publish();
    return;
  }
  if (!shouldFetchSessionState({ ...entry.support, stateRev: requestedStateRev }, opts)) return;
  entry.support.stateLoading = true;
  this.clearSupportLoadError(entry, "state");
  const requestRev = requestedStateRev;
  entry.support.stateFetchToken += 1;
  const fetchToken = entry.support.stateFetchToken;
  entry.updatedAtMs = Date.now();
  this.publish();
  let requestRef: Promise<void> | undefined;
  let needsRetry = false;
  const request = (async () => {
    try {
      const state = await getSessionState(entry.sessionId);
      const liveEntry = this.entries.get(entry.sessionId);
      if (!liveEntry || liveEntry.support.stateFetchToken !== fetchToken) return;
      const liveRequestedStateRev = resolveSupportRequestedStateRev(this, liveEntry, opts);
      if (isRequestedStateRevStale(requestRev, liveRequestedStateRev)) {
        needsRetry = true;
        return;
      }
      this.stateCacheBySessionId.set(entry.sessionId, {
        state,
        stateRev: requestRev,
      });
      this.applyState(liveEntry, state, requestRev);
    } catch (err) {
      const liveEntry = this.entries.get(entry.sessionId);
      if (!liveEntry || liveEntry.support.stateFetchToken !== fetchToken) return;
      const liveRequestedStateRev = resolveSupportRequestedStateRev(this, liveEntry, opts);
      if (isRequestedStateRevStale(requestRev, liveRequestedStateRev)) {
        needsRetry = true;
        return;
      }
      this.setSupportLoadError(liveEntry, "state", err);
    } finally {
      if (requestRef && this.stateRequestsInFlight.get(entry.sessionId) === requestRef) {
        this.stateRequestsInFlight.delete(entry.sessionId);
      }
      const liveEntry = this.entries.get(entry.sessionId);
      if (liveEntry && liveEntry.support.stateFetchToken === fetchToken) {
        liveEntry.support.stateLoading = false;
        this.syncSupportLoadsForOpenSession(liveEntry);
        liveEntry.updatedAtMs = Date.now();
        this.publish();
        if (needsRetry && liveEntry.refCount <= 0 && opts?.allowEntryStateRevFallback) {
          void this.ensureState(liveEntry, opts);
        }
      }
    }
  })();
  requestRef = request;
  this.stateRequestsInFlight.set(entry.sessionId, request);
}

export async function ensureSubagentInvocations(
  this: SessionSupervisorHydrationHost,
  entry: SessionSupervisorHydrationEntry,
  opts?: SupportLoadOpts,
) {
  const requestedStateRev = resolveSupportRequestedStateRev(this, entry, opts);
  const cached = this.subagentInvocationsCacheBySessionId.get(entry.sessionId);
  const cacheMatchesRequestedRev =
    typeof requestedStateRev === "number" &&
    typeof cached?.stateRev === "number" &&
    cached.stateRev >= requestedStateRev;
  if (!opts?.force && cached && cacheMatchesRequestedRev) {
    entry.support.subagentInvocations = cached.invocations.slice();
    entry.support.subagentInvocationsLoaded = true;
    entry.support.subagentInvocationsLoading = false;
    entry.support.subagentInvocationsAppliedRev = cached.stateRev;
    entry.support.subagentInvocationsFetchedAtMs = Date.now();
    this.clearSupportLoadError(entry, "subagentInvocations");
    entry.updatedAtMs = Date.now();
    this.publish();
    return;
  }
  if (this.subagentInvocationsRequestsInFlight.has(entry.sessionId)) {
    entry.support.subagentInvocationsLoading = true;
    entry.updatedAtMs = Date.now();
    this.publish();
    return;
  }
  if (!shouldFetchSubagentInvocations(entry.support, requestedStateRev, opts)) return;
  entry.support.subagentInvocationsLoading = true;
  this.clearSupportLoadError(entry, "subagentInvocations");
  entry.support.subagentInvocationsFetchToken += 1;
  const fetchToken = entry.support.subagentInvocationsFetchToken;
  entry.updatedAtMs = Date.now();
  this.publish();
  let requestRef: Promise<void> | undefined;
  let needsRetry = false;
  const request = (async () => {
    try {
      const invocations = await listSessionSubagentInvocations(entry.sessionId);
      const liveEntry = this.entries.get(entry.sessionId);
      if (!liveEntry || liveEntry.support.subagentInvocationsFetchToken !== fetchToken) return;
      const liveRequestedStateRev = resolveSupportRequestedStateRev(this, liveEntry, opts);
      if (isRequestedStateRevStale(requestedStateRev, liveRequestedStateRev)) {
        needsRetry = true;
        return;
      }
      const appliedStateRev = requestedStateRev;
      liveEntry.support.subagentInvocations = invocations;
      liveEntry.support.subagentInvocationsLoaded = true;
      liveEntry.support.subagentInvocationsAppliedRev =
        typeof appliedStateRev === "number" ? appliedStateRev : undefined;
      liveEntry.support.subagentInvocationsFetchedAtMs = Date.now();
      if (typeof appliedStateRev === "number") {
        this.subagentInvocationsCacheBySessionId.set(entry.sessionId, {
          invocations: invocations.slice(),
          stateRev: appliedStateRev,
        });
      } else {
        this.subagentInvocationsCacheBySessionId.delete(entry.sessionId);
      }
      this.clearSupportLoadError(liveEntry, "subagentInvocations");
    } catch (err) {
      const liveEntry = this.entries.get(entry.sessionId);
      if (!liveEntry || liveEntry.support.subagentInvocationsFetchToken !== fetchToken) return;
      const liveRequestedStateRev = resolveSupportRequestedStateRev(this, liveEntry, opts);
      if (isRequestedStateRevStale(requestedStateRev, liveRequestedStateRev)) {
        needsRetry = true;
        return;
      }
      this.setSupportLoadError(liveEntry, "subagentInvocations", err);
    } finally {
      if (
        requestRef &&
        this.subagentInvocationsRequestsInFlight.get(entry.sessionId) === requestRef
      ) {
        this.subagentInvocationsRequestsInFlight.delete(entry.sessionId);
      }
      const liveEntry = this.entries.get(entry.sessionId);
      if (liveEntry && liveEntry.support.subagentInvocationsFetchToken === fetchToken) {
        liveEntry.support.subagentInvocationsLoading = false;
        this.syncSupportLoadsForOpenSession(liveEntry);
        liveEntry.updatedAtMs = Date.now();
        this.publish();
        if (needsRetry && liveEntry.refCount <= 0 && opts?.allowEntryStateRevFallback) {
          void this.ensureSubagentInvocations(liveEntry, opts);
        }
      }
    }
  })();
  requestRef = request;
  this.subagentInvocationsRequestsInFlight.set(entry.sessionId, request);
}
