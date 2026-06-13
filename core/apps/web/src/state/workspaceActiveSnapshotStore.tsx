import React, { createContext, useContext, useEffect, useMemo, useRef, useSyncExternalStore } from "react";
import type { WorkspaceActiveSnapshotEvent } from "@ctx/types";
import {
  WorkspaceActiveSnapshotStoreImpl,
  type WorkspaceActiveSnapshotEventSource,
  type WorkspaceActiveSnapshotState,
  type WorkspaceActiveSnapshotItem,
} from "./workspaceActiveSnapshotStoreCore";
import { clearUiDiagnostics, getUiDiagnostics } from "./diagnosticsChannel";

export type { WorkspaceActiveSnapshotEventSource, WorkspaceActiveSnapshotItem, WorkspaceActiveSnapshotState };

const WorkspaceActiveSnapshotContext = createContext<WorkspaceActiveSnapshotStoreImpl | null>(null);

type CtxE2EWorkspaceStream = {
  getConnectionState?: () => WorkspaceActiveSnapshotState["connection"];
  close?: () => void;
  setDropMessages?: (drop: boolean) => void;
  injectMessage?: (data: unknown) => boolean;
  getCanonicalUrl?: () => string | null;
};

type CtxE2EBridge = {
  getSessionHeadMessages?: (sessionId: string) => string[];
  getSessionHeadUserMessages?: (sessionId: string) => string[];
  getSessionLastEventSeq?: (sessionId: string) => number | null;
  getSessionProjectionRev?: (sessionId: string) => number | null;
  getWorkspaceSnapshot?: () => WorkspaceActiveSnapshotState | null;
  getDiagnostics?: () => ReturnType<typeof getUiDiagnostics>;
  clearDiagnostics?: () => void;
  workspaceStream?: CtxE2EWorkspaceStream;
};

type WindowWithCtxE2E = Window & {
  __ctxE2E?: CtxE2EBridge;
};

const shouldExposeE2E = (): boolean => {
  if (typeof window === "undefined") return false;
  if (window.sessionStorage.getItem("ctxE2E") === "1") return true;
  const params = new URLSearchParams(window.location.search);
  if (params.get("ctxE2E") === "1") {
    try {
      window.sessionStorage.setItem("ctxE2E", "1");
    } catch {
      // ignore
    }
    return true;
  }
  return false;
};

export function WorkspaceActiveSnapshotProvider({
  workspaceId,
  children,
}: {
  workspaceId: string;
  children: React.ReactNode;
}) {
  const storeRef = useRef<WorkspaceActiveSnapshotStoreImpl | null>(null);
  const lastWorkspaceRef = useRef<string | null>(null);
  const exposeE2E = shouldExposeE2E();
  if (!storeRef.current || lastWorkspaceRef.current !== workspaceId) {
    storeRef.current?.destroy();
    storeRef.current = new WorkspaceActiveSnapshotStoreImpl(workspaceId, { e2eEnabled: exposeE2E });
    lastWorkspaceRef.current = workspaceId;
  }

  useEffect(() => {
    storeRef.current?.init();
    return () => storeRef.current?.destroy();
  }, [workspaceId]);

  useEffect(() => {
    storeRef.current?.setE2EEnabled(exposeE2E);
    if (!exposeE2E) return;
    if (!storeRef.current) return;
    const win = window as WindowWithCtxE2E;
    win.__ctxE2E ??= {};
    win.__ctxE2E.getSessionHeadMessages = (sessionId: string) => {
      const head = storeRef.current?.getSessionHeadSnapshot(sessionId);
      return head?.messages?.map((message) => message.content) ?? [];
    };
    win.__ctxE2E.getSessionHeadUserMessages = (sessionId: string) => {
      const head = storeRef.current?.getSessionHeadSnapshot(sessionId);
      return head?.messages?.filter((message) => message.role === "user").map((message) => message.content) ?? [];
    };
    win.__ctxE2E.getSessionLastEventSeq = (sessionId: string) => {
      const head = storeRef.current?.getSessionHeadSnapshot(sessionId);
      return head?.last_event_seq ?? null;
    };
    win.__ctxE2E.getSessionProjectionRev = (sessionId: string) => {
      const head = storeRef.current?.getSessionHeadSnapshot(sessionId);
      return head?.projection_rev ?? null;
    };
    win.__ctxE2E.getWorkspaceSnapshot = () => storeRef.current?.getSnapshot() ?? null;
    win.__ctxE2E.workspaceStream ??= {};
    win.__ctxE2E.workspaceStream.getConnectionState = () => storeRef.current?.getSnapshot().connection ?? "idle";
    win.__ctxE2E.workspaceStream.close = () => storeRef.current?.e2eCloseActiveSnapshotStream();
    win.__ctxE2E.workspaceStream.setDropMessages = (drop: boolean) =>
      storeRef.current?.e2eSetDropActiveSnapshotMessages(Boolean(drop));
    win.__ctxE2E.workspaceStream.injectMessage = (data: unknown) =>
      storeRef.current?.e2eInjectActiveSnapshotStreamMessage(data) ?? false;
    win.__ctxE2E.workspaceStream.getCanonicalUrl = () => storeRef.current?.e2eGetCanonicalStreamUrl?.() ?? null;
    win.__ctxE2E.getDiagnostics = () => getUiDiagnostics();
    win.__ctxE2E.clearDiagnostics = () => clearUiDiagnostics();
    return () => {
      if (!win.__ctxE2E) return;
      delete win.__ctxE2E.getSessionHeadMessages;
      delete win.__ctxE2E.getSessionHeadUserMessages;
      delete win.__ctxE2E.getSessionLastEventSeq;
      delete win.__ctxE2E.getSessionProjectionRev;
      delete win.__ctxE2E.getWorkspaceSnapshot;
      delete win.__ctxE2E.getDiagnostics;
      delete win.__ctxE2E.clearDiagnostics;
      if (win.__ctxE2E.workspaceStream) {
        delete win.__ctxE2E.workspaceStream.getConnectionState;
        delete win.__ctxE2E.workspaceStream.close;
        delete win.__ctxE2E.workspaceStream.setDropMessages;
        delete win.__ctxE2E.workspaceStream.injectMessage;
        delete win.__ctxE2E.workspaceStream.getCanonicalUrl;
      }
    };
  }, [workspaceId, exposeE2E]);

  return (
    <WorkspaceActiveSnapshotContext.Provider value={storeRef.current}>
      {children}
    </WorkspaceActiveSnapshotContext.Provider>
  );
}

export function useWorkspaceActiveSnapshotStore() {
  const store = useContext(WorkspaceActiveSnapshotContext);
  if (!store) throw new Error("WorkspaceActiveSnapshotProvider missing");
  return store;
}

export function useWorkspaceActiveSnapshotSnapshot(): WorkspaceActiveSnapshotState {
  const store = useWorkspaceActiveSnapshotStore();
  return useSyncExternalStore(store.subscribe, store.getSnapshot, store.getSnapshot);
}

export function useWorkspaceActiveSnapshotEvents(handler: (event: WorkspaceActiveSnapshotEvent) => void) {
  const store = useWorkspaceActiveSnapshotStore();
  const stableHandler = useMemo(() => handler, [handler]);
  useEffect(() => store.subscribeEvents(stableHandler), [store, stableHandler]);
}
