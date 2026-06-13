import type { ProviderOptions, ProviderStatus } from "../../api/client";
import { isInstalledVisibleHarnessProviderStatus } from "../../utils/providerInventory";
import { hasConfiguredHarnessAuth } from "../../utils/providerAuthStatus";

const PREFERRED_DEFAULT_PROVIDER_IDS = [
  "codex",
  "claude-crp",
  "gemini",
  "qwen",
  "opencode",
  "mistral",
  "kimi",
  "auggie",
] as const;

type HarnessDraftSelection = {
  providerId: string;
  modelId: string;
};

export function getHarnessMruStorageKey(workspaceId: string): string {
  return `wb.harnessMru.${workspaceId}`;
}

export function collectSelectableHarnessProviderIds(
  providersById: Record<string, ProviderStatus>,
): string[] {
  return Object.values(providersById)
    .filter((provider) => isInstalledVisibleHarnessProviderStatus(provider))
    .map((provider) => provider.provider_id);
}

export function resolveDefaultHarnessProviderId(
  providers: ReadonlyArray<ProviderStatus | undefined>,
): string {
  const installedProviderIds = providers
    .filter((provider) => isInstalledVisibleHarnessProviderStatus(provider))
    .map((provider) => provider.provider_id);

  for (const providerId of PREFERRED_DEFAULT_PROVIDER_IDS) {
    if (installedProviderIds.includes(providerId)) {
      return providerId;
    }
  }

  return installedProviderIds[0] ?? "codex";
}

type ResolveDraftHarnessReplacementArgs = {
  draftHarness: HarnessDraftSelection | null;
  providersById: Record<string, ProviderStatus | undefined>;
  defaultProviderId: string;
};

export function resolveDraftHarnessReplacement({
  draftHarness,
  providersById,
  defaultProviderId,
}: ResolveDraftHarnessReplacementArgs): HarnessDraftSelection | null {
  if (isInstalledVisibleHarnessProviderStatus(providersById.codex) || defaultProviderId === "codex") {
    return draftHarness;
  }

  if (!draftHarness) {
    return draftHarness;
  }

  const isPlaceholderCodexDraft =
    draftHarness.providerId === "codex" && draftHarness.modelId.trim().length === 0;
  if (!isPlaceholderCodexDraft) {
    return draftHarness;
  }

  return { ...draftHarness, providerId: defaultProviderId };
}

type ResolveInitialHarnessSelectionArgs = {
  providerIds: string[];
  providerOptions: Record<string, ProviderOptions | undefined>;
  mruProviderId?: string | null;
  disableAutoselect?: boolean;
};

const NON_AUTOSELECTABLE_PROVIDER_IDS = new Set(["fake"]);

export function resolveInitialHarnessSelection({
  providerIds,
  providerOptions,
  mruProviderId,
  disableAutoselect,
}: ResolveInitialHarnessSelectionArgs): string | null {
  if (disableAutoselect) {
    return null;
  }
  const authedProviderIds = providerIds.filter((providerId) =>
    hasConfiguredHarnessAuth(providerId, providerOptions[providerId]),
  );
  const hasRealInstalledProvider = providerIds.some(
    (providerId) => !NON_AUTOSELECTABLE_PROVIDER_IDS.has(providerId),
  );

  const mru = (mruProviderId ?? "").trim();
  if (mru && authedProviderIds.includes(mru)) {
    if (NON_AUTOSELECTABLE_PROVIDER_IDS.has(mru) && hasRealInstalledProvider) {
      return null;
    }
    return mru;
  }

  if (authedProviderIds.length === 1) {
    const [onlyAuthedProviderId] = authedProviderIds;
    if (
      NON_AUTOSELECTABLE_PROVIDER_IDS.has(onlyAuthedProviderId)
      && hasRealInstalledProvider
    ) {
      return null;
    }
    return onlyAuthedProviderId;
  }

  return null;
}

export function shouldFinalizeInitialHarnessSelection(
  selectedProviderId: string | null,
): selectedProviderId is string {
  return selectedProviderId !== null;
}
