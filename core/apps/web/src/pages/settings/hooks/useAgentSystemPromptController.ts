import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  getAgentSystemPrompt,
  getSubagentSystemPrompt,
  updateAgentSystemPrompt,
  updateSubagentSystemPrompt,
  type AgentSystemPromptConfig,
  type SubagentSystemPromptConfig,
} from "../../../api/client";
import { AGENT_PROMPT_DEFAULT, SUBAGENT_PROMPT_DEFAULT } from "../SettingsPage.constants";
import type { PromptAutosaveStatus } from "../SettingsPage.utils";

type UseAgentSystemPromptControllerArgs = {
  workspaceId: string | null;
  enabled: boolean;
};

type AgentSystemPromptController = {
  agentPromptLoading: boolean;
  agentPromptSaving: boolean;
  agentPromptError: string | null;
  agentPromptText: string;
  setAgentPromptText: (value: string) => void;
  agentPromptAutosaveState: PromptAutosaveStatus;
  subagentPromptLoading: boolean;
  subagentPromptSaving: boolean;
  subagentPromptError: string | null;
  subagentPromptText: string;
  setSubagentPromptText: (value: string) => void;
  subagentPromptAutosaveState: PromptAutosaveStatus;
};

const isWorkspaceNotFoundMessage = (message: string): boolean => {
  const lower = message.toLowerCase();
  return lower.includes("404") || lower.includes("workspace not found");
};

const messageFromError = (error: unknown): string => {
  if (error instanceof Error && error.message) {
    return error.message;
  }
  return String(error);
};

export function useAgentSystemPromptController({
  workspaceId,
  enabled,
}: UseAgentSystemPromptControllerArgs): AgentSystemPromptController {
  const [agentPromptConfig, setAgentPromptConfig] = useState<AgentSystemPromptConfig | null>(null);
  const [agentPromptLoading, setAgentPromptLoading] = useState(false);
  const [agentPromptError, setAgentPromptError] = useState<string | null>(null);
  const [agentPromptSaving, setAgentPromptSaving] = useState(false);
  const [agentPromptText, setAgentPromptText] = useState<string>(AGENT_PROMPT_DEFAULT);
  const [agentPromptAutosaveState, setAgentPromptAutosaveState] = useState<PromptAutosaveStatus>("idle");

  const [subagentPromptConfig, setSubagentPromptConfig] = useState<SubagentSystemPromptConfig | null>(null);
  const [subagentPromptLoading, setSubagentPromptLoading] = useState(false);
  const [subagentPromptError, setSubagentPromptError] = useState<string | null>(null);
  const [subagentPromptSaving, setSubagentPromptSaving] = useState(false);
  const [subagentPromptText, setSubagentPromptText] = useState<string>(SUBAGENT_PROMPT_DEFAULT);
  const [subagentPromptAutosaveState, setSubagentPromptAutosaveState] = useState<PromptAutosaveStatus>("idle");

  const promptAutosaveDebounceRef = useRef<number | null>(null);
  const promptAutosaveResetRef = useRef<number | null>(null);
  const subagentPromptAutosaveDebounceRef = useRef<number | null>(null);
  const subagentPromptAutosaveResetRef = useRef<number | null>(null);

  const agentPromptBase = useMemo(
    () => agentPromptConfig?.configured_append ?? agentPromptConfig?.default_append ?? AGENT_PROMPT_DEFAULT,
    [agentPromptConfig],
  );
  const subagentPromptBase = useMemo(
    () =>
      subagentPromptConfig?.configured_append
      ?? subagentPromptConfig?.default_append
      ?? SUBAGENT_PROMPT_DEFAULT,
    [subagentPromptConfig],
  );

  const agentPromptDirty = useMemo(
    () => (agentPromptConfig ? agentPromptText.trim() !== agentPromptBase.trim() : false),
    [agentPromptConfig, agentPromptText, agentPromptBase],
  );
  const subagentPromptDirty = useMemo(
    () => (subagentPromptConfig ? subagentPromptText.trim() !== subagentPromptBase.trim() : false),
    [subagentPromptConfig, subagentPromptText, subagentPromptBase],
  );

  const refreshAgentPrompt = useCallback(async () => {
    if (!workspaceId) return;
    setAgentPromptLoading(true);
    setAgentPromptError(null);
    try {
      const next = await getAgentSystemPrompt(workspaceId);
      setAgentPromptConfig(next);
      setAgentPromptText(next.configured_append ?? next.default_append ?? AGENT_PROMPT_DEFAULT);
      setAgentPromptAutosaveState("idle");
    } catch (error) {
      const message = messageFromError(error);
      if (isWorkspaceNotFoundMessage(message)) {
        setAgentPromptConfig({
          default_append: AGENT_PROMPT_DEFAULT,
          configured_append: null,
          effective_append: AGENT_PROMPT_DEFAULT,
          source: "default",
        });
        setAgentPromptText(AGENT_PROMPT_DEFAULT);
        setAgentPromptError(null);
        setAgentPromptAutosaveState("idle");
      } else {
        setAgentPromptError(message);
      }
    } finally {
      setAgentPromptLoading(false);
    }
  }, [workspaceId]);

  const refreshSubagentPrompt = useCallback(async () => {
    if (!workspaceId) return;
    setSubagentPromptLoading(true);
    setSubagentPromptError(null);
    try {
      const next = await getSubagentSystemPrompt(workspaceId);
      setSubagentPromptConfig(next);
      setSubagentPromptText(next.configured_append ?? next.default_append ?? SUBAGENT_PROMPT_DEFAULT);
      setSubagentPromptAutosaveState("idle");
    } catch (error) {
      const message = messageFromError(error);
      if (isWorkspaceNotFoundMessage(message)) {
        setSubagentPromptConfig({
          default_append: SUBAGENT_PROMPT_DEFAULT,
          configured_append: null,
          effective_append: SUBAGENT_PROMPT_DEFAULT,
          source: "default",
        });
        setSubagentPromptText(SUBAGENT_PROMPT_DEFAULT);
        setSubagentPromptError(null);
        setSubagentPromptAutosaveState("idle");
      } else {
        setSubagentPromptError(message);
      }
    } finally {
      setSubagentPromptLoading(false);
    }
  }, [workspaceId]);

  const saveAgentPrompt = useCallback(async (): Promise<boolean> => {
    if (!workspaceId || agentPromptSaving) return false;
    setAgentPromptSaving(true);
    setAgentPromptError(null);
    try {
      const payload = agentPromptText.trim() ? agentPromptText.trim() : null;
      const next = await updateAgentSystemPrompt(workspaceId, { system_prompt_append: payload });
      setAgentPromptConfig(next);
      const nextText = next.configured_append ?? next.default_append ?? AGENT_PROMPT_DEFAULT;
      if (agentPromptText.trim() !== nextText.trim()) {
        setAgentPromptText(nextText);
      }
      return true;
    } catch (error) {
      setAgentPromptError(messageFromError(error));
      return false;
    } finally {
      setAgentPromptSaving(false);
    }
  }, [agentPromptSaving, agentPromptText, workspaceId]);

  const saveSubagentPrompt = useCallback(async (): Promise<boolean> => {
    if (!workspaceId || subagentPromptSaving) return false;
    setSubagentPromptSaving(true);
    setSubagentPromptError(null);
    try {
      const payload = subagentPromptText.trim() ? subagentPromptText.trim() : null;
      const next = await updateSubagentSystemPrompt(workspaceId, { system_prompt_append: payload });
      setSubagentPromptConfig(next);
      const nextText = next.configured_append ?? next.default_append ?? SUBAGENT_PROMPT_DEFAULT;
      if (subagentPromptText.trim() !== nextText.trim()) {
        setSubagentPromptText(nextText);
      }
      return true;
    } catch (error) {
      setSubagentPromptError(messageFromError(error));
      return false;
    } finally {
      setSubagentPromptSaving(false);
    }
  }, [subagentPromptSaving, subagentPromptText, workspaceId]);

  const flushAgentAutosave = useCallback(async () => {
    if (promptAutosaveDebounceRef.current) {
      window.clearTimeout(promptAutosaveDebounceRef.current);
      promptAutosaveDebounceRef.current = null;
    }
    if (promptAutosaveResetRef.current) {
      window.clearTimeout(promptAutosaveResetRef.current);
      promptAutosaveResetRef.current = null;
    }
    if (!workspaceId || !agentPromptDirty) return;
    setAgentPromptAutosaveState("saving");
    const ok = await saveAgentPrompt();
    if (!ok) {
      setAgentPromptAutosaveState("error");
      return;
    }
    setAgentPromptAutosaveState("saved");
    promptAutosaveResetRef.current = window.setTimeout(() => {
      setAgentPromptAutosaveState("idle");
      promptAutosaveResetRef.current = null;
    }, 1200);
  }, [agentPromptDirty, saveAgentPrompt, workspaceId]);

  const flushSubagentAutosave = useCallback(async () => {
    if (subagentPromptAutosaveDebounceRef.current) {
      window.clearTimeout(subagentPromptAutosaveDebounceRef.current);
      subagentPromptAutosaveDebounceRef.current = null;
    }
    if (subagentPromptAutosaveResetRef.current) {
      window.clearTimeout(subagentPromptAutosaveResetRef.current);
      subagentPromptAutosaveResetRef.current = null;
    }
    if (!workspaceId || !subagentPromptDirty) return;
    setSubagentPromptAutosaveState("saving");
    const ok = await saveSubagentPrompt();
    if (!ok) {
      setSubagentPromptAutosaveState("error");
      return;
    }
    setSubagentPromptAutosaveState("saved");
    subagentPromptAutosaveResetRef.current = window.setTimeout(() => {
      setSubagentPromptAutosaveState("idle");
      subagentPromptAutosaveResetRef.current = null;
    }, 1200);
  }, [saveSubagentPrompt, subagentPromptDirty, workspaceId]);

  useEffect(() => {
    if (!enabled || !workspaceId) return;
    refreshAgentPrompt().catch(() => {});
    refreshSubagentPrompt().catch(() => {});
  }, [enabled, refreshAgentPrompt, refreshSubagentPrompt, workspaceId]);

  useEffect(() => {
    if (!enabled || !workspaceId || !agentPromptConfig) return;
    if (!agentPromptDirty || agentPromptSaving) return;
    if (promptAutosaveResetRef.current) {
      window.clearTimeout(promptAutosaveResetRef.current);
      promptAutosaveResetRef.current = null;
    }
    setAgentPromptAutosaveState("pending");
    if (promptAutosaveDebounceRef.current) {
      window.clearTimeout(promptAutosaveDebounceRef.current);
    }
    promptAutosaveDebounceRef.current = window.setTimeout(() => {
      promptAutosaveDebounceRef.current = null;
      flushAgentAutosave().catch(() => {});
    }, 1000);
  }, [
    agentPromptConfig,
    agentPromptDirty,
    agentPromptSaving,
    agentPromptText,
    enabled,
    flushAgentAutosave,
    workspaceId,
  ]);

  useEffect(() => {
    if (!enabled) return;
    if (agentPromptDirty) return;
    if (promptAutosaveDebounceRef.current) {
      window.clearTimeout(promptAutosaveDebounceRef.current);
      promptAutosaveDebounceRef.current = null;
    }
    if (agentPromptAutosaveState === "pending") {
      setAgentPromptAutosaveState("idle");
    }
  }, [agentPromptAutosaveState, agentPromptDirty, enabled]);

  useEffect(() => {
    if (!enabled || !workspaceId || !subagentPromptConfig) return;
    if (!subagentPromptDirty || subagentPromptSaving) return;
    if (subagentPromptAutosaveResetRef.current) {
      window.clearTimeout(subagentPromptAutosaveResetRef.current);
      subagentPromptAutosaveResetRef.current = null;
    }
    setSubagentPromptAutosaveState("pending");
    if (subagentPromptAutosaveDebounceRef.current) {
      window.clearTimeout(subagentPromptAutosaveDebounceRef.current);
    }
    subagentPromptAutosaveDebounceRef.current = window.setTimeout(() => {
      subagentPromptAutosaveDebounceRef.current = null;
      flushSubagentAutosave().catch(() => {});
    }, 1000);
  }, [
    enabled,
    flushSubagentAutosave,
    subagentPromptConfig,
    subagentPromptDirty,
    subagentPromptSaving,
    subagentPromptText,
    workspaceId,
  ]);

  useEffect(() => {
    if (!enabled) return;
    if (subagentPromptDirty) return;
    if (subagentPromptAutosaveDebounceRef.current) {
      window.clearTimeout(subagentPromptAutosaveDebounceRef.current);
      subagentPromptAutosaveDebounceRef.current = null;
    }
    if (subagentPromptAutosaveState === "pending") {
      setSubagentPromptAutosaveState("idle");
    }
  }, [enabled, subagentPromptAutosaveState, subagentPromptDirty]);

  useEffect(() => {
    if (!enabled) return;
    const handleBeforeUnload = () => {
      if (agentPromptDirty) {
        saveAgentPrompt().catch(() => {});
      }
      if (subagentPromptDirty) {
        saveSubagentPrompt().catch(() => {});
      }
    };
    window.addEventListener("beforeunload", handleBeforeUnload);
    return () => window.removeEventListener("beforeunload", handleBeforeUnload);
  }, [enabled, agentPromptDirty, saveAgentPrompt, subagentPromptDirty, saveSubagentPrompt]);

  useEffect(() => {
    return () => {
      if (promptAutosaveDebounceRef.current) {
        window.clearTimeout(promptAutosaveDebounceRef.current);
      }
      if (promptAutosaveResetRef.current) {
        window.clearTimeout(promptAutosaveResetRef.current);
      }
      if (subagentPromptAutosaveDebounceRef.current) {
        window.clearTimeout(subagentPromptAutosaveDebounceRef.current);
      }
      if (subagentPromptAutosaveResetRef.current) {
        window.clearTimeout(subagentPromptAutosaveResetRef.current);
      }
    };
  }, []);

  return {
    agentPromptLoading,
    agentPromptSaving,
    agentPromptError,
    agentPromptText,
    setAgentPromptText,
    agentPromptAutosaveState,
    subagentPromptLoading,
    subagentPromptSaving,
    subagentPromptError,
    subagentPromptText,
    setSubagentPromptText,
    subagentPromptAutosaveState,
  };
}
