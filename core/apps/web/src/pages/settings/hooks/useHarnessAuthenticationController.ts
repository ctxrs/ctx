import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type Dispatch,
  type SetStateAction,
} from "react";
import {
  type AmpAccountsResponse,
  type ClaudeAccountsResponse,
  type CodexAccountsResponse,
  type CodexLoginStatus,
  type CopilotAccountsResponse,
  type CursorAccountsResponse,
  type GeminiAccountsResponse,
  type HarnessProviderSourceConfig,
  type KimiAccountsResponse,
  type MistralAccountsResponse,
  type ProviderStatus,
  type QwenAccountsResponse,
} from "../../../api/client";
import { subscribeDaemonConnection } from "../../../api/daemonConnection";
import {
  invalidateHostProvidersBootstrap,
  invalidateProvidersBootstrap,
  updateHostProvidersBootstrap,
  updateProvidersBootstrap,
} from "../../../state/providersBootstrapStore";
import {
  useProviderOnboardingCoordinator,
  type ProviderOnboardingInstallState,
} from "../../../state/providerOnboardingCoordinator";
import {
  createMissingProviderOwnerScopeError,
  getProviderOwnerScopeKeyOrNull,
  getProviderOwnerScopeOrNull,
} from "../../../state/providerScopeAdapters";
import type { HarnessAuthModalState, InstallSession } from "../SettingsPage.types";
import type { HarnessAuthRow } from "../harnessAuthRows";
import {
  harnessEndpointRequiresBaseUrl,
  messageFromError,
  supportsHarnessEndpointConfigStatic,
  supportsHarnessSubscriptionAuth,
  toErrorObject,
} from "./harnessAuth/capabilities";
import { useHarnessAuthAccountCollections } from "./harnessAuth/useHarnessAuthAccountCollections";
import { useHarnessAuthenticationActions } from "./harnessAuth/useHarnessAuthenticationActions";
import { useHarnessAuthModalController } from "./harnessAuth/useHarnessAuthModalController";

export {
  extractGithubDeviceCodeFromAuthUrl,
  resolveHarnessAuthModalInitialStage,
  shouldAutoOpenCopilotAuthUrl,
  shouldAutoOpenKimiAuthUrl,
  shouldOpenPolledAuthUrlForStatus,
  shouldSkipDuplicateAmpLoginStart,
  supportsHarnessSubscriptionAuth,
  toErrorObject,
} from "./harnessAuth/capabilities";
export { resolveUpsertedEndpoint } from "../../../state/providerOnboardingActions";

type UseHarnessAuthenticationControllerArgs = {
  workspaceId: string | null;
  enabled: boolean;
};

export type HarnessAuthenticationController = {
  providers: ProviderStatus[];
  installs: Record<string, InstallSession>;
  installBusy: string | null;
  onInstallAll: () => Promise<void>;
  onInstall: (providerId: string) => Promise<void>;
  onCancelInstall: (providerId: string) => Promise<void>;
  providerHarnessConfig: Record<string, HarnessProviderSourceConfig | undefined>;
  providerHarnessBusy: Record<string, boolean>;
  codexAccounts: CodexAccountsResponse | null;
  codexAccountsBusy: boolean;
  claudeAccounts: ClaudeAccountsResponse | null;
  claudeAccountsBusy: boolean;
  geminiAccounts: GeminiAccountsResponse | null;
  geminiAccountsBusy: boolean;
  qwenAccounts: QwenAccountsResponse | null;
  qwenAccountsBusy: boolean;
  kimiAccounts: KimiAccountsResponse | null;
  kimiAccountsBusy: boolean;
  mistralAccounts: MistralAccountsResponse | null;
  mistralAccountsBusy: boolean;
  copilotAccounts: CopilotAccountsResponse | null;
  copilotAccountsBusy: boolean;
  cursorAccounts: CursorAccountsResponse | null;
  cursorAccountsBusy: boolean;
  ampAccounts: AmpAccountsResponse | null;
  ampAccountsBusy: boolean;
  harnessAuthModal: HarnessAuthModalState | null;
  openHarnessAuthModal: (providerId: string) => void;
  closeHarnessAuthModal: () => void;
  patchHarnessAuthModal: (patch: Partial<HarnessAuthModalState>) => void;
  submitHarnessSubscriptionModal: () => Promise<void>;
  submitHarnessApiKeyModal: () => Promise<void>;
  onSelectHarnessAuthRow: (providerId: string, row: HarnessAuthRow) => Promise<void>;
  onDeleteProviderEndpoint: (providerId: string, endpointId: string) => Promise<void>;
  onRefreshProviderEndpointModels: (providerId: string, endpointId: string) => Promise<void>;
  onCodexDelete: (accountId: string) => Promise<void>;
  onClaudeDelete: (accountId: string) => Promise<void>;
  onGeminiDelete: (accountId: string) => Promise<void>;
  onQwenDelete: (accountId: string) => Promise<void>;
  onKimiDelete: (accountId: string) => Promise<void>;
  onMistralDelete: (accountId: string) => Promise<void>;
  onCopilotDelete: (accountId: string) => Promise<void>;
  onCursorDelete: (accountId: string) => Promise<void>;
  onAmpDelete: (accountId: string) => Promise<void>;
  providerError: string | null;
  supportsHarnessEndpointConfig: (providerId: string) => boolean;
  supportsHarnessSubscriptionAuth: (providerId: string) => boolean;
  harnessEndpointRequiresBaseUrl: (providerId: string) => boolean;
};

type StateSetter<T> = Dispatch<SetStateAction<T>>;
type StoreChangeListener = () => void;

const toInstallSession = (
  session: ProviderOnboardingInstallState,
): InstallSession => ({
  installId: session.installId,
  state: session.state,
  pct: session.pct,
  target: session.target,
  errorCode: session.errorCode,
  streamError: undefined,
  error: session.error,
});

const toInstallSessionMap = (
  installsById: Record<string, ProviderOnboardingInstallState>,
): Record<string, InstallSession> =>
  Object.fromEntries(
    Object.entries(installsById).map(([providerId, install]) => [providerId, toInstallSession(install)]),
  );

const mutateProviderAccount = async (params: {
  mutate: () => Promise<void>;
  setBusy: StateSetter<boolean>;
  setProviderError: StateSetter<string | null>;
}): Promise<void> => {
  params.setBusy(true);
  params.setProviderError(null);
  try {
    await params.mutate();
  } catch (error) {
    params.setProviderError(messageFromError(error));
  } finally {
    params.setBusy(false);
  }
};

export function useHarnessAuthenticationController({
  workspaceId,
  enabled,
}: UseHarnessAuthenticationControllerArgs): HarnessAuthenticationController {
  const [providerError, setProviderError] = useState<string | null>(null);
  const [providerHarnessBusy, setProviderHarnessBusy] = useState<Record<string, boolean>>({});
  const [providerEndpointUnsupported, setProviderEndpointUnsupported] = useState<Record<string, boolean>>({});
  const {
    harnessAuthModal,
    openHarnessAuthModal: baseOpenHarnessAuthModal,
    closeHarnessAuthModal: baseCloseHarnessAuthModal,
    patchHarnessAuthModal: basePatchHarnessAuthModal,
    startOperation: startHarnessAuthModalOperation,
    finishOperation: finishHarnessAuthModalOperation,
    hasActiveOperation: hasActiveHarnessAuthModalOperation,
    patchHarnessAuthModalForOperation,
    markAwaitingBrowserForOperation,
    markFinalizingForOperation,
    failSubscriptionFlowForOperation,
    closeHarnessAuthModalForOperation,
  } = useHarnessAuthModalController();
  const [installBusy, setInstallBusy] = useState<string | null>(null);

  const [codexAccountsBusy, setCodexAccountsBusy] = useState(false);
  const [claudeAccountsBusy, setClaudeAccountsBusy] = useState(false);
  const [geminiAccountsBusy, setGeminiAccountsBusy] = useState(false);
  const [qwenAccountsBusy, setQwenAccountsBusy] = useState(false);
  const [kimiAccountsBusy, setKimiAccountsBusy] = useState(false);
  const [mistralAccountsBusy, setMistralAccountsBusy] = useState(false);
  const [copilotAccountsBusy, setCopilotAccountsBusy] = useState(false);
  const [cursorAccountsBusy, setCursorAccountsBusy] = useState(false);
  const [ampAccountsBusy, setAmpAccountsBusy] = useState(false);
  const ownerScopeKey = useSyncExternalStore(
    useCallback((listener: StoreChangeListener) => subscribeDaemonConnection(() => listener()), []),
    useCallback(() => getProviderOwnerScopeKeyOrNull(workspaceId), [workspaceId]),
    useCallback(() => getProviderOwnerScopeKeyOrNull(workspaceId), [workspaceId]),
  );
  const ownerScope = useMemo(
    () => getProviderOwnerScopeOrNull(workspaceId),
    [ownerScopeKey, workspaceId],
  );
  const requireOwnerScope = useCallback(() => {
    if (!ownerScope) {
      throw createMissingProviderOwnerScopeError();
    }
    return ownerScope;
  }, [ownerScope]);
  const handleOnboardingLoadError = useCallback((error: unknown) => {
    setProviderError(messageFromError(error));
  }, []);
  const onboarding = useProviderOnboardingCoordinator({
    workspaceId,
    enabled,
    onLoadError: workspaceId ? undefined : handleOnboardingLoadError,
  });
  const providers = onboarding.bootstrap.providers;
  const providerHarnessConfig = onboarding.bootstrap.provider_harness_config;
  const codexAccounts = onboarding.bootstrap.codex_accounts;
  const claudeAccounts = onboarding.bootstrap.claude_accounts;
  const geminiAccounts = onboarding.bootstrap.gemini_accounts;
  const qwenAccounts = onboarding.bootstrap.qwen_accounts;
  const kimiAccounts = onboarding.bootstrap.kimi_accounts;
  const mistralAccounts = onboarding.bootstrap.mistral_accounts;
  const copilotAccounts = onboarding.bootstrap.copilot_accounts;
  const cursorAccounts = onboarding.bootstrap.cursor_accounts;
  const ampAccounts = onboarding.bootstrap.amp_accounts;
  const installs = useMemo(
    () => toInstallSessionMap(onboarding.installsById),
    [onboarding.installsById],
  );
  const providerHarnessConfigRef = useRef<Record<string, HarnessProviderSourceConfig | undefined>>({});
  const providerEndpointUnsupportedRef = useRef<Record<string, boolean>>({});
  const previousCodexPendingRef = useRef(false);

  const supportsHarnessEndpointConfig = useCallback(
    (providerId: string): boolean =>
      supportsHarnessEndpointConfigStatic(providerId) && providerEndpointUnsupportedRef.current[providerId] !== true,
    [],
  );

  useEffect(() => {
    providerHarnessConfigRef.current = providerHarnessConfig;
  }, [providerHarnessConfig]);

  useEffect(() => {
    providerEndpointUnsupportedRef.current = providerEndpointUnsupported;
  }, [providerEndpointUnsupported]);

  const markProviderEndpointUnsupported = useCallback((providerId: string) => {
    setProviderEndpointUnsupported((prev) => {
      if (prev[providerId]) return prev;
      const next = { ...prev, [providerId]: true };
      providerEndpointUnsupportedRef.current = next;
      return next;
    });
  }, []);

  const setProviderHarnessConfigForProvider = useCallback(
    (providerId: string, nextConfig: HarnessProviderSourceConfig) => {
      if (workspaceId) {
        const next = updateProvidersBootstrap(workspaceId, (current) => ({
          ...current,
          provider_harness_config: {
            ...current.provider_harness_config,
            [providerId]: nextConfig,
          },
        }));
        providerHarnessConfigRef.current = next.provider_harness_config;
        return;
      }
      const next = updateHostProvidersBootstrap((current) => ({
        ...current,
        provider_harness_config: {
          ...current.provider_harness_config,
          [providerId]: nextConfig,
        },
      }));
      providerHarnessConfigRef.current = next.provider_harness_config;
    },
    [workspaceId],
  );

  const setSubscriptionSourceFallback = useCallback(
    (providerId: string) => {
      setProviderHarnessConfigForProvider(providerId, {
        provider_id: providerId,
        selected_source_kind: "subscription",
        selected_endpoint_id: null,
        endpoints: providerHarnessConfigRef.current[providerId]?.endpoints ?? [],
      });
    },
    [setProviderHarnessConfigForProvider],
  );

  const setProviderHarnessBusyForProvider = useCallback((providerId: string, busy: boolean) => {
    setProviderHarnessBusy((prev) => ({ ...prev, [providerId]: busy }));
  }, []);

  const refreshProvidersBootstrapState = useCallback(async (opts?: { force?: boolean; silent?: boolean }) => {
    if (!workspaceId) return null;
    try {
      return opts?.force
        ? await onboarding.refreshBootstrap()
        : await onboarding.loadBootstrap();
    } catch (error) {
      if (!opts?.silent) {
        setProviderError(messageFromError(error));
      }
      return null;
    }
  }, [onboarding, workspaceId]);

  const refreshProviderSlicesAfterMutation = useCallback(async (providerId?: string) => {
    if (workspaceId) {
      invalidateProvidersBootstrap(workspaceId);
      await refreshProvidersBootstrapState({ force: true });
      if (providerId) {
        try {
          await onboarding.ensureProviderAuthSummary(providerId, { trigger: "explicit" });
        } catch {
          // Keep the refreshed bootstrap snapshot even when live model hydration fails.
        }
      }
      return;
    }
    try {
      invalidateHostProvidersBootstrap();
      await onboarding.refreshBootstrap();
    } catch (error) {
      setProviderError(messageFromError(error));
    }
  }, [onboarding, refreshProvidersBootstrapState, workspaceId]);

  const refreshProviders = useCallback(async () => {
    if (workspaceId) {
      const bootstrap = await refreshProvidersBootstrapState({ force: true, silent: true });
      return bootstrap?.providers ?? [];
    }
    try {
      const bootstrap = await onboarding.refreshBootstrap();
      return bootstrap.providers;
    } catch (error) {
      setProviderError(messageFromError(error));
      return [];
    }
  }, [onboarding, refreshProvidersBootstrapState, workspaceId]);

  const {
    applyCursorAccounts,
    refreshCodexAccounts,
    refreshClaudeAccounts,
    refreshGeminiAccounts,
    refreshQwenAccounts,
    refreshKimiAccounts,
    refreshMistralAccounts,
    refreshCursorAccounts,
    refreshAmpAccounts,
    setScopedClaudeAccounts,
    setScopedCopilotAccounts,
  } = useHarnessAuthAccountCollections({
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
  });

  useEffect(() => {
    if (!enabled || !workspaceId) return;
    const pending = codexAccounts?.logins?.some(
      (login: CodexLoginStatus) => login.status === "pending",
    ) ?? false;
    if (pending) {
      previousCodexPendingRef.current = true;
      return;
    }
    const previousPending = previousCodexPendingRef.current;
    previousCodexPendingRef.current = pending;
    if (!previousPending) return;
    void onboarding.ensureProviderAuthSummary("codex", {
      force: true,
      trigger: "explicit",
    }).catch(() => {});
  }, [codexAccounts, enabled, onboarding, workspaceId]);

  const {
    closeHarnessAuthModal,
    onAmpDelete,
    onCancelInstall,
    onClaudeDelete,
    onCodexDelete,
    onCopilotDelete,
    onCursorDelete,
    onDeleteProviderEndpoint,
    onGeminiDelete,
    onInstall,
    onInstallAll,
    onKimiDelete,
    onMistralDelete,
    onQwenDelete,
    onRefreshProviderEndpointModels,
    onSelectHarnessAuthRow,
    openHarnessAuthModal,
    patchHarnessAuthModal,
    submitHarnessApiKeyModal,
    submitHarnessSubscriptionModal,
  } = useHarnessAuthenticationActions({
    workspaceId,
    ownerScopeKey,
    harnessAuthModal,
    onboarding,
    providerHarnessConfigRef,
    supportsHarnessEndpointConfig,
    requireOwnerScope,
    mutateProviderAccount,
    markProviderEndpointUnsupported,
    setProviderHarnessBusyForProvider,
    setSubscriptionSourceFallback,
    refreshProviderSlicesAfterMutation,
    setProviderError,
    setInstallBusy,
    applyCursorAccounts,
    baseOpenHarnessAuthModal,
    baseCloseHarnessAuthModal,
    basePatchHarnessAuthModal,
    startHarnessAuthModalOperation,
    finishHarnessAuthModalOperation,
    hasActiveHarnessAuthModalOperation,
    patchHarnessAuthModalForOperation,
    markAwaitingBrowserForOperation,
    markFinalizingForOperation,
    failSubscriptionFlowForOperation,
    closeHarnessAuthModalForOperation,
    refreshCodexAccounts,
    refreshClaudeAccounts,
    refreshGeminiAccounts,
    refreshQwenAccounts,
    refreshKimiAccounts,
    refreshCursorAccounts,
    refreshAmpAccounts,
    refreshMistralAccounts,
    setScopedClaudeAccounts,
    setScopedCopilotAccounts,
    setCodexAccountsBusy,
    setClaudeAccountsBusy,
    setGeminiAccountsBusy,
    setQwenAccountsBusy,
    setKimiAccountsBusy,
    setMistralAccountsBusy,
    setCopilotAccountsBusy,
    setCursorAccountsBusy,
    setAmpAccountsBusy,
  });

  useEffect(() => {
    if (!enabled) return;
    const pending = codexAccounts?.logins?.some(
      (login: CodexLoginStatus) => login.status === "pending",
    );
    if (!pending) return;
    const interval = window.setInterval(() => {
      refreshCodexAccounts({ silent: true }).catch(() => {});
    }, 2000);
    return () => window.clearInterval(interval);
  }, [codexAccounts, enabled, refreshCodexAccounts]);

  useEffect(() => {
    if (enabled) return;
    closeHarnessAuthModal();
  }, [closeHarnessAuthModal, enabled]);

  return {
    providers,
    installs,
    installBusy,
    onInstallAll,
    onInstall,
    onCancelInstall,
    providerHarnessConfig,
    providerHarnessBusy,
    codexAccounts,
    codexAccountsBusy,
    claudeAccounts,
    claudeAccountsBusy,
    geminiAccounts,
    geminiAccountsBusy,
    qwenAccounts,
    qwenAccountsBusy,
    kimiAccounts,
    kimiAccountsBusy,
    mistralAccounts,
    mistralAccountsBusy,
    copilotAccounts,
    copilotAccountsBusy,
    cursorAccounts,
    cursorAccountsBusy,
    ampAccounts,
    ampAccountsBusy,
    harnessAuthModal,
    openHarnessAuthModal,
    closeHarnessAuthModal,
    patchHarnessAuthModal,
    submitHarnessSubscriptionModal,
    submitHarnessApiKeyModal,
    onSelectHarnessAuthRow,
    onDeleteProviderEndpoint,
    onRefreshProviderEndpointModels,
    onCodexDelete,
    onClaudeDelete,
    onGeminiDelete,
    onQwenDelete,
    onKimiDelete,
    onMistralDelete,
    onCopilotDelete,
    onCursorDelete,
    onAmpDelete,
    providerError,
    supportsHarnessEndpointConfig,
    supportsHarnessSubscriptionAuth,
    harnessEndpointRequiresBaseUrl,
  };
}
