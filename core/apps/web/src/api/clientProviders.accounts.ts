import { apiAny } from "./clientBase";
import type { ProviderUsageSnapshot } from "./clientProviders.catalog";

export type CodexAccountEntry = {
  id: string;
  label: string;
  kind?: string;
  email?: string | null;
  provider_account_id?: string | null;
  plan_type?: string | null;
  created_at: string;
  last_used_at?: string | null;
};

export type ClaudeAccountEntry = {
  id: string;
  label: string;
  kind?: string;
  email?: string | null;
  subscription_type?: string | null;
  created_at: string;
  last_used_at?: string | null;
};

export type GeminiAccountEntry = {
  id: string;
  label: string;
  kind?: string;
  email?: string | null;
  created_at: string;
  last_used_at?: string | null;
};

export type QwenAccountEntry = {
  id: string;
  label: string;
  kind?: string;
  email?: string | null;
  created_at: string;
  last_used_at?: string | null;
};

export type KimiAccountEntry = {
  id: string;
  label: string;
  kind?: string;
  email?: string | null;
  created_at: string;
  last_used_at?: string | null;
};

export type MistralAccountEntry = {
  id: string;
  label: string;
  kind?: string;
  email?: string | null;
  created_at: string;
  last_used_at?: string | null;
};

export type CopilotAccountEntry = {
  id: string;
  label: string;
  kind?: string;
  email?: string | null;
  created_at: string;
  last_used_at?: string | null;
};

export type CursorAccountEntry = {
  id: string;
  label: string;
  kind?: string;
  email?: string | null;
  created_at: string;
  last_used_at?: string | null;
};

export type AmpAccountEntry = {
  id: string;
  label: string;
  kind?: string;
  email?: string | null;
  created_at: string;
  last_used_at?: string | null;
};

export type AuggieAccountEntry = {
  id: string;
  label: string;
  kind?: string;
  email?: string | null;
  created_at: string;
  last_used_at?: string | null;
};

export type CodexAccountUsageEntry = {
  account_id: string | null;
  label: string;
  email?: string | null;
  plan_type?: string | null;
  last_used_at?: string | null;
  usage: ProviderUsageSnapshot;
};

export type CodexAccountUsageResponse = {
  entries: CodexAccountUsageEntry[];
};

export type CodexLoginStatus = {
  account_id: string;
  auth_url: string;
  expected_callback_url?: string | null;
  completion_token?: string | null;
  status: string;
  error?: string | null;
};

export type CodexAccountsResponse = {
  active_account_id: string | null;
  accounts: CodexAccountEntry[];
  logins: CodexLoginStatus[];
};

export type ClaudeAccountsResponse = {
  active_account_id: string | null;
  accounts: ClaudeAccountEntry[];
};

export type GeminiAccountsResponse = {
  active_account_id: string | null;
  accounts: GeminiAccountEntry[];
};

export type QwenAccountsResponse = {
  active_account_id: string | null;
  accounts: QwenAccountEntry[];
};

export type KimiAccountsResponse = {
  active_account_id: string | null;
  accounts: KimiAccountEntry[];
};

export type MistralAccountsResponse = {
  active_account_id: string | null;
  accounts: MistralAccountEntry[];
};

export type CopilotAccountsResponse = {
  active_account_id: string | null;
  accounts: CopilotAccountEntry[];
};

export type CursorAccountsResponse = {
  active_account_id: string | null;
  accounts: CursorAccountEntry[];
};

export type AmpAccountsResponse = {
  active_account_id: string | null;
  accounts: AmpAccountEntry[];
};

export type AuggieAccountsResponse = {
  active_account_id: string | null;
  accounts: AuggieAccountEntry[];
};

export type CodexLoginStartResponse = {
  account_id: string;
  auth_url: string;
  expected_callback_url?: string | null;
  completion_token: string;
};

export type CodexLoginCompleteResponse = {
  accepted: boolean;
  status_code: number;
};

export type ClaudeLoginStatus = {
  login_id: string;
  auth_url?: string | null;
  status: string;
  account_id?: string | null;
  error?: string | null;
};

export type ClaudeLoginStartResponse = {
  login_id: string;
  auth_url?: string | null;
};

export type GeminiLoginStatus = {
  login_id: string;
  auth_url?: string | null;
  status: string;
  account_id?: string | null;
  error?: string | null;
};

export type GeminiLoginStartResponse = {
  login_id: string;
  auth_url?: string | null;
};

export type QwenLoginStatus = {
  login_id: string;
  auth_url?: string | null;
  status: string;
  account_id?: string | null;
  error?: string | null;
};

export type QwenLoginStartResponse = {
  login_id: string;
  auth_url?: string | null;
};

export type KimiLoginStatus = {
  login_id: string;
  auth_url?: string | null;
  device_code?: string | null;
  status: string;
  account_id?: string | null;
  error?: string | null;
};

export type KimiLoginStartResponse = {
  login_id: string;
  auth_url?: string | null;
  device_code?: string | null;
};

export type AmpLoginStatus = {
  login_id: string;
  auth_url?: string | null;
  status: string;
  error?: string | null;
};

export type AmpLoginStartResponse = {
  login_id: string;
  auth_url?: string | null;
};

export type CursorLoginStatus = {
  login_id: string;
  auth_url?: string | null;
  status: string;
  account_id?: string | null;
  error?: string | null;
};

export type CursorLoginStartResponse = {
  login_id: string;
  auth_url?: string | null;
};

export type MistralLoginStatus = {
  login_id: string;
  auth_url?: string | null;
  status: string;
  error?: string | null;
};

export type MistralLoginStartResponse = {
  login_id: string;
  auth_url?: string | null;
};

export type CodexHostImportProbe = {
  available: boolean;
  path?: string | null;
  auth_kind?: string | null;
  error?: string | null;
};

export type ProviderAuthImportResult = {
  candidate_id: string;
  provider_id: string;
  status: string;
  profile_id?: string | null;
  message?: string | null;
};

export type ProviderImportedAuthProfile = {
  id: string;
  provider_id: string;
  provider_label: string;
  label: string;
  account_identity?: string | null;
  endpoint?: string | null;
  auth_type?: string | null;
  source_path: string;
  source_kind: string;
  secret_fingerprint: string;
  imported_at: string;
  updated_at: string;
};

export const listCodexAccounts = () =>
  apiAny<CodexAccountsResponse>(`/api/providers/codex/accounts`);

export const probeCodexHostImport = () =>
  apiAny<CodexHostImportProbe>(`/api/providers/codex/import/host`);

export const importCodexHostAuth = (label?: string) =>
  apiAny<CodexAccountsResponse>(`/api/providers/codex/import/host`, {
    method: "POST",
    body: JSON.stringify(label ? { label } : {}),
  });

export const getCodexAccountUsage = (refresh?: boolean) => {
  const params = refresh ? "?refresh=true" : "";
  return apiAny<CodexAccountUsageResponse>(`/api/providers/codex/accounts/usage${params}`);
};

export const startCodexLogin = (label?: string) =>
  apiAny<CodexLoginStartResponse>(`/api/providers/codex/accounts/login/start`, {
    method: "POST",
    body: JSON.stringify(label ? { label } : {}),
  });

export const getCodexLogin = (accountId: string) =>
  apiAny<CodexLoginStatus>(`/api/providers/codex/accounts/login/${accountId}`);

export const completeCodexLogin = (accountId: string, callbackUrl: string, completionToken: string) =>
  apiAny<CodexLoginCompleteResponse>(`/api/providers/codex/accounts/login/${accountId}`, {
    method: "POST",
    body: JSON.stringify({
      callback_url: callbackUrl,
      completion_token: completionToken,
    }),
  });

export const setCodexActiveAccount = (accountId: string | null) =>
  apiAny<CodexAccountsResponse>(`/api/providers/codex/active-account`, {
    method: "PUT",
    body: JSON.stringify({ account_id: accountId }),
  });

export const deleteCodexAccount = (accountId: string) =>
  apiAny<CodexAccountsResponse>(`/api/providers/codex/accounts/${accountId}`, {
    method: "DELETE",
  });

export const listClaudeAccounts = () =>
  apiAny<ClaudeAccountsResponse>(`/api/providers/claude-crp/accounts`);

export const startClaudeLogin = (label?: string) =>
  apiAny<ClaudeLoginStartResponse>(`/api/providers/claude-crp/accounts/login/start`, {
    method: "POST",
    body: JSON.stringify(label ? { label } : {}),
  });

export const getClaudeLogin = (loginId: string) =>
  apiAny<ClaudeLoginStatus>(`/api/providers/claude-crp/accounts/login/${loginId}`);

export const upsertClaudeAccount = (setupToken: string, label?: string) =>
  apiAny<ClaudeAccountsResponse>(`/api/providers/claude-crp/accounts`, {
    method: "POST",
    body: JSON.stringify(label ? { setup_token: setupToken, label } : { setup_token: setupToken }),
  });

export const setClaudeActiveAccount = (accountId: string | null) =>
  apiAny<ClaudeAccountsResponse>(`/api/providers/claude-crp/active-account`, {
    method: "PUT",
    body: JSON.stringify({ account_id: accountId }),
  });

export const deleteClaudeAccount = (accountId: string) =>
  apiAny<ClaudeAccountsResponse>(`/api/providers/claude-crp/accounts/${accountId}`, {
    method: "DELETE",
  });

export const listGeminiAccounts = () =>
  apiAny<GeminiAccountsResponse>(`/api/providers/gemini/accounts`);

export const startGeminiLogin = (label?: string) =>
  apiAny<GeminiLoginStartResponse>(`/api/providers/gemini/accounts/login/start`, {
    method: "POST",
    body: JSON.stringify(label ? { label } : {}),
  });

export const getGeminiLogin = (loginId: string) =>
  apiAny<GeminiLoginStatus>(`/api/providers/gemini/accounts/login/${loginId}`);

export const listQwenAccounts = () =>
  apiAny<QwenAccountsResponse>(`/api/providers/qwen/accounts`);

export const startQwenLogin = (label?: string) =>
  apiAny<QwenLoginStartResponse>(`/api/providers/qwen/accounts/login/start`, {
    method: "POST",
    body: JSON.stringify(label ? { label } : {}),
  });

export const getQwenLogin = (loginId: string) =>
  apiAny<QwenLoginStatus>(`/api/providers/qwen/accounts/login/${loginId}`);

export const setQwenActiveAccount = (accountId: string | null) =>
  apiAny<QwenAccountsResponse>(`/api/providers/qwen/active-account`, {
    method: "PUT",
    body: JSON.stringify({ account_id: accountId }),
  });

export const deleteQwenAccount = (accountId: string) =>
  apiAny<QwenAccountsResponse>(`/api/providers/qwen/accounts/${accountId}`, {
    method: "DELETE",
  });

export const startAmpLogin = (label?: string) =>
  apiAny<AmpLoginStartResponse>(`/api/providers/amp/accounts/login/start`, {
    method: "POST",
    body: JSON.stringify(label ? { label } : {}),
  });

export const getAmpLogin = (loginId: string) =>
  apiAny<AmpLoginStatus>(`/api/providers/amp/accounts/login/${loginId}`);

export const startCursorLogin = (label?: string) =>
  apiAny<CursorLoginStartResponse>(`/api/providers/cursor/accounts/login/start`, {
    method: "POST",
    body: JSON.stringify(label ? { label } : {}),
  });

export const getCursorLogin = (loginId: string) =>
  apiAny<CursorLoginStatus>(`/api/providers/cursor/accounts/login/${loginId}`);

export const listAmpAccounts = () =>
  apiAny<AmpAccountsResponse>(`/api/providers/amp/accounts`);

export const setAmpActiveAccount = (accountId: string | null) =>
  apiAny<AmpAccountsResponse>(`/api/providers/amp/active-account`, {
    method: "PUT",
    body: JSON.stringify({ account_id: accountId }),
  });

export const deleteAmpAccount = (accountId: string) =>
  apiAny<AmpAccountsResponse>(`/api/providers/amp/accounts/${accountId}`, {
    method: "DELETE",
  });

export const listMistralAccounts = () =>
  apiAny<MistralAccountsResponse>(`/api/providers/mistral/accounts`);

export const startMistralLogin = (label?: string) =>
  apiAny<MistralLoginStartResponse>(`/api/providers/mistral/accounts/login/start`, {
    method: "POST",
    body: JSON.stringify(label ? { label } : {}),
  });

export const getMistralLogin = (loginId: string) =>
  apiAny<MistralLoginStatus>(`/api/providers/mistral/accounts/login/${loginId}`);

export const setMistralActiveAccount = (accountId: string | null) =>
  apiAny<MistralAccountsResponse>(`/api/providers/mistral/active-account`, {
    method: "PUT",
    body: JSON.stringify({ account_id: accountId }),
  });

export const deleteMistralAccount = (accountId: string) =>
  apiAny<MistralAccountsResponse>(`/api/providers/mistral/accounts/${accountId}`, {
    method: "DELETE",
  });

export const upsertGeminiAccount = (
  oauthCredsJson: string,
  opts?: { label?: string; googleAccountsJson?: string; email?: string },
) =>
  apiAny<GeminiAccountsResponse>(`/api/providers/gemini/accounts`, {
    method: "POST",
    body: JSON.stringify({
      oauth_creds_json: oauthCredsJson,
      ...(opts?.label ? { label: opts.label } : {}),
      ...(opts?.googleAccountsJson ? { google_accounts_json: opts.googleAccountsJson } : {}),
      ...(opts?.email ? { email: opts.email } : {}),
    }),
  });

export const setGeminiActiveAccount = (accountId: string | null) =>
  apiAny<GeminiAccountsResponse>(`/api/providers/gemini/active-account`, {
    method: "PUT",
    body: JSON.stringify({ account_id: accountId }),
  });

export const deleteGeminiAccount = (accountId: string) =>
  apiAny<GeminiAccountsResponse>(`/api/providers/gemini/accounts/${accountId}`, {
    method: "DELETE",
  });

export const listKimiAccounts = () =>
  apiAny<KimiAccountsResponse>(`/api/providers/kimi/accounts`);

export const startKimiLogin = (label?: string) =>
  apiAny<KimiLoginStartResponse>(`/api/providers/kimi/accounts/login/start`, {
    method: "POST",
    body: JSON.stringify(label ? { label } : {}),
  });

export const getKimiLogin = (loginId: string) =>
  apiAny<KimiLoginStatus>(`/api/providers/kimi/accounts/login/${loginId}`);

export const setKimiActiveAccount = (accountId: string | null) =>
  apiAny<KimiAccountsResponse>(`/api/providers/kimi/active-account`, {
    method: "PUT",
    body: JSON.stringify({ account_id: accountId }),
  });

export const deleteKimiAccount = (accountId: string) =>
  apiAny<KimiAccountsResponse>(`/api/providers/kimi/accounts/${accountId}`, {
    method: "DELETE",
  });

export const listCopilotAccounts = () =>
  apiAny<CopilotAccountsResponse>(`/api/providers/copilot/accounts`);

export const upsertCopilotAccount = (
  token: string,
  opts?: { label?: string; email?: string },
) =>
  apiAny<CopilotAccountsResponse>(`/api/providers/copilot/accounts`, {
    method: "POST",
    body: JSON.stringify({
      token,
      ...(opts?.label ? { label: opts.label } : {}),
      ...(opts?.email ? { email: opts.email } : {}),
    }),
  });

export const setCopilotActiveAccount = (accountId: string | null) =>
  apiAny<CopilotAccountsResponse>(`/api/providers/copilot/active-account`, {
    method: "PUT",
    body: JSON.stringify({ account_id: accountId }),
  });

export const deleteCopilotAccount = (accountId: string) =>
  apiAny<CopilotAccountsResponse>(`/api/providers/copilot/accounts/${accountId}`, {
    method: "DELETE",
  });

export const listCursorAccounts = () =>
  apiAny<CursorAccountsResponse>(`/api/providers/cursor/accounts`);

export const upsertCursorAccount = (
  token: string,
  opts?: { label?: string; email?: string },
) =>
  apiAny<CursorAccountsResponse>(`/api/providers/cursor/accounts`, {
    method: "POST",
    body: JSON.stringify({
      token,
      ...(opts?.label ? { label: opts.label } : {}),
      ...(opts?.email ? { email: opts.email } : {}),
    }),
  });

export const setCursorActiveAccount = (accountId: string | null) =>
  apiAny<CursorAccountsResponse>(`/api/providers/cursor/active-account`, {
    method: "PUT",
    body: JSON.stringify({ account_id: accountId }),
  });

export const deleteCursorAccount = (accountId: string) =>
  apiAny<CursorAccountsResponse>(`/api/providers/cursor/accounts/${accountId}`, {
    method: "DELETE",
  });
