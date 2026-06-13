import {
  useCallback,
  useEffect,
  useMemo,
  useState,
  type Dispatch,
  type SetStateAction,
} from "react";
import {
  getHealth,
  type ProviderOptions,
} from "../../api/client";
import type { DraftHarness } from "../../components/WorkbenchComposer";
import {
  resolveProviderOptionsUpdate,
  shouldHydrateProviderModels,
  useProviderOnboardingCoordinator,
  type ProviderAuthSummaryTrigger,
} from "../../state/providerOnboardingCoordinator";
import {
  resolveDefaultHarnessProviderId,
  resolveDraftHarnessReplacement,
} from "./harnessSelection";
import { getProviderOwnerScopeKeyOrNull } from "../../state/providerScopeAdapters";
import {
  acknowledgeProviderRuntimeWarnings,
  getProviderRuntimeWarningIds,
} from "../../utils/providerRuntimeWarnings";

type UseWorkbenchProvidersArgs = {
  workspaceId: string;
  setDraftHarness: Dispatch<SetStateAction<DraftHarness | null>>;
  onStartError: (message: string | null) => void;
};

export { resolveProviderOptionsUpdate, shouldHydrateProviderModels } from "../../state/providerOnboardingCoordinator";

const PROVIDER_RUNTIME_BUILD_REFRESH_KEY_PREFIX = "ctx.provider_runtime.checked_build";

const buildRefreshStorageKey = (workspaceId: string): string =>
  `${PROVIDER_RUNTIME_BUILD_REFRESH_KEY_PREFIX}.${workspaceId}`;

const providerRuntimeBuildKey = (
  health: Awaited<ReturnType<typeof getHealth>>,
): string => {
  const buildId = String(health.compatibility?.desktop_build_id ?? "").trim();
  const exactVersion = String(health.compatibility?.desktop_exact_version ?? "").trim();
  const fallback = String(health.version ?? "").trim();
  return buildId || exactVersion || fallback;
};

const toErrorMessage = (error: unknown): string => {
  if (error instanceof Error) return error.message;
  return String(error);
};

export function useWorkbenchProviders({
  workspaceId,
  setDraftHarness,
  onStartError,
}: UseWorkbenchProvidersArgs) {
  const [installAllBusy, setInstallAllBusy] = useState(false);
  const onboarding = useProviderOnboardingCoordinator({
    workspaceId,
  });

  const providers = onboarding.bootstrap.providers;
  const providerOptions = onboarding.bootstrap.provider_options;
  const providersById = onboarding.providersById;
  const providerInstallsById = onboarding.installsById;

  const acknowledgeCurrentProviderRuntimeWarnings = useCallback(() => {
    const ownerScopeKey = getProviderOwnerScopeKeyOrNull(workspaceId) ?? workspaceId;
    acknowledgeProviderRuntimeWarnings(ownerScopeKey, getProviderRuntimeWarningIds(providersById));
  }, [providersById, workspaceId]);

  const defaultProviderId = useMemo(() => resolveDefaultHarnessProviderId(providers), [providers]);

  useEffect(() => {
    if (!providers.length) return;
    setDraftHarness((prev) =>
      resolveDraftHarnessReplacement({
        draftHarness: prev,
        providersById,
        defaultProviderId,
      }));
  }, [defaultProviderId, providers.length, providersById, setDraftHarness]);

  useEffect(() => {
    let cancelled = false;
    void getHealth()
      .then((health) => {
        if (cancelled) return;
        const currentBuild = providerRuntimeBuildKey(health);
        if (!currentBuild) return;
        const storageKey = buildRefreshStorageKey(workspaceId);
        let previousBuild = "";
        try {
          previousBuild = String(window.localStorage.getItem(storageKey) ?? "").trim();
        } catch {
          previousBuild = "";
        }
        try {
          window.localStorage.setItem(storageKey, currentBuild);
        } catch {
          // Ignore storage failures and continue without the upgrade refresh hint.
        }
        if (previousBuild && previousBuild !== currentBuild) {
          void onboarding.refreshBootstrap().catch(() => {});
        }
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [onboarding, workspaceId]);

  const installProviderFromMenu = useCallback(
    async (providerId: string) => {
      onStartError(null);
      try {
        await onboarding.startProviderInstall(providerId);
      } catch (error: unknown) {
        onStartError(toErrorMessage(error));
      }
    },
    [onStartError, onboarding],
  );

  const installAllProvidersFromMenu = useCallback(async () => {
    onStartError(null);
    setInstallAllBusy(true);
    try {
      acknowledgeCurrentProviderRuntimeWarnings();
      await onboarding.startAllProviderInstalls();
    } catch (error: unknown) {
      onStartError(toErrorMessage(error));
    } finally {
      setInstallAllBusy(false);
    }
  }, [acknowledgeCurrentProviderRuntimeWarnings, onStartError, onboarding]);

  const updateProvidersFromMenu = useCallback(
    async (providerIds: string[]) => {
      const uniqueProviderIds = Array.from(
        new Set(providerIds.map((providerId) => providerId.trim()).filter(Boolean)),
      );
      if (uniqueProviderIds.length === 0) return;
      onStartError(null);
      setInstallAllBusy(true);
      try {
        acknowledgeCurrentProviderRuntimeWarnings();
        for (const providerId of uniqueProviderIds) {
          await onboarding.startProviderInstall(providerId);
        }
      } catch (error: unknown) {
        onStartError(toErrorMessage(error));
        throw error;
      } finally {
        setInstallAllBusy(false);
      }
    },
    [acknowledgeCurrentProviderRuntimeWarnings, onStartError, onboarding],
  );

  const cancelProviderInstallFromMenu = useCallback(
    async (providerId: string) => {
      onStartError(null);
      try {
        await onboarding.cancelProviderInstall(providerId);
      } catch (error: unknown) {
        onStartError(toErrorMessage(error));
      }
    },
    [onStartError, onboarding],
  );

  const ensureProviderAuthSummary = useCallback(
    async (
      providerId: string,
      opts?: { force?: boolean; trigger?: ProviderAuthSummaryTrigger },
    ): Promise<ProviderOptions | undefined> => {
      return onboarding.ensureProviderAuthSummary(providerId, opts);
    },
    [onboarding],
  );

  return {
    providersById,
    defaultProviderId,
    providerInstallsById,
    providerOptions,
    bootstrapState: onboarding.bootstrapState,
    bootstrapError: onboarding.bootstrapError,
    installAllBusy,
    installProviderFromMenu,
    cancelProviderInstallFromMenu,
    installAllProvidersFromMenu,
    updateProvidersFromMenu,
    ensureProviderAuthSummary,
    refreshBootstrap: onboarding.refreshBootstrap,
  };
}
