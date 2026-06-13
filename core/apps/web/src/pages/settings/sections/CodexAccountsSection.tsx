import type { ProviderUsageSnapshot } from "../../../api/client";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "../../../components/ui/select";
import { Card, Row } from "../SettingsPage.components";
import { TextInput } from "../../../components/ui/text-input";
import { formatPct, formatResetLabel, summarizeCodexUsage } from "../SettingsPage.utils";
import { useCodexAccountsController } from "../hooks/useCodexAccountsController";

type CodexAccountsSectionProps = {
  active: boolean;
};

export function CodexAccountsSection({ active }: CodexAccountsSectionProps) {
  const {
    providers,
    codexAccounts,
    codexAccountsBusy,
    codexAccountsError,
    codexUsage,
    codexUsageBusy,
    codexUsageError,
    codexImportProbe,
    codexImportBusy,
    codexCallbackBusy,
    codexCallbackUrls,
    setCodexCallbackUrls,
    codexNewLabel,
    setCodexNewLabel,
    refreshCodexUsage,
    onCodexDelete,
    onCodexSetActive,
    onCodexLogin,
    onCodexImportHost,
    openCodexAuthUrl,
    onCodexCompleteCallback,
  } = useCodexAccountsController(active);

  const codexProvider = providers.find((provider) => provider.provider_id === "codex");
  const codexAccountsList = codexAccounts?.accounts ?? [];
  const codexActiveId = codexAccounts?.active_account_id ?? null;
  const codexLogins = codexAccounts?.logins ?? [];
  const codexPendingLogins = codexLogins.filter((login) => login.status === "pending");
  const codexFailedLogins = codexLogins.filter((login) => login.status === "failed");
  const codexImportPath = codexImportProbe?.path ?? "~/.codex/auth.json";
  const codexImportLabel = codexImportProbe?.auth_kind === "oauth"
    ? "Detected subscription tokens"
    : codexImportProbe?.auth_kind === "api_key"
      ? "Detected API key auth"
      : "No import candidate detected";
  const codexImportCanRun = codexImportProbe?.available === true;
  const usageEntries = codexUsage?.entries ?? [];
  const usageById = new Map<string, ProviderUsageSnapshot>();
  for (const entry of usageEntries) {
    if (!entry.account_id) continue;
    usageById.set(entry.account_id, entry.usage);
  }

  const accountRows = codexAccountsList.map((account) => ({
    account_id: account.id,
    label: account.label,
    email: account.email ?? null,
    plan_type: account.plan_type ?? null,
    last_used_at: account.last_used_at ?? null,
  }));

  if (!codexProvider) {
    return <div className="settings-empty">Codex is not installed on this host.</div>;
  }

  return (
    <Card title="Codex">
      <Row
        title="Usage"
        description={codexUsageBusy ? "Refreshing usage…" : "Remaining usage for each saved Codex account."}
        control={
          <button
            type="button"
            className="settings-btn settings-btn-secondary"
            onClick={() => {
              void refreshCodexUsage({ refresh: true });
            }}
            disabled={codexUsageBusy}
          >
            {codexUsageBusy ? "Refreshing…" : "Refresh"}
          </button>
        }
      />
      <div className="settings-card-block">
        {accountRows.length ? (
          <div className="settings-table settings-table-codex-usage">
            <div className="settings-table-head">
              <div>Account</div>
              <div>Remaining (5h)</div>
              <div>Remaining (weekly)</div>
              <div>Credits</div>
              <div>Updated</div>
              <div />
            </div>
            {accountRows.map((account) => {
              const key = account.account_id;
              const accountId = account.account_id;
              const usage = usageById.get(key) ?? null;
              const summary = summarizeCodexUsage(usage);
              const planLabel = account.plan_type ?? summary.planType;
              const primaryResetLabel = formatResetLabel(summary.primaryResetAt);
              const secondaryResetLabel = formatResetLabel(summary.secondaryResetAt);
              const accountSub = (() => {
                const base = account.email ?? account.account_id;
                return planLabel ? `${base} · ${planLabel}` : base;
              })();
              const isActive = accountId === codexActiveId;
              return (
                <div key={key} className="settings-table-row">
                  <div>
                    <div className="settings-table-title">
                      {account.label}
                      {isActive ? (
                        <span className="settings-pill settings-pill-ok" style={{ marginLeft: 8 }}>
                          Active
                        </span>
                      ) : null}
                    </div>
                    <div className="settings-table-sub">{accountSub}</div>
                  </div>
                  <div>
                    <div className="settings-table-mono">{formatPct(summary.primaryRemaining)}</div>
                    <div className="settings-table-sub">{primaryResetLabel}</div>
                  </div>
                  <div>
                    <div className="settings-table-mono">{formatPct(summary.secondaryRemaining)}</div>
                    <div className="settings-table-sub">{secondaryResetLabel}</div>
                  </div>
                  <div>
                    <div className="settings-table-mono">{summary.creditsValue}</div>
                    <div className="settings-table-sub">{summary.creditsSub}</div>
                  </div>
                  <div>
                    <div className="settings-table-mono">{summary.updatedLabel ? summary.updatedLabel : "—"}</div>
                    {summary.source ? <div className="settings-table-sub">Source: {summary.source}</div> : null}
                    {summary.error ? (
                      <div className="settings-table-sub settings-table-sub-error">{summary.error}</div>
                    ) : null}
                  </div>
                  <div>
                    <button
                      type="button"
                      className="settings-btn settings-btn-secondary settings-btn-compact"
                      onClick={() => {
                        void onCodexDelete(accountId);
                      }}
                      disabled={codexAccountsBusy}
                    >
                      Remove
                    </button>
                  </div>
                </div>
              );
            })}
          </div>
        ) : (
          <div className="settings-empty-compact">No Codex usage data yet.</div>
        )}
        {codexUsageError ? <div className="settings-banner settings-banner-error">{codexUsageError}</div> : null}
      </div>
      <Row
        title="Active account"
        description="All Codex sessions use this account until changed."
        control={
          <Select
            value={codexActiveId ?? undefined}
            onValueChange={(value) => {
              void onCodexSetActive(value || null);
            }}
            disabled={codexAccountsBusy || codexAccountsList.length === 0}
          >
            <SelectTrigger className="tw-min-w-[10rem]">
              <SelectValue placeholder={codexAccountsList.length ? "Select an account" : "No accounts connected"} />
            </SelectTrigger>
            <SelectContent>
              {codexAccountsList.map((account) => (
                <SelectItem key={account.id} value={account.id}>
                  {account.label}
                  {account.email ? ` · ${account.email}` : ""}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        }
      />
      <Row
        title="Add account"
        description="Starts the Codex login flow on this daemon."
        control={
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap", justifyContent: "flex-end" }}>
            <TextInput
              className="settings-control"
              value={codexNewLabel}
              onChange={(e) => setCodexNewLabel(e.target.value)}
              placeholder="Label (optional)"
            />
            <button
              type="button"
              className="settings-btn settings-btn-secondary"
              onClick={() => {
                void onCodexLogin();
              }}
              disabled={codexAccountsBusy}
            >
              {codexAccountsBusy ? "Starting…" : "Log in"}
            </button>
          </div>
        }
      />
      <Row
        title="Import existing auth"
        description={`Reads ${codexImportPath} on the daemon host and imports it into ctx-managed Codex credentials.`}
        control={
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap", justifyContent: "flex-end" }}>
            <button
              type="button"
              className="settings-btn settings-btn-secondary"
              onClick={() => {
                void onCodexImportHost();
              }}
              disabled={codexImportBusy || codexAccountsBusy || !codexImportCanRun}
            >
              {codexImportBusy ? "Importing…" : "Import"}
            </button>
          </div>
        }
      />
      <div className="settings-card-block">
        <div className="settings-table-sub">
          {codexImportLabel}
          {codexImportProbe?.error ? ` · ${codexImportProbe.error}` : ""}
        </div>
        {codexPendingLogins.length ? (
          <div className="settings-table settings-table-codex-logins">
            <div className="settings-table-head">
              <div>Pending logins</div>
              <div />
            </div>
            {codexPendingLogins.map((login) => {
              const callbackBusy = codexCallbackBusy[login.account_id] === true;
              return (
                <div key={login.account_id} className="settings-table-row">
                  <div className="settings-table-sub">
                    <div>Login in progress for {login.account_id}</div>
                    <div>Expected callback: {login.expected_callback_url ?? "Not provided by provider"}</div>
                  </div>
                  <div style={{ display: "flex", gap: 8, flexWrap: "wrap", justifyContent: "flex-end" }}>
                    <button
                      type="button"
                      className="settings-btn settings-btn-secondary settings-btn-compact"
                      onClick={() => {
                        void openCodexAuthUrl(login.auth_url, {
                          accountId: login.account_id,
                          expectedCallbackUrl: login.expected_callback_url ?? null,
                          completionToken: login.completion_token ?? null,
                        });
                      }}
                    >
                      Open login
                    </button>
                    <TextInput
                      className="settings-control"
                      style={{ minWidth: 280 }}
                      placeholder={login.expected_callback_url ?? "Paste callback URL"}
                      value={codexCallbackUrls[login.account_id] ?? ""}
                      onChange={(e) => {
                        const value = e.target.value;
                        setCodexCallbackUrls((prev) => ({ ...prev, [login.account_id]: value }));
                      }}
                      disabled={callbackBusy}
                    />
                    <button
                      type="button"
                      className="settings-btn settings-btn-secondary settings-btn-compact"
                      onClick={() => {
                        void onCodexCompleteCallback(login);
                      }}
                      disabled={callbackBusy || !login.completion_token}
                    >
                      {callbackBusy ? "Completing…" : "Complete callback"}
                    </button>
                  </div>
                </div>
              );
            })}
          </div>
        ) : null}
        {codexFailedLogins.length ? (
          <div className="settings-table settings-table-codex-logins">
            <div className="settings-table-head">
              <div>Failed logins</div>
              <div />
            </div>
            {codexFailedLogins.map((login) => (
              <div key={login.account_id} className="settings-table-row">
                <div className="settings-table-sub">{login.error ?? "Login failed."}</div>
                <div>
                  <button
                    type="button"
                    className="settings-btn settings-btn-secondary settings-btn-compact"
                    onClick={() => {
                      void openCodexAuthUrl(login.auth_url);
                    }}
                  >
                    Retry
                  </button>
                </div>
              </div>
            ))}
          </div>
        ) : null}
        {codexAccountsError ? <div className="settings-banner settings-banner-error">{codexAccountsError}</div> : null}
      </div>
    </Card>
  );
}
