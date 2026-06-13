import type { ProviderStatus } from "../api/client";
import { providerDetailFlag } from "./boolish";
import { findHarnessCatalogEntry } from "./harnessCatalog";
import { isVisibleHarnessProviderStatus, providerUsabilityReason } from "./providerInventory";
import { formatProviderVersionDisplay, getMatrixVersionDisplay } from "./providerVersionLabel";

export type ProviderRuntimeWarningProvider = {
  providerId: string;
  label: string;
  installSupported: boolean;
  installedVersion: string | null;
  recommendedVersion: string | null;
  reason: string | null;
};

export type ProviderRuntimeWarning = {
  title: string;
  providers: ProviderRuntimeWarningProvider[];
  providerIds: string[];
  installableProviderIds: string[];
};

const ACKNOWLEDGED_WARNING_PROVIDER_IDS_STORAGE_KEY_PREFIX = "wb.provider_runtime_warning.acknowledged_provider_ids";

const labelForProvider = (providerId: string): string =>
  findHarnessCatalogEntry(providerId)?.label ?? providerId;

export const normalizeProviderRuntimeWarningIds = (providerIds: string[]): string[] =>
  Array.from(new Set(providerIds.map((providerId) => providerId.trim()).filter(Boolean))).sort();

export const providerRequiresRuntimeWarning = (
  provider: ProviderStatus,
): boolean => {
  if (!isVisibleHarnessProviderStatus(provider)) return false;
  if (!provider.installed) return false;
  return provider.health === "unsupported_version"
    || providerDetailFlag(provider.details, "matrix_update_available")
    || providerDetailFlag(provider.details, "managed_dependency_update_available")
    || providerDetailFlag(provider.details, "managed_fingerprint_mismatch");
};

const summarizeProviderRuntimeWarning = (
  provider: ProviderStatus,
): ProviderRuntimeWarningProvider | null => {
  if (!providerRequiresRuntimeWarning(provider)) return null;
  return {
    providerId: provider.provider_id,
    label: labelForProvider(provider.provider_id),
    installSupported: providerDetailFlag(provider.details, "install_supported"),
    installedVersion: formatProviderVersionDisplay(provider),
    recommendedVersion: getMatrixVersionDisplay(provider.details, "recommended"),
    reason: providerUsabilityReason(provider),
  };
};

export const buildProviderRuntimeWarning = (
  providersById: Record<string, ProviderStatus>,
): ProviderRuntimeWarning | null => {
  const flagged = Object.values(providersById)
    .map(summarizeProviderRuntimeWarning)
    .filter((provider): provider is ProviderRuntimeWarningProvider => provider !== null)
    .sort((lhs, rhs) => lhs.label.localeCompare(rhs.label));

  if (flagged.length === 0) return null;

  const providerIds = flagged.map((provider) => provider.providerId);
  const installableProviderIds = flagged
    .filter((provider) => provider.installSupported)
    .map((provider) => provider.providerId);

  return {
    title: `${flagged.length} provider runtime${flagged.length === 1 ? "" : "s"} need${flagged.length === 1 ? "s" : ""} an update.`,
    providers: flagged,
    providerIds,
    installableProviderIds,
  };
};

export const getProviderRuntimeWarningIds = (
  providersById: Record<string, ProviderStatus>,
): string[] => buildProviderRuntimeWarning(providersById)?.providerIds ?? [];

const warningAcknowledgementStorageKey = (scopeId: string): string =>
  `${ACKNOWLEDGED_WARNING_PROVIDER_IDS_STORAGE_KEY_PREFIX}.${scopeId}`;

export const readAcknowledgedProviderRuntimeWarningIds = (scopeId: string | null | undefined): string[] => {
  if (!scopeId || typeof window === "undefined") return [];
  try {
    const stored = window.sessionStorage.getItem(warningAcknowledgementStorageKey(scopeId));
    if (!stored) return [];
    const parsed = JSON.parse(stored);
    if (!Array.isArray(parsed)) return [];
    return normalizeProviderRuntimeWarningIds(parsed.filter((value): value is string => typeof value === "string"));
  } catch {
    return [];
  }
};

export const persistAcknowledgedProviderRuntimeWarningIds = (
  scopeId: string | null | undefined,
  providerIds: string[],
): string[] => {
  const normalized = normalizeProviderRuntimeWarningIds(providerIds);
  if (!scopeId || typeof window === "undefined") return normalized;
  try {
    window.sessionStorage.setItem(
      warningAcknowledgementStorageKey(scopeId),
      JSON.stringify(normalized),
    );
  } catch {
    // Ignore storage failures and rely on in-memory state for the current render tree.
  }
  return normalized;
};

export const clearAcknowledgedProviderRuntimeWarningIds = (
  scopeId: string | null | undefined,
): void => {
  if (!scopeId || typeof window === "undefined") return;
  try {
    window.sessionStorage.removeItem(warningAcknowledgementStorageKey(scopeId));
  } catch {
    // Ignore storage failures and let the next navigation clear the acknowledgement.
  }
};

export const acknowledgeProviderRuntimeWarnings = (
  scopeId: string | null | undefined,
  providerIds: string[],
): string[] => {
  const nextAcknowledgedProviderIds = normalizeProviderRuntimeWarningIds([
    ...readAcknowledgedProviderRuntimeWarningIds(scopeId),
    ...providerIds,
  ]);
  return persistAcknowledgedProviderRuntimeWarningIds(scopeId, nextAcknowledgedProviderIds);
};
