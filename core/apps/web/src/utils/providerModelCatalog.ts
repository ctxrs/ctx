import type { ProviderOptions } from "../api/client";

export const SUBSCRIPTION_MODEL_DISCOVERY_PROVIDER_IDS = new Set([
  "amp",
  "codex",
  "claude-crp",
  "copilot",
  "cursor",
  "gemini",
  "kimi",
  "qwen",
]);

const asRecord = (value: unknown): Record<string, unknown> => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, unknown>;
};

export const providerModelsRecord = (options?: ProviderOptions): Record<string, unknown> =>
  asRecord(options?.models);

export const providerModelsMetaRecord = (options?: ProviderOptions): Record<string, unknown> =>
  asRecord(providerModelsRecord(options).meta);

export const providerModelsList = (options?: ProviderOptions): unknown[] => {
  const record = providerModelsRecord(options);
  const list = record.availableModels ?? record.available_models ?? record.models;
  return Array.isArray(list) ? list : [];
};

export const hasProviderModels = (options?: ProviderOptions): boolean => {
  const record = providerModelsRecord(options);
  const current = record.currentModelId ?? record.current_model_id;
  if (typeof current === "string" && current.trim().length > 0) return true;
  return providerModelsList(options).length > 0;
};

export const hasFailedProviderModelProbe = (options?: ProviderOptions): boolean => {
  if (!options) return false;
  if (options.probe_ok === false) return true;
  return typeof options.probe_error === "string" && options.probe_error.trim().length > 0;
};

export const isEndpointProviderSourceSelected = (options?: ProviderOptions): boolean =>
  options?.source?.selected_source_kind === "endpoint";

export const isPinnedSubscriptionBootstrapCatalog = (options?: ProviderOptions): boolean => {
  const meta = providerModelsMetaRecord(options);
  return meta.source_kind === "subscription" && meta.refresh_pending === true;
};

export const isFinalProviderModelCatalog = (options?: ProviderOptions): boolean => {
  if (!hasProviderModels(options)) return false;
  const meta = providerModelsMetaRecord(options);
  if (meta.source_kind === "subscription") {
    return meta.refresh_pending === false;
  }
  if (meta.source_kind === "endpoint") {
    return meta.catalog_status === "ready" || meta.catalog_status === "manual_only";
  }
  return false;
};

export const isProvisionalProviderModelCatalog = (options?: ProviderOptions): boolean =>
  hasProviderModels(options) && !isFinalProviderModelCatalog(options);
