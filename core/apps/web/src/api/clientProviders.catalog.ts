import type { ProviderStatus } from "@ctx/types";
import { apiAny } from "./clientBase";
import type {
  AmpAccountsResponse,
  AuggieAccountsResponse,
  ClaudeAccountsResponse,
  CodexAccountsResponse,
  CopilotAccountsResponse,
  CursorAccountsResponse,
  GeminiAccountsResponse,
  KimiAccountsResponse,
  MistralAccountsResponse,
  ProviderAuthImportResult,
  ProviderImportedAuthProfile,
  QwenAccountsResponse,
} from "./clientProviders.accounts";
import { withInstallTargetParam, type InstallTarget } from "./clientProviders.install";

export const listProviders = (target?: InstallTarget) =>
  apiAny<ProviderStatus[]>(withInstallTargetParam(`/api/providers`, target));

export type ProviderOptions = {
  provider_id: string;
  workspace_id: string;
  preferred_model_id?: string;
  installed?: boolean;
  probe_ok?: boolean;
  probe_error?: string;
  supports_load: boolean;
  auth_required: boolean;
  has_active_auth?: boolean;
  auth_mode?: "subscription" | "endpoint" | "none";
  account_identity?: string | null;
  auth_methods?: unknown;
  modes?: unknown;
  models?: unknown;
  verify?: unknown;
  source?: HarnessProviderSourceConfig;
  probed_at: string;
};

export const getProviderOptions = (workspaceId: string, providerId: string) =>
  apiAny<ProviderOptions>(`/api/workspaces/${workspaceId}/providers/${providerId}/options`);

export type ProvidersBootstrapResponse = {
  providers: ProviderStatus[];
  provider_options: Record<string, ProviderOptions>;
  provider_harness_config: Record<string, HarnessProviderSourceConfig>;
  codex_accounts: CodexAccountsResponse;
  claude_accounts: ClaudeAccountsResponse;
  gemini_accounts: GeminiAccountsResponse;
  qwen_accounts: QwenAccountsResponse;
  kimi_accounts: KimiAccountsResponse;
  mistral_accounts: MistralAccountsResponse;
  copilot_accounts: CopilotAccountsResponse;
  cursor_accounts: CursorAccountsResponse;
  amp_accounts: AmpAccountsResponse;
  auggie_accounts?: AuggieAccountsResponse;
};

export const getProvidersBootstrap = (workspaceId: string) =>
  apiAny<ProvidersBootstrapResponse>(`/api/workspaces/${workspaceId}/providers/bootstrap`);

export type ProviderAuthCheck = {
  provider_id: string;
  workspace_id: string;
  status: string;
  auth_required?: boolean;
  auth_methods?: unknown;
  checked_at?: string;
  message?: string;
};

export type HarnessSourceKind = "subscription" | "endpoint";
export type HarnessApiShape = "openai_responses" | "anthropic_messages";
export type HarnessEndpointVerificationStatus = "unknown" | "valid" | "invalid" | "error";
export type EndpointModelCatalogStatus = "unknown" | "ready" | "manual_only" | "error";

export type EndpointModelRecord = {
  id: string;
  name?: string | null;
};

export type HarnessEndpointRecord = {
  id: string;
  provider_id: string;
  name: string;
  base_url?: string | null;
  api_shape: HarnessApiShape;
  auth_type: string;
  model_override?: string | null;
  created_at: string;
  updated_at: string;
  last_verification_status: HarnessEndpointVerificationStatus;
  last_verification_at?: string | null;
  last_error?: string | null;
  has_api_key: boolean;
  model_catalog_status?: EndpointModelCatalogStatus;
  model_catalog_fetched_at?: string | null;
  model_catalog_error?: string | null;
  model_catalog_models?: EndpointModelRecord[];
  manual_model_ids?: string[];
  model_catalog_source?: string | null;
};

export type HarnessProviderSourceConfig = {
  provider_id: string;
  selected_source_kind: HarnessSourceKind;
  selected_endpoint_id?: string | null;
  endpoints: HarnessEndpointRecord[];
};

export type UpsertHarnessEndpointRequest = {
  endpoint_id?: string | null;
  name: string;
  base_url?: string | null;
  api_shape?: HarnessApiShape | null;
  auth_type?: string | null;
  model_override?: string | null;
  api_key?: string | null;
  service_account_json?: string | null;
  project_id?: string | null;
  location?: string | null;
  manual_model_ids?: string[] | null;
};

export type ProviderUsageSnapshot = {
  provider_id: string;
  source: string;
  fetched_at: string;
  payload?: unknown;
  error?: string;
};

export const getProviderUsage = (providerId: string, refresh?: boolean) => {
  const params = refresh ? "?refresh=true" : "";
  return apiAny<ProviderUsageSnapshot>(`/api/providers/${providerId}/usage${params}`);
};

export const getProviderHarnessConfig = (providerId: string) =>
  apiAny<HarnessProviderSourceConfig>(`/api/providers/${providerId}/harness_config`);

export const selectProviderHarnessSource = (
  providerId: string,
  sourceKind: HarnessSourceKind,
  endpointId?: string | null,
) =>
  apiAny<HarnessProviderSourceConfig>(`/api/providers/${providerId}/harness_config/select`, {
    method: "POST",
    body: JSON.stringify({
      source_kind: sourceKind,
      endpoint_id: endpointId ?? null,
    }),
  });

export const upsertProviderHarnessEndpoint = (providerId: string, req: UpsertHarnessEndpointRequest) =>
  apiAny<HarnessProviderSourceConfig>(`/api/providers/${providerId}/harness_config/endpoints`, {
    method: "POST",
    body: JSON.stringify(req),
  });

export const deleteProviderHarnessEndpoint = (providerId: string, endpointId: string) =>
  apiAny<HarnessProviderSourceConfig>(`/api/providers/${providerId}/harness_config/endpoints/${endpointId}`, {
    method: "DELETE",
  });

export const refreshProviderHarnessEndpointModels = (providerId: string, endpointId: string) =>
  apiAny<HarnessProviderSourceConfig>(`/api/providers/${providerId}/harness_config/endpoints/${endpointId}/models/refresh`, {
    method: "POST",
    body: JSON.stringify({}),
  });

export const setProviderHarnessEndpointManualModels = (
  providerId: string,
  endpointId: string,
  modelIds: string[],
) =>
  apiAny<HarnessProviderSourceConfig>(`/api/providers/${providerId}/harness_config/endpoints/${endpointId}/models/manual`, {
    method: "PUT",
    body: JSON.stringify({ model_ids: modelIds }),
  });

export const authenticateProviderForWorkspace = (
  workspaceId: string,
  providerId: string,
  methodId?: string,
) =>
  apiAny<ProviderAuthCheck>(`/api/workspaces/${workspaceId}/providers/${providerId}/authenticate`, {
    method: "POST",
    body: JSON.stringify(methodId ? { method_id: methodId } : {}),
  });

export const verifyProviderForWorkspace = (workspaceId: string, providerId: string) =>
  apiAny<ProviderAuthCheck>(`/api/workspaces/${workspaceId}/providers/${providerId}/verify`, {
    method: "POST",
    body: JSON.stringify({}),
  });

export type ProviderAuthImportCandidate = {
  id: string;
  provider_id: string;
  provider_label: string;
  kind: string;
  path: string;
  signal_strength: string;
  confidence: string;
  parse_status: string;
  unsupported_reason?: string | null;
  summary?: string | null;
  account_identity?: string | null;
  endpoint?: string | null;
  auth_type?: string | null;
  fingerprint?: string | null;
  last_modified?: string | null;
};

export const listProviderAuthImportCandidates = () =>
  apiAny<{ candidates: ProviderAuthImportCandidate[] }>(`/api/providers/auth/import/candidates`);

export const listProviderAuthImportProfiles = () =>
  apiAny<{ profiles: ProviderImportedAuthProfile[] }>(`/api/providers/auth/import/profiles`);

export const importProviderAuthCandidates = (candidateIds: string[]) =>
  apiAny<{ results: ProviderAuthImportResult[] }>(`/api/providers/auth/import`, {
    method: "POST",
    body: JSON.stringify({ candidate_ids: candidateIds }),
  });
