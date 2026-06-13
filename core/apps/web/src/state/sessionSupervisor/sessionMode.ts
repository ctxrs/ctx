import { resolveSessionModeFromWorkspaceState } from "../workspaceActiveSnapshot/projection";
import type { InternalEntry, SessionMode } from "./entryState";
import type { SessionSupervisorWorkspaceSnapshotState } from "./workspaceInputs";

export type SessionSupervisorSessionModeHost = {
  workspaceSnapshotState: SessionSupervisorWorkspaceSnapshotState;
};

export function resolveSessionMode(
  this: SessionSupervisorSessionModeHost,
  sessionId: string,
  entry?: InternalEntry,
  explicitMode?: SessionMode,
): SessionMode | null {
  const id = String(sessionId ?? "").trim();
  if (!id) return null;
  if (explicitMode) {
    if (entry) entry.mode = explicitMode;
    return explicitMode;
  }
  if (entry?.mode) return entry.mode;
  const state = this.workspaceSnapshotState;
  if (!state) {
    if (entry) {
      entry.mode = "active";
    }
    return "active";
  }
  const mode = resolveSessionModeFromWorkspaceState(state, id);
  if (mode && entry) {
    entry.mode = mode;
  }
  return mode;
}

export function shouldFailPendingSessionOpen(
  state: SessionSupervisorWorkspaceSnapshotState,
) {
  if (!state) return false;
  return state.initialized && state.fetchState.active === "idle";
}
