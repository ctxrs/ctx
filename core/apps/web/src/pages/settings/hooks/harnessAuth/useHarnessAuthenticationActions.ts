import { useCallback, type Dispatch, type MutableRefObject, type SetStateAction } from "react";
import {
  upsertCursorAccount,
  type AmpAccountsResponse,
  type ClaudeAccountsResponse,
  type CodexAccountsResponse,
  type CopilotAccountsResponse,
  type CursorAccountsResponse,
  type GeminiAccountsResponse,
  type HarnessProviderSourceConfig,
  type KimiAccountsResponse,
  type MistralAccountsResponse,
  type ProviderStatus,
  type QwenAccountsResponse,
} from "../../../../api/client";
import { trackFeatureUsed } from "../../../../utils/analytics";
import {
  deleteProviderAccount as executeDeleteProviderAccount,
  deleteProviderEndpoint as executeDeleteProviderEndpoint,
  refreshProviderEndpointModels as executeRefreshProviderEndpointModels,
  selectProviderSubscriptionAccount as executeSelectProviderSubscriptionAccount,
  selectProviderSource as executeSelectProviderSource,
  selectSubscriptionSourceIfSupported as executeSelectSubscriptionSourceIfSupported,
  submitProviderEndpointAuth,
  type ProviderAccountMutationProviderId,
} from "../../../../state/providerOnboardingActions";
import type { OwnerScope } from "../../../../state/scopeIdentity";
import type { HarnessAuthModalState } from "../../SettingsPage.types";
import type { HarnessAuthRow } from "../../harnessAuthRows";
import {
  defaultShapeForHarnessProvider,
  normalizeOptionalBaseUrl,
  nextDefaultEndpointName,
  nextTokenEndpointName,
} from "../../harnessEndpointProviders";
import {
  harnessEndpointRequiresApiShape,
  harnessEndpointRequiresBaseUrl,
  messageFromError,
  preferredModelIdFromEndpointSummary,
  toErrorObject,
  validateHarnessEndpointConfigForOwnerScope,
} from "../harnessAuth/capabilities";
import { runHarnessSubscriptionFlow } from "../harnessAuth/subscriptionFlow";
import { acknowledgeProviderRuntimeWarnings, getProviderRuntimeWarningIds } from "../../../../utils/providerRuntimeWarnings";
import { openCodexAuthUrlWithDesktopRelay } from "./codexDesktopRelay";
import type { HarnessAuthModalOperation } from "./useHarnessAuthModalController";

type StateSetter<T> = Dispatch<SetStateAction<T>>;

type OnboardingLike = {
  providersById: Record<string, ProviderStatus>;
  ensureProviderAuthSummary: (
    providerId: string,
    opts?: { force?: boolean; trigger?: "passive" | "explicit" },
  ) => Promise<unknown>;
  startProviderInstall: (providerId: string) => Promise<unknown>;
  startAllProviderInstalls: () => Promise<unknown>;
  cancelProviderInstall: (providerId: string) => Promise<unknown>;
};

type HarnessAuthenticationActionArgs = {
  workspaceId: string | null;
  ownerScopeKey: string | null;
  harnessAuthModal: HarnessAuthModalState | null;
  onboarding: OnboardingLike;
  providerHarnessConfigRef: MutableRefObject<Record<string, HarnessProviderSourceConfig | undefined>>;
  supportsHarnessEndpointConfig: (providerId: string) => boolean;
  requireOwnerScope: () => OwnerScope;
  mutateProviderAccount: (params: {
    mutate: () => Promise<void>;
    setBusy: StateSetter<boolean>;
    setProviderError: StateSetter<string | null>;
  }) => Promise<void>;
  markProviderEndpointUnsupported: (providerId: string) => void;
  setProviderHarnessBusyForProvider: (providerId: string, busy: boolean) => void;
  setSubscriptionSourceFallback: (providerId: string) => void;
  refreshProviderSlicesAfterMutation: (providerId?: string) => Promise<void>;
  setProviderError: StateSetter<string | null>;
  setInstallBusy: StateSetter<string | null>;
  applyCursorAccounts: (next: CursorAccountsResponse) => void;
  baseOpenHarnessAuthModal: (providerId: string) => void;
  baseCloseHarnessAuthModal: () => void;
  basePatchHarnessAuthModal: (patch: Partial<HarnessAuthModalState>) => void;
  startHarnessAuthModalOperation: (
    source: "modal-action" | "subscription-flow",
  ) => HarnessAuthModalOperation;
  finishHarnessAuthModalOperation: (operation: HarnessAuthModalOperation) => void;
  hasActiveHarnessAuthModalOperation: (source: "subscription-flow") => boolean;
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
  refreshCodexAccounts: (opts?: { silent?: boolean }) => Promise<CodexAccountsResponse | null>;
  refreshClaudeAccounts: (opts?: { silent?: boolean }) => Promise<ClaudeAccountsResponse | null>;
  refreshGeminiAccounts: (opts?: { silent?: boolean }) => Promise<GeminiAccountsResponse | null>;
  refreshQwenAccounts: (opts?: { silent?: boolean }) => Promise<QwenAccountsResponse | null>;
  refreshKimiAccounts: (opts?: { silent?: boolean }) => Promise<KimiAccountsResponse | null>;
  refreshCursorAccounts: (opts?: { silent?: boolean }) => Promise<CursorAccountsResponse | null>;
  refreshAmpAccounts: (opts?: { silent?: boolean }) => Promise<AmpAccountsResponse | null>;
  refreshMistralAccounts: (opts?: { silent?: boolean }) => Promise<MistralAccountsResponse | null>;
  setScopedClaudeAccounts: Dispatch<SetStateAction<ClaudeAccountsResponse | null>>;
  setScopedCopilotAccounts: Dispatch<SetStateAction<CopilotAccountsResponse | null>>;
  setCodexAccountsBusy: StateSetter<boolean>;
  setClaudeAccountsBusy: StateSetter<boolean>;
  setGeminiAccountsBusy: StateSetter<boolean>;
  setQwenAccountsBusy: StateSetter<boolean>;
  setKimiAccountsBusy: StateSetter<boolean>;
  setMistralAccountsBusy: StateSetter<boolean>;
  setCopilotAccountsBusy: StateSetter<boolean>;
  setCursorAccountsBusy: StateSetter<boolean>;
  setAmpAccountsBusy: StateSetter<boolean>;
};

export function useHarnessAuthenticationActions(args: HarnessAuthenticationActionArgs) {
  const runDeleteProviderAccount = useCallback(
    async <TKey extends ProviderAccountMutationProviderId>(
      providerId: TKey,
      accountId: string,
      setBusy: StateSetter<boolean>,
    ) => {
      await args.mutateProviderAccount({
        mutate: async () => {
          await executeDeleteProviderAccount(args.requireOwnerScope(), providerId, accountId);
        },
        setBusy,
        setProviderError: args.setProviderError,
      });
    },
    [args],
  );

  const runSelectProviderSubscriptionAccount = useCallback(
    async <TKey extends ProviderAccountMutationProviderId>(
      providerId: TKey,
      accountId: string | null,
      setBusy: StateSetter<boolean>,
    ) => {
      await args.mutateProviderAccount({
        mutate: async () => {
          await executeSelectProviderSubscriptionAccount({
            ownerScope: args.requireOwnerScope(),
            providerId,
            accountId,
            supportsEndpointConfig: args.supportsHarnessEndpointConfig(providerId),
          });
          await args.onboarding.ensureProviderAuthSummary(providerId, { force: true, trigger: "explicit" });
        },
        setBusy,
        setProviderError: args.setProviderError,
      });
    },
    [args],
  );

  const onDeleteProviderEndpoint = useCallback(async (providerId: string, endpointId: string) => {
    args.setProviderHarnessBusyForProvider(providerId, true);
    args.setProviderError(null);
    try {
      await executeDeleteProviderEndpoint(args.requireOwnerScope(), providerId, endpointId);
      await args.refreshProviderSlicesAfterMutation(providerId);
    } catch (error) {
      args.setProviderError(messageFromError(error));
    } finally {
      args.setProviderHarnessBusyForProvider(providerId, false);
    }
  }, [args]);

  const onRefreshProviderEndpointModels = useCallback(async (providerId: string, endpointId: string) => {
    args.setProviderHarnessBusyForProvider(providerId, true);
    args.setProviderError(null);
    try {
      await executeRefreshProviderEndpointModels(args.requireOwnerScope(), providerId, endpointId);
      await args.refreshProviderSlicesAfterMutation(providerId);
    } catch (error) {
      args.setProviderError(messageFromError(error));
    } finally {
      args.setProviderHarnessBusyForProvider(providerId, false);
    }
  }, [args]);

  const onSelectProviderSource = useCallback(
    async (providerId: string, sourceKind: "subscription" | "endpoint", endpointId?: string | null) => {
      const ownerScope = args.requireOwnerScope();
      args.setProviderHarnessBusyForProvider(providerId, true);
      args.setProviderError(null);
      try {
        await executeSelectProviderSource(ownerScope, providerId, {
          sourceKind,
          endpointId: endpointId ?? null,
        });
        trackFeatureUsed("provider_source_selected", {
          provider_id: providerId,
          source_kind: sourceKind,
          scope_kind: ownerScope.kind,
        });
      } catch (error) {
        args.setProviderError(messageFromError(error));
        throw toErrorObject(error);
      } finally {
        args.setProviderHarnessBusyForProvider(providerId, false);
      }
    },
    [args],
  );

  const selectSubscriptionSourceIfSupported = useCallback(
    async (providerId: string) => {
      await executeSelectSubscriptionSourceIfSupported({
        ownerScope: args.requireOwnerScope(),
        providerId,
        supportsEndpointConfig: args.supportsHarnessEndpointConfig(providerId),
        onEndpointUnsupported: () => {
          args.markProviderEndpointUnsupported(providerId);
        },
      });
    },
    [args],
  );

  const openHarnessAuthModal = useCallback((providerId: string) => {
    args.setProviderError(null);
    args.baseOpenHarnessAuthModal(providerId);
  }, [args]);

  const closeHarnessAuthModal = useCallback(() => {
    args.baseCloseHarnessAuthModal();
  }, [args]);

  const patchHarnessAuthModal = useCallback((patch: Partial<HarnessAuthModalState>) => {
    args.basePatchHarnessAuthModal(patch);
  }, [args]);

  const submitHarnessApiKeyModal = useCallback(async () => {
    const modal = args.harnessAuthModal;
    if (!modal || modal.stage !== "api_key") return;
    if (modal.api_key_busy) return;

    const isCursor = modal.provider_id === "cursor";
    if (!isCursor && !args.supportsHarnessEndpointConfig(modal.provider_id)) {
      args.setProviderError("API key auth is not configurable for this harness yet.");
      return;
    }

    const requiresBaseUrl = harnessEndpointRequiresBaseUrl(modal.provider_id);
    const requiresApiShape = harnessEndpointRequiresApiShape(modal.provider_id);
    const nameInput = modal.endpoint_name.trim();
    const endpoints: HarnessProviderSourceConfig["endpoints"] =
      args.providerHarnessConfigRef.current[modal.provider_id]?.endpoints ?? [];
    const existingNames = endpoints
      .map((endpoint) => endpoint.name);
    const name = nameInput
      || (requiresBaseUrl
        ? nextDefaultEndpointName(modal.endpoint_provider_id, existingNames)
        : nextTokenEndpointName(modal.provider_id, existingNames));
    const geminiAuthType = modal.provider_id === "gemini" ? modal.gemini_endpoint_auth_type : null;
    const usesGeminiVertexServiceAccount = modal.provider_id === "gemini" && geminiAuthType === "vertex_ai";
    const base = modal.base_url.trim();
    const normalizedBase = normalizeOptionalBaseUrl(base);
    const effectiveBaseUrl = modal.provider_id === "gemini" ? null : normalizedBase;
    const key = modal.api_key.trim();
    const serviceAccountJson = modal.service_account_json.trim();
    const projectId = modal.project_id.trim();
    const location = modal.location.trim();
    const manualModelIds = modal.manual_model_ids
      .split(/[\n,]/)
      .map((value: string) => value.trim())
      .filter((value: string) => value.length > 0);
    const ownerScope = args.requireOwnerScope();
    const existingEndpoint = endpoints
      .find((endpoint) => endpoint.id === modal.endpoint_id);
    const ownerScopeValidationError = validateHarnessEndpointConfigForOwnerScope({
      ownerScopeKind: ownerScope.kind,
      providerId: modal.provider_id,
      baseUrl: effectiveBaseUrl,
      manualModelIds,
      existingPreferredModelId: preferredModelIdFromEndpointSummary(existingEndpoint),
    });

    if (requiresBaseUrl && !base) {
      args.setProviderError("Endpoint base URL is required.");
      return;
    }
    if (usesGeminiVertexServiceAccount) {
      if (!serviceAccountJson) {
        args.setProviderError("Service account JSON is required.");
        return;
      }
    } else if (!key) {
      args.setProviderError("API key is required.");
      return;
    }
    if (ownerScopeValidationError) {
      args.setProviderError(ownerScopeValidationError);
      return;
    }

    const operation = args.startHarnessAuthModalOperation("modal-action");
    args.patchHarnessAuthModalForOperation(operation, { api_key_busy: true });
    args.setProviderError(null);

    const previousSourceConfig = args.providerHarnessConfigRef.current[modal.provider_id];
    const previousSourceKind = previousSourceConfig?.selected_source_kind ?? "subscription";
    const previousEndpointId = previousSourceKind === "endpoint"
      ? previousSourceConfig?.selected_endpoint_id ?? null
      : null;

    try {
      if (isCursor) {
        const label = name.trim();
        const next = await upsertCursorAccount(key, label ? { label } : undefined);
        await args.refreshProviderSlicesAfterMutation(modal.provider_id);
        if (!operation.isCurrent()) return;
        args.applyCursorAccounts(next);
        await selectSubscriptionSourceIfSupported(modal.provider_id);
        if (!operation.isCurrent()) return;
        args.setSubscriptionSourceFallback(modal.provider_id);
        args.closeHarnessAuthModalForOperation(operation);
        return;
      }

      const result = await submitProviderEndpointAuth({
        ownerScope,
        providerId: modal.provider_id,
        requestedEndpointId: modal.endpoint_id?.trim() || null,
        name,
        baseUrl: effectiveBaseUrl,
        apiShape: requiresApiShape ? defaultShapeForHarnessProvider(modal.provider_id) : null,
        authType: geminiAuthType,
        apiKey: usesGeminiVertexServiceAccount ? null : key,
        serviceAccountJson: usesGeminiVertexServiceAccount ? serviceAccountJson : null,
        projectId: usesGeminiVertexServiceAccount ? (projectId || null) : null,
        location: usesGeminiVertexServiceAccount ? (location || null) : null,
        manualModelIds,
        previousSelection: {
          sourceKind: previousSourceKind,
          endpointId: previousEndpointId,
        },
        isStale: () => !operation.isCurrent(),
      });

      if (operation.isCurrent()) {
        args.patchHarnessAuthModalForOperation(operation, { endpoint_id: result.selectedEndpointId });
      }

      if (result.status === "applied") {
        args.closeHarnessAuthModalForOperation(operation);
        return;
      }

      if (result.status === "rolled_back") {
        if (operation.isCurrent()) {
          args.setProviderError(result.message);
        }
        return;
      }

      if (result.status === "rollback_failed") {
        if (operation.isCurrent()) {
          args.setProviderError(
            result.message
              ? `${result.message} Failed to restore previous provider source: ${messageFromError(result.rollbackError)}`
              : `Failed to restore previous provider source: ${messageFromError(result.rollbackError)}`,
          );
        }
        return;
      }
    } catch (error) {
      if (operation.isCurrent()) {
        args.setProviderError(messageFromError(error));
      }
    } finally {
      args.patchHarnessAuthModalForOperation(operation, { api_key_busy: false });
      args.finishHarnessAuthModalOperation(operation);
    }
  }, [args, selectSubscriptionSourceIfSupported]);

  const openCodexAuthUrl = useCallback(
    async (
      url: string,
      params?: { accountId: string; expectedCallbackUrl: string | null; completionToken: string | null },
    ) => {
      await openCodexAuthUrlWithDesktopRelay(url, params);
    },
    [],
  );

  const submitHarnessSubscriptionModal = useCallback(async () => {
    const modal = args.harnessAuthModal;
    if (!modal || modal.subscription_busy || args.hasActiveHarnessAuthModalOperation("subscription-flow")) {
      return;
    }

    const flow = args.startHarnessAuthModalOperation("subscription-flow");
    args.patchHarnessAuthModalForOperation(flow, {
      stage: "subscription",
      subscription_phase: "editing",
      subscription_busy: true,
      subscription_status: "Starting subscription flow...",
    });
    args.setProviderError(null);
    try {
      await runHarnessSubscriptionFlow({
        modal,
        workspaceId: args.workspaceId,
        flow,
        patchHarnessAuthModalForOperation: args.patchHarnessAuthModalForOperation,
        markAwaitingBrowserForOperation: args.markAwaitingBrowserForOperation,
        markFinalizingForOperation: args.markFinalizingForOperation,
        failSubscriptionFlowForOperation: args.failSubscriptionFlowForOperation,
        closeHarnessAuthModalForOperation: args.closeHarnessAuthModalForOperation,
        setProviderError: args.setProviderError,
        refreshBootstrapAfterMutation: args.refreshProviderSlicesAfterMutation,
        selectSubscriptionSourceIfSupported,
        refreshCodexAccounts: args.refreshCodexAccounts,
        refreshClaudeAccounts: args.refreshClaudeAccounts,
        refreshGeminiAccounts: args.refreshGeminiAccounts,
        refreshQwenAccounts: args.refreshQwenAccounts,
        refreshKimiAccounts: args.refreshKimiAccounts,
        refreshCursorAccounts: args.refreshCursorAccounts,
        refreshAmpAccounts: args.refreshAmpAccounts,
        refreshMistralAccounts: args.refreshMistralAccounts,
        setClaudeAccounts: args.setScopedClaudeAccounts,
        setCopilotAccounts: args.setScopedCopilotAccounts,
        openCodexAuthUrl,
      });
    } finally {
      args.patchHarnessAuthModalForOperation(flow, { subscription_busy: false });
      args.finishHarnessAuthModalOperation(flow);
    }
  }, [args, openCodexAuthUrl, selectSubscriptionSourceIfSupported]);

  const onCodexDelete = useCallback(async (accountId: string) => {
    await runDeleteProviderAccount("codex", accountId, args.setCodexAccountsBusy);
  }, [args.setCodexAccountsBusy, runDeleteProviderAccount]);

  const onClaudeDelete = useCallback(async (accountId: string) => {
    await runDeleteProviderAccount("claude-crp", accountId, args.setClaudeAccountsBusy);
  }, [args.setClaudeAccountsBusy, runDeleteProviderAccount]);

  const onGeminiDelete = useCallback(async (accountId: string) => {
    await runDeleteProviderAccount("gemini", accountId, args.setGeminiAccountsBusy);
  }, [args.setGeminiAccountsBusy, runDeleteProviderAccount]);

  const onQwenDelete = useCallback(async (accountId: string) => {
    await runDeleteProviderAccount("qwen", accountId, args.setQwenAccountsBusy);
  }, [args.setQwenAccountsBusy, runDeleteProviderAccount]);

  const onKimiDelete = useCallback(async (accountId: string) => {
    await runDeleteProviderAccount("kimi", accountId, args.setKimiAccountsBusy);
  }, [args.setKimiAccountsBusy, runDeleteProviderAccount]);

  const onMistralDelete = useCallback(async (accountId: string) => {
    await runDeleteProviderAccount("mistral", accountId, args.setMistralAccountsBusy);
  }, [args.setMistralAccountsBusy, runDeleteProviderAccount]);

  const onCopilotDelete = useCallback(async (accountId: string) => {
    await runDeleteProviderAccount("copilot", accountId, args.setCopilotAccountsBusy);
  }, [args.setCopilotAccountsBusy, runDeleteProviderAccount]);

  const onCursorDelete = useCallback(async (accountId: string) => {
    await runDeleteProviderAccount("cursor", accountId, args.setCursorAccountsBusy);
  }, [args.setCursorAccountsBusy, runDeleteProviderAccount]);

  const onAmpDelete = useCallback(async (accountId: string) => {
    await runDeleteProviderAccount("amp", accountId, args.setAmpAccountsBusy);
  }, [args.setAmpAccountsBusy, runDeleteProviderAccount]);

  const onSelectHarnessAuthRow = useCallback(async (providerId: string, row: HarnessAuthRow) => {
    try {
      if (!row.selectable) return;
      if (row.kind === "api_key" && row.endpoint_id) {
        await onSelectProviderSource(providerId, "endpoint", row.endpoint_id);
        return;
      }
      if (providerId === "codex" && row.account_id) {
        await runSelectProviderSubscriptionAccount("codex", row.account_id, args.setCodexAccountsBusy);
      } else if (providerId === "claude-crp" && row.account_id) {
        await runSelectProviderSubscriptionAccount("claude-crp", row.account_id, args.setClaudeAccountsBusy);
      } else if (providerId === "gemini" && row.account_id) {
        await runSelectProviderSubscriptionAccount("gemini", row.account_id, args.setGeminiAccountsBusy);
      } else if (providerId === "qwen" && row.account_id) {
        await runSelectProviderSubscriptionAccount("qwen", row.account_id, args.setQwenAccountsBusy);
      } else if (providerId === "kimi" && row.account_id) {
        await runSelectProviderSubscriptionAccount("kimi", row.account_id, args.setKimiAccountsBusy);
      } else if (providerId === "mistral" && row.account_id) {
        await runSelectProviderSubscriptionAccount("mistral", row.account_id, args.setMistralAccountsBusy);
      } else if (providerId === "copilot" && row.account_id) {
        await runSelectProviderSubscriptionAccount("copilot", row.account_id, args.setCopilotAccountsBusy);
      } else if (providerId === "cursor" && row.account_id) {
        await runSelectProviderSubscriptionAccount("cursor", row.account_id, args.setCursorAccountsBusy);
      } else if (providerId === "amp" && row.account_id) {
        await runSelectProviderSubscriptionAccount("amp", row.account_id, args.setAmpAccountsBusy);
      }
    } catch (error) {
      args.setProviderError(messageFromError(error));
    }
  }, [args, onSelectProviderSource, runSelectProviderSubscriptionAccount]);

  const onInstall = useCallback(async (providerId: string) => {
    args.setInstallBusy(providerId);
    args.setProviderError(null);
    try {
      await args.onboarding.startProviderInstall(providerId);
    } catch (error) {
      args.setProviderError(messageFromError(error));
    } finally {
      args.setInstallBusy(null);
    }
  }, [args]);

  const onInstallAll = useCallback(async () => {
    args.setInstallBusy("all");
    args.setProviderError(null);
    try {
      acknowledgeProviderRuntimeWarnings(
        args.ownerScopeKey ?? args.workspaceId,
        getProviderRuntimeWarningIds(args.onboarding.providersById),
      );
      await args.onboarding.startAllProviderInstalls();
    } catch (error) {
      args.setProviderError(messageFromError(error));
    } finally {
      args.setInstallBusy(null);
    }
  }, [args]);

  const onCancelInstall = useCallback(async (providerId: string) => {
    args.setProviderError(null);
    try {
      await args.onboarding.cancelProviderInstall(providerId);
    } catch (error) {
      args.setProviderError(messageFromError(error));
    }
  }, [args]);

  return {
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
  };
}
