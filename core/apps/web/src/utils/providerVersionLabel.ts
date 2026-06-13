import type { ProviderStatus } from "@ctx/types";

type ProviderDetails = ProviderStatus["details"];

const readLabel = (value?: string | null): string | null => {
  const trimmed = String(value ?? "").trim();
  return trimmed.length > 0 ? trimmed : null;
};

export type ProviderVersionDisplay = {
  primary: string | null;
  secondary: string | null;
};

export function getProviderVersionDisplay(
  provider: Pick<ProviderStatus, "version" | "details">,
): ProviderVersionDisplay {
  const details = provider.details ?? {};
  const detected = readLabel(provider.version);
  const upstream = readLabel(details.matrix_detected_upstream_version);
  if (upstream) {
    return {
      primary: upstream,
      secondary: detected && detected !== upstream ? `ctx build ${detected}` : null,
    };
  }
  return { primary: detected, secondary: null };
}

export function formatProviderVersionDisplay(
  provider: Pick<ProviderStatus, "version" | "details">,
): string | null {
  const version = getProviderVersionDisplay(provider);
  if (!version.primary) return null;
  if (!version.secondary) return version.primary;
  return `${version.primary} (${version.secondary})`;
}

export function getMatrixVersionDisplay(
  details: ProviderDetails | undefined,
  kind: "recommended" | "latest",
): string | null {
  const source = details ?? {};
  if (kind === "recommended") {
    return readLabel(source.matrix_recommended_upstream_version) ?? readLabel(source.matrix_recommended_version);
  }
  return readLabel(source.matrix_latest_upstream_version) ?? readLabel(source.matrix_latest_version);
}
