import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  getWorkspaceExecutionConfig,
  updateWorkspaceExecutionConfig,
  type WorkspaceExecutionConfig,
} from "../../../api/client";
import { isContainerizedEnvironment, type PromptAutosaveStatus } from "../SettingsPage.utils";

type WorkspaceNetworkMode = NonNullable<WorkspaceExecutionConfig["network_mode"]>;

type UseContainerNetworkControllerArgs = {
  workspaceId: string | null;
  enabled: boolean;
};

type ContainerNetworkController = {
  workspaceExecution: WorkspaceExecutionConfig | null;
  workspaceExecutionLoading: boolean;
  workspaceExecutionError: string | null;
  workspaceNetworkPolicySaving: boolean;
  workspaceAllowlistSaving: boolean;
  workspaceAllowlistAutosaveState: PromptAutosaveStatus;
  workspaceAllowlistText: string;
  setWorkspaceAllowlistText: (value: string) => void;
  handleUpdateWorkspaceNetworkPolicy: (mode: WorkspaceNetworkMode) => Promise<void>;
};

const messageFromError = (error: unknown): string => {
  if (error instanceof Error && error.message) {
    return error.message;
  }
  return String(error);
};

export function useContainerNetworkController({
  workspaceId,
  enabled,
}: UseContainerNetworkControllerArgs): ContainerNetworkController {
  const [workspaceExecution, setWorkspaceExecution] = useState<WorkspaceExecutionConfig | null>(null);
  const [workspaceExecutionLoading, setWorkspaceExecutionLoading] = useState(false);
  const [workspaceExecutionError, setWorkspaceExecutionError] = useState<string | null>(null);

  const [workspaceAllowlistText, setWorkspaceAllowlistTextState] = useState("");
  const [workspaceAllowlistDirty, setWorkspaceAllowlistDirty] = useState(false);
  const [workspaceAllowlistSaving, setWorkspaceAllowlistSaving] = useState(false);
  const [workspaceAllowlistAutosaveState, setWorkspaceAllowlistAutosaveState] = useState<PromptAutosaveStatus>("idle");

  const [workspaceNetworkPolicySaving, setWorkspaceNetworkPolicySaving] = useState(false);

  const allowlistAutosaveDebounceRef = useRef<number | null>(null);
  const allowlistAutosaveResetRef = useRef<number | null>(null);

  const refreshWorkspaceExecutionConfig = useCallback(async () => {
    if (!workspaceId) return;
    setWorkspaceExecutionLoading(true);
    setWorkspaceExecutionError(null);
    try {
      const next = await getWorkspaceExecutionConfig(workspaceId);
      setWorkspaceExecution(next);
      const initialAllowlist = Array.isArray(next.allowlist) ? next.allowlist : [];
      setWorkspaceAllowlistTextState(initialAllowlist.join("\n"));
      setWorkspaceAllowlistDirty(false);
      setWorkspaceAllowlistAutosaveState("idle");
    } catch (error) {
      setWorkspaceExecutionError(messageFromError(error));
      setWorkspaceExecution(null);
      setWorkspaceAllowlistAutosaveState("idle");
    } finally {
      setWorkspaceExecutionLoading(false);
    }
  }, [workspaceId]);

  const saveWorkspaceAllowlist = useCallback(async (): Promise<boolean> => {
    if (!workspaceId || !workspaceExecution || workspaceAllowlistSaving) return false;

    const allowlist = workspaceAllowlistText
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter(Boolean);

    if (workspaceExecution.environment === "host") {
      setWorkspaceExecutionError("Allowlist only applies in sandbox mode.");
      return false;
    }
    if (workspaceExecution.network_mode !== "allowlist") {
      setWorkspaceExecutionError("Allowlist is only active when the network policy is set to Allowlist.");
      return false;
    }
    if (allowlist.length === 0) {
      setWorkspaceExecutionError("Allowlist must include at least one host.");
      return false;
    }

    setWorkspaceAllowlistSaving(true);
    setWorkspaceExecutionError(null);
    try {
      await updateWorkspaceExecutionConfig(workspaceId, {
        environment: workspaceExecution.environment,
        network_mode: workspaceExecution.network_mode ?? null,
        allowlist,
      });
      await refreshWorkspaceExecutionConfig();
      return true;
    } catch (error) {
      setWorkspaceExecutionError(messageFromError(error));
      return false;
    } finally {
      setWorkspaceAllowlistSaving(false);
    }
  }, [
    refreshWorkspaceExecutionConfig,
    workspaceAllowlistSaving,
    workspaceAllowlistText,
    workspaceExecution,
    workspaceId,
  ]);

  const flushWorkspaceAllowlistAutosave = useCallback(async () => {
    if (allowlistAutosaveDebounceRef.current) {
      window.clearTimeout(allowlistAutosaveDebounceRef.current);
      allowlistAutosaveDebounceRef.current = null;
    }
    if (allowlistAutosaveResetRef.current) {
      window.clearTimeout(allowlistAutosaveResetRef.current);
      allowlistAutosaveResetRef.current = null;
    }
    if (!workspaceId || !workspaceExecution || !workspaceAllowlistDirty) return;
    if (!isContainerizedEnvironment(workspaceExecution.environment) || workspaceExecution.network_mode !== "allowlist") {
      return;
    }
    setWorkspaceAllowlistAutosaveState("saving");
    const ok = await saveWorkspaceAllowlist();
    if (!ok) {
      setWorkspaceAllowlistAutosaveState("error");
      return;
    }
    setWorkspaceAllowlistAutosaveState("saved");
    allowlistAutosaveResetRef.current = window.setTimeout(() => {
      setWorkspaceAllowlistAutosaveState("idle");
      allowlistAutosaveResetRef.current = null;
    }, 1200);
  }, [workspaceAllowlistDirty, workspaceExecution, workspaceId, saveWorkspaceAllowlist]);

  const handleUpdateWorkspaceNetworkPolicy = useCallback(
    async (nextMode: WorkspaceNetworkMode) => {
      if (!workspaceId || !workspaceExecution || workspaceNetworkPolicySaving) return;
      if (!isContainerizedEnvironment(workspaceExecution.environment)) return;
      if ((workspaceExecution.network_mode ?? "llm_only") === nextMode) return;

      const previous = workspaceExecution;
      setWorkspaceExecution({
        ...workspaceExecution,
        network_mode: nextMode,
      });
      setWorkspaceExecutionError(null);
      setWorkspaceNetworkPolicySaving(true);
      try {
        await updateWorkspaceExecutionConfig(workspaceId, {
          environment: workspaceExecution.environment,
          network_mode: nextMode,
          allowlist: Array.isArray(workspaceExecution.allowlist) ? workspaceExecution.allowlist : null,
        });
        await refreshWorkspaceExecutionConfig();
      } catch (error) {
        setWorkspaceExecution(previous);
        setWorkspaceExecutionError(messageFromError(error));
      } finally {
        setWorkspaceNetworkPolicySaving(false);
      }
    },
    [refreshWorkspaceExecutionConfig, workspaceExecution, workspaceId, workspaceNetworkPolicySaving],
  );

  const setWorkspaceAllowlistText = useCallback((value: string) => {
    setWorkspaceAllowlistTextState(value);
    setWorkspaceAllowlistDirty(true);
  }, []);

  const allowlistAutosaveEnabled = useMemo(() => {
    return Boolean(
      enabled
        && workspaceId
        && workspaceExecution
        && isContainerizedEnvironment(workspaceExecution.environment)
        && workspaceExecution.network_mode === "allowlist",
    );
  }, [enabled, workspaceExecution, workspaceId]);

  useEffect(() => {
    if (!enabled || !workspaceId) return;
    refreshWorkspaceExecutionConfig().catch(() => {});
  }, [enabled, refreshWorkspaceExecutionConfig, workspaceId]);

  useEffect(() => {
    if (!enabled) return;
    if (!allowlistAutosaveEnabled) {
      if (allowlistAutosaveDebounceRef.current) {
        window.clearTimeout(allowlistAutosaveDebounceRef.current);
        allowlistAutosaveDebounceRef.current = null;
      }
      setWorkspaceAllowlistAutosaveState("idle");
      return;
    }

    if (!workspaceAllowlistDirty || workspaceAllowlistSaving) return;
    if (allowlistAutosaveResetRef.current) {
      window.clearTimeout(allowlistAutosaveResetRef.current);
      allowlistAutosaveResetRef.current = null;
    }
    setWorkspaceAllowlistAutosaveState("pending");
    if (allowlistAutosaveDebounceRef.current) {
      window.clearTimeout(allowlistAutosaveDebounceRef.current);
    }
    allowlistAutosaveDebounceRef.current = window.setTimeout(() => {
      allowlistAutosaveDebounceRef.current = null;
      flushWorkspaceAllowlistAutosave().catch(() => {});
    }, 1000);
  }, [
    allowlistAutosaveEnabled,
    enabled,
    flushWorkspaceAllowlistAutosave,
    workspaceAllowlistDirty,
    workspaceAllowlistSaving,
    workspaceAllowlistText,
  ]);

  useEffect(() => {
    if (!enabled) return;
    if (workspaceAllowlistDirty) return;
    if (allowlistAutosaveDebounceRef.current) {
      window.clearTimeout(allowlistAutosaveDebounceRef.current);
      allowlistAutosaveDebounceRef.current = null;
    }
    if (workspaceAllowlistAutosaveState === "pending") {
      setWorkspaceAllowlistAutosaveState("idle");
    }
  }, [enabled, workspaceAllowlistAutosaveState, workspaceAllowlistDirty]);

  useEffect(() => {
    if (!enabled) return;
    const handleBeforeUnload = () => {
      if (workspaceAllowlistDirty) {
        saveWorkspaceAllowlist().catch(() => {});
      }
    };
    window.addEventListener("beforeunload", handleBeforeUnload);
    return () => window.removeEventListener("beforeunload", handleBeforeUnload);
  }, [enabled, saveWorkspaceAllowlist, workspaceAllowlistDirty]);

  useEffect(() => {
    return () => {
      if (allowlistAutosaveDebounceRef.current) {
        window.clearTimeout(allowlistAutosaveDebounceRef.current);
      }
      if (allowlistAutosaveResetRef.current) {
        window.clearTimeout(allowlistAutosaveResetRef.current);
      }
    };
  }, []);

  return {
    workspaceExecution,
    workspaceExecutionLoading,
    workspaceExecutionError,
    workspaceNetworkPolicySaving,
    workspaceAllowlistSaving,
    workspaceAllowlistAutosaveState,
    workspaceAllowlistText,
    setWorkspaceAllowlistText,
    handleUpdateWorkspaceNetworkPolicy,
  };
}
