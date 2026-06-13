import type { InstallInfo } from "../../api/client";

export type SectionId =
  | "general"
  | "notifications"
  | "analytics"
  | "agent_harnesses"
  | "harness_subscriptions"
  | "models_routing"
  | "container_network"
  | "worktree_bootstrap"
  | "agent_system_prompt"
  | "workspace_attachments"
  | "merge_queue"
  | "context_pack"
  | "resource_governance"
  | "mobile_access"
  | "resource_utilization"
  | "dictation"
  | "title_generation"
  | "billing"
  | "team_enterprise"
  | "usage_analytics"
  | "dev_tools";

export type InstallSession = {
  installId: string;
  state: InstallInfo["state"];
  pct: number | null;
  target?: InstallInfo["target"];
  errorCode?: InstallInfo["error_code"];
  streamError?: string;
  error?: string;
};

export type SettingsSectionGroup = "main" | "advanced";

export type SettingsSectionMeta = {
  id: SectionId;
  label: string;
  group?: SettingsSectionGroup;
  navHidden?: boolean;
};

export type SettingsSectionComponentId =
  | "general"
  | "notifications"
  | "analytics"
  | "harness_authentication"
  | "container_network"
  | "worktree_bootstrap"
  | "agent_system_prompt"
  | "workspace_attachments"
  | "merge_queue"
  | "dictation"
  | "title_generation"
  | "team_enterprise"
  | "dev_tools"
  | "legacy";

export type HarnessAuthSubscriptionPhase = "editing" | "awaiting_browser" | "finalizing";

export type HarnessAuthModalState = {
  provider_id: string;
  stage: "choose" | "subscription" | "api_key";
  endpoint_id: string | null;
  endpoint_provider_id: string;
  gemini_endpoint_auth_type: "gemini_api_key" | "vertex_ai";
  endpoint_name: string;
  base_url: string;
  api_key: string;
  service_account_json: string;
  project_id: string;
  location: string;
  manual_model_ids: string;
  subscription_label: string;
  subscription_token: string;
  subscription_email: string;
  subscription_provider: string;
  subscription_credentials_json: string;
  subscription_config_toml: string;
  subscription_auth_token_json: string;
  subscription_oauth_creds_json: string;
  subscription_google_accounts_json: string;
  subscription_device_code?: string | null;
  subscription_auth_url: string | null;
  subscription_phase?: HarnessAuthSubscriptionPhase;
  subscription_status: string | null;
  subscription_busy: boolean;
  api_key_busy: boolean;
};
