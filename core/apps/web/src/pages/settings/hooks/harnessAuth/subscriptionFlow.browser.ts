import type { Dispatch, SetStateAction } from "react";
import { getCodexLogin } from "../../../../api/client";
import {
  trackProviderAuthCompleted,
  trackProviderAuthFailed,
} from "../../../../utils/analytics";
import { openExternalLink } from "../../../../utils/desktop";
import type { HarnessAuthModalState } from "../../../SettingsPage.types";
import { delayWithAbort, isCancelledOperationError } from "./operationOwner";
import {
  shouldOpenPolledAuthUrlForStatus,
  takeNextAuthUrlToOpen,
} from "./capabilities";
import type { HarnessAuthModalOperation } from "./useHarnessAuthModalController";

export type RefreshAccountsOptions = {
  silent?: boolean;
};

type ReservedBrowserWindow = Window | null;

type BrowserLoginOutcome = {
  status: "success" | "failed" | "timeout";
  error?: string | null;
};

export type BrowserLoginStatus = {
  status: string;
  auth_url?: string | null;
  device_code?: string | null;
  error?: string | null;
};

const BROWSER_OPEN_FAILURE_MESSAGE = "Failed to launch the sign-in browser window.";

export type BrowserLoginDefinition = {
  providerId: "claude-crp" | "gemini" | "qwen" | "cursor" | "kimi" | "amp" | "mistral";
  waitingMessage: string;
  timeoutMessage: string;
  startLogin: (
    label?: string,
  ) => Promise<{ login_id: string; auth_url?: string | null; device_code?: string | null }>;
  getLogin: (loginId: string) => Promise<BrowserLoginStatus>;
  maxAttempts: number;
  pollIntervalMs: number;
  refreshAccounts?: (opts?: RefreshAccountsOptions) => Promise<unknown>;
  shouldAutoOpenAuthUrl?: (params: {
    authUrl: string;
    phase: "initial" | "poll";
    status?: BrowserLoginStatus;
  }) => boolean;
  syncBrowserLoginState?: (state: {
    authUrl: string | null;
    deviceCode: string | null;
  }) => void;
};

export type BrowserSubscriptionFlowDeps = {
  modal: HarnessAuthModalState;
  flow: HarnessAuthModalOperation;
  patchHarnessAuthModalForOperation: (
    operation: HarnessAuthModalOperation,
    patch: Partial<HarnessAuthModalState>,
  ) => boolean;
  markAwaitingBrowserForOperation: (
    operation: HarnessAuthModalOperation,
    status: string,
    patch?: Partial<HarnessAuthModalState>,
  ) => boolean;
  markFinalizingForOperation: (
    operation: HarnessAuthModalOperation,
    status?: string,
  ) => boolean;
  failSubscriptionFlowForOperation: (
    operation: HarnessAuthModalOperation,
    status: string,
  ) => boolean;
  closeHarnessAuthModalForOperation: (operation: HarnessAuthModalOperation) => boolean;
  setProviderError: Dispatch<SetStateAction<string | null>>;
  refreshBootstrapAfterMutation: (providerId?: string) => Promise<void>;
  selectSubscriptionSourceIfSupported: (providerId: string) => Promise<void>;
};

const finalizeSuccessfulSubscription = async (
  deps: Pick<
    BrowserSubscriptionFlowDeps,
    | "closeHarnessAuthModalForOperation"
    | "flow"
    | "markFinalizingForOperation"
    | "selectSubscriptionSourceIfSupported"
  >,
  providerId: string,
): Promise<void> => {
  if (!deps.flow.isCurrent()) return;
  deps.markFinalizingForOperation(deps.flow);
  await deps.selectSubscriptionSourceIfSupported(providerId);
  deps.closeHarnessAuthModalForOperation(deps.flow);
};

const refreshAccountsAfterFlow = async <TResponse>(
  flow: HarnessAuthModalOperation,
  refresh: ((opts?: RefreshAccountsOptions) => Promise<TResponse | null>) | undefined,
): Promise<TResponse | null> => {
  if (!refresh) return null;
  return refresh({ silent: !flow.isCurrent() });
};

const normalizeOptionalString = (value: string | null | undefined): string | null => {
  const normalized = value?.trim() ?? "";
  return normalized.length > 0 ? normalized : null;
};

const reserveBrowserWindowForFlow = (): ReservedBrowserWindow => {
  if (typeof window === "undefined") return null;
  if (typeof navigator !== "undefined" && /\bjsdom\b/i.test(navigator.userAgent)) {
    return null;
  }
  try {
    const popup = window.open("about:blank", "_blank");
    if (!popup) return null;
    try {
      popup.opener = null;
    } catch {
      // ignore browsers that forbid touching opener
    }
    return popup;
  } catch {
    return null;
  }
};

const navigateReservedBrowserWindow = (
  reservedWindow: ReservedBrowserWindow,
  authUrl: string,
): boolean => {
  if (!reservedWindow || reservedWindow.closed) return false;
  try {
    reservedWindow.location.href = authUrl;
    reservedWindow.focus?.();
    return true;
  } catch {
    return false;
  }
};

const closeReservedBrowserWindow = (reservedWindow: ReservedBrowserWindow): void => {
  if (!reservedWindow || reservedWindow.closed) return;
  try {
    reservedWindow.close();
  } catch {
    // ignore
  }
};

export const waitForCodexLoginOutcome = async (
  accountId: string,
  flow: HarnessAuthModalOperation,
): Promise<"success" | "failed" | "timeout"> => {
  const attempts = 75;
  for (let attempt = 0; attempt < attempts; attempt += 1) {
    flow.throwIfCancelled();
    try {
      const status = await getCodexLogin(accountId);
      flow.throwIfCancelled();
      if (status.status === "success") return "success";
      if (status.status === "failed") return "failed";
    } catch (error) {
      if (isCancelledOperationError(error)) throw error;
    }
    await delayWithAbort(1600, flow.signal);
  }
  return "timeout";
};

const waitForBrowserLoginOutcome = async (params: {
  flow: HarnessAuthModalOperation;
  loginId: string;
  getStatus: (loginId: string) => Promise<BrowserLoginStatus>;
  onAuthUrl?: (authUrl: string, status: BrowserLoginStatus) => Promise<void>;
  onStatus?: (status: BrowserLoginStatus) => void;
  openedAuthUrl?: string | null;
  maxAttempts: number;
  intervalMs: number;
}): Promise<BrowserLoginOutcome> => {
  const openedAuthUrls = new Set<string>();
  takeNextAuthUrlToOpen(params.openedAuthUrl, openedAuthUrls);
  for (let attempt = 0; attempt < params.maxAttempts; attempt += 1) {
    params.flow.throwIfCancelled();
    try {
      const status = await params.getStatus(params.loginId);
      params.flow.throwIfCancelled();
      params.onStatus?.(status);
      if (status.status === "success") return { status: "success" };
      if (status.status === "failed") return { status: "failed", error: status.error };
      if (status.status === "timeout") return { status: "timeout", error: status.error };
      if (shouldOpenPolledAuthUrlForStatus(status.status)) {
        const authUrl = takeNextAuthUrlToOpen(status.auth_url, openedAuthUrls);
        if (authUrl && params.onAuthUrl) {
          await params.onAuthUrl(authUrl, status);
          params.flow.throwIfCancelled();
        }
      }
    } catch (error) {
      if (isCancelledOperationError(error)) throw error;
    }
    await delayWithAbort(params.intervalMs, params.flow.signal);
  }
  return { status: "timeout" };
};

const openExternalAuthUrlForFlow = async (
  deps: Pick<BrowserSubscriptionFlowDeps, "flow" | "patchHarnessAuthModalForOperation">,
  authUrl: string,
  reservedWindow: ReservedBrowserWindow = null,
): Promise<boolean> => {
  if (navigateReservedBrowserWindow(reservedWindow, authUrl)) {
    return true;
  }
  const opened = await openExternalLink(authUrl);
  if (opened) return true;
  deps.patchHarnessAuthModalForOperation(deps.flow, {
    subscription_auth_url: authUrl,
  });
  throw new Error(BROWSER_OPEN_FAILURE_MESSAGE);
};

export const runBrowserSubscriptionFlow = async (
  deps: BrowserSubscriptionFlowDeps,
  definition: BrowserLoginDefinition,
): Promise<void> => {
  definition.syncBrowserLoginState?.({ authUrl: null, deviceCode: null });
  const reservedWindow =
    definition.providerId === "amp" || definition.providerId === "claude-crp"
      ? null
      : reserveBrowserWindowForFlow();
  const label = deps.modal.subscription_label.trim();
  let reservedWindowUsed = false;
  try {
    const login = await definition.startLogin(label ? label : undefined);
    deps.flow.throwIfCancelled();

    const initialAuthUrl = takeNextAuthUrlToOpen(login.auth_url, new Set<string>());
    definition.syncBrowserLoginState?.({
      authUrl: normalizeOptionalString(login.auth_url),
      deviceCode: normalizeOptionalString(login.device_code),
    });
    let initialAuthOpened = false;
    const shouldAutoOpenInitialAuthUrl = initialAuthUrl
      ? (definition.shouldAutoOpenAuthUrl?.({
        authUrl: initialAuthUrl,
        phase: "initial",
      }) ?? true)
      : false;
    if (initialAuthUrl && shouldAutoOpenInitialAuthUrl) {
      initialAuthOpened = await openExternalAuthUrlForFlow(deps, initialAuthUrl, reservedWindow);
      reservedWindowUsed = initialAuthOpened;
      deps.flow.throwIfCancelled();
    }

    deps.markAwaitingBrowserForOperation(deps.flow, definition.waitingMessage);

    const outcome = await waitForBrowserLoginOutcome({
      flow: deps.flow,
      loginId: login.login_id,
      getStatus: definition.getLogin,
      onAuthUrl: async (authUrl, status) => {
        if (!(definition.shouldAutoOpenAuthUrl?.({
          authUrl,
          phase: "poll",
          status,
        }) ?? true)) {
          return;
        }
        const opened = await openExternalAuthUrlForFlow(deps, authUrl, reservedWindow);
        if (opened) {
          reservedWindowUsed = true;
        }
      },
      onStatus: (status) => {
        definition.syncBrowserLoginState?.({
          authUrl: normalizeOptionalString(status.auth_url),
          deviceCode: normalizeOptionalString(status.device_code),
        });
      },
      openedAuthUrl: initialAuthOpened ? initialAuthUrl : null,
      maxAttempts: definition.maxAttempts,
      intervalMs: definition.pollIntervalMs,
    });

    if (outcome.status === "success") {
      deps.markFinalizingForOperation(deps.flow);
      await refreshAccountsAfterFlow(deps.flow, definition.refreshAccounts);
      if (!deps.flow.isCurrent()) return;
      await deps.refreshBootstrapAfterMutation(definition.providerId);
      if (!deps.flow.isCurrent()) return;
      await finalizeSuccessfulSubscription(deps, definition.providerId);
      trackProviderAuthCompleted({
        providerId: definition.providerId,
        authMethod: "subscription_browser",
      });
      return;
    }

    await refreshAccountsAfterFlow(deps.flow, definition.refreshAccounts);

    if (!deps.flow.isCurrent()) return;

    if (outcome.error && outcome.error.trim()) {
      deps.setProviderError(outcome.error);
    }

    if (outcome.status === "failed") {
      trackProviderAuthFailed({
        providerId: definition.providerId,
        authMethod: "subscription_browser",
        failureKind: "provider_failed",
      });
      deps.failSubscriptionFlowForOperation(deps.flow, outcome.error?.trim() || "Sign-in failed. Retry.");
      return;
    }

    if (outcome.status === "timeout") {
      trackProviderAuthFailed({
        providerId: definition.providerId,
        authMethod: "subscription_browser",
        failureKind: "timeout",
      });
      deps.failSubscriptionFlowForOperation(deps.flow, outcome.error?.trim() || definition.timeoutMessage);
      return;
    }

    trackProviderAuthFailed({
      providerId: definition.providerId,
      authMethod: "subscription_browser",
      failureKind: "unknown",
    });
    deps.failSubscriptionFlowForOperation(
      deps.flow,
      "Still waiting for completion. Keep this dialog open or retry.",
    );
  } catch (error) {
    trackProviderAuthFailed({
      providerId: definition.providerId,
      authMethod: "subscription_browser",
      failureKind: isCancelledOperationError(error)
        ? "user_cancelled"
        : error instanceof Error && error.message === BROWSER_OPEN_FAILURE_MESSAGE
          ? "browser_open_failed"
          : "request_failed",
    });
    throw error;
  } finally {
    if (!reservedWindowUsed) {
      closeReservedBrowserWindow(reservedWindow);
    }
  }
};
