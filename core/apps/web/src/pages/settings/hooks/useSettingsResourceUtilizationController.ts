import { useCallback, useEffect, useRef, useState } from "react";
import type { ResourceUtilization, Workspace } from "../../../api/client";
import { getResourceUtilization } from "../../../api/client";
import { errorMessage } from "../../../utils/errorMessage";
import type { SectionId } from "../SettingsPage.types";

type SettingsResourceUtilizationController = {
  workspaces: Workspace[];
  snapshot: ResourceUtilization | null;
  loading: boolean;
  error: string | null;
  expandedProcessPids: Record<number, boolean>;
  onToggleExpanded: (pid: number) => void;
};

type Params = {
  active: SectionId;
  workspaceId: string | null;
  workspaces: Workspace[];
};

export function useSettingsResourceUtilizationController({
  active,
  workspaceId,
  workspaces,
}: Params): SettingsResourceUtilizationController {
  const [resourceSnapshot, setResourceSnapshot] = useState<ResourceUtilization | null>(null);
  const [resourceLoading, setResourceLoading] = useState(false);
  const [resourceError, setResourceError] = useState<string | null>(null);
  const [expandedProcessPids, setExpandedProcessPids] = useState<Record<number, boolean>>({});
  const resourcePollRef = useRef<number | null>(null);

  useEffect(() => {
    if (active !== "resource_utilization") return;
    if (!workspaceId) return;
    let cancelled = false;

    const poll = async () => {
      if (cancelled) return;
      setResourceLoading(true);
      setResourceError(null);
      try {
        const snapshot = await getResourceUtilization(workspaceId);
        if (!cancelled) {
          setResourceSnapshot(snapshot);
        }
      } catch (error: unknown) {
        if (!cancelled) {
          setResourceError(errorMessage(error));
        }
      } finally {
        if (!cancelled) {
          setResourceLoading(false);
        }
      }
      if (!cancelled) {
        resourcePollRef.current = window.setTimeout(poll, 3000);
      }
    };

    void poll();
    return () => {
      cancelled = true;
      if (resourcePollRef.current) {
        window.clearTimeout(resourcePollRef.current);
        resourcePollRef.current = null;
      }
    };
  }, [active, workspaceId]);

  const onToggleExpanded = useCallback((pid: number) => {
    setExpandedProcessPids((current) => ({ ...current, [pid]: !current[pid] }));
  }, []);

  return {
    workspaces,
    snapshot: resourceSnapshot,
    loading: resourceLoading,
    error: resourceError,
    expandedProcessPids,
    onToggleExpanded,
  };
}

export type { SettingsResourceUtilizationController };
