import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  getWorkspaceWorktreeBootstrapConfig,
  updateWorkspaceWorktreeBootstrapConfig,
} from "../../../api/client";
import {
  worktreeBootstrapFormFromConfig,
  type PromptAutosaveStatus,
  type WorktreeBootstrapFormState,
} from "../SettingsPage.utils";

type UseWorktreeBootstrapControllerArgs = {
  workspaceId: string | null;
  enabled: boolean;
};

type WorktreeBootstrapController = {
  worktreeBootstrapLoading: boolean;
  worktreeBootstrapSaving: boolean;
  worktreeBootstrapError: string | null;
  worktreeBootstrapAutosaveState: PromptAutosaveStatus;
  worktreeBootstrapForm: WorktreeBootstrapFormState;
  setWorktreeBootstrapForm: (updater: (prev: WorktreeBootstrapFormState) => WorktreeBootstrapFormState) => void;
  worktreeWaitInfoOpen: boolean;
  setWorktreeWaitInfoOpen: (next: boolean) => void;
};

const messageFromError = (error: unknown): string => {
  if (error instanceof Error && error.message) {
    return error.message;
  }
  return String(error);
};

export function useWorktreeBootstrapController({
  workspaceId,
  enabled,
}: UseWorktreeBootstrapControllerArgs): WorktreeBootstrapController {
  const [worktreeBootstrapLoading, setWorktreeBootstrapLoading] = useState(false);
  const [worktreeBootstrapSaving, setWorktreeBootstrapSaving] = useState(false);
  const [worktreeBootstrapError, setWorktreeBootstrapError] = useState<string | null>(null);
  const [worktreeBootstrapAutosaveState, setWorktreeBootstrapAutosaveState] = useState<PromptAutosaveStatus>("idle");
  const [worktreeBootstrapForm, setWorktreeBootstrapFormState] = useState<WorktreeBootstrapFormState>(() =>
    worktreeBootstrapFormFromConfig(null),
  );
  const [worktreeBootstrapInitialForm, setWorktreeBootstrapInitialForm] = useState<WorktreeBootstrapFormState>(() =>
    worktreeBootstrapFormFromConfig(null),
  );
  const [worktreeWaitInfoOpen, setWorktreeWaitInfoOpen] = useState(false);

  const worktreeBootstrapAutosaveDebounceRef = useRef<number | null>(null);
  const worktreeBootstrapAutosaveResetRef = useRef<number | null>(null);

  const worktreeBootstrapDirty = useMemo(() => {
    return (
      worktreeBootstrapForm.setup_command.trim() !== worktreeBootstrapInitialForm.setup_command.trim()
      || worktreeBootstrapForm.timeout_sec.trim() !== worktreeBootstrapInitialForm.timeout_sec.trim()
      || worktreeBootstrapForm.wait_for_completion !== worktreeBootstrapInitialForm.wait_for_completion
    );
  }, [worktreeBootstrapForm, worktreeBootstrapInitialForm]);

  const refreshWorktreeBootstrapConfig = useCallback(async () => {
    if (!workspaceId) return;
    setWorktreeBootstrapLoading(true);
    setWorktreeBootstrapError(null);
    try {
      const cfg = await getWorkspaceWorktreeBootstrapConfig(workspaceId);
      const nextForm = worktreeBootstrapFormFromConfig(cfg);
      setWorktreeBootstrapFormState(nextForm);
      setWorktreeBootstrapInitialForm(nextForm);
      setWorktreeBootstrapAutosaveState("idle");
    } catch (error) {
      setWorktreeBootstrapError(messageFromError(error));
      const blankForm = worktreeBootstrapFormFromConfig(null);
      setWorktreeBootstrapFormState(blankForm);
      setWorktreeBootstrapInitialForm(blankForm);
      setWorktreeBootstrapAutosaveState("idle");
    } finally {
      setWorktreeBootstrapLoading(false);
    }
  }, [workspaceId]);

  const saveWorktreeBootstrapConfig = useCallback(async (): Promise<boolean> => {
    if (!workspaceId || worktreeBootstrapSaving) return false;

    const setupCommand = worktreeBootstrapForm.setup_command.trim();
    const timeoutRaw = worktreeBootstrapForm.timeout_sec.trim();
    const hasCommand = setupCommand.length > 0;

    let timeoutSec: number | null = null;
    if (hasCommand && timeoutRaw.length > 0) {
      if (!/^\d+$/.test(timeoutRaw)) {
        setWorktreeBootstrapError("Timeout must be a whole number of seconds.");
        return false;
      }
      const parsed = Number(timeoutRaw);
      if (!Number.isSafeInteger(parsed) || parsed <= 0) {
        setWorktreeBootstrapError("Timeout must be greater than 0.");
        return false;
      }
      timeoutSec = parsed;
    }

    setWorktreeBootstrapSaving(true);
    setWorktreeBootstrapError(null);
    try {
      await updateWorkspaceWorktreeBootstrapConfig(workspaceId, {
        setup_command: hasCommand ? setupCommand : null,
        timeout_sec: hasCommand ? timeoutSec : null,
        wait_for_completion: hasCommand ? worktreeBootstrapForm.wait_for_completion : null,
      });
      await refreshWorktreeBootstrapConfig();
      return true;
    } catch (error) {
      setWorktreeBootstrapError(messageFromError(error));
      return false;
    } finally {
      setWorktreeBootstrapSaving(false);
    }
  }, [refreshWorktreeBootstrapConfig, workspaceId, worktreeBootstrapForm, worktreeBootstrapSaving]);

  const flushWorktreeBootstrapAutosave = useCallback(async () => {
    if (worktreeBootstrapAutosaveDebounceRef.current) {
      window.clearTimeout(worktreeBootstrapAutosaveDebounceRef.current);
      worktreeBootstrapAutosaveDebounceRef.current = null;
    }
    if (worktreeBootstrapAutosaveResetRef.current) {
      window.clearTimeout(worktreeBootstrapAutosaveResetRef.current);
      worktreeBootstrapAutosaveResetRef.current = null;
    }
    if (!workspaceId || !worktreeBootstrapDirty) return;
    setWorktreeBootstrapAutosaveState("saving");
    const ok = await saveWorktreeBootstrapConfig();
    if (!ok) {
      setWorktreeBootstrapAutosaveState("error");
      return;
    }
    setWorktreeBootstrapAutosaveState("saved");
    worktreeBootstrapAutosaveResetRef.current = window.setTimeout(() => {
      setWorktreeBootstrapAutosaveState("idle");
      worktreeBootstrapAutosaveResetRef.current = null;
    }, 1200);
  }, [saveWorktreeBootstrapConfig, workspaceId, worktreeBootstrapDirty]);

  const setWorktreeBootstrapForm = useCallback(
    (updater: (prev: WorktreeBootstrapFormState) => WorktreeBootstrapFormState) => {
      setWorktreeBootstrapFormState((prev) => updater(prev));
    },
    [],
  );

  useEffect(() => {
    if (!enabled || !workspaceId) return;
    refreshWorktreeBootstrapConfig().catch(() => {});
  }, [enabled, refreshWorktreeBootstrapConfig, workspaceId]);

  useEffect(() => {
    if (!enabled || !workspaceId) return;
    if (!worktreeBootstrapDirty || worktreeBootstrapSaving || worktreeBootstrapLoading) return;
    if (worktreeBootstrapAutosaveResetRef.current) {
      window.clearTimeout(worktreeBootstrapAutosaveResetRef.current);
      worktreeBootstrapAutosaveResetRef.current = null;
    }
    setWorktreeBootstrapAutosaveState("pending");
    if (worktreeBootstrapAutosaveDebounceRef.current) {
      window.clearTimeout(worktreeBootstrapAutosaveDebounceRef.current);
    }
    worktreeBootstrapAutosaveDebounceRef.current = window.setTimeout(() => {
      worktreeBootstrapAutosaveDebounceRef.current = null;
      flushWorktreeBootstrapAutosave().catch(() => {});
    }, 1000);
  }, [
    enabled,
    flushWorktreeBootstrapAutosave,
    workspaceId,
    worktreeBootstrapDirty,
    worktreeBootstrapForm,
    worktreeBootstrapLoading,
    worktreeBootstrapSaving,
  ]);

  useEffect(() => {
    if (!enabled) return;
    if (worktreeBootstrapDirty) return;
    if (worktreeBootstrapAutosaveDebounceRef.current) {
      window.clearTimeout(worktreeBootstrapAutosaveDebounceRef.current);
      worktreeBootstrapAutosaveDebounceRef.current = null;
    }
    if (worktreeBootstrapAutosaveState === "pending") {
      setWorktreeBootstrapAutosaveState("idle");
    }
  }, [enabled, worktreeBootstrapAutosaveState, worktreeBootstrapDirty]);

  useEffect(() => {
    if (!enabled) return;
    const handleBeforeUnload = () => {
      if (worktreeBootstrapDirty) {
        saveWorktreeBootstrapConfig().catch(() => {});
      }
    };
    window.addEventListener("beforeunload", handleBeforeUnload);
    return () => window.removeEventListener("beforeunload", handleBeforeUnload);
  }, [enabled, saveWorktreeBootstrapConfig, worktreeBootstrapDirty]);

  useEffect(() => {
    if (enabled) return;
    setWorktreeWaitInfoOpen(false);
  }, [enabled]);

  useEffect(() => {
    return () => {
      if (worktreeBootstrapAutosaveDebounceRef.current) {
        window.clearTimeout(worktreeBootstrapAutosaveDebounceRef.current);
      }
      if (worktreeBootstrapAutosaveResetRef.current) {
        window.clearTimeout(worktreeBootstrapAutosaveResetRef.current);
      }
    };
  }, []);

  return {
    worktreeBootstrapLoading,
    worktreeBootstrapSaving,
    worktreeBootstrapError,
    worktreeBootstrapAutosaveState,
    worktreeBootstrapForm,
    setWorktreeBootstrapForm,
    worktreeWaitInfoOpen,
    setWorktreeWaitInfoOpen,
  };
}
