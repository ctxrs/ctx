import type { ProviderStatus } from "../api/client";
import { providerDetailFlag } from "./boolish";

export function isVisibleHarnessProviderStatus(
  provider: ProviderStatus | null | undefined,
): provider is ProviderStatus {
  if (!provider) return false;
  return !providerDetailFlag(provider.details, "ui_hidden")
    && provider.details?.provider_kind !== "dependency";
}

export function isInstalledVisibleHarnessProviderStatus(
  provider: ProviderStatus | null | undefined,
): provider is ProviderStatus {
  return isVisibleHarnessProviderStatus(provider)
    && provider.usability.usable === true;
}

export function isReadyVisibleHarnessProviderStatus(
  provider: ProviderStatus | null | undefined,
): provider is ProviderStatus {
  return isInstalledVisibleHarnessProviderStatus(provider);
}

export function providerUsabilityReason(
  provider: ProviderStatus | null | undefined,
): string | null {
  if (!provider) return null;
  const usabilityReason = provider.usability.reason?.trim();
  if (usabilityReason) return usabilityReason;
  const diagnostic = provider.diagnostics.find((value) => value.trim().length > 0)?.trim();
  return diagnostic ?? null;
}
