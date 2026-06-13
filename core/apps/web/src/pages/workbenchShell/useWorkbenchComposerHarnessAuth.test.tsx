import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useMemo, useState } from "react";
import { describe, expect, it, vi } from "vitest";
import type { ProviderOptions } from "../../api/client";
import type { HarnessAuthModalState } from "../SettingsPage.types";
import type { HarnessAuthenticationController } from "../settings/hooks/useHarnessAuthenticationController";
import { HarnessAuthenticationSectionView } from "../settings/sections/HarnessAuthenticationSection";
import { useWorkbenchComposerHarnessAuth } from "./useWorkbenchComposerHarnessAuth";

function makeModal(providerId: string): HarnessAuthModalState {
  return {
    provider_id: providerId,
    stage: "choose",
    endpoint_id: null,
    endpoint_provider_id: "anthropic",
    gemini_endpoint_auth_type: "gemini_api_key",
    endpoint_name: "",
    base_url: "",
    api_key: "",
    service_account_json: "",
    project_id: "",
    location: "",
    manual_model_ids: "",
    subscription_label: "",
    subscription_token: "",
    subscription_email: "",
    subscription_provider: "",
    subscription_credentials_json: "",
    subscription_config_toml: "",
    subscription_auth_token_json: "",
    subscription_oauth_creds_json: "",
    subscription_google_accounts_json: "",
    subscription_device_code: null,
    subscription_auth_url: null,
    subscription_phase: "editing",
    subscription_status: null,
    subscription_busy: false,
    api_key_busy: false,
  };
}

function makeController(overrides: Partial<HarnessAuthenticationController>): HarnessAuthenticationController {
  const asyncNoop = async () => {};
  return {
    providers: [],
    installs: {},
    installBusy: null,
    onInstallAll: asyncNoop,
    onInstall: asyncNoop,
    onCancelInstall: asyncNoop,
    providerHarnessConfig: {},
    providerHarnessBusy: {},
    codexAccounts: null,
    codexAccountsBusy: false,
    claudeAccounts: null,
    claudeAccountsBusy: false,
    geminiAccounts: null,
    geminiAccountsBusy: false,
    qwenAccounts: null,
    qwenAccountsBusy: false,
    kimiAccounts: null,
    kimiAccountsBusy: false,
    mistralAccounts: null,
    mistralAccountsBusy: false,
    copilotAccounts: null,
    copilotAccountsBusy: false,
    cursorAccounts: null,
    cursorAccountsBusy: false,
    ampAccounts: null,
    ampAccountsBusy: false,
    harnessAuthModal: null,
    openHarnessAuthModal: () => {},
    closeHarnessAuthModal: () => {},
    patchHarnessAuthModal: () => {},
    submitHarnessSubscriptionModal: asyncNoop,
    submitHarnessApiKeyModal: asyncNoop,
    onSelectHarnessAuthRow: asyncNoop,
    onDeleteProviderEndpoint: asyncNoop,
    onRefreshProviderEndpointModels: asyncNoop,
    onCodexDelete: asyncNoop,
    onClaudeDelete: asyncNoop,
    onGeminiDelete: asyncNoop,
    onQwenDelete: asyncNoop,
    onKimiDelete: asyncNoop,
    onMistralDelete: asyncNoop,
    onCopilotDelete: asyncNoop,
    onCursorDelete: asyncNoop,
    onAmpDelete: asyncNoop,
    providerError: null,
    supportsHarnessEndpointConfig: () => true,
    supportsHarnessSubscriptionAuth: () => true,
    harnessEndpointRequiresBaseUrl: () => false,
    ...overrides,
  };
}

function ComposerHarnessAuthHarness({
  ensureProviderAuthSummary,
  providerOptions,
  setSingleDraftHarness,
}: {
  ensureProviderAuthSummary: (providerId: string, opts?: { force?: boolean }) => Promise<ProviderOptions | undefined>;
  providerOptions: Record<string, ProviderOptions | undefined>;
  setSingleDraftHarness: (providerId: string) => void;
}) {
  const [modal, setModal] = useState<HarnessAuthModalState | null>(null);
  const [showView, setShowView] = useState(true);

  const controller = useMemo<HarnessAuthenticationController>(() => makeController({
    harnessAuthModal: modal,
    openHarnessAuthModal: (providerId: string) => {
      setModal(makeModal(providerId));
    },
    closeHarnessAuthModal: () => {
      setModal(null);
    },
    patchHarnessAuthModal: (patch: Partial<HarnessAuthModalState>) => {
      setModal((prev) => (prev ? { ...prev, ...patch } : prev));
    },
  }), [modal]);

  const { requestHarnessAuthFromComposer } = useWorkbenchComposerHarnessAuth({
    activeTaskId: null,
    controller,
    ensureProviderAuthSummary,
    providerOptions,
    setSingleDraftHarness,
  });

  return (
    <>
      <button type="button" onClick={() => requestHarnessAuthFromComposer("claude-crp")}>request-auth</button>
      <button type="button" onClick={() => setShowView((prev) => !prev)}>toggle-view</button>
      {showView ? <HarnessAuthenticationSectionView controller={controller} modalOnly /> : null}
    </>
  );
}

describe("useWorkbenchComposerHarnessAuth", () => {
  it("keeps modal state in the parent bridge across view remounts", async () => {
    const ensureProviderAuthSummary = vi.fn(async () => undefined);
    const setSingleDraftHarness = vi.fn();

    render(
      <ComposerHarnessAuthHarness
        ensureProviderAuthSummary={ensureProviderAuthSummary}
        providerOptions={{}}
        setSingleDraftHarness={setSingleDraftHarness}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "request-auth" }));
    expect(screen.getByText("claude-crp")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "toggle-view" }));
    expect(screen.queryByText("claude-crp")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "toggle-view" }));
    expect(screen.getByText("claude-crp")).toBeInTheDocument();
    expect(ensureProviderAuthSummary).not.toHaveBeenCalled();
    expect(setSingleDraftHarness).not.toHaveBeenCalled();
  });

  it("does not reopen a closed modal after remount and finalizes pending provider selection once", async () => {
    const ensureProviderAuthSummary = vi.fn(async () => ({ has_active_auth: true } as ProviderOptions));
    const setSingleDraftHarness = vi.fn();

    render(
      <ComposerHarnessAuthHarness
        ensureProviderAuthSummary={ensureProviderAuthSummary}
        providerOptions={{}}
        setSingleDraftHarness={setSingleDraftHarness}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "request-auth" }));
    fireEvent.click(screen.getByRole("button", { name: "Close" }));

    await waitFor(() => {
      expect(ensureProviderAuthSummary).toHaveBeenCalledWith("claude-crp", { force: true });
      expect(setSingleDraftHarness).toHaveBeenCalledWith("claude-crp");
    });

    fireEvent.click(screen.getByRole("button", { name: "toggle-view" }));
    fireEvent.click(screen.getByRole("button", { name: "toggle-view" }));

    expect(screen.queryByText("claude-crp")).not.toBeInTheDocument();
    expect(ensureProviderAuthSummary).toHaveBeenCalledTimes(1);
    expect(setSingleDraftHarness).toHaveBeenCalledTimes(1);
  });
});
