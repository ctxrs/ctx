import { useEffect, useMemo, useSyncExternalStore } from "react";
import {
  getClientSettingsState,
  subscribeClientSettings,
} from "../../state/clientSettings";
import {
  desktopClearWindowAttention,
  desktopSyncWorkspaceAttention,
  isDesktopApp,
} from "../../utils/desktop";
import type { WorkspaceActiveSnapshotItem } from "../../state/workspaceActiveSnapshotStore";
import {
  deriveWorkspaceAttentionState,
  type WorkbenchTaskLiveInfo,
} from "./workbenchTaskActivity";

type UseWorkbenchDesktopAttentionArgs = {
  workspaceId: string;
  activeTaskIds: string[];
  tasksById: Record<string, WorkspaceActiveSnapshotItem>;
  taskLiveInfo: WorkbenchTaskLiveInfo;
};

export function useWorkbenchDesktopAttention({
  workspaceId,
  activeTaskIds,
  tasksById,
  taskLiveInfo,
}: UseWorkbenchDesktopAttentionArgs) {
  const desktopUi = isDesktopApp();
  const clientSettingsState = useSyncExternalStore(
    subscribeClientSettings,
    getClientSettingsState,
    getClientSettingsState,
  );
  const attention = useMemo(
    () => deriveWorkspaceAttentionState({ activeTaskIds, tasksById, taskLiveInfo }),
    [activeTaskIds, taskLiveInfo, tasksById],
  );

  useEffect(() => {
    if (!desktopUi || !workspaceId) return;
    if (!clientSettingsState.loaded) return;
    const badgeEnabled = clientSettingsState.settings.desktopNotifications.badgeUnreadCount;
    void desktopSyncWorkspaceAttention({
      workspace_id: workspaceId,
      unread_primary_task_count: badgeEnabled ? attention.unreadPrimaryTaskCount : 0,
      has_unread_error: badgeEnabled && attention.hasUnreadError,
    }).catch((err) => {
      console.warn("desktop attention sync failed", err);
    });
  }, [
    attention.hasUnreadError,
    attention.unreadPrimaryTaskCount,
    clientSettingsState.loaded,
    clientSettingsState.settings.desktopNotifications.badgeUnreadCount,
    desktopUi,
    workspaceId,
  ]);

  useEffect(() => {
    if (!desktopUi) return;
    return () => {
      void desktopClearWindowAttention().catch((err) => {
        console.warn("desktop attention clear failed", err);
      });
    };
  }, [desktopUi]);
}
