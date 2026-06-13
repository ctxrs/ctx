import {
  cancelInstall,
  getProviderOptions,
  installAllProviders,
  installProvider,
  type InstallInfo,
  type InstallStartResponse,
  type ProviderOptions,
  type ProvidersBootstrapResponse,
} from "../api/client";
import { computeInstallPct } from "../utils/providerInstallUi";
import { isReadyVisibleHarnessProviderStatus } from "../utils/providerInventory";
import {
  getProvidersBootstrapSnapshotForScope,
  loadHostProvidersBootstrap,
  loadProvidersBootstrap,
  loadProvidersBootstrapForScope,
  refreshHostProvidersBootstrap,
  refreshProvidersBootstrap,
  refreshProvidersBootstrapForScope,
  resolveProviderOptionsUpdate,
  resolveProviderOptionsUpdateForScope,
  subscribeProvidersBootstrapForScope,
  updateProvidersBootstrapForScope,
} from "./providersBootstrapStore";
import {
  subscribeProviderInstallProgressForScope,
  upsertProviderInstallProgressForScope,
} from "./providerInstallProgressStore";
import {
  normalizeProviderInstallFailureKind,
  trackProviderInstallCompleted,
  trackProviderInstallFailed,
  trackProviderInstallStarted,
} from "../utils/analytics";
import { getProviderOwnerScope } from "./providerScopeAdapters";
import {
  createProviderAuthScopeFromOptions,
  serializeProviderAuthScope,
  type OwnerScope,
} from "./scopeIdentity";
import {
  hasFailedProviderModelProbe,
  hasProviderModels,
  isEndpointProviderSourceSelected,
  isFinalProviderModelCatalog,
  isPinnedSubscriptionBootstrapCatalog,
  SUBSCRIPTION_MODEL_DISCOVERY_PROVIDER_IDS,
} from "../utils/providerModelCatalog";
import {
  attachInstallObserver,
  cleanupSucceededInstalls,
  EMPTY_PROVIDER_ONBOARDING_SNAPSHOT,
  foregroundRefreshScopeKeys,
  getOrCreateEntry,
  getProviderAccountIdentityById,
  getProviderOnboardingEntry,
  maybeDeleteEntry,
  mergeProviderOptionsMap,
  providerInstallTargetForProvider,
  providerOnboardingByScope,
  reconcileRunningInstalls,
  setEntryBootstrapState,
  syncForegroundRefreshListeners,
  toErrorMessage,
  updateEntrySnapshot,
  withProviderAccountIdentity,
  type ProviderAuthSummaryTrigger,
  type ProviderOnboardingEntry,
  type ProviderOnboardingListener,
  type ProviderOnboardingSnapshot,
} from "./providerOnboardingCoordinatorCore";

export { resolveProviderOptionsUpdate } from "./providersBootstrapStore";
export {
  EMPTY_PROVIDER_ONBOARDING_SNAPSHOT,
  getOrCreateEntry,
  getProviderOnboardingEntry,
  setEntryBootstrapState,
  toErrorMessage,
  type ProviderAuthSummaryTrigger,
  type ProviderOnboardingBootstrapState,
  type ProviderOnboardingInstallState,
  type ProviderOnboardingSnapshot,
} from "./providerOnboardingCoordinatorCore";

export const shouldHydrateProviderModels = (
  providerId: string,
  options: ProviderOptions | undefined,
  trigger: ProviderAuthSummaryTrigger = "passive",
): boolean => {
  if (!SUBSCRIPTION_MODEL_DISCOVERY_PROVIDER_IDS.has(providerId)) return false;
  if (!options) return false;
  if (isEndpointProviderSourceSelected(options)) return false;
  if (options.has_active_auth !== true) return false;
  if (trigger === "passive" && hasFailedProviderModelProbe(options)) return false;
  if (isPinnedSubscriptionBootstrapCatalog(options)) return true;
  if (hasProviderModels(options)) return !isFinalProviderModelCatalog(options);
  return true;
};

const writeProviderOptionsForEntry = (
  entry: ProviderOnboardingEntry,
  providerId: string,
  next: ProviderOptions,
): ProviderOptions | undefined => {
  const updated = updateProvidersBootstrapForScope(entry.ownerScope, (current) => {
    const accountIdentityById = getProviderAccountIdentityById(current);
    const normalizedProviderOptions = Object.fromEntries(
      Object.entries(current.provider_options).map(([nextProviderId, options]) => [
        nextProviderId,
        withProviderAccountIdentity(nextProviderId, options, accountIdentityById),
      ]),
    ) as Record<string, ProviderOptions>;
    const normalizedNext =
      withProviderAccountIdentity(providerId, next, accountIdentityById) ?? next;
    const resolved = resolveProviderOptionsUpdateForScope(
      entry.workspaceOwnerScope,
      normalizedProviderOptions[providerId],
      normalizedNext,
    ) ?? normalizedNext;
    const nextProviderOptions = mergeProviderOptionsMap(
      normalizedProviderOptions,
      {
        ...normalizedProviderOptions,
        [providerId]: resolved,
      },
      entry.workspaceOwnerScope,
    );
    if (nextProviderOptions === current.provider_options) {
      return current;
    }
    return {
      ...current,
      provider_options: nextProviderOptions,
    };
  });
  return updated.provider_options[providerId];
};

const loadDetailedProviderOptionsForEntry = async (
  entry: ProviderOnboardingEntry,
  providerId: string,
): Promise<ProviderOptions | undefined> => {
  if (!entry.workspaceId) return undefined;
  const detailed = await getProviderOptions(entry.workspaceId, providerId);
  return writeProviderOptionsForEntry(entry, providerId, detailed);
};

const ensureProviderAuthSummaryForEntry = async (
  entry: ProviderOnboardingEntry,
  providerId: string,
  opts?: { force?: boolean; trigger?: ProviderAuthSummaryTrigger },
): Promise<ProviderOptions | undefined> => {
  if (!entry.workspaceOwnerScope || !entry.workspaceId) return undefined;

  const ready = isReadyVisibleHarnessProviderStatus(entry.snapshot.providersById[providerId]);
  if (!ready) return undefined;

  const force = opts?.force ?? false;
  const trigger = opts?.trigger ?? (force ? "explicit" : "passive");
  const requestKey = serializeProviderAuthScope(
    createProviderAuthScopeFromOptions(
      entry.workspaceOwnerScope,
      providerId,
      entry.snapshot.bootstrap.provider_options[providerId],
    ),
  );
  const existing = entry.providerAuthSummaryInFlightByKey[requestKey];
  if (existing && !force) return existing;

  const cached = getProvidersBootstrapSnapshotForScope(entry.ownerScope).provider_options[providerId];
  if (!force && cached && !shouldHydrateProviderModels(providerId, cached, trigger)) {
    return cached;
  }

  const request = (
    force
      ? loadDetailedProviderOptionsForEntry(entry, providerId)
      : loadProvidersBootstrapForScope(entry.ownerScope)
        .then(async (latestBootstrap) => {
          let next = resolveProviderOptionsUpdateForScope(
            entry.workspaceOwnerScope,
            cached,
            latestBootstrap.provider_options[providerId],
          );

          if (shouldHydrateProviderModels(providerId, next, trigger)) {
            try {
              next = await loadDetailedProviderOptionsForEntry(entry, providerId);
            } catch {
              // Keep bootstrap options when probe hydration is unavailable.
            }
          }

          return next;
        })
  )
    .finally(() => {
      if (entry.providerAuthSummaryInFlightByKey[requestKey] === request) {
        delete entry.providerAuthSummaryInFlightByKey[requestKey];
      }
    });

  entry.providerAuthSummaryInFlightByKey[requestKey] = request;
  return request;
};

const handleInstallTransitions = (entry: ProviderOnboardingEntry): void => {
  if (entry.disposed) return;

  let needsBootstrapRefresh = false;
  const completedProvidersNeedingAuthSummary: string[] = [];

  for (const [providerId, install] of Object.entries(entry.snapshot.installsById)) {
    const previous = entry.previousInstallsById[providerId];
    if (!install || install.state === "running") continue;
    if (previous?.installId === install.installId && previous.state === install.state) {
      continue;
    }

    needsBootstrapRefresh = true;
    if (previous?.installId === install.installId && previous.state === "running") {
      if (install.state === "succeeded") {
        trackProviderInstallCompleted({
          providerId,
          target: install.target,
        });
      } else if (install.state === "failed" || install.state === "cancelled") {
        trackProviderInstallFailed({
          providerId,
          target: install.target,
          status: install.state,
          failureKind: normalizeProviderInstallFailureKind(install.errorCode ?? install.state),
          installErrorCode: install.errorCode,
        });
      }
    }
    if (
      entry.workspaceId
      && install.state === "succeeded"
      && !entry.postInstallHandledIds.has(install.installId)
    ) {
      entry.postInstallHandledIds.add(install.installId);
      completedProvidersNeedingAuthSummary.push(providerId);
    }
  }

  entry.previousInstallsById = entry.snapshot.installsById;

  if (!needsBootstrapRefresh) return;

  void refreshProvidersBootstrapForScope(entry.ownerScope)
    .then(() => {
      if (entry.disposed) return;
      for (const providerId of completedProvidersNeedingAuthSummary) {
        if (entry.postInstallInFlightProviderIds.has(providerId)) continue;
        entry.postInstallInFlightProviderIds.add(providerId);
        void ensureProviderAuthSummaryForEntry(entry, providerId)
          .catch(() => {})
          .finally(() => {
            entry.postInstallInFlightProviderIds.delete(providerId);
          });
      }
    })
    .catch(() => {});
};

const startEntry = (entry: ProviderOnboardingEntry): void => {
  if (entry.refCount <= 0) return;

  entry.disposed = false;
  foregroundRefreshScopeKeys.add(entry.scopeKey);
  syncForegroundRefreshListeners();

  entry.bootstrapUnsubscribe = subscribeProvidersBootstrapForScope(entry.ownerScope, () => {
    updateEntrySnapshot(entry);
    reconcileRunningInstalls(entry);
    cleanupSucceededInstalls(entry);
  });
  entry.installProgressUnsubscribe = subscribeProviderInstallProgressForScope(entry.ownerScope, (snapshot) => {
    updateEntrySnapshot(entry, snapshot);
    handleInstallTransitions(entry);
    cleanupSucceededInstalls(entry);
  });

  updateEntrySnapshot(entry);
  reconcileRunningInstalls(entry);
  handleInstallTransitions(entry);
  cleanupSucceededInstalls(entry);
};

const stopEntry = (entry: ProviderOnboardingEntry): void => {
  entry.disposed = true;
  entry.bootstrapUnsubscribe?.();
  entry.bootstrapUnsubscribe = undefined;
  entry.installProgressUnsubscribe?.();
  entry.installProgressUnsubscribe = undefined;

  for (const stop of Object.values(entry.installObserversByProviderId)) {
    stop();
  }
  entry.installObserversByProviderId = {};
  entry.providerAuthSummaryInFlightByKey = {};
  entry.postInstallInFlightProviderIds.clear();

  foregroundRefreshScopeKeys.delete(entry.scopeKey);
  syncForegroundRefreshListeners();
};

export const retainEntry = (ownerScope: OwnerScope): (() => void) => {
  const entry = getOrCreateEntry(ownerScope);
  entry.refCount += 1;
  if (entry.refCount === 1) {
    startEntry(entry);
  }

  return () => {
    const current = getProviderOnboardingEntry(ownerScope);
    if (!current) return;
    current.refCount = Math.max(0, current.refCount - 1);
    if (current.refCount === 0) {
      stopEntry(current);
    }
    maybeDeleteEntry(current);
  };
};

export const getProviderOnboardingSnapshotForOwner = (
  ownerScope: OwnerScope,
): ProviderOnboardingSnapshot => getOrCreateEntry(ownerScope).snapshot;

export const subscribeProviderOnboardingForOwner = (
  ownerScope: OwnerScope,
  listener: ProviderOnboardingListener,
): (() => void) => {
  const entry = getOrCreateEntry(ownerScope);
  entry.listeners.add(listener);
  return () => {
    entry.listeners.delete(listener);
    maybeDeleteEntry(entry);
  };
};

export const ensureProviderAuthSummaryForOwner = (
  ownerScope: OwnerScope,
  providerId: string,
  opts?: { force?: boolean; trigger?: ProviderAuthSummaryTrigger },
): Promise<ProviderOptions | undefined> => ensureProviderAuthSummaryForEntry(
  getOrCreateEntry(ownerScope),
  providerId,
  opts,
);

export const getProviderOnboardingSnapshot = (
  workspaceId: string | null,
): ProviderOnboardingSnapshot => getProviderOnboardingSnapshotForOwner(getProviderOwnerScope(workspaceId));

export const subscribeProviderOnboarding = (
  workspaceId: string | null,
  listener: ProviderOnboardingListener,
): (() => void) => subscribeProviderOnboardingForOwner(getProviderOwnerScope(workspaceId), listener);

export const loadProviderOnboardingBootstrap = (
  workspaceId: string | null,
): Promise<ProvidersBootstrapResponse> =>
  workspaceId ? loadProvidersBootstrap(workspaceId) : loadHostProvidersBootstrap();

export const refreshProviderOnboardingBootstrap = (
  workspaceId: string | null,
): Promise<ProvidersBootstrapResponse> =>
  workspaceId ? refreshProvidersBootstrap(workspaceId) : refreshHostProvidersBootstrap();

export const ensureProviderAuthSummary = (
  workspaceId: string | null,
  providerId: string,
  opts?: { force?: boolean; trigger?: ProviderAuthSummaryTrigger },
): Promise<ProviderOptions | undefined> => ensureProviderAuthSummaryForOwner(
  getProviderOwnerScope(workspaceId),
  providerId,
  opts,
);

export const startProviderInstall = async (
  workspaceId: string | null,
  providerId: string,
): Promise<InstallStartResponse> => {
  const entry = getOrCreateEntry(getProviderOwnerScope(workspaceId));
  const target = providerInstallTargetForProvider(entry.snapshot.providersById[providerId]) ?? "host";
  let started: InstallStartResponse;
  try {
    started = await installProvider(providerId, target);
  } catch (error) {
    trackProviderInstallFailed({
      providerId,
      target,
      failureKind: "request_failed",
    });
    throw error;
  }
  trackProviderInstallStarted({
    providerId,
    target: started.target,
  });
  attachInstallObserver(entry, providerId, started.install_id, started.target);
  return started;
};

export const startAllProviderInstalls = async (
  workspaceId: string | null,
): Promise<InstallStartResponse[]> => {
  const entry = getOrCreateEntry(getProviderOwnerScope(workspaceId));
  const target = providerInstallTargetForProvider(
    entry.snapshot.bootstrap.providers.find((provider) => provider.details?.install_target),
  ) ?? "host";
  let started: InstallStartResponse[];
  try {
    started = await installAllProviders(target);
  } catch (error) {
    trackProviderInstallFailed({
      target,
      failureKind: "request_failed",
    });
    throw error;
  }
  for (const install of started) {
    trackProviderInstallStarted({
      providerId: install.provider_id,
      target: install.target,
    });
    attachInstallObserver(entry, install.provider_id, install.install_id, install.target);
  }
  return started;
};

export const cancelProviderOnboardingInstall = async (
  workspaceId: string | null,
  providerId: string,
): Promise<InstallInfo | undefined> => {
  const entry = getOrCreateEntry(getProviderOwnerScope(workspaceId));
  const install = entry.snapshot.installsById[providerId];
  const installId = install?.installId ?? entry.snapshot.providersById[providerId]?.details?.install_id;
  if (!installId) return undefined;

  const info = await cancelInstall(installId);
  upsertProviderInstallProgressForScope(entry.ownerScope, providerId, {
    installId,
    state: info.state,
    pct: computeInstallPct(info, install?.pct ?? null),
    target: info.target,
    errorCode: info.error_code,
    error: info.error,
  });
  return info;
};

export const resetProviderOnboardingCoordinatorForTests = (): void => {
  for (const entry of providerOnboardingByScope.values()) {
    stopEntry(entry);
  }
  providerOnboardingByScope.clear();
  foregroundRefreshScopeKeys.clear();
  syncForegroundRefreshListeners();
};
