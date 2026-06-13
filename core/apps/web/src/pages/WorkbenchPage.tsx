import { useEffect } from "react";
import { useParams } from "react-router-dom";
import { WorkbenchStoreProvider } from "../workbench/store";
import { WorkspaceActiveSnapshotProvider } from "../state/workspaceActiveSnapshotStore";
import { WorkspaceVcsProvider } from "../state/workspaceVcsStore";
import { WorkbenchPageInner } from "./WorkbenchPage.shell";
import {
  trackFeatureUsed,
  trackWorkspaceOpened,
  trackWorkspaceRouteOpenedFromPending,
} from "../utils/analytics";
import { desktopGetConnection, isDesktopApp } from "../utils/desktop";

export { TaskRow } from "./WorkbenchPage.taskRow";

export default function WorkbenchPage() {
  const { id: workspaceId } = useParams<{ id: string }>();
  useEffect(() => {
    if (!workspaceId) return;
    let cancelled = false;

    const emitOpened = (workspaceKind: "local" | "remote") => {
      trackWorkspaceRouteOpenedFromPending(workspaceId);
      trackWorkspaceOpened(workspaceKind);
      trackFeatureUsed("workbench_opened", { workspace_kind: workspaceKind });
    };

    if (!isDesktopApp()) {
      emitOpened("local");
      return;
    }

    desktopGetConnection()
      .then((connection) => {
        if (cancelled) return;
        emitOpened(connection.kind === "ssh" ? "remote" : "local");
      })
      .catch(() => {
        if (cancelled) return;
        emitOpened("local");
      });

    return () => {
      cancelled = true;
    };
  }, [workspaceId]);
  if (!workspaceId) return null;
  return (
    <WorkspaceActiveSnapshotProvider workspaceId={workspaceId}>
      <WorkspaceVcsProvider workspaceId={workspaceId}>
        <WorkbenchStoreProvider workspaceId={workspaceId}>
          <WorkbenchPageInner workspaceId={workspaceId} />
        </WorkbenchStoreProvider>
      </WorkspaceVcsProvider>
    </WorkspaceActiveSnapshotProvider>
  );
}
