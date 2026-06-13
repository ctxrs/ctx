import type { SessionSupervisor, SessionMode } from "./sessionSupervisorCore";
import type { WorkspaceActiveSnapshotState } from "./workspaceActiveSnapshotStore";
import { resolveSessionModeFromWorkspaceState } from "./workspaceActiveSnapshot/projection";

export type SessionLifecycleOpenOptions = Parameters<SessionSupervisor["openSession"]>[1];

type SessionLifecycleSupervisor = Pick<
  SessionSupervisor,
  "openSession" | "beginSessionOpen" | "commitSessionOpenMode" | "failPendingSessionOpen" | "closeSession"
>;

type RouteOpenRequest = {
  sessionId: string;
  opts?: SessionLifecycleOpenOptions;
  resolvedMode: SessionMode | null;
  failedMissing: boolean;
  closeRouteOpen?: () => void;
};

const cloneOpenOptions = (opts?: SessionLifecycleOpenOptions): SessionLifecycleOpenOptions | undefined => {
  if (!opts) return undefined;
  return {
    watchDiff: opts.watchDiff ?? false,
    force: opts.force ?? false,
    silent: opts.silent ?? false,
    mode: opts.mode,
  };
};

export const shouldFailMissingRouteSession = (state: WorkspaceActiveSnapshotState | null): boolean => {
  if (!state) return false;
  return state.initialized && state.fetchState.active === "idle";
};

export const resolveRouteSessionMode = (
  state: WorkspaceActiveSnapshotState | null,
  sessionId: string,
  modeHint?: SessionMode,
): SessionMode | null => {
  if (modeHint) return modeHint;
  return resolveSessionModeFromWorkspaceState(state, sessionId);
};

export class SessionLifecycleCoordinator {
  private workspaceSnapshotState: WorkspaceActiveSnapshotState | null = null;
  private nextRequestId = 1;
  private routeOpenRequests = new Map<number, RouteOpenRequest>();

  constructor(private readonly supervisor: SessionLifecycleSupervisor) {}

  registerRouteOpen(sessionId: string, opts?: SessionLifecycleOpenOptions): () => void {
    const id = String(sessionId ?? "").trim();
    if (!id) return () => {};

    const requestId = this.nextRequestId++;
    const request: RouteOpenRequest = {
      sessionId: id,
      opts: cloneOpenOptions(opts),
      resolvedMode: null,
      failedMissing: false,
    };

    const initialMode = resolveRouteSessionMode(this.workspaceSnapshotState, id, request.opts?.mode);
    const initialFailure = !initialMode && shouldFailMissingRouteSession(this.workspaceSnapshotState);
    this.routeOpenRequests.set(requestId, request);
    if (initialMode || initialFailure) {
      request.resolvedMode = initialMode;
      request.failedMissing = initialFailure;
      request.closeRouteOpen = this.supervisor.openSession(id, request.opts);
    } else {
      this.supervisor.beginSessionOpen(id, request.opts);
    }

    return () => {
      const current = this.routeOpenRequests.get(requestId);
      if (!current) return;
      this.routeOpenRequests.delete(requestId);
      if (current.closeRouteOpen) {
        current.closeRouteOpen();
        return;
      }
      this.supervisor.closeSession(current.sessionId, current.opts);
    };
  }

  setWorkspaceSnapshotState(state: WorkspaceActiveSnapshotState | null) {
    this.workspaceSnapshotState = state;
    for (const request of this.routeOpenRequests.values()) {
      this.reconcileRequest(request);
    }
  }

  private reconcileRequest(request: RouteOpenRequest) {
    const mode = resolveRouteSessionMode(this.workspaceSnapshotState, request.sessionId, request.opts?.mode);
    if (mode) {
      request.failedMissing = false;
      if (request.resolvedMode === mode) return;
      request.resolvedMode = mode;
      this.supervisor.commitSessionOpenMode(request.sessionId, mode, request.opts);
      return;
    }

    request.resolvedMode = null;
    if (!shouldFailMissingRouteSession(this.workspaceSnapshotState)) {
      request.failedMissing = false;
      return;
    }
    if (request.failedMissing) return;
    request.failedMissing = true;
    this.supervisor.failPendingSessionOpen(request.sessionId);
  }
}
