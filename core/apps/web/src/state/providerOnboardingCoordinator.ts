import { useCallback, useEffect, useMemo, useSyncExternalStore } from "react";
import type { ProvidersBootstrapResponse } from "../api/client";
import { subscribeDaemonConnection } from "../api/daemonConnection";
import {
  createMissingProviderOwnerScopeError,
  getProviderOwnerScopeKeyOrNull,
  getProviderOwnerScopeOrNull,
} from "./providerScopeAdapters";
import { withProviderBootstrapTimeout } from "../utils/providerBootstrapTimeout";
import {
  EMPTY_PROVIDER_ONBOARDING_SNAPSHOT,
  cancelProviderOnboardingInstall,
  ensureProviderAuthSummaryForOwner,
  getProviderOnboardingEntry,
  getOrCreateEntry,
  getProviderOnboardingSnapshotForOwner,
  loadProviderOnboardingBootstrap,
  refreshProviderOnboardingBootstrap,
  retainEntry,
  setEntryBootstrapState,
  startAllProviderInstalls,
  startProviderInstall,
  subscribeProviderOnboardingForOwner,
  type ProviderAuthSummaryTrigger,
} from "./providerOnboardingCoordinatorStore";

export {
  EMPTY_PROVIDER_ONBOARDING_SNAPSHOT,
  ensureProviderAuthSummary,
  getProviderOnboardingSnapshot,
  loadProviderOnboardingBootstrap,
  refreshProviderOnboardingBootstrap,
  resetProviderOnboardingCoordinatorForTests,
  resolveProviderOptionsUpdate,
  shouldHydrateProviderModels,
  startAllProviderInstalls,
  startProviderInstall,
  subscribeProviderOnboarding,
  type ProviderAuthSummaryTrigger,
  type ProviderOnboardingBootstrapState,
  type ProviderOnboardingInstallState,
  type ProviderOnboardingSnapshot,
} from "./providerOnboardingCoordinatorStore";

const toErrorMessage = (error: unknown): string =>
  error instanceof Error ? error.message : String(error);

export const useProviderOnboardingCoordinator = ({
  workspaceId,
  enabled = true,
  onLoadError,
}: {
  workspaceId: string | null;
  enabled?: boolean;
  onLoadError?: (error: unknown) => void;
}) => {
  const ownerScopeKey = useSyncExternalStore(
    useCallback((listener: () => void) => subscribeDaemonConnection(() => listener()), []),
    useCallback(() => getProviderOwnerScopeKeyOrNull(workspaceId), [workspaceId]),
    useCallback(() => getProviderOwnerScopeKeyOrNull(workspaceId), [workspaceId]),
  );
  const ownerScope = useMemo(
    () => getProviderOwnerScopeOrNull(workspaceId),
    [ownerScopeKey, workspaceId],
  );

  const snapshot = useSyncExternalStore(
    useCallback(
      (listener: () => void) =>
        ownerScope ? subscribeProviderOnboardingForOwner(ownerScope, listener) : () => {},
      [ownerScope],
    ),
    useCallback(
      () => ownerScope ? getProviderOnboardingSnapshotForOwner(ownerScope) : EMPTY_PROVIDER_ONBOARDING_SNAPSHOT,
      [ownerScope],
    ),
    useCallback(
      () => ownerScope ? getProviderOnboardingSnapshotForOwner(ownerScope) : EMPTY_PROVIDER_ONBOARDING_SNAPSHOT,
      [ownerScope],
    ),
  );

  const runBootstrapRequest = useCallback(
    async (mode: "load" | "refresh"): Promise<ProvidersBootstrapResponse> => {
      if (!ownerScope) {
        throw createMissingProviderOwnerScopeError();
      }
      const entry = getOrCreateEntry(ownerScope);
      const wasReady = entry.snapshot.bootstrapState === "ready";
      if (!wasReady) {
        setEntryBootstrapState(entry, "loading", null);
      }
      try {
        const result = await withProviderBootstrapTimeout(
          mode === "refresh"
            ? refreshProviderOnboardingBootstrap(workspaceId)
            : loadProviderOnboardingBootstrap(workspaceId),
        );
        const current = getProviderOnboardingEntry(ownerScope);
        if (current) {
          setEntryBootstrapState(current, "ready", null);
        }
        return result;
      } catch (error) {
        const current = getProviderOnboardingEntry(ownerScope);
        if (current) {
          if (mode === "refresh" && wasReady) {
            setEntryBootstrapState(current, "ready", null);
          } else {
            setEntryBootstrapState(current, "error", toErrorMessage(error));
          }
        }
        onLoadError?.(error);
        throw error;
      }
    },
    [onLoadError, ownerScope, workspaceId],
  );

  useEffect(() => {
    if (!enabled || !ownerScope) return;
    return retainEntry(ownerScope);
  }, [enabled, ownerScope]);

  useEffect(() => {
    if (!enabled || !ownerScope) return;
    runBootstrapRequest("load").catch(() => {});
  }, [enabled, ownerScope, ownerScopeKey, runBootstrapRequest]);

  const loadBootstrap = useCallback(
    () => runBootstrapRequest("load"),
    [runBootstrapRequest],
  );
  const refreshBootstrap = useCallback(
    () => runBootstrapRequest("refresh"),
    [runBootstrapRequest],
  );
  const ensureAuthSummary = useCallback(
    (providerId: string, opts?: { force?: boolean; trigger?: ProviderAuthSummaryTrigger }) =>
      ownerScope
        ? ensureProviderAuthSummaryForOwner(ownerScope, providerId, opts)
        : Promise.reject(createMissingProviderOwnerScopeError()),
    [ownerScope],
  );
  const onInstallProvider = useCallback(
    (providerId: string) => startProviderInstall(workspaceId, providerId),
    [workspaceId],
  );
  const onInstallAllProviders = useCallback(
    () => startAllProviderInstalls(workspaceId),
    [workspaceId],
  );
  const onCancelInstall = useCallback(
    (providerId: string) => cancelProviderOnboardingInstall(workspaceId, providerId),
    [workspaceId],
  );

  return useMemo(() => ({
    ...snapshot,
    loadBootstrap,
    refreshBootstrap,
    ensureProviderAuthSummary: ensureAuthSummary,
    startProviderInstall: onInstallProvider,
    startAllProviderInstalls: onInstallAllProviders,
    cancelProviderInstall: onCancelInstall,
  }), [
    ensureAuthSummary,
    loadBootstrap,
    onCancelInstall,
    onInstallAllProviders,
    onInstallProvider,
    refreshBootstrap,
    snapshot,
  ]);
};
