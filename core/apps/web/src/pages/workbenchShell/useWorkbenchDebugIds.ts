import { useCallback, useMemo } from "react";

import { copyTextToClipboard } from "../../utils/clipboard";

export function useWorkbenchDebugIds({
  activeSessionId,
  activeTaskId,
  workspaceId,
}: {
  activeSessionId: string | null;
  activeTaskId: string | null;
  workspaceId: string;
}) {
  const showDebugIds = useMemo(() => {
    const params = new URLSearchParams(window.location.search);
    const ids = params.get("ids");
    const debug = params.get("debug");
    if (ids === "1" || debug === "1") {
      localStorage.setItem("contextDebugIds", "1");
      return true;
    }
    if (ids === "0" || debug === "0") {
      localStorage.removeItem("contextDebugIds");
      return false;
    }
    return localStorage.getItem("contextDebugIds") === "1";
  }, []);

  const debugIdLabel = useMemo(() => {
    const short = (value: string | null) => {
      const text = String(value ?? "");
      return text ? text.slice(0, 8) : "-";
    };
    return `task:${short(activeTaskId)} session:${short(activeSessionId)}`;
  }, [activeTaskId, activeSessionId]);

  const copyDebugIds = useCallback(() => {
    void copyTextToClipboard(
      JSON.stringify(
        {
          workspaceId,
          taskId: activeTaskId,
          sessionId: activeSessionId,
        },
        null,
        2,
      ),
    );
  }, [activeSessionId, activeTaskId, workspaceId]);

  return { showDebugIds, debugIdLabel, copyDebugIds };
}
