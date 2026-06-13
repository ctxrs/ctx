import {
  type InstallInfo,
  type InstallTarget,
  type ProviderOptions,
  type ProviderStatus,
  type ProvidersBootstrapResponse,
} from "../api/client";
import { parseInstallTarget } from "../utils/providerInstallUi";
import { isReadyVisibleHarnessProviderStatus } from "../utils/providerInventory";
import {
  EMPTY_PROVIDERS_BOOTSTRAP,
  refreshProvidersBootstrapForScope,
  hasCachedProvidersBootstrapForScope,
  resolveProviderOptionsUpdateForScope,
  getProvidersBootstrapSnapshotForScope,
} from "./providersBootstrapStore";
import {
  getProviderInstallProgressSnapshotForScope,
  resolveProviderInstallProgressSession,
  type ProviderInstallProgressSnapshot,
} from "./providerInstallProgressStore";
import { observeInstall } from "./installProgressMonitor";
import { providerDetailFlag } from "../utils/boolish";
import {
  serializeOwnerScope,
  type OwnerScope,
  type WorkspaceOwnerScope,
} from "./scopeIdentity";

export type ProviderAuthSummaryTrigger = "passive" | "explicit";
export type ProviderOnboardingBootstrapState = "idle" | "loading" | "ready" | "error";

export type ProviderOnboardingInstallState = {
  installId: string;
  state: InstallInfo["state"];
  pct: number | null;
  target?: InstallTarget;
  errorCode?: InstallInfo["error_code"];
  error?: string;
};

export type ProviderOnboardingSnapshot = {
  bootstrap: ProvidersBootstrapResponse;
  bootstrapState: ProviderOnboardingBootstrapState;
  bootstrapError: string | null;
  providersById: Record<string, ProviderStatus>;
  installsById: Record<string, ProviderOnboardingInstallState>;
};

const EMPTY_PROVIDERS_BY_ID: Record<string, ProviderStatus> = {};
const EMPTY_INSTALLS_BY_ID: Record<string, ProviderOnboardingInstallState> = {};

export const EMPTY_PROVIDER_ONBOARDING_SNAPSHOT: ProviderOnboardingSnapshot = Object.freeze({
  bootstrap: EMPTY_PROVIDERS_BOOTSTRAP,
  bootstrapState: "idle",
  bootstrapError: null,
  providersById: EMPTY_PROVIDERS_BY_ID,
  installsById: EMPTY_INSTALLS_BY_ID,
});

export type ProviderOnboardingListener = () => void;

export type ProviderOnboardingEntry = {
  scopeKey: string;
  ownerScope: OwnerScope;
  workspaceOwnerScope: WorkspaceOwnerScope | null;
  workspaceId: string | null;
  refCount: number;
  disposed: boolean;
  listeners: Set<ProviderOnboardingListener>;
  snapshot: ProviderOnboardingSnapshot;
  bootstrapUnsubscribe?: () => void;
  installProgressUnsubscribe?: () => void;
  installObserversByProviderId: Record<string, () => void>;
  previousInstallsById: Record<string, ProviderOnboardingInstallState>;
  postInstallHandledIds: Set<string>;
  postInstallInFlightProviderIds: Set<string>;
  providerAuthSummaryInFlightByKey: Record<string, Promise<ProviderOptions | undefined>>;
};

export const providerOnboardingByScope = new Map<string, ProviderOnboardingEntry>();
export const foregroundRefreshScopeKeys = new Set<string>();

let foregroundRefreshListenersInstalled = false;

export const toErrorMessage = (error: unknown): string =>
  error instanceof Error ? error.message : String(error);

const sameProviderInstallState = (
  lhs: ProviderOnboardingInstallState | undefined,
  rhs: ProviderOnboardingInstallState | undefined,
): boolean => {
  if (!lhs && !rhs) return true;
  if (!lhs || !rhs) return false;
  return lhs.installId === rhs.installId
    && lhs.state === rhs.state
    && lhs.pct === rhs.pct
    && lhs.target === rhs.target
    && lhs.errorCode === rhs.errorCode
    && lhs.error === rhs.error;
};

const sameProviderInstallStateMap = (
  lhs: Record<string, ProviderOnboardingInstallState>,
  rhs: Record<string, ProviderOnboardingInstallState>,
): boolean => {
  const lhsKeys = Object.keys(lhs);
  const rhsKeys = Object.keys(rhs);
  if (lhsKeys.length !== rhsKeys.length) return false;
  for (const key of lhsKeys) {
    if (!sameProviderInstallState(lhs[key], rhs[key])) {
      return false;
    }
  }
  return true;
};

export const getProviderAccountIdentityById = (
  bootstrap: ProvidersBootstrapResponse,
): Record<string, string | null | undefined> => ({
  codex: bootstrap.codex_accounts.active_account_id,
  "claude-crp": bootstrap.claude_accounts.active_account_id,
  gemini: bootstrap.gemini_accounts.active_account_id,
  qwen: bootstrap.qwen_accounts.active_account_id,
  kimi: bootstrap.kimi_accounts.active_account_id,
  mistral: bootstrap.mistral_accounts.active_account_id,
  copilot: bootstrap.copilot_accounts.active_account_id,
  cursor: bootstrap.cursor_accounts.active_account_id,
  amp: bootstrap.amp_accounts.active_account_id,
  auggie: bootstrap.auggie_accounts?.active_account_id,
});

export const withProviderAccountIdentity = (
  providerId: string,
  options: ProviderOptions | undefined,
  accountIdentityById: Record<string, string | null | undefined>,
): ProviderOptions | undefined => {
  if (!options) return options;
  const accountIdentity = accountIdentityById[providerId] ?? null;
  if (options.account_identity === accountIdentity) return options;
  return {
    ...options,
    account_identity: accountIdentity,
  };
};

export const mergeProviderOptionsMap = (
  previous: Record<string, ProviderOptions>,
  next: Record<string, ProviderOptions>,
  workspaceOwnerScope: WorkspaceOwnerScope | null,
): Record<string, ProviderOptions> => {
  const merged = Object.fromEntries(
    Object.entries(next).map(([providerId, options]) => {
      const resolved = resolveProviderOptionsUpdateForScope(
        workspaceOwnerScope,
        previous[providerId],
        options,
      ) ?? options;
      return [providerId, resolved] as const;
    }),
  ) as Record<string, ProviderOptions>;

  const previousKeys = Object.keys(previous);
  const mergedKeys = Object.keys(merged);
  if (
    previousKeys.length === mergedKeys.length
    && mergedKeys.every((providerId) => previous[providerId] === merged[providerId])
  ) {
    return previous;
  }
  return merged;
};

const toProvidersById = (
  providers: ProviderStatus[],
): Record<string, ProviderStatus> => Object.fromEntries(
  providers.map((provider) => [provider.provider_id, provider]),
);

export const providerInstallTargetForProvider = (
  provider: ProviderStatus | undefined,
): InstallTarget | undefined => parseInstallTarget(provider?.details?.install_target);

const toProviderInstallState = (
  session: NonNullable<ReturnType<typeof resolveProviderInstallProgressSession>>,
): ProviderOnboardingInstallState => ({
  installId: session.installId,
  state: session.state,
  pct: session.pct,
  target: session.target,
  errorCode: session.errorCode,
  error: session.error,
});

const providerInstallsFromSnapshot = (
  snapshot: ProviderInstallProgressSnapshot,
  providersById: Record<string, ProviderStatus>,
): Record<string, ProviderOnboardingInstallState> =>
  Object.fromEntries(
    Array.from(new Set([...Object.keys(snapshot), ...Object.keys(providersById)]))
      .map((providerId) => {
        const session = resolveProviderInstallProgressSession(
          snapshot,
          providerId,
          providerInstallTargetForProvider(providersById[providerId]),
        );
        return session ? ([providerId, toProviderInstallState(session)] as const) : null;
      })
      .filter((entry): entry is readonly [string, ProviderOnboardingInstallState] => entry !== null),
  );

const shouldInstallForegroundRefreshListeners = (): boolean =>
  foregroundRefreshScopeKeys.size > 0
  && typeof window !== "undefined"
  && typeof document !== "undefined";

const refreshForegroundScopes = (): void => {
  for (const scopeKey of foregroundRefreshScopeKeys) {
    const entry = providerOnboardingByScope.get(scopeKey);
    if (!entry || entry.disposed || entry.refCount <= 0) continue;
    void refreshProvidersBootstrapForScope(entry.ownerScope).catch(() => {});
  }
};

const onVisibilityChange = (): void => {
  if (document.visibilityState === "visible") {
    refreshForegroundScopes();
  }
};

export const syncForegroundRefreshListeners = (): void => {
  const shouldInstall = shouldInstallForegroundRefreshListeners();
  if (shouldInstall && !foregroundRefreshListenersInstalled) {
    window.addEventListener("focus", refreshForegroundScopes);
    window.addEventListener("online", refreshForegroundScopes);
    document.addEventListener("visibilitychange", onVisibilityChange);
    foregroundRefreshListenersInstalled = true;
    return;
  }
  if (!shouldInstall && foregroundRefreshListenersInstalled) {
    window.removeEventListener("focus", refreshForegroundScopes);
    window.removeEventListener("online", refreshForegroundScopes);
    document.removeEventListener("visibilitychange", onVisibilityChange);
    foregroundRefreshListenersInstalled = false;
  }
};

const emit = (entry: ProviderOnboardingEntry): void => {
  for (const listener of entry.listeners) {
    listener();
  }
};

const workspaceOwnerScopeFromOwner = (
  ownerScope: OwnerScope,
): WorkspaceOwnerScope | null => ownerScope.kind === "workspace" ? ownerScope : null;

const buildSnapshot = (ownerScope: OwnerScope): ProviderOnboardingSnapshot => {
  const bootstrap = getProvidersBootstrapSnapshotForScope(ownerScope);
  const bootstrapState = hasCachedProvidersBootstrapForScope(ownerScope) ? "ready" : "idle";
  const providersById = toProvidersById(bootstrap.providers);
  const installsById = providerInstallsFromSnapshot(
    getProviderInstallProgressSnapshotForScope(ownerScope),
    providersById,
  );
  return {
    bootstrap,
    bootstrapState,
    bootstrapError: null,
    providersById,
    installsById,
  };
};

export const getOrCreateEntry = (ownerScope: OwnerScope): ProviderOnboardingEntry => {
  const scopeKey = serializeOwnerScope(ownerScope);
  const existing = providerOnboardingByScope.get(scopeKey);
  if (existing) return existing;

  const workspaceOwnerScope = workspaceOwnerScopeFromOwner(ownerScope);
  const entry: ProviderOnboardingEntry = {
    scopeKey,
    ownerScope,
    workspaceOwnerScope,
    workspaceId: workspaceOwnerScope?.workspaceId ?? null,
    refCount: 0,
    disposed: false,
    listeners: new Set(),
    snapshot: buildSnapshot(ownerScope),
    installObserversByProviderId: {},
    previousInstallsById: {},
    postInstallHandledIds: new Set(),
    postInstallInFlightProviderIds: new Set(),
    providerAuthSummaryInFlightByKey: {},
  };
  providerOnboardingByScope.set(scopeKey, entry);
  return entry;
};

export const getProviderOnboardingEntry = (
  ownerScope: OwnerScope,
): ProviderOnboardingEntry | undefined => providerOnboardingByScope.get(serializeOwnerScope(ownerScope));

export const maybeDeleteEntry = (entry: ProviderOnboardingEntry): void => {
  if (entry.refCount > 0 || entry.listeners.size > 0) return;
  providerOnboardingByScope.delete(entry.scopeKey);
};

export const updateEntrySnapshot = (
  entry: ProviderOnboardingEntry,
  installProgressSnapshot?: ProviderInstallProgressSnapshot,
): void => {
  if (entry.disposed) return;

  const nextBootstrap = getProvidersBootstrapSnapshotForScope(entry.ownerScope);
  const nextBootstrapState = hasCachedProvidersBootstrapForScope(entry.ownerScope)
    ? "ready"
    : entry.snapshot.bootstrapState;
  const nextBootstrapError = nextBootstrapState === "ready" ? null : entry.snapshot.bootstrapError;
  const nextProvidersById = toProvidersById(nextBootstrap.providers);
  const nextInstallsById = providerInstallsFromSnapshot(
    installProgressSnapshot ?? getProviderInstallProgressSnapshotForScope(entry.ownerScope),
    nextProvidersById,
  );

  if (
    entry.snapshot.bootstrap === nextBootstrap
    && entry.snapshot.bootstrapState === nextBootstrapState
    && entry.snapshot.bootstrapError === nextBootstrapError
    && sameProviderInstallStateMap(entry.snapshot.installsById, nextInstallsById)
  ) {
    return;
  }

  entry.snapshot = {
    bootstrap: nextBootstrap,
    bootstrapState: nextBootstrapState,
    bootstrapError: nextBootstrapError,
    providersById: nextProvidersById,
    installsById: nextInstallsById,
  };
  emit(entry);
};

export const setEntryBootstrapState = (
  entry: ProviderOnboardingEntry,
  bootstrapState: ProviderOnboardingBootstrapState,
  bootstrapError: string | null,
): void => {
  if (
    entry.snapshot.bootstrapState === bootstrapState
    && entry.snapshot.bootstrapError === bootstrapError
  ) {
    return;
  }
  entry.snapshot = {
    ...entry.snapshot,
    bootstrapState,
    bootstrapError,
  };
  emit(entry);
};

const detachInstallObserver = (entry: ProviderOnboardingEntry, providerId: string): void => {
  const stop = entry.installObserversByProviderId[providerId];
  if (!stop) return;
  stop();
  delete entry.installObserversByProviderId[providerId];
};

export const attachInstallObserver = (
  entry: ProviderOnboardingEntry,
  providerId: string,
  installId: string,
  initialTarget?: InstallTarget,
): void => {
  const existingInstallId = entry.snapshot.installsById[providerId]?.installId;
  if (existingInstallId === installId && entry.installObserversByProviderId[providerId]) {
    return;
  }

  detachInstallObserver(entry, providerId);
  entry.installObserversByProviderId[providerId] = observeInstall(installId, {
    ownerScope: entry.ownerScope,
    providerId,
    initialState: {
      state: "running",
      pct: entry.snapshot.installsById[providerId]?.pct ?? 0,
      target: initialTarget ?? entry.snapshot.installsById[providerId]?.target,
      errorCode: undefined,
      error: undefined,
    },
  });
};

export const reconcileRunningInstalls = (entry: ProviderOnboardingEntry): void => {
  for (const provider of entry.snapshot.bootstrap.providers) {
    const installId = provider.details?.install_id;
    const running = providerDetailFlag(provider.details, "install_running");
    const tracked = entry.snapshot.installsById[provider.provider_id];
    if (
      running
      && installId
      && (!tracked || tracked.installId !== installId || tracked.state !== "running")
    ) {
      attachInstallObserver(
        entry,
        provider.provider_id,
        installId,
        providerInstallTargetForProvider(provider),
      );
    }
  }
};

export const cleanupSucceededInstalls = (entry: ProviderOnboardingEntry): void => {
  if (entry.disposed) return;

  for (const [providerId, install] of Object.entries(entry.snapshot.installsById)) {
    const provider = entry.snapshot.providersById[providerId];
    const stillRunning = providerDetailFlag(provider?.details, "install_running");
    if (install.state === "succeeded" && isReadyVisibleHarnessProviderStatus(provider) && !stillRunning) {
      detachInstallObserver(entry, providerId);
    }
  }
};
