import {
  getInstallStatuses,
  listInstallEvents,
  type InstallInfo,
  type InstallProgressEvent,
  type InstallTarget,
} from "../api/client";
import { computeInstallPct } from "../utils/providerInstallUi";
import {
  removeProviderInstallProgressForScope,
  upsertProviderInstallProgressForScope,
} from "./providerInstallProgressStore";
import { getProviderHostOwnerScopeOrNull } from "./providerScopeAdapters";
import { serializeOwnerScope, type OwnerScope } from "./scopeIdentity";

const INSTALL_POLL_MS = 900;
const INSTALL_EVENT_HISTORY_LIMIT = 200;
const MISSING_INSTALL_ERROR = "Install is no longer tracked by the daemon. Retry from this screen.";

type Listener = (snapshot: InstallProgressSnapshot) => void;

type InstallState = InstallInfo["state"];

type InstallProgressInternalEntry = InstallProgressEntry & {
  refCount: number;
  providerAliases: Map<string, { ownerScope: OwnerScope; providerId: string }>;
  historyRequested: boolean;
};

export type InstallProgressEntry = {
  installId: string;
  providerId: string | null;
  state: InstallState;
  pct: number | null;
  target?: InstallTarget;
  errorCode?: InstallInfo["error_code"];
  error?: string;
  lastEvent: InstallProgressEvent | null;
  events: InstallProgressEvent[];
  historyLoaded: boolean;
  updatedAtMs: number;
};

export type InstallProgressSnapshot = Record<string, InstallProgressEntry>;

export type ObserveInstallOptions = {
  ownerScope?: OwnerScope;
  providerId?: string | null;
  loadHistory?: boolean;
  initialState?: Partial<Pick<InstallProgressEntry, "state" | "pct" | "target" | "errorCode" | "error">>;
};

const installsById = new Map<string, InstallProgressInternalEntry>();
const listeners = new Set<Listener>();

let pollTimer: number | null = null;
let pollInFlight: Promise<void> | null = null;

const cloneEntry = (entry: InstallProgressInternalEntry): InstallProgressEntry => ({
  installId: entry.installId,
  providerId: entry.providerId,
  state: entry.state,
  pct: entry.pct,
  target: entry.target,
  errorCode: entry.errorCode,
  error: entry.error,
  lastEvent: entry.lastEvent ? { ...entry.lastEvent } : null,
  events: entry.events.map((event) => ({ ...event })),
  historyLoaded: entry.historyLoaded,
  updatedAtMs: entry.updatedAtMs,
});

const cloneSnapshot = (): InstallProgressSnapshot => {
  const snapshot: InstallProgressSnapshot = {};
  for (const [installId, entry] of installsById.entries()) {
    snapshot[installId] = cloneEntry(entry);
  }
  return snapshot;
};

const emitChange = (): void => {
  if (listeners.size === 0) return;
  const snapshot = cloneSnapshot();
  for (const listener of listeners) {
    listener(snapshot);
  }
};

const installEventIdentity = (event: InstallProgressEvent): string =>
  [
    event.at,
    event.stage,
    event.level,
    event.message,
    event.bytes ?? "",
    event.total_bytes ?? "",
    event.error_code ?? "",
  ].join("|");

const appendInstallEvent = (
  events: InstallProgressEvent[],
  nextEvent: InstallProgressEvent | null,
): InstallProgressEvent[] => {
  if (!nextEvent) return events;
  const nextIdentity = installEventIdentity(nextEvent);
  if (events.some((event) => installEventIdentity(event) === nextIdentity)) {
    return events;
  }
  const next = [...events, { ...nextEvent }];
  if (next.length > INSTALL_EVENT_HISTORY_LIMIT) {
    next.splice(0, next.length - INSTALL_EVENT_HISTORY_LIMIT);
  }
  return next;
};

const aliasKey = (ownerScope: OwnerScope, providerId: string): string =>
  `${serializeOwnerScope(ownerScope)}|${providerId}`;

const resolveAliasOwnerScope = (options?: ObserveInstallOptions): OwnerScope | null =>
  options?.ownerScope ?? getProviderHostOwnerScopeOrNull();

const clearProviderAliases = (entry: InstallProgressInternalEntry): void => {
  for (const alias of entry.providerAliases.values()) {
    removeProviderInstallProgressForScope(alias.ownerScope, alias.providerId, { installId: entry.installId });
  }
};

const syncProviderAliases = (entry: InstallProgressInternalEntry): void => {
  for (const alias of entry.providerAliases.values()) {
    upsertProviderInstallProgressForScope(alias.ownerScope, alias.providerId, {
      installId: entry.installId,
      state: entry.state,
      pct: entry.pct,
      target: entry.target,
      errorCode: entry.errorCode,
      error: entry.error,
      updatedAtMs: entry.updatedAtMs,
    });
  }
};

const isActiveInstall = (entry: InstallProgressInternalEntry): boolean =>
  entry.refCount > 0 && entry.state === "running";

const activeInstallIds = (): string[] =>
  Array.from(installsById.values())
    .filter(isActiveInstall)
    .map((entry) => entry.installId);

const clearPollTimer = (): void => {
  if (pollTimer !== null) {
    window.clearTimeout(pollTimer);
    pollTimer = null;
  }
};

const schedulePoll = (delayMs = INSTALL_POLL_MS): void => {
  clearPollTimer();
  if (activeInstallIds().length === 0) return;
  pollTimer = window.setTimeout(() => {
    pollTimer = null;
    void runPollCycle();
  }, delayMs);
};

const mergeInstallInfo = (entry: InstallProgressInternalEntry, info: InstallInfo): boolean => {
  const nextPct = computeInstallPct(info, entry.pct ?? null);
  const nextLastEvent = info.last_event ?? null;
  const nextEvents = appendInstallEvent(entry.events, nextLastEvent);
  const previousLastEventIdentity = entry.lastEvent ? installEventIdentity(entry.lastEvent) : null;
  const nextLastEventIdentity = nextLastEvent ? installEventIdentity(nextLastEvent) : null;
  const changed =
    entry.providerId !== info.provider_id
    || entry.state !== info.state
    || entry.pct !== nextPct
    || entry.target !== info.target
    || entry.errorCode !== info.error_code
    || entry.error !== info.error
    || previousLastEventIdentity !== nextLastEventIdentity
    || entry.events.length !== nextEvents.length;

  entry.providerId = info.provider_id;
  entry.state = info.state;
  entry.pct = nextPct;
  entry.target = info.target;
  entry.errorCode = info.error_code;
  entry.error = info.error;
  entry.lastEvent = nextLastEvent ? { ...nextLastEvent } : null;
  entry.events = nextEvents;
  entry.updatedAtMs = Date.now();
  return changed;
};

const markInstallMissingAsTerminal = (entry: InstallProgressInternalEntry): boolean => {
  const changed =
    entry.state !== "failed"
    || entry.errorCode !== "unknown"
    || entry.error !== MISSING_INSTALL_ERROR;

  entry.state = "failed";
  entry.errorCode = "unknown";
  entry.error = MISSING_INSTALL_ERROR;
  entry.updatedAtMs = Date.now();
  return changed;
};

async function runPollCycle(): Promise<void> {
  if (pollInFlight) {
    await pollInFlight;
    return;
  }
  const installIds = activeInstallIds();
  if (installIds.length === 0) {
    clearPollTimer();
    return;
  }

  pollInFlight = (async () => {
    let changed = false;
    try {
      const response = await getInstallStatuses(installIds);
      for (const item of response.installs) {
        const entry = installsById.get(item.install_id);
        if (!entry) continue;
        if (item.info ? mergeInstallInfo(entry, item.info) : markInstallMissingAsTerminal(entry)) {
          changed = true;
        }
        syncProviderAliases(entry);
      }
    } catch {
      // Keep the shared poller alive on transient transport failures.
    } finally {
      pollInFlight = null;
      if (changed) {
        emitChange();
      }
      if (activeInstallIds().length > 0) {
        schedulePoll();
      } else {
        clearPollTimer();
      }
    }
  })();

  await pollInFlight;
}

const ensureEntry = (
  installId: string,
  options?: ObserveInstallOptions,
): InstallProgressInternalEntry => {
  const aliasOwnerScope = options?.providerId ? resolveAliasOwnerScope(options) : null;
  const existing = installsById.get(installId);
  if (existing) {
    existing.refCount += 1;
    if (options?.providerId) {
      existing.providerId = options.providerId;
      if (aliasOwnerScope) {
        existing.providerAliases.set(aliasKey(aliasOwnerScope, options.providerId), {
          ownerScope: aliasOwnerScope,
          providerId: options.providerId,
        });
      }
      syncProviderAliases(existing);
    }
    if (options?.initialState) {
      existing.state = options.initialState.state ?? existing.state;
      existing.pct = options.initialState.pct ?? existing.pct;
      existing.target = options.initialState.target ?? existing.target;
      existing.errorCode = options.initialState.errorCode ?? existing.errorCode;
      existing.error = options.initialState.error ?? existing.error;
      existing.updatedAtMs = Date.now();
      syncProviderAliases(existing);
    }
    return existing;
  }

  const entry: InstallProgressInternalEntry = {
    installId,
    providerId: options?.providerId ?? null,
    state: options?.initialState?.state ?? "running",
    pct: options?.initialState?.pct ?? null,
    target: options?.initialState?.target,
    errorCode: options?.initialState?.errorCode,
    error: options?.initialState?.error,
    lastEvent: null,
    events: [],
    historyLoaded: false,
    updatedAtMs: Date.now(),
    refCount: 1,
    providerAliases: new Map(
      options?.providerId && aliasOwnerScope
        ? [[aliasKey(aliasOwnerScope, options.providerId), {
          ownerScope: aliasOwnerScope,
          providerId: options.providerId,
        }]]
        : [],
    ),
    historyRequested: false,
  };
  installsById.set(installId, entry);
  syncProviderAliases(entry);
  return entry;
};

const maybeLoadInstallHistory = async (entry: InstallProgressInternalEntry): Promise<void> => {
  if (entry.historyRequested || entry.refCount <= 0) return;
  entry.historyRequested = true;
  try {
    const history = await listInstallEvents(entry.installId);
    const nextEntry = installsById.get(entry.installId);
    if (!nextEntry) return;
    nextEntry.events = history.slice(-INSTALL_EVENT_HISTORY_LIMIT);
    nextEntry.lastEvent = nextEntry.events[nextEntry.events.length - 1] ?? nextEntry.lastEvent;
    nextEntry.historyLoaded = true;
    nextEntry.updatedAtMs = Date.now();
    emitChange();
  } catch {
    const nextEntry = installsById.get(entry.installId);
    if (!nextEntry) return;
    nextEntry.historyLoaded = true;
    nextEntry.updatedAtMs = Date.now();
    emitChange();
  }
};

export const getInstallProgressSnapshot = (): InstallProgressSnapshot => cloneSnapshot();

export const subscribeInstallProgress = (listener: Listener): (() => void) => {
  listeners.add(listener);
  listener(cloneSnapshot());
  return () => {
    listeners.delete(listener);
  };
};

export const observeInstall = (installId: string, options?: ObserveInstallOptions): (() => void) => {
  const normalizedInstallId = installId.trim();
  if (!normalizedInstallId) {
    return () => {};
  }
  const entry = ensureEntry(normalizedInstallId, options);
  emitChange();
  if (options?.loadHistory) {
    void maybeLoadInstallHistory(entry);
  }
  schedulePoll(0);
  return () => {
    const current = installsById.get(normalizedInstallId);
    if (!current) return;
    current.refCount = Math.max(0, current.refCount - 1);
    if (current.refCount === 0) {
      installsById.delete(normalizedInstallId);
      clearProviderAliases(current);
      emitChange();
      schedulePoll();
      return;
    }
    emitChange();
  };
};

export const clearInstallProgress = (): void => {
  clearPollTimer();
  pollInFlight = null;
  for (const entry of installsById.values()) {
    clearProviderAliases(entry);
  }
  installsById.clear();
  emitChange();
};
