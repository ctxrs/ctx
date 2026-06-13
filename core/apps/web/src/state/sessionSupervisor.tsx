import React, { createContext, useContext, useEffect, useMemo, useRef, useSyncExternalStore } from "react";
import { SessionLifecycleCoordinator } from "./sessionLifecycleCoordinator";
import {
  SessionSupervisor,
  type SessionCacheEntry,
  type SessionLoadState,
  type SessionMode,
  type SessionSupervisorSnapshot,
} from "./sessionSupervisorCore";

export { SessionSupervisor };
export type { SessionCacheEntry, SessionLoadState, SessionMode, SessionSupervisorSnapshot };

type OpenOptions = Parameters<SessionSupervisor["openSession"]>[1];

const SessionSupervisorContext = createContext<SessionSupervisor | null>(null);
const SessionLifecycleCoordinatorContext = createContext<SessionLifecycleCoordinator | null>(null);

export function SessionSupervisorProvider({ children }: { children: React.ReactNode }) {
  const supRef = useRef<SessionSupervisor | null>(null);
  const lifecycleRef = useRef<SessionLifecycleCoordinator | null>(null);
  if (!supRef.current) {
    supRef.current = new SessionSupervisor();
    lifecycleRef.current = new SessionLifecycleCoordinator(supRef.current);
  }

  return (
    <SessionLifecycleCoordinatorContext.Provider value={lifecycleRef.current}>
      <SessionSupervisorContext.Provider value={supRef.current}>
        {children}
      </SessionSupervisorContext.Provider>
    </SessionLifecycleCoordinatorContext.Provider>
  );
}

export function useSessionSupervisor() {
  const sup = useContext(SessionSupervisorContext);
  if (!sup) throw new Error("SessionSupervisorProvider missing");
  return sup;
}

export function useSessionLifecycleCoordinator() {
  const coordinator = useContext(SessionLifecycleCoordinatorContext);
  if (!coordinator) throw new Error("SessionSupervisorProvider missing");
  return coordinator;
}

export function useSessionCacheSnapshot(): SessionSupervisorSnapshot {
  const sup = useSessionSupervisor();
  return useSyncExternalStore(sup.subscribe, sup.getSnapshot, sup.getSnapshot);
}

export function useSessionEntry(sessionId: string): SessionCacheEntry | null {
  const snap = useSessionCacheSnapshot();
  return snap.sessions[String(sessionId)] ?? null;
}

export function useOpenSession(sessionId: string, opts?: OpenOptions) {
  const coordinator = useSessionLifecycleCoordinator();
  const isOptimisticSessionId = useMemo(() => String(sessionId || "").startsWith("optimistic-session-"), [sessionId]);
  const stableOpts = useMemo(
    () => ({
      watchDiff: opts?.watchDiff ?? false,
      force: opts?.force ?? false,
      silent: opts?.silent ?? false,
      mode: opts?.mode,
    }),
    [opts?.watchDiff, opts?.force, opts?.silent, opts?.mode],
  );
  useEffect(() => {
    if (!sessionId) return;
    if (isOptimisticSessionId) return;
    return coordinator.registerRouteOpen(String(sessionId), stableOpts);
  }, [coordinator, sessionId, stableOpts, isOptimisticSessionId]);
}
