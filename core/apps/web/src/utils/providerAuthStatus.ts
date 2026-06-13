import type { ProviderOptions } from "../api/client";

export function hasConfiguredHarnessAuth(
  providerId: string,
  providerOptions: ProviderOptions | undefined,
): boolean {
  if (!providerOptions) return false;
  if (providerOptions.has_active_auth === true) return true;

  const endpointSelected =
    providerOptions.source?.selected_source_kind === "endpoint"
    && Boolean(providerOptions.source?.selected_endpoint_id);
  if (endpointSelected) return true;

  return false;
}
