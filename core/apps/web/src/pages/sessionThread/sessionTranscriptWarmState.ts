import type { SessionViewVerbosity } from "../../state/uiStateStore";

type SessionTranscriptWarmState = {
  viewportWidth: number;
  viewportHeight: number;
  verbosity: SessionViewVerbosity;
};

const state: SessionTranscriptWarmState = {
  viewportWidth: 0,
  viewportHeight: 0,
  verbosity: "default",
};
let snapshot: SessionTranscriptWarmState = { ...state };
const listeners = new Set<() => void>();

function emitIfChanged(changed: boolean): void {
  if (!changed) return;
  snapshot = { ...state };
  for (const listener of listeners) {
    listener();
  }
}

export function noteSessionTranscriptWarmViewport(viewport: {
  width: number;
  height: number;
}): void {
  const width = Number.isFinite(viewport.width) && viewport.width > 0 ? Math.round(viewport.width) : 0;
  const height = Number.isFinite(viewport.height) && viewport.height > 0 ? Math.round(viewport.height) : 0;
  let changed = false;
  if (width > 0) {
    changed ||= state.viewportWidth !== width;
    state.viewportWidth = width;
  }
  if (height > 0) {
    changed ||= state.viewportHeight !== height;
    state.viewportHeight = height;
  }
  emitIfChanged(changed);
}

export function noteSessionTranscriptWarmVerbosity(verbosity: SessionViewVerbosity): void {
  const changed = state.verbosity !== verbosity;
  state.verbosity = verbosity;
  emitIfChanged(changed);
}

export function getSessionTranscriptWarmState(): SessionTranscriptWarmState {
  return snapshot;
}

export function subscribeSessionTranscriptWarmState(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function resetSessionTranscriptWarmStateForTests(): void {
  state.viewportWidth = 0;
  state.viewportHeight = 0;
  state.verbosity = "default";
  snapshot = { ...state };
  listeners.clear();
}
