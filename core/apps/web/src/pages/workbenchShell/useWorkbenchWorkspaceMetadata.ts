import { useEffect, useState } from "react";
import type { NavigateFunction } from "react-router-dom";

import { daemonFetchRaw, getHealth, type Workspace } from "../../api/client";

export function useWorkbenchWorkspaceMetadata({
  navigate,
  workspaceId,
}: {
  navigate: NavigateFunction;
  workspaceId: string;
}) {
  const [workspace, setWorkspace] = useState<Workspace | null>(null);
  const [daemonDataRoot, setDaemonDataRoot] = useState<string | null>(null);

  useEffect(() => {
    if (!workspaceId) return;
    let cancelled = false;
    const loadWorkspace = async () => {
      const response = await daemonFetchRaw(`/api/workspaces/${workspaceId}`);
      if (cancelled) return;
      if (response.status === 404 || response.status === 400) {
        navigate("/", { replace: true });
        return;
      }
      if (response.status >= 200 && response.status < 300 && response.body) {
        try {
          setWorkspace(JSON.parse(response.body) as Workspace);
          return;
        } catch {
          // ignore parse errors and fall through to null
        }
      }
      setWorkspace(null);
    };
    loadWorkspace().catch(() => setWorkspace(null));
    return () => {
      cancelled = true;
    };
  }, [navigate, workspaceId]);

  useEffect(() => {
    if (!workspaceId) return;
    let cancelled = false;
    getHealth()
      .then((health) => {
        if (cancelled) return;
        const root = String(health.data_root ?? "").trim();
        setDaemonDataRoot(root || null);
      })
      .catch(() => {
        if (cancelled) return;
        setDaemonDataRoot(null);
      });
    return () => {
      cancelled = true;
    };
  }, [workspaceId]);

  return { workspace, daemonDataRoot };
}
