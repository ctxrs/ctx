import type { CSSProperties } from "react";
import { Ellipsis } from "lucide-react";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "../../../components/ui/dropdown-menu";
import { providerDetailFlag } from "../../../utils/boolish";
import { HARNESS_CATALOG, type HarnessCatalogEntry, UNSUPPORTED_HARNESS_IDS } from "../../../utils/harnessCatalog";
import { PROVIDER_INSTALLS_ENABLED } from "../../../utils/providerInstallGate";
import {
  isReadyVisibleHarnessProviderStatus,
  isVisibleHarnessProviderStatus,
} from "../../../utils/providerInventory";
import { Card, Row } from "../SettingsPage.components";
import { clampPct } from "../SettingsPage.utils";
import {
  installErrorSummary,
} from "../../../utils/providerInstallUi";
import { buildHarnessAuthRows } from "../harnessAuthRows";
import {
  useHarnessAuthenticationController,
  type HarnessAuthenticationController,
} from "../hooks/useHarnessAuthenticationController";
import {
  HarnessAuthenticationModal,
  type HarnessAuthenticationModalHarness,
} from "./HarnessAuthenticationModal";

export {
  canSubmitSubscriptionModal,
  shouldAutoStartSubscriptionFlow,
  shouldSubmitClaudeSubscriptionOnEnter,
  subscriptionPrimaryActionLabel,
} from "./HarnessAuthenticationModal";

type HarnessAuthenticationSectionProps = {
  workspaceId: string | null;
  active: boolean;
  modalOnly?: boolean;
};

type HarnessAuthenticationSectionViewProps = {
  controller: HarnessAuthenticationController;
  modalOnly?: boolean;
};

export function HarnessAuthenticationSectionView({
  controller,
  modalOnly = false,
}: HarnessAuthenticationSectionViewProps) {
  const {
    providers,
    installs,
    installBusy,
    onInstallAll,
    onInstall,
    providerHarnessConfig,
    providerHarnessBusy,
    codexAccounts,
    codexAccountsBusy,
    claudeAccounts,
    claudeAccountsBusy,
    geminiAccounts,
    geminiAccountsBusy,
    qwenAccounts,
    qwenAccountsBusy,
    kimiAccounts,
    kimiAccountsBusy,
    mistralAccounts,
    mistralAccountsBusy,
    copilotAccounts,
    copilotAccountsBusy,
    cursorAccounts,
    cursorAccountsBusy,
    ampAccounts,
    ampAccountsBusy,
    harnessAuthModal,
    openHarnessAuthModal,
    closeHarnessAuthModal,
    patchHarnessAuthModal,
    submitHarnessSubscriptionModal,
    submitHarnessApiKeyModal,
    onSelectHarnessAuthRow,
    onDeleteProviderEndpoint,
    onRefreshProviderEndpointModels,
    onCodexDelete,
    onClaudeDelete,
    onGeminiDelete,
    onQwenDelete,
    onKimiDelete,
    onMistralDelete,
    onCopilotDelete,
    onCursorDelete,
    onAmpDelete,
    providerError,
    supportsHarnessEndpointConfig,
    supportsHarnessSubscriptionAuth,
    harnessEndpointRequiresBaseUrl,
  } = controller;

  const visibleProviders = providers
    .filter((provider) => isVisibleHarnessProviderStatus(provider))
    .filter((provider) => !UNSUPPORTED_HARNESS_IDS.has(provider.provider_id))
    .slice();
  const installControlsEnabled = PROVIDER_INSTALLS_ENABLED;
  const scopedVisibleProviders = visibleProviders;
  const providersById = new Map(scopedVisibleProviders.map((provider) => [provider.provider_id, provider]));

  const order = new Map<string, number>(HARNESS_CATALOG.map((harness, index) => [harness.id, index]));
  const curated = HARNESS_CATALOG.filter((harness) => providersById.has(harness.id));
  const extras: HarnessCatalogEntry[] = scopedVisibleProviders
    .filter((provider) => !order.has(provider.provider_id))
    .map((provider) => ({ id: provider.provider_id, label: provider.provider_id, logoSrc: "" }))
    .sort((a, b) => a.id.localeCompare(b.id));

  const harnesses = [...curated, ...extras];
  const harnessDisplayById = new Map<
    string,
    HarnessAuthenticationModalHarness
  >(harnesses.map((entry) => [entry.id, entry]));
  const activeModalHarness = harnessAuthModal ? harnessDisplayById.get(harnessAuthModal.provider_id) : undefined;

  return (
    <>
      {!modalOnly ? (
        <>
          <p className="settings-harness-intro">
            Authenticate each agent harness with the provider&apos;s subscription or API key.
          </p>
          {installControlsEnabled ? (
            <Card>
              <Row
                title="Install all"
                description="Installs supported harnesses to ~/.ctx/providers/agent-servers."
                control={
                  <button
                    type="button"
                    className="settings-btn"
                    onClick={() => {
                      void onInstallAll();
                    }}
                    disabled={installBusy !== null}
                  >
                    {installBusy === "all" ? "Installing…" : "Install all"}
                  </button>
                }
              />
            </Card>
          ) : null}

          <div className="settings-card settings-harness-list">
            <div className="settings-card-rows">
              {harnesses.map((harness) => {
            const id = harness.id;
            const provider = providersById.get(id);
            if (!provider) return null;

            const installed = isReadyVisibleHarnessProviderStatus(provider);
            const updateAvailable =
              providerDetailFlag(provider.details, "matrix_update_available")
              || providerDetailFlag(provider.details, "managed_dependency_update_available");
            const showInstallActions = !installed || updateAvailable;
            const installSupported = providerDetailFlag(provider.details, "install_supported");
            const installUi = installs[id];
            const installRunning = installUi?.state === "running" || providerDetailFlag(provider.details, "install_running");
            const installBusyLocal = installBusy !== null || installRunning;
            const installPct = typeof installUi?.pct === "number" ? clampPct(installUi.pct) : null;
            const installLabel =
              installBusyLocal && installPct !== null
                ? `${installPct}%`
                : installBusyLocal
                  ? "Installing…"
                  : installUi?.state === "cancelled"
                    ? "Cancelled"
                  : updateAvailable
                    ? "Update"
                    : "Install";
            const installFailureMessage = installUi?.state === "failed" || installUi?.state === "cancelled"
              ? installErrorSummary(installUi.errorCode, installUi.error)
              : null;

            const harnessCfg = providerHarnessConfig[id];
            const sourceBusy = providerHarnessBusy[id] || false;
            const authRows = buildHarnessAuthRows({
              provider_id: id,
              selected_source_kind: harnessCfg?.selected_source_kind ?? "subscription",
              selected_endpoint_id: harnessCfg?.selected_endpoint_id ?? null,
              endpoints: harnessCfg?.endpoints ?? [],
              codex_accounts: id === "codex" ? (codexAccounts?.accounts ?? []) : [],
              codex_active_account_id: id === "codex" ? (codexAccounts?.active_account_id ?? null) : null,
              claude_accounts: id === "claude-crp" ? (claudeAccounts?.accounts ?? []) : [],
              claude_active_account_id: id === "claude-crp" ? (claudeAccounts?.active_account_id ?? null) : null,
              gemini_accounts: id === "gemini" ? (geminiAccounts?.accounts ?? []) : [],
              gemini_active_account_id: id === "gemini" ? (geminiAccounts?.active_account_id ?? null) : null,
              qwen_accounts: id === "qwen" ? (qwenAccounts?.accounts ?? []) : [],
              qwen_active_account_id: id === "qwen" ? (qwenAccounts?.active_account_id ?? null) : null,
              kimi_accounts: id === "kimi" ? (kimiAccounts?.accounts ?? []) : [],
              kimi_active_account_id: id === "kimi" ? (kimiAccounts?.active_account_id ?? null) : null,
              mistral_accounts: id === "mistral" ? (mistralAccounts?.accounts ?? []) : [],
              mistral_active_account_id: id === "mistral" ? (mistralAccounts?.active_account_id ?? null) : null,
              copilot_accounts: id === "copilot" ? (copilotAccounts?.accounts ?? []) : [],
              copilot_active_account_id: id === "copilot" ? (copilotAccounts?.active_account_id ?? null) : null,
              cursor_accounts: id === "cursor" ? (cursorAccounts?.accounts ?? []) : [],
              cursor_active_account_id: id === "cursor" ? (cursorAccounts?.active_account_id ?? null) : null,
              amp_accounts: id === "amp" ? (ampAccounts?.accounts ?? []) : [],
              amp_active_account_id: id === "amp" ? (ampAccounts?.active_account_id ?? null) : null,
            });
            const addBusy = harnessAuthModal?.provider_id === id
              ? harnessAuthModal.api_key_busy || harnessAuthModal.subscription_busy
              : false;
            const rowBusy =
              sourceBusy
              || (id === "codex" && codexAccountsBusy)
              || (id === "claude-crp" && claudeAccountsBusy)
              || (id === "gemini" && geminiAccountsBusy)
              || (id === "qwen" && qwenAccountsBusy)
              || (id === "kimi" && kimiAccountsBusy)
              || (id === "mistral" && mistralAccountsBusy)
              || (id === "copilot" && copilotAccountsBusy)
              || (id === "cursor" && cursorAccountsBusy)
              || (id === "amp" && ampAccountsBusy);

            const installStyle: CSSProperties | undefined =
              installBusyLocal && installPct !== null
                ? ({ ["--settings-install-pct" as "--settings-install-pct"]: `${installPct}%` } as CSSProperties)
                : undefined;

            return (
              <div key={id} className={`settings-row settings-harness-row ${installed ? "" : "settings-harness-row-disabled"}`}>
                <div className="settings-row-left">
                  <div className="settings-row-title settings-harness-title">
                    {harness.logoSrc ? (
                      <img
                        className={`settings-harness-logo ${harness.invertInDark ? "wb-invert" : ""} ${
                          harness.invertInLight ? "wb-invert-light" : ""
                        }`}
                        src={harness.logoSrc}
                        alt=""
                      />
                    ) : (
                      <span className="settings-harness-logo-fallback" aria-hidden="true" />
                    )}
                    <span className="settings-harness-name">{harness.label}</span>
                    {installed ? (
                      <button
                        type="button"
                        className="settings-harness-add"
                        onClick={() => openHarnessAuthModal(id)}
                        disabled={addBusy}
                        title="Add authentication method"
                        aria-label={`Add auth for ${harness.label}`}
                      >
                        +
                      </button>
                    ) : null}
                  </div>
                  {installed && authRows.length > 0 ? (
                    <div className="settings-harness-auth-list" style={{ marginTop: 10 }}>
                      {authRows.map((row) => {
                        const verificationLabel =
                          row.verification_status && row.verification_status !== "unknown"
                            ? row.verification_status
                            : null;
                        const verificationClass =
                          verificationLabel === "valid"
                            ? "settings-pill-ok"
                            : verificationLabel === "invalid" || verificationLabel === "error"
                              ? "settings-pill-err"
                              : "";
                        const catalogLabel =
                          row.model_catalog_status && row.kind === "api_key"
                            ? row.model_catalog_status
                            : null;
                        const catalogClass =
                          catalogLabel === "ready"
                            ? "settings-pill-ok"
                            : catalogLabel === "manual_only"
                              ? "settings-pill"
                              : catalogLabel === "error"
                                ? "settings-pill-err"
                                : "";

                        return (
                          <div
                            key={row.key}
                            className={`settings-harness-auth-row ${row.active ? "settings-harness-auth-row-active" : ""}`}
                          >
                            <button
                              type="button"
                              className="settings-harness-auth-main"
                              onClick={() => {
                                void onSelectHarnessAuthRow(id, row);
                              }}
                              disabled={rowBusy || !row.selectable}
                            >
                              <span className="settings-harness-auth-kind">
                                {row.kind === "subscription" ? "Subscription" : "API Key"}
                              </span>
                              <span className="settings-harness-auth-primary">
                                <span className="settings-harness-auth-label">{row.label}</span>
                                {row.detail ? <span className="settings-harness-auth-detail">{row.detail}</span> : null}
                              </span>
                            </button>
                            <div className="settings-harness-auth-actions">
                              {verificationLabel ? (
                                <span className={`settings-pill ${verificationClass}`}>{verificationLabel}</span>
                              ) : null}
                              {catalogLabel ? (
                                <span className={`settings-pill ${catalogClass}`}>{catalogLabel}</span>
                              ) : null}
                              {row.last_error ? (
                                <span className="settings-pill settings-pill-err" title={row.last_error}>
                                  Error
                                </span>
                              ) : null}
                              {row.model_catalog_error ? (
                                <span className="settings-pill settings-pill-err" title={row.model_catalog_error}>
                                  Models
                                </span>
                              ) : null}
                              {row.active ? <span className="settings-pill settings-pill-ok">Active</span> : null}
                              <DropdownMenu>
                                <DropdownMenuTrigger asChild>
                                  <button
                                    type="button"
                                    className="settings-harness-auth-menu-trigger"
                                    disabled={rowBusy}
                                    aria-label="More actions"
                                    title="More actions"
                                  >
                                    <Ellipsis size={14} aria-hidden="true" />
                                  </button>
                                </DropdownMenuTrigger>
                                <DropdownMenuContent align="end">
                                  {row.active ? <DropdownMenuItem disabled>Active source</DropdownMenuItem> : null}
                                  {row.selectable && !row.active ? (
                                    <DropdownMenuItem
                                      onSelect={() => {
                                        void onSelectHarnessAuthRow(id, row);
                                      }}
                                    >
                                      Set active
                                    </DropdownMenuItem>
                                  ) : null}
                                  {row.endpoint_id && row.can_delete ? (
                                    <DropdownMenuItem
                                      onSelect={() => {
                                        void onRefreshProviderEndpointModels(id, row.endpoint_id!);
                                      }}
                                    >
                                      Refresh models
                                    </DropdownMenuItem>
                                  ) : null}
                                  {row.endpoint_id && row.can_delete ? (
                                    <DropdownMenuItem
                                      className="tw-text-[var(--error-contrast)] focus:tw-bg-[var(--error-soft)]"
                                      onSelect={() => {
                                        void onDeleteProviderEndpoint(id, row.endpoint_id!);
                                      }}
                                    >
                                      Delete
                                    </DropdownMenuItem>
                                  ) : null}
                                  {row.kind === "subscription" && id === "codex" && row.account_id && row.can_delete ? (
                                    <DropdownMenuItem
                                      className="tw-text-[var(--error-contrast)] focus:tw-bg-[var(--error-soft)]"
                                      onSelect={() => {
                                        const accountId = row.account_id;
                                        if (!accountId) return;
                                        void onCodexDelete(accountId);
                                      }}
                                    >
                                      Delete
                                    </DropdownMenuItem>
                                  ) : null}
                                  {row.kind === "subscription" && id === "claude-crp" && row.account_id && row.can_delete ? (
                                    <DropdownMenuItem
                                      className="tw-text-[var(--error-contrast)] focus:tw-bg-[var(--error-soft)]"
                                      onSelect={() => {
                                        const accountId = row.account_id;
                                        if (!accountId) return;
                                        void onClaudeDelete(accountId);
                                      }}
                                    >
                                      Delete
                                    </DropdownMenuItem>
                                  ) : null}
                                  {row.kind === "subscription" && id === "gemini" && row.account_id && row.can_delete ? (
                                    <DropdownMenuItem
                                      className="tw-text-[var(--error-contrast)] focus:tw-bg-[var(--error-soft)]"
                                      onSelect={() => {
                                        const accountId = row.account_id;
                                        if (!accountId) return;
                                        void onGeminiDelete(accountId);
                                      }}
                                    >
                                      Delete
                                    </DropdownMenuItem>
                                  ) : null}
                                  {row.kind === "subscription" && id === "qwen" && row.account_id && row.can_delete ? (
                                    <DropdownMenuItem
                                      className="tw-text-[var(--error-contrast)] focus:tw-bg-[var(--error-soft)]"
                                      onSelect={() => {
                                        const accountId = row.account_id;
                                        if (!accountId) return;
                                        void onQwenDelete(accountId);
                                      }}
                                    >
                                      Delete
                                    </DropdownMenuItem>
                                  ) : null}
                                  {row.kind === "subscription" && id === "kimi" && row.account_id && row.can_delete ? (
                                    <DropdownMenuItem
                                      className="tw-text-[var(--error-contrast)] focus:tw-bg-[var(--error-soft)]"
                                      onSelect={() => {
                                        const accountId = row.account_id;
                                        if (!accountId) return;
                                        void onKimiDelete(accountId);
                                      }}
                                    >
                                      Delete
                                    </DropdownMenuItem>
                                  ) : null}
                                  {row.kind === "subscription" && id === "mistral" && row.account_id && row.can_delete ? (
                                    <DropdownMenuItem
                                      className="tw-text-[var(--error-contrast)] focus:tw-bg-[var(--error-soft)]"
                                      onSelect={() => {
                                        const accountId = row.account_id;
                                        if (!accountId) return;
                                        void onMistralDelete(accountId);
                                      }}
                                    >
                                      Delete
                                    </DropdownMenuItem>
                                  ) : null}
                                  {row.kind === "subscription" && id === "copilot" && row.account_id && row.can_delete ? (
                                    <DropdownMenuItem
                                      className="tw-text-[var(--error-contrast)] focus:tw-bg-[var(--error-soft)]"
                                      onSelect={() => {
                                        const accountId = row.account_id;
                                        if (!accountId) return;
                                        void onCopilotDelete(accountId);
                                      }}
                                    >
                                      Delete
                                    </DropdownMenuItem>
                                  ) : null}
                                  {row.kind === "subscription" && id === "cursor" && row.account_id && row.can_delete ? (
                                    <DropdownMenuItem
                                      className="tw-text-[var(--error-contrast)] focus:tw-bg-[var(--error-soft)]"
                                      onSelect={() => {
                                        const accountId = row.account_id;
                                        if (!accountId) return;
                                        void onCursorDelete(accountId);
                                      }}
                                    >
                                      Delete
                                    </DropdownMenuItem>
                                  ) : null}
                                  {row.kind === "subscription" && id === "amp" && row.account_id && row.can_delete ? (
                                    <DropdownMenuItem
                                      className="tw-text-[var(--error-contrast)] focus:tw-bg-[var(--error-soft)]"
                                      onSelect={() => {
                                        const accountId = row.account_id;
                                        if (!accountId) return;
                                        void onAmpDelete(accountId);
                                      }}
                                    >
                                      Delete
                                    </DropdownMenuItem>
                                  ) : null}
                                </DropdownMenuContent>
                              </DropdownMenu>
                            </div>
                          </div>
                        );
                      })}
                    </div>
                  ) : null}
                </div>
                {showInstallActions ? (
                  <div className="settings-row-right settings-harness-actions">
                    {installControlsEnabled ? (
                      <button
                        type="button"
                        className="settings-btn settings-btn-secondary settings-harness-install-btn"
                        onClick={() => {
                          void onInstall(id);
                        }}
                        disabled={!installSupported || installBusyLocal}
                        style={installStyle}
                        title={
                          !installSupported
                            ? "Install not supported yet"
                            : installBusyLocal
                              ? "Install in progress"
                              : `${updateAvailable ? "Update" : "Install"} this harness`
                        }
                      >
                        {installLabel}
                      </button>
                    ) : null}
                    {installFailureMessage ? (
                      <span className="settings-harness-inline-error" title={installFailureMessage}>
                        {installFailureMessage}
                      </span>
                    ) : null}
                  </div>
                ) : null}
              </div>
            );
              })}
              {harnesses.length === 0 ? <div className="settings-empty">No harnesses.</div> : null}
            </div>
          </div>
        </>
      ) : null}

      <HarnessAuthenticationModal
        harnessAuthModal={harnessAuthModal}
        activeModalHarness={activeModalHarness}
        closeHarnessAuthModal={closeHarnessAuthModal}
        patchHarnessAuthModal={patchHarnessAuthModal}
        submitHarnessSubscriptionModal={submitHarnessSubscriptionModal}
        submitHarnessApiKeyModal={submitHarnessApiKeyModal}
        supportsHarnessEndpointConfig={supportsHarnessEndpointConfig}
        supportsHarnessSubscriptionAuth={supportsHarnessSubscriptionAuth}
        harnessEndpointRequiresBaseUrl={harnessEndpointRequiresBaseUrl}
      />

      {providerError ? <div className="settings-banner settings-banner-error">{providerError}</div> : null}
    </>
  );
}

export function HarnessAuthenticationSection({
  workspaceId,
  active,
  modalOnly = false,
}: HarnessAuthenticationSectionProps) {
  const controller = useHarnessAuthenticationController({
    workspaceId,
    enabled: active,
  });

  return (
    <HarnessAuthenticationSectionView
      controller={controller}
      modalOnly={modalOnly}
    />
  );
}
