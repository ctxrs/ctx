import type { Dispatch, SetStateAction } from "react";
import {
  authenticateProviderForWorkspace,
  getAmpLogin,
  getClaudeLogin,
  getCursorLogin,
  getGeminiLogin,
  getKimiLogin,
  getMistralLogin,
  getQwenLogin,
  startAmpLogin,
  startClaudeLogin,
  startCodexLogin,
  startCursorLogin,
  startGeminiLogin,
  startKimiLogin,
  startMistralLogin,
  startQwenLogin,
  upsertClaudeAccount,
  upsertCopilotAccount,
  type ClaudeAccountsResponse,
  type CodexAccountsResponse,
  type CopilotAccountsResponse,
  type GeminiAccountsResponse,
  type MistralAccountsResponse,
  type QwenAccountsResponse,
} from "../../../../api/client";
import type { HarnessAuthModalState } from "../../../SettingsPage.types";
import {
  trackProviderAuthCompleted,
  trackProviderAuthFailed,
  trackProviderAuthStarted,
  type ProviderAuthMethod,
} from "../../../../utils/analytics";
import { isCancelledOperationError } from "./operationOwner";
import {
  AMP_LOGIN_POLL_ATTEMPTS,
  AMP_LOGIN_POLL_INTERVAL_MS,
  CLAUDE_LOGIN_POLL_ATTEMPTS,
  CLAUDE_LOGIN_POLL_INTERVAL_MS,
  GEMINI_LOGIN_POLL_ATTEMPTS,
  GEMINI_LOGIN_POLL_INTERVAL_MS,
  MISTRAL_LOGIN_POLL_ATTEMPTS,
  MISTRAL_LOGIN_POLL_INTERVAL_MS,
  QWEN_LOGIN_POLL_ATTEMPTS,
  QWEN_LOGIN_POLL_INTERVAL_MS,
  messageFromError,
  shouldAutoOpenAmpAuthUrl,
  shouldAutoOpenKimiAuthUrl,
} from "./capabilities";
import {
  runBrowserSubscriptionFlow,
  waitForCodexLoginOutcome,
  type BrowserSubscriptionFlowDeps,
} from "./subscriptionFlow.browser";
import type { HarnessAuthModalOperation } from "./useHarnessAuthModalController";

type RefreshAccountsOptions = {
  silent?: boolean;
};

type SubscriptionFlowDeps = BrowserSubscriptionFlowDeps & {
  modal: HarnessAuthModalState;
  workspaceId: string | null;
  flow: HarnessAuthModalOperation;
  refreshCodexAccounts: (opts?: RefreshAccountsOptions) => Promise<CodexAccountsResponse | null>;
  refreshClaudeAccounts: (opts?: RefreshAccountsOptions) => Promise<ClaudeAccountsResponse | null>;
  refreshGeminiAccounts: (opts?: RefreshAccountsOptions) => Promise<GeminiAccountsResponse | null>;
  refreshQwenAccounts: (opts?: RefreshAccountsOptions) => Promise<QwenAccountsResponse | null>;
  refreshKimiAccounts: (opts?: RefreshAccountsOptions) => Promise<unknown>;
  refreshCursorAccounts: (opts?: RefreshAccountsOptions) => Promise<unknown>;
  refreshAmpAccounts: (opts?: RefreshAccountsOptions) => Promise<unknown>;
  refreshMistralAccounts: (opts?: RefreshAccountsOptions) => Promise<MistralAccountsResponse | null>;
  setClaudeAccounts: Dispatch<SetStateAction<ClaudeAccountsResponse | null>>;
  setCopilotAccounts: Dispatch<SetStateAction<CopilotAccountsResponse | null>>;
  openCodexAuthUrl: (
    url: string,
    params?: { accountId: string; expectedCallbackUrl: string | null; completionToken: string | null },
  ) => Promise<void>;
};

const setCurrentProviderError = (
  deps: Pick<SubscriptionFlowDeps, "flow" | "setProviderError">,
  message: string,
): void => {
  if (!deps.flow.isCurrent()) return;
  deps.setProviderError(message);
};

const authMethodForModal = (modal: HarnessAuthModalState): ProviderAuthMethod => {
  const providerId = modal.provider_id;
  const hasToken = modal.subscription_token.trim().length > 0;
  if (providerId === "claude-crp" && hasToken) return "subscription_token";
  if (providerId === "copilot") return "subscription_token";
  if (
    providerId === "codex"
    || providerId === "claude-crp"
    || providerId === "gemini"
    || providerId === "qwen"
    || providerId === "cursor"
    || providerId === "amp"
    || providerId === "mistral"
    || providerId === "kimi"
  ) {
    return "subscription_browser";
  }
  return "workspace_auth";
};

const refreshAccountsAfterFlow = async <TResponse>(
  flow: HarnessAuthModalOperation,
  refresh: ((opts?: RefreshAccountsOptions) => Promise<TResponse | null>) | undefined,
): Promise<TResponse | null> => {
  if (!refresh) return null;
  return refresh({ silent: !flow.isCurrent() });
};
const runCodexSubscriptionFlow = async (deps: SubscriptionFlowDeps): Promise<void> => {
  try {
    const login = await startCodexLogin();
    deps.flow.throwIfCancelled();

    await deps.openCodexAuthUrl(login.auth_url, {
      accountId: login.account_id,
      expectedCallbackUrl: login.expected_callback_url ?? null,
      completionToken: login.completion_token,
    });
    deps.flow.throwIfCancelled();

    deps.markAwaitingBrowserForOperation(
      deps.flow,
      "Waiting for browser sign-in to complete. You can close this dialog after finishing auth.",
    );

    const outcome = await waitForCodexLoginOutcome(login.account_id, deps.flow);

    if (outcome === "success") {
      deps.markFinalizingForOperation(deps.flow);
      await refreshAccountsAfterFlow(deps.flow, deps.refreshCodexAccounts);
      if (!deps.flow.isCurrent()) return;
      await deps.refreshBootstrapAfterMutation("codex");
      if (!deps.flow.isCurrent()) return;
      await deps.selectSubscriptionSourceIfSupported("codex");
      deps.closeHarnessAuthModalForOperation(deps.flow);
      trackProviderAuthCompleted({
        providerId: "codex",
        authMethod: "subscription_browser",
      });
      return;
    }

    await refreshAccountsAfterFlow(deps.flow, deps.refreshCodexAccounts);

    if (!deps.flow.isCurrent()) return;

    if (outcome === "failed") {
      trackProviderAuthFailed({
        providerId: "codex",
        authMethod: "subscription_browser",
        failureKind: "provider_failed",
      });
      deps.failSubscriptionFlowForOperation(
        deps.flow,
        "Sign-in failed. Please retry or use the callback completion flow.",
      );
      return;
    }

    trackProviderAuthFailed({
      providerId: "codex",
      authMethod: "subscription_browser",
      failureKind: "timeout",
    });
    deps.failSubscriptionFlowForOperation(
      deps.flow,
      "Still waiting for callback completion. Continue in Harness Subscriptions if needed.",
    );
  } catch (error) {
    trackProviderAuthFailed({
      providerId: "codex",
      authMethod: "subscription_browser",
      failureKind: isCancelledOperationError(error) ? "user_cancelled" : "request_failed",
    });
    throw error;
  }
};

const runClaudeSubscriptionFlow = async (deps: SubscriptionFlowDeps): Promise<void> => {
  const token = deps.modal.subscription_token.trim();
  const label = deps.modal.subscription_label.trim();

  if (token) {
    if (!token.startsWith("sk-ant-oat")) {
      throw new Error("Claude setup token must start with sk-ant-oat.");
    }
    const next = await upsertClaudeAccount(token, label ? label : undefined);
    await deps.refreshBootstrapAfterMutation("claude-crp");
    if (!deps.flow.isCurrent()) return;
    deps.setClaudeAccounts(next);
    deps.markFinalizingForOperation(deps.flow);
    await deps.selectSubscriptionSourceIfSupported("claude-crp");
    deps.closeHarnessAuthModalForOperation(deps.flow);
    trackProviderAuthCompleted({
      providerId: "claude-crp",
      authMethod: "subscription_token",
    });
    return;
  }

  await runBrowserSubscriptionFlow(deps, {
    providerId: "claude-crp",
    waitingMessage: "Waiting for Claude setup-token sign-in to complete in your browser...",
    timeoutMessage: "Timed out waiting for Claude setup-token completion. Retry.",
    startLogin: startClaudeLogin,
    getLogin: getClaudeLogin,
    maxAttempts: CLAUDE_LOGIN_POLL_ATTEMPTS,
    pollIntervalMs: CLAUDE_LOGIN_POLL_INTERVAL_MS,
    refreshAccounts: deps.refreshClaudeAccounts,
    // Claude browser ownership lives in the daemon-side `claude setup-token`
    // runner; the web client must not try to open this URL itself.
    shouldAutoOpenAuthUrl: () => false,
    syncBrowserLoginState: ({ authUrl }) => {
      deps.patchHarnessAuthModalForOperation(deps.flow, {
        subscription_auth_url: authUrl,
      });
    },
  });
};

const runCopilotSubscriptionFlow = async (deps: SubscriptionFlowDeps): Promise<void> => {
  const token = deps.modal.subscription_token.trim();
  if (!token) {
    throw new Error("Token is required.");
  }

  const label = deps.modal.subscription_label.trim();
  const email = deps.modal.subscription_email.trim();
  const next = await upsertCopilotAccount(token, {
    ...(label ? { label } : {}),
    ...(email ? { email } : {}),
  });

  await deps.refreshBootstrapAfterMutation("copilot");
  if (!deps.flow.isCurrent()) return;

  deps.setCopilotAccounts(next);
  deps.markFinalizingForOperation(deps.flow);
  await deps.selectSubscriptionSourceIfSupported("copilot");
  deps.closeHarnessAuthModalForOperation(deps.flow);
  trackProviderAuthCompleted({
    providerId: "copilot",
    authMethod: "subscription_token",
  });
};

const runWorkspaceSubscriptionFlow = async (deps: SubscriptionFlowDeps): Promise<void> => {
  if (!deps.workspaceId) {
    throw new Error("Select a workspace first.");
  }

  await authenticateProviderForWorkspace(deps.workspaceId, deps.modal.provider_id);
  await deps.refreshBootstrapAfterMutation(deps.modal.provider_id);

  if (!deps.flow.isCurrent()) return;
  deps.markFinalizingForOperation(deps.flow);
  await deps.selectSubscriptionSourceIfSupported(deps.modal.provider_id);
  deps.closeHarnessAuthModalForOperation(deps.flow);
  trackProviderAuthCompleted({
    providerId: deps.modal.provider_id,
    authMethod: "workspace_auth",
  });
};

export const runHarnessSubscriptionFlow = async (deps: SubscriptionFlowDeps): Promise<void> => {
  const authMethod = authMethodForModal(deps.modal);
  trackProviderAuthStarted({
    providerId: deps.modal.provider_id,
    authMethod,
  });
  try {
    switch (deps.modal.provider_id) {
      case "codex":
        await runCodexSubscriptionFlow(deps);
        return;
      case "claude-crp":
        await runClaudeSubscriptionFlow(deps);
        return;
      case "gemini":
        await runBrowserSubscriptionFlow(deps, {
          providerId: "gemini",
          waitingMessage: "Waiting for Google sign-in to complete in your browser...",
          timeoutMessage: "Timed out waiting for Gemini sign-in completion. Retry.",
          startLogin: startGeminiLogin,
          getLogin: getGeminiLogin,
          maxAttempts: GEMINI_LOGIN_POLL_ATTEMPTS,
          pollIntervalMs: GEMINI_LOGIN_POLL_INTERVAL_MS,
          refreshAccounts: deps.refreshGeminiAccounts,
        });
        return;
      case "qwen":
        await runBrowserSubscriptionFlow(deps, {
          providerId: "qwen",
          waitingMessage: "Waiting for Qwen sign-in to complete in your browser...",
          timeoutMessage: "Timed out waiting for Qwen sign-in completion. Retry.",
          startLogin: startQwenLogin,
          getLogin: getQwenLogin,
          maxAttempts: QWEN_LOGIN_POLL_ATTEMPTS,
          pollIntervalMs: QWEN_LOGIN_POLL_INTERVAL_MS,
          refreshAccounts: deps.refreshQwenAccounts,
        });
        return;
      case "cursor":
        await runBrowserSubscriptionFlow(deps, {
          providerId: "cursor",
          waitingMessage: "Waiting for Cursor sign-in to complete in your browser...",
          timeoutMessage: "Timed out waiting for Cursor sign-in completion. Retry.",
          startLogin: startCursorLogin,
          getLogin: getCursorLogin,
          maxAttempts: QWEN_LOGIN_POLL_ATTEMPTS,
          pollIntervalMs: QWEN_LOGIN_POLL_INTERVAL_MS,
          refreshAccounts: deps.refreshCursorAccounts,
        });
        return;
      case "amp":
        await runBrowserSubscriptionFlow(deps, {
          providerId: "amp",
          waitingMessage: "Waiting for Amp sign-in to complete in your browser...",
          timeoutMessage: "Timed out waiting for Amp sign-in completion. Retry.",
          startLogin: startAmpLogin,
          getLogin: getAmpLogin,
          maxAttempts: AMP_LOGIN_POLL_ATTEMPTS,
          pollIntervalMs: AMP_LOGIN_POLL_INTERVAL_MS,
          refreshAccounts: deps.refreshAmpAccounts,
          shouldAutoOpenAuthUrl: () => shouldAutoOpenAmpAuthUrl(),
          syncBrowserLoginState: ({ authUrl, deviceCode }) => {
            deps.patchHarnessAuthModalForOperation(deps.flow, {
              subscription_auth_url: authUrl,
              subscription_device_code: deviceCode,
            });
          },
        });
        return;
      case "mistral":
        await runBrowserSubscriptionFlow(deps, {
          providerId: "mistral",
          waitingMessage: "Waiting for Mistral sign-in to complete in your browser...",
          timeoutMessage: "Timed out waiting for Mistral sign-in completion. Retry.",
          startLogin: startMistralLogin,
          getLogin: getMistralLogin,
          maxAttempts: MISTRAL_LOGIN_POLL_ATTEMPTS,
          pollIntervalMs: MISTRAL_LOGIN_POLL_INTERVAL_MS,
          refreshAccounts: deps.refreshMistralAccounts,
        });
        return;
      case "kimi":
        await runBrowserSubscriptionFlow(deps, {
          providerId: "kimi",
          waitingMessage: "Waiting for Kimi sign-in to complete in your browser...",
          timeoutMessage: "Timed out waiting for Kimi sign-in completion. Retry.",
          startLogin: startKimiLogin,
          getLogin: getKimiLogin,
          maxAttempts: GEMINI_LOGIN_POLL_ATTEMPTS,
          pollIntervalMs: GEMINI_LOGIN_POLL_INTERVAL_MS,
          refreshAccounts: deps.refreshKimiAccounts,
          shouldAutoOpenAuthUrl: ({ authUrl }) => shouldAutoOpenKimiAuthUrl() && authUrl.length > 0,
          syncBrowserLoginState: ({ authUrl, deviceCode }) => {
            deps.patchHarnessAuthModalForOperation(deps.flow, {
              subscription_auth_url: authUrl,
              subscription_device_code: deviceCode,
            });
          },
        });
        return;
      case "copilot":
        await runCopilotSubscriptionFlow(deps);
        return;
      default:
        await runWorkspaceSubscriptionFlow(deps);
    }
  } catch (error) {
    if (authMethod !== "subscription_browser") {
      trackProviderAuthFailed({
        providerId: deps.modal.provider_id,
        authMethod,
        failureKind: isCancelledOperationError(error) ? "user_cancelled" : "request_failed",
      });
    }
    if (isCancelledOperationError(error)) {
      return;
    }
    const message = messageFromError(error);
    setCurrentProviderError(deps, message);
    deps.failSubscriptionFlowForOperation(deps.flow, "Subscription flow failed. Check error details below.");
  }
};
