export type SessionThreadProjectionDebugEntry = {
  seq: number;
  atMs: number;
  sessionId: string;
  source: "raw" | "supervisor";
  loaded: boolean;
  sessionProjectionReady: boolean;
  freshness: string | null;
  loadState: string | null;
  lastTurnStatus: string | null;
  turnsStamp: string;
  messagesStamp: string;
  eventsStamp: string;
  projectionRev: number;
  opKind?: string;
  listItemCount: number;
};

type SessionThreadProjectionDebugStore = {
  seq: number;
  entries: SessionThreadProjectionDebugEntry[];
};

declare global {
  interface Window {
    __wbSessionThreadProjectionDebug?: SessionThreadProjectionDebugStore;
  }
}

const MAX_SESSION_THREAD_PROJECTION_DEBUG_ENTRIES = 400;

function getStore(): SessionThreadProjectionDebugStore {
  const existing = window.__wbSessionThreadProjectionDebug;
  if (existing) {
    existing.seq = Number.isFinite(existing.seq) ? existing.seq : 0;
    existing.entries = Array.isArray(existing.entries) ? existing.entries : [];
    return existing;
  }
  const created: SessionThreadProjectionDebugStore = {
    seq: 0,
    entries: [],
  };
  window.__wbSessionThreadProjectionDebug = created;
  return created;
}

export function recordSessionThreadProjectionDebugEntry(
  entry: Omit<SessionThreadProjectionDebugEntry, "seq" | "atMs">,
) {
  const store = getStore();
  const next: SessionThreadProjectionDebugEntry = {
    ...entry,
    seq: store.seq + 1,
    atMs: Math.round(performance.now() * 100) / 100,
  };
  store.seq = next.seq;
  store.entries.push(next);
  if (store.entries.length > MAX_SESSION_THREAD_PROJECTION_DEBUG_ENTRIES) {
    store.entries.splice(0, store.entries.length - MAX_SESSION_THREAD_PROJECTION_DEBUG_ENTRIES);
  }
}
