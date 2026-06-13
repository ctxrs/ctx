import { useEffect, useMemo, useRef } from "react";
import type {
  DesktopRecordWorkbenchRouteReq,
  DesktopTaskRoutePayload,
  DesktopWorkbenchRouteTask,
} from "../../utils/desktop";
import {
  desktopAckTaskRoute,
  desktopConsumePendingTaskRoute,
  desktopListen,
  desktopRecordWorkbenchRoute,
  isDesktopApp,
} from "../../utils/desktop";
import type { DaemonConnection } from "../../api/daemonConnection.types";
import type { PersistedWorkbenchWindowV1, LayoutNode } from "../../workbench/types";
import type { WorkbenchStore } from "../../workbench/store";

export const DESKTOP_TASK_DEEPLINK_OPEN_EVENT = "desktop_task_deeplink_open";

type UseWorkbenchDesktopTaskRoutingArgs = {
  activeSessionId: string | null;
  activeTaskId: string | null;
  connection: DaemonConnection;
  windowState: PersistedWorkbenchWindowV1;
  workbenchStore: WorkbenchStore;
  workspaceId: string;
  workspaceName: string | null;
};

type ApplyDesktopTaskRouteArgs = {
  payload: DesktopTaskRoutePayload;
  workbenchStore: Pick<WorkbenchStore, "focusTask" | "getNavToken">;
  workspaceId: string;
};

export function collectWorkbenchRouteTasks(
  layout: LayoutNode,
): DesktopWorkbenchRouteTask[] {
  const byTaskAndSession = new Map<string, DesktopWorkbenchRouteTask>();
  const visit = (node: LayoutNode) => {
    if (node.kind === "split") {
      visit(node.first);
      visit(node.second);
      return;
    }
    for (const tab of node.tabs) {
      if (tab.kind !== "task") continue;
      const taskId = String(tab.ref.taskId || "").trim();
      if (!taskId) continue;
      const sessionId = String(tab.ref.sessionId ?? "").trim();
      const key = `${taskId}\u0000${sessionId}`;
      byTaskAndSession.set(key, {
        task_id: taskId,
        ...(sessionId ? { session_id: sessionId } : {}),
      });
    }
  };
  visit(layout);
  return Array.from(byTaskAndSession.values()).sort((left, right) => {
    const taskOrder = left.task_id.localeCompare(right.task_id);
    if (taskOrder !== 0) return taskOrder;
    return String(left.session_id ?? "").localeCompare(String(right.session_id ?? ""));
  });
}

export function buildWorkbenchRoutePublishReq({
  activeSessionId,
  activeTaskId,
  windowState,
  workspaceId,
  workspaceName,
}: Omit<UseWorkbenchDesktopTaskRoutingArgs, "connection" | "workbenchStore">): DesktopRecordWorkbenchRouteReq | null {
  const workspaceIdValue = String(workspaceId || "").trim();
  if (!workspaceIdValue) return null;
  const activeTaskIdValue = String(activeTaskId ?? "").trim();
  const activeSessionIdValue = String(activeSessionId ?? "").trim();
  return {
    ...(activeTaskIdValue ? { active_task_id: activeTaskIdValue } : {}),
    ...(activeTaskIdValue && activeSessionIdValue ? { active_session_id: activeSessionIdValue } : {}),
    open_tasks: collectWorkbenchRouteTasks(windowState.layout),
    workspace_id: workspaceIdValue,
    workspace_label: String(workspaceName || "").trim() || workspaceIdValue,
  };
}

export function applyDesktopTaskRoutePayload({
  payload,
  workbenchStore,
  workspaceId,
}: ApplyDesktopTaskRouteArgs): boolean {
  const payloadWorkspaceId = String(payload.workspace_id || "").trim();
  if (payloadWorkspaceId !== String(workspaceId || "").trim()) return false;
  const taskId = String(payload.task_id || "").trim();
  if (!taskId) return false;
  const sessionId = String(payload.session_id ?? "").trim();
  const navToken = workbenchStore.getNavToken();
  return workbenchStore.focusTask(taskId, sessionId || null, {
    navToken,
    source: "system",
  });
}

export function workbenchDesktopRouteConnectionSignature(
  connection: DaemonConnection,
): string {
  const targetScope = connection.targetScope
    ? JSON.stringify(connection.targetScope)
    : "";
  return [
    connection.baseUrl ?? "",
    connection.wsBaseUrl ?? "",
    connection.authToken ? "auth" : "no-auth",
    connection.runId ?? "",
    connection.source ?? "",
    targetScope,
  ].join("|");
}

const serializeRouteReq = (req: DesktopRecordWorkbenchRouteReq | null): string =>
  req ? JSON.stringify(req) : "";

export function useWorkbenchDesktopTaskRouting({
  activeSessionId,
  activeTaskId,
  connection,
  windowState,
  workbenchStore,
  workspaceId,
  workspaceName,
}: UseWorkbenchDesktopTaskRoutingArgs): void {
  const desktopUi = isDesktopApp();
  const publishReq = useMemo(
    () =>
      buildWorkbenchRoutePublishReq({
        activeSessionId,
        activeTaskId,
        windowState,
        workspaceId,
        workspaceName,
      }),
    [activeSessionId, activeTaskId, windowState, workspaceId, workspaceName],
  );
  const publishSignature = useMemo(
    () =>
      `${workbenchDesktopRouteConnectionSignature(connection)}:${serializeRouteReq(publishReq)}`,
    [connection, publishReq],
  );
  const lastPublishedSignatureRef = useRef<string | null>(null);

  useEffect(() => {
    if (!desktopUi || !publishReq || !publishSignature) return;
    if (lastPublishedSignatureRef.current === publishSignature) return;
    lastPublishedSignatureRef.current = publishSignature;
    void desktopRecordWorkbenchRoute(publishReq).catch(() => {
      if (lastPublishedSignatureRef.current === publishSignature) {
        lastPublishedSignatureRef.current = null;
      }
    });
  }, [desktopUi, publishReq, publishSignature]);

  useEffect(() => {
    if (!desktopUi) return;
    let disposed = false;
    let cleanup: (() => void) | null = null;
    const applyPayload = (payload: DesktopTaskRoutePayload | null | undefined) => {
      if (!payload || disposed) return;
      const applied = applyDesktopTaskRoutePayload({ payload, workbenchStore, workspaceId });
      const routeId = String(payload.route_id || "").trim();
      if (applied && routeId) {
        void desktopAckTaskRoute(routeId).catch(() => {});
      }
    };
    const consumePending = () => {
      void desktopConsumePendingTaskRoute()
        .then((payload) => applyPayload(payload))
        .catch(() => {});
    };

    void desktopListen<DesktopTaskRoutePayload>(DESKTOP_TASK_DEEPLINK_OPEN_EVENT, applyPayload)
      .then((unlisten) => {
        if (disposed) {
          unlisten();
          return;
        }
        cleanup = unlisten;
        consumePending();
      })
      .catch(() => {
        consumePending();
      });

    return () => {
      disposed = true;
      if (cleanup) cleanup();
    };
  }, [desktopUi, workbenchStore, workspaceId]);
}
