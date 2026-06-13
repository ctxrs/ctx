import { useEffect } from "react";

import type { useWorkbenchStore } from "../../workbench/store";

type WorkbenchStore = ReturnType<typeof useWorkbenchStore>;

export function useWorkbenchE2EFocusBridge(workbenchStore: WorkbenchStore) {
  useEffect(() => {
    if (window.sessionStorage.getItem("ctxE2E") !== "1") return;
    const win = window as Window & {
      __ctxE2E?: {
        focusTask?: (taskId: string, sessionId?: string | null) => boolean;
      };
    };
    win.__ctxE2E ??= {};
    win.__ctxE2E.focusTask = (taskId: string, sessionId?: string | null) => {
      const navToken = workbenchStore.getNavToken();
      return workbenchStore.focusTask(taskId, sessionId, { navToken, source: "system" });
    };
    return () => {
      if (!win.__ctxE2E) return;
      delete win.__ctxE2E.focusTask;
    };
  }, [workbenchStore]);
}
