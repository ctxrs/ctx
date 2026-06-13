import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  getWorkspaceMergeQueueConfig,
  updateWorkspaceMergeQueueConfig,
  type WorkspaceMergeQueueConfig,
} from "../../../api/client";
import { readBoolish } from "../../../utils/boolish";

type MergeQueueFormState = {
  target_branch: string;
  verify_command: string;
  push_on_success: boolean;
  push_remote: string;
  push_branch: string;
};

type UseMergeQueueControllerArgs = {
  workspaceId: string | null;
  enabled: boolean;
};

type MergeQueueController = {
  mergeQueueConfigLoading: boolean;
  mergeQueueConfigSaving: boolean;
  mergeQueueConfigError: string | null;
  mergeQueueForm: MergeQueueFormState;
  setMergeQueueForm: (updater: (prev: MergeQueueFormState) => MergeQueueFormState) => void;
};

const mergeQueueFormFromConfig = (cfg: WorkspaceMergeQueueConfig): MergeQueueFormState => {
  const targetBranch = (cfg.target_branch || "main").trim() || "main";
  const pushRemote = (cfg.push_remote || "origin").trim() || "origin";
  const pushBranch = (cfg.push_branch || targetBranch).trim() || targetBranch;
  return {
    target_branch: targetBranch,
    verify_command: (cfg.verify_command || "").trim(),
    push_on_success: readBoolish(cfg.push_on_success) ?? false,
    push_remote: pushRemote,
    push_branch: pushBranch,
  };
};

const messageFromError = (error: unknown): string => {
  if (error instanceof Error && error.message) {
    return error.message;
  }
  return String(error);
};

export function useMergeQueueController({ workspaceId, enabled }: UseMergeQueueControllerArgs): MergeQueueController {
  const [mergeQueueConfigLoading, setMergeQueueConfigLoading] = useState(false);
  const [mergeQueueConfigSaving, setMergeQueueConfigSaving] = useState(false);
  const [mergeQueueConfigError, setMergeQueueConfigError] = useState<string | null>(null);
  const [mergeQueueForm, setMergeQueueFormState] = useState<MergeQueueFormState>({
    target_branch: "main",
    verify_command: "",
    push_on_success: false,
    push_remote: "origin",
    push_branch: "main",
  });
  const [mergeQueueInitialForm, setMergeQueueInitialForm] = useState<MergeQueueFormState | null>(null);

  const mergeQueueAutosaveDebounceRef = useRef<number | null>(null);

  const mergeQueueDirty = useMemo(() => {
    if (!mergeQueueInitialForm) return false;
    const targetBranch = mergeQueueForm.target_branch.trim();
    const verifyCommand = mergeQueueForm.verify_command.trim();
    const pushRemote = mergeQueueForm.push_remote.trim();
    const pushBranch = mergeQueueForm.push_branch.trim();
    return (
      targetBranch !== mergeQueueInitialForm.target_branch.trim()
      || verifyCommand !== mergeQueueInitialForm.verify_command.trim()
      || mergeQueueForm.push_on_success !== mergeQueueInitialForm.push_on_success
      || pushRemote !== mergeQueueInitialForm.push_remote.trim()
      || pushBranch !== mergeQueueInitialForm.push_branch.trim()
    );
  }, [mergeQueueForm, mergeQueueInitialForm]);

  const refreshMergeQueueConfig = useCallback(async () => {
    if (!workspaceId) return;
    setMergeQueueConfigLoading(true);
    setMergeQueueConfigError(null);
    try {
      const cfg = await getWorkspaceMergeQueueConfig(workspaceId);
      const nextForm = mergeQueueFormFromConfig(cfg);
      setMergeQueueFormState(nextForm);
      setMergeQueueInitialForm(nextForm);
    } catch (error) {
      setMergeQueueConfigError(messageFromError(error));
    } finally {
      setMergeQueueConfigLoading(false);
    }
  }, [workspaceId]);

  const saveMergeQueueConfig = useCallback(
    async (opts?: { showValidationErrors?: boolean }): Promise<boolean> => {
      const showValidationErrors = opts?.showValidationErrors ?? true;
      if (!workspaceId || mergeQueueConfigSaving) return false;

      const targetBranch = mergeQueueForm.target_branch.trim();
      if (!targetBranch) {
        if (showValidationErrors) {
          setMergeQueueConfigError("Target branch is required.");
        }
        return false;
      }
      const verifyCommand = mergeQueueForm.verify_command.trim();
      const pushRemote = mergeQueueForm.push_remote.trim();
      const pushBranch = mergeQueueForm.push_branch.trim();

      if (mergeQueueForm.push_on_success && !pushRemote) {
        if (showValidationErrors) {
          setMergeQueueConfigError("Push remote is required when push-on-success is enabled.");
        }
        return false;
      }
      if (mergeQueueForm.push_on_success && !pushBranch) {
        if (showValidationErrors) {
          setMergeQueueConfigError("Push branch is required when push-on-success is enabled.");
        }
        return false;
      }

      setMergeQueueConfigSaving(true);
      setMergeQueueConfigError(null);
      try {
        await updateWorkspaceMergeQueueConfig(workspaceId, {
          enabled: true,
          target_branch: targetBranch,
          verify_command: verifyCommand || null,
          push_on_success: mergeQueueForm.push_on_success,
          push_remote: mergeQueueForm.push_on_success ? pushRemote : null,
          push_branch: mergeQueueForm.push_on_success ? pushBranch : null,
        });
        await refreshMergeQueueConfig();
        return true;
      } catch (error) {
        setMergeQueueConfigError(messageFromError(error));
        return false;
      } finally {
        setMergeQueueConfigSaving(false);
      }
    },
    [workspaceId, mergeQueueConfigSaving, mergeQueueForm, refreshMergeQueueConfig],
  );

  const setMergeQueueForm = useCallback(
    (updater: (prev: MergeQueueFormState) => MergeQueueFormState) => {
      setMergeQueueFormState((prev) => updater(prev));
    },
    [],
  );

  useEffect(() => {
    if (!enabled || !workspaceId) return;
    refreshMergeQueueConfig().catch(() => {});
  }, [enabled, refreshMergeQueueConfig, workspaceId]);

  useEffect(() => {
    if (!enabled || !workspaceId) return;
    if (!mergeQueueDirty || mergeQueueConfigSaving || mergeQueueConfigLoading) return;
    const targetBranch = mergeQueueForm.target_branch.trim();
    if (!targetBranch) return;
    if (mergeQueueForm.push_on_success && (!mergeQueueForm.push_remote.trim() || !mergeQueueForm.push_branch.trim())) {
      return;
    }
    if (mergeQueueAutosaveDebounceRef.current) {
      window.clearTimeout(mergeQueueAutosaveDebounceRef.current);
    }
    mergeQueueAutosaveDebounceRef.current = window.setTimeout(() => {
      mergeQueueAutosaveDebounceRef.current = null;
      saveMergeQueueConfig({ showValidationErrors: false }).catch(() => {});
    }, 1000);
  }, [
    enabled,
    mergeQueueConfigLoading,
    mergeQueueConfigSaving,
    mergeQueueDirty,
    mergeQueueForm,
    saveMergeQueueConfig,
    workspaceId,
  ]);

  useEffect(() => {
    if (!enabled) return;
    if (mergeQueueDirty) return;
    if (mergeQueueAutosaveDebounceRef.current) {
      window.clearTimeout(mergeQueueAutosaveDebounceRef.current);
      mergeQueueAutosaveDebounceRef.current = null;
    }
  }, [enabled, mergeQueueDirty]);

  useEffect(() => {
    if (!enabled) return;
    const handleBeforeUnload = () => {
      if (mergeQueueDirty) {
        saveMergeQueueConfig({ showValidationErrors: false }).catch(() => {});
      }
    };
    window.addEventListener("beforeunload", handleBeforeUnload);
    return () => window.removeEventListener("beforeunload", handleBeforeUnload);
  }, [enabled, mergeQueueDirty, saveMergeQueueConfig]);

  useEffect(() => {
    return () => {
      if (mergeQueueAutosaveDebounceRef.current) {
        window.clearTimeout(mergeQueueAutosaveDebounceRef.current);
      }
    };
  }, []);

  return {
    mergeQueueConfigLoading,
    mergeQueueConfigSaving,
    mergeQueueConfigError,
    mergeQueueForm,
    setMergeQueueForm,
  };
}
