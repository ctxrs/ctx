import { useCallback, type Dispatch, type SetStateAction } from "react";
import {
  listAmpAccounts,
  listClaudeAccounts,
  listCodexAccounts,
  listCopilotAccounts,
  listCursorAccounts,
  listGeminiAccounts,
  listKimiAccounts,
  listMistralAccounts,
  listQwenAccounts,
  type AmpAccountsResponse,
  type ClaudeAccountsResponse,
  type CodexAccountsResponse,
  type CopilotAccountsResponse,
  type CursorAccountsResponse,
  type GeminiAccountsResponse,
  type KimiAccountsResponse,
  type MistralAccountsResponse,
  type QwenAccountsResponse,
} from "../../../../api/client";
import {
  EMPTY_PROVIDERS_BOOTSTRAP,
  updateHostProvidersBootstrap,
  updateProvidersBootstrap,
} from "../../../../state/providersBootstrapStore";
import { messageFromError } from "./capabilities";

type RefreshOptions = {
  silent?: boolean;
};

type StateSetter<T> = Dispatch<SetStateAction<T>>;

type UseHarnessAuthAccountCollectionsArgs = {
  workspaceId: string | null;
  setProviderError: StateSetter<string | null>;
  setCodexAccountsBusy: StateSetter<boolean>;
  setClaudeAccountsBusy: StateSetter<boolean>;
  setGeminiAccountsBusy: StateSetter<boolean>;
  setQwenAccountsBusy: StateSetter<boolean>;
  setKimiAccountsBusy: StateSetter<boolean>;
  setMistralAccountsBusy: StateSetter<boolean>;
  setCopilotAccountsBusy: StateSetter<boolean>;
  setCursorAccountsBusy: StateSetter<boolean>;
  setAmpAccountsBusy: StateSetter<boolean>;
  claudeAccounts: ClaudeAccountsResponse | null;
  kimiAccounts: KimiAccountsResponse | null;
  copilotAccounts: CopilotAccountsResponse | null;
};

const resolveNextNullableState = <T,>(
  update: SetStateAction<T | null>,
  current: T | null,
): T | null =>
  typeof update === "function"
    ? (update as (previous: T | null) => T | null)(current)
    : update;

const refreshAccountCollection = async <TResponse>(params: {
  silent?: boolean;
  list: () => Promise<TResponse>;
  applyData: (next: TResponse) => void;
  setBusy: StateSetter<boolean>;
  setProviderError: StateSetter<string | null>;
}): Promise<TResponse | null> => {
  if (!params.silent) {
    params.setBusy(true);
  }
  try {
    const next = await params.list();
    params.applyData(next);
    return next;
  } catch (error) {
    params.setProviderError(messageFromError(error));
    return null;
  } finally {
    if (!params.silent) {
      params.setBusy(false);
    }
  }
};

export function useHarnessAuthAccountCollections({
  workspaceId,
  setProviderError,
  setCodexAccountsBusy,
  setClaudeAccountsBusy,
  setGeminiAccountsBusy,
  setQwenAccountsBusy,
  setKimiAccountsBusy,
  setMistralAccountsBusy,
  setCopilotAccountsBusy,
  setCursorAccountsBusy,
  setAmpAccountsBusy,
  claudeAccounts,
  kimiAccounts,
  copilotAccounts,
}: UseHarnessAuthAccountCollectionsArgs) {
  const applyCodexAccounts = useCallback((next: CodexAccountsResponse) => {
    if (workspaceId) {
      updateProvidersBootstrap(workspaceId, (current) => ({ ...current, codex_accounts: next }));
      return;
    }
    updateHostProvidersBootstrap((current) => ({ ...current, codex_accounts: next }));
  }, [workspaceId]);

  const applyClaudeAccounts = useCallback((next: ClaudeAccountsResponse) => {
    if (workspaceId) {
      updateProvidersBootstrap(workspaceId, (current) => ({ ...current, claude_accounts: next }));
      return;
    }
    updateHostProvidersBootstrap((current) => ({ ...current, claude_accounts: next }));
  }, [workspaceId]);

  const applyGeminiAccounts = useCallback((next: GeminiAccountsResponse) => {
    if (workspaceId) {
      updateProvidersBootstrap(workspaceId, (current) => ({ ...current, gemini_accounts: next }));
      return;
    }
    updateHostProvidersBootstrap((current) => ({ ...current, gemini_accounts: next }));
  }, [workspaceId]);

  const applyQwenAccounts = useCallback((next: QwenAccountsResponse) => {
    if (workspaceId) {
      updateProvidersBootstrap(workspaceId, (current) => ({ ...current, qwen_accounts: next }));
      return;
    }
    updateHostProvidersBootstrap((current) => ({ ...current, qwen_accounts: next }));
  }, [workspaceId]);

  const applyKimiAccounts = useCallback((next: KimiAccountsResponse) => {
    if (workspaceId) {
      updateProvidersBootstrap(workspaceId, (current) => ({ ...current, kimi_accounts: next }));
      return;
    }
    updateHostProvidersBootstrap((current) => ({ ...current, kimi_accounts: next }));
  }, [workspaceId]);

  const applyMistralAccounts = useCallback((next: MistralAccountsResponse) => {
    if (workspaceId) {
      updateProvidersBootstrap(workspaceId, (current) => ({ ...current, mistral_accounts: next }));
      return;
    }
    updateHostProvidersBootstrap((current) => ({ ...current, mistral_accounts: next }));
  }, [workspaceId]);

  const applyCopilotAccounts = useCallback((next: CopilotAccountsResponse) => {
    if (workspaceId) {
      updateProvidersBootstrap(workspaceId, (current) => ({ ...current, copilot_accounts: next }));
      return;
    }
    updateHostProvidersBootstrap((current) => ({ ...current, copilot_accounts: next }));
  }, [workspaceId]);

  const applyCursorAccounts = useCallback((next: CursorAccountsResponse) => {
    if (workspaceId) {
      updateProvidersBootstrap(workspaceId, (current) => ({ ...current, cursor_accounts: next }));
      return;
    }
    updateHostProvidersBootstrap((current) => ({ ...current, cursor_accounts: next }));
  }, [workspaceId]);

  const applyAmpAccounts = useCallback((next: AmpAccountsResponse) => {
    if (workspaceId) {
      updateProvidersBootstrap(workspaceId, (current) => ({ ...current, amp_accounts: next }));
      return;
    }
    updateHostProvidersBootstrap((current) => ({ ...current, amp_accounts: next }));
  }, [workspaceId]);

  const setScopedClaudeAccounts = useCallback((update: SetStateAction<ClaudeAccountsResponse | null>) => {
    const next = resolveNextNullableState(update, claudeAccounts);
    if (next) {
      applyClaudeAccounts(next);
      return;
    }
    if (!workspaceId) {
      updateHostProvidersBootstrap((current) => ({
        ...current,
        claude_accounts: EMPTY_PROVIDERS_BOOTSTRAP.claude_accounts,
      }));
    }
  }, [applyClaudeAccounts, claudeAccounts, workspaceId]);

  const setScopedKimiAccounts = useCallback((update: SetStateAction<KimiAccountsResponse | null>) => {
    const next = resolveNextNullableState(update, kimiAccounts);
    if (next) {
      applyKimiAccounts(next);
      return;
    }
    if (!workspaceId) {
      updateHostProvidersBootstrap((current) => ({
        ...current,
        kimi_accounts: EMPTY_PROVIDERS_BOOTSTRAP.kimi_accounts,
      }));
    }
  }, [applyKimiAccounts, kimiAccounts, workspaceId]);

  const setScopedCopilotAccounts = useCallback((update: SetStateAction<CopilotAccountsResponse | null>) => {
    const next = resolveNextNullableState(update, copilotAccounts);
    if (next) {
      applyCopilotAccounts(next);
      return;
    }
    if (!workspaceId) {
      updateHostProvidersBootstrap((current) => ({
        ...current,
        copilot_accounts: EMPTY_PROVIDERS_BOOTSTRAP.copilot_accounts,
      }));
    }
  }, [applyCopilotAccounts, copilotAccounts, workspaceId]);

  const refreshCodexAccounts = useCallback(
    async (opts?: RefreshOptions) =>
      refreshAccountCollection({
        silent: opts?.silent,
        list: listCodexAccounts,
        applyData: applyCodexAccounts,
        setBusy: setCodexAccountsBusy,
        setProviderError,
      }),
    [applyCodexAccounts, setCodexAccountsBusy, setProviderError],
  );

  const refreshClaudeAccounts = useCallback(
    async (opts?: RefreshOptions) =>
      refreshAccountCollection({
        silent: opts?.silent,
        list: listClaudeAccounts,
        applyData: applyClaudeAccounts,
        setBusy: setClaudeAccountsBusy,
        setProviderError,
      }),
    [applyClaudeAccounts, setClaudeAccountsBusy, setProviderError],
  );

  const refreshGeminiAccounts = useCallback(
    async (opts?: RefreshOptions) =>
      refreshAccountCollection({
        silent: opts?.silent,
        list: listGeminiAccounts,
        applyData: applyGeminiAccounts,
        setBusy: setGeminiAccountsBusy,
        setProviderError,
      }),
    [applyGeminiAccounts, setGeminiAccountsBusy, setProviderError],
  );

  const refreshQwenAccounts = useCallback(
    async (opts?: RefreshOptions) =>
      refreshAccountCollection({
        silent: opts?.silent,
        list: listQwenAccounts,
        applyData: applyQwenAccounts,
        setBusy: setQwenAccountsBusy,
        setProviderError,
      }),
    [applyQwenAccounts, setProviderError, setQwenAccountsBusy],
  );

  const refreshKimiAccounts = useCallback(
    async (opts?: RefreshOptions) =>
      refreshAccountCollection({
        silent: opts?.silent,
        list: listKimiAccounts,
        applyData: applyKimiAccounts,
        setBusy: setKimiAccountsBusy,
        setProviderError,
      }),
    [applyKimiAccounts, setKimiAccountsBusy, setProviderError],
  );

  const refreshMistralAccounts = useCallback(
    async (opts?: RefreshOptions) =>
      refreshAccountCollection({
        silent: opts?.silent,
        list: listMistralAccounts,
        applyData: applyMistralAccounts,
        setBusy: setMistralAccountsBusy,
        setProviderError,
      }),
    [applyMistralAccounts, setMistralAccountsBusy, setProviderError],
  );

  const refreshCopilotAccounts = useCallback(
    async (opts?: RefreshOptions) =>
      refreshAccountCollection({
        silent: opts?.silent,
        list: listCopilotAccounts,
        applyData: applyCopilotAccounts,
        setBusy: setCopilotAccountsBusy,
        setProviderError,
      }),
    [applyCopilotAccounts, setCopilotAccountsBusy, setProviderError],
  );

  const refreshCursorAccounts = useCallback(
    async (opts?: RefreshOptions) =>
      refreshAccountCollection({
        silent: opts?.silent,
        list: listCursorAccounts,
        applyData: applyCursorAccounts,
        setBusy: setCursorAccountsBusy,
        setProviderError,
      }),
    [applyCursorAccounts, setCursorAccountsBusy, setProviderError],
  );

  const refreshAmpAccounts = useCallback(
    async (opts?: RefreshOptions) =>
      refreshAccountCollection({
        silent: opts?.silent,
        list: listAmpAccounts,
        applyData: applyAmpAccounts,
        setBusy: setAmpAccountsBusy,
        setProviderError,
      }),
    [applyAmpAccounts, setAmpAccountsBusy, setProviderError],
  );

  return {
    applyCursorAccounts,
    refreshCodexAccounts,
    refreshClaudeAccounts,
    refreshGeminiAccounts,
    refreshQwenAccounts,
    refreshKimiAccounts,
    refreshMistralAccounts,
    refreshCopilotAccounts,
    refreshCursorAccounts,
    refreshAmpAccounts,
    setScopedClaudeAccounts,
    setScopedKimiAccounts,
    setScopedCopilotAccounts,
  };
}
