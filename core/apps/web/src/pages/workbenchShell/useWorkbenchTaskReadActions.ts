import { useCallback, useRef } from "react";

import {
  markTaskRead as markTaskReadApi,
  markTaskUnread as markTaskUnreadApi,
} from "../../api/client";
import type { useWorkspaceActiveSnapshotStore } from "../../state/workspaceActiveSnapshotStore";

type WorkspaceSnapshotStore = Pick<ReturnType<typeof useWorkspaceActiveSnapshotStore>, "applyTaskUpdate">;

export function useWorkbenchTaskReadActions(workspaceSnapshotStore: WorkspaceSnapshotStore) {
  const markTaskReadInFlightRef = useRef<Record<string, Promise<void> | undefined>>({});

  const markTaskRead = useCallback(
    async (taskId: string) => {
      if (markTaskReadInFlightRef.current[taskId]) return;
      const promise = (async () => {
        try {
          const updated = await markTaskReadApi(taskId);
          workspaceSnapshotStore.applyTaskUpdate(updated);
        } catch {
          // ignore
        }
      })().finally(() => {
        delete markTaskReadInFlightRef.current[taskId];
      });
      markTaskReadInFlightRef.current[taskId] = promise;
      await promise;
    },
    [workspaceSnapshotStore],
  );

  const markTaskUnread = useCallback(
    async (taskId: string) => {
      try {
        const updated = await markTaskUnreadApi(taskId);
        workspaceSnapshotStore.applyTaskUpdate(updated);
      } catch {
        // ignore
      }
    },
    [workspaceSnapshotStore],
  );

  return { markTaskRead, markTaskUnread };
}
