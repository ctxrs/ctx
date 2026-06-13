type VisibleSessionSwitchSnapshot = {
  pending: boolean;
  sessionId: string | null;
};

const state: VisibleSessionSwitchSnapshot = {
  pending: false,
  sessionId: null,
};

let snapshot: VisibleSessionSwitchSnapshot = { ...state };
const listeners = new Set<() => void>();

function emitIfChanged(changed: boolean): void {
  if (!changed) return;
  snapshot = { ...state };
  for (const listener of listeners) {
    listener();
  }
}

function normalizeSessionId(sessionId: string | null | undefined): string | null {
  const normalized = typeof sessionId === "string" ? sessionId.trim() : "";
  return normalized.length > 0 ? normalized : null;
}

export function noteVisibleSessionSwitchStarted(sessionId: string | null | undefined): void {
  const normalized = normalizeSessionId(sessionId);
  const changed = state.pending !== Boolean(normalized) || state.sessionId !== normalized;
  state.pending = Boolean(normalized);
  state.sessionId = normalized;
  emitIfChanged(changed);
}

export function noteVisibleSessionSwitchSettled(sessionId: string | null | undefined): void {
  if (!state.pending) return;
  const normalized = normalizeSessionId(sessionId);
  if (normalized && state.sessionId && normalized !== state.sessionId) {
    return;
  }
  state.pending = false;
  state.sessionId = null;
  emitIfChanged(true);
}

export function getVisibleSessionSwitchSnapshot(): VisibleSessionSwitchSnapshot {
  return snapshot;
}

export function subscribeVisibleSessionSwitch(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function resetVisibleSessionSwitchStateForTests(): void {
  state.pending = false;
  state.sessionId = null;
  snapshot = { ...state };
  listeners.clear();
}
