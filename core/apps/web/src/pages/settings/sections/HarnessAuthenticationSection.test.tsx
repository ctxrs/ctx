import { fireEvent, render, screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { ProviderStatus } from "../../../api/client";
import { isDesktopApp, openExternalLink } from "../../../utils/desktop";
import type { HarnessAuthModalState } from "../SettingsPage.types";
import type { useHarnessAuthenticationController } from "../hooks/useHarnessAuthenticationController";
import { HarnessAuthenticationSection } from "./HarnessAuthenticationSection";

const mockUseHarnessAuthenticationController = vi.fn();

vi.mock("../hooks/useHarnessAuthenticationController", () => ({
  useHarnessAuthenticationController: (...args: unknown[]) => mockUseHarnessAuthenticationController(...args),
}));

vi.mock("../../../utils/desktop", () => ({
  isDesktopApp: vi.fn(() => false),
  openExternalLink: vi.fn(async () => true),
}));

type HarnessAuthController = ReturnType<typeof useHarnessAuthenticationController>;

function makeProviderStatus(
  providerId: string,
  overrides: Partial<ProviderStatus> = {},
): ProviderStatus {
  return {
    provider_id: providerId,
    installed: true,
    health: "ok",
    diagnostics: [],
    details: {},
    usability: {
      usable: true,
      status: "ready",
      blocking_provider_ids: [],
      recommended_action: "none",
    },
    ...overrides,
  };
}

function makeGeminiSubscriptionModal(): HarnessAuthModalState {
  return {
    provider_id: "gemini",
    stage: "subscription",
    endpoint_id: null,
    endpoint_provider_id: "gemini",
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
    subscription_auth_url: null,
    subscription_status: null,
    subscription_busy: false,
    api_key_busy: false,
  };
}

function makeCopilotSubscriptionModal(): HarnessAuthModalState {
  return {
    provider_id: "copilot",
    stage: "subscription",
    endpoint_provider_id: "openai",
    gemini_endpoint_auth_type: "gemini_api_key",
    endpoint_name: "",
    base_url: "",
    api_key: "",
    service_account_json: "",
    project_id: "",
    location: "",
    endpoint_id: null,
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
    subscription_status: null,
    subscription_busy: false,
    api_key_busy: false,
  };
}

function makeKimiSubscriptionModal(): HarnessAuthModalState {
  return {
    provider_id: "kimi",
    stage: "subscription",
    endpoint_provider_id: "openrouter",
    gemini_endpoint_auth_type: "gemini_api_key",
    endpoint_name: "",
    base_url: "",
    api_key: "",
    service_account_json: "",
    project_id: "",
    location: "",
    endpoint_id: null,
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
    subscription_status: null,
    subscription_busy: false,
    api_key_busy: false,
  };
}

function makeApiKeyModal(providerId: "cursor" | "gemini" | "opencode" | "pi"): HarnessAuthModalState {
  return {
    provider_id: providerId,
    stage: "api_key",
    endpoint_id: null,
    endpoint_provider_id: providerId === "gemini" ? "google_ai_studio" : "other",
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
    subscription_status: null,
    subscription_busy: false,
    api_key_busy: false,
  };
}

function makeEndpointApiKeyModal(): HarnessAuthModalState {
  return {
    provider_id: "codex",
    stage: "api_key",
    endpoint_id: null,
    endpoint_provider_id: "openai",
    gemini_endpoint_auth_type: "gemini_api_key",
    endpoint_name: "",
    base_url: "https://api.openai.com/v1",
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
    subscription_status: null,
    subscription_busy: false,
    api_key_busy: false,
  };
}

function makeClaudeSubscriptionModal(): HarnessAuthModalState {
  return {
    provider_id: "claude-crp",
    stage: "subscription",
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
    subscription_status: null,
    subscription_busy: false,
    api_key_busy: false,
  };
}

function makeChooseModal(providerId: "amp" | "pi" | "claude-crp"): HarnessAuthModalState {
  return {
    provider_id: providerId,
    stage: "choose",
    endpoint_id: null,
    endpoint_provider_id: "openrouter",
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
    subscription_status: null,
    subscription_busy: false,
    api_key_busy: false,
  };
}

function makeController(
  overrides: Partial<HarnessAuthController> = {},
): HarnessAuthController {
  const asyncNoop = async () => {};
  return {
    providers: [],
    installs: {},
    installBusy: null,
    onInstallAll: asyncNoop,
    onInstall: asyncNoop,
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
    harnessAuthModal: makeGeminiSubscriptionModal(),
    openHarnessAuthModal: () => {},
    closeHarnessAuthModal: () => {},
    patchHarnessAuthModal: () => {},
    submitHarnessSubscriptionModal: asyncNoop,
    submitHarnessApiKeyModal: asyncNoop,
    onSelectHarnessAuthRow: asyncNoop,
    onDeleteProviderEndpoint: asyncNoop,
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
  } as HarnessAuthController;
}

describe("HarnessAuthenticationSection Gemini subscription modal", () => {
  it("opens Cursor provider help through the desktop browser bridge", () => {
    vi.mocked(isDesktopApp).mockReturnValue(true);
    vi.mocked(openExternalLink).mockResolvedValue(true);
    mockUseHarnessAuthenticationController.mockReturnValue(
      makeController({ harnessAuthModal: makeApiKeyModal("cursor") }),
    );

    render(
      <HarnessAuthenticationSection
        workspaceId="ws-1"
        active
        modalOnly
      />,
    );

    fireEvent.click(screen.getByRole("link", { name: "Cursor Integrations" }));

    expect(openExternalLink).toHaveBeenCalledWith("https://cursor.com/dashboard?tab=integrations");
  });

  it("shows oauth-only sign-in flow without manual JSON fields", () => {
    mockUseHarnessAuthenticationController.mockReturnValue(makeController());

    render(
      <HarnessAuthenticationSection
        workspaceId="ws-1"
        active
        modalOnly
      />,
    );

    expect(
      screen.getByText("Sign in with Google to capture managed Gemini OAuth credentials automatically."),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Sign in with Google" })).toBeInTheDocument();
    expect(screen.queryByText("Leave JSON fields blank to run guided browser sign-in.")).not.toBeInTheDocument();
    expect(screen.queryByText("OAuth Credentials JSON (fallback)")).not.toBeInTheDocument();
    expect(screen.queryByText("Google Accounts JSON (optional)")).not.toBeInTheDocument();
  });

  it("shows Claude managed setup-token copy", () => {
    mockUseHarnessAuthenticationController.mockReturnValue(
      makeController({ harnessAuthModal: makeClaudeSubscriptionModal() }),
    );

    render(
      <HarnessAuthenticationSection
        workspaceId="ws-1"
        active
        modalOnly
      />,
    );

    expect(
      screen.getByText(
        "Start the managed Claude setup-token flow here, or paste a long-lived setup token if you already have one.",
      ),
    ).toBeInTheDocument();
    expect(screen.getByText("Setup token (recommended)")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Start sign-in" })).toBeEnabled();
  });

  it("hides the Claude manual sign-in link while browser auto-open is in progress", () => {
    mockUseHarnessAuthenticationController.mockReturnValue(
      makeController({
        harnessAuthModal: {
          ...makeClaudeSubscriptionModal(),
          subscription_auth_url: "https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A58215%2Fcallback",
          subscription_status: "Waiting for Claude setup-token sign-in to complete in your browser...",
        },
      }),
    );

    render(
      <HarnessAuthenticationSection
        workspaceId="ws-1"
        active
        modalOnly
      />,
    );

    expect(screen.queryByRole("link", { name: "Open Claude sign-in" })).not.toBeInTheDocument();
  });

  it("never shows a Claude manual sign-in link", () => {
    mockUseHarnessAuthenticationController.mockReturnValue(
      makeController({
        harnessAuthModal: {
          ...makeClaudeSubscriptionModal(),
          subscription_auth_url: "https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A58215%2Fcallback",
          subscription_status: "Sign-in failed. Retry.",
        },
      }),
    );

    render(
      <HarnessAuthenticationSection
        workspaceId="ws-1"
        active
        modalOnly
      />,
    );

    expect(screen.queryByRole("link", { name: "Open Claude sign-in" })).not.toBeInTheDocument();
  });

  it("submits guided sign-in when clicking the subscription action", () => {
    const submitHarnessSubscriptionModal = vi.fn(async () => {});
    mockUseHarnessAuthenticationController.mockReturnValue(
      makeController({ submitHarnessSubscriptionModal }),
    );

    render(
      <HarnessAuthenticationSection
        workspaceId="ws-1"
        active
        modalOnly
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Sign in with Google" }));
    expect(submitHarnessSubscriptionModal).toHaveBeenCalledTimes(1);
  });

  it("does not expose a manual sign-in link when an auth URL is present", () => {
    const authUrl = "https://example.com/oauth/start";
    mockUseHarnessAuthenticationController.mockReturnValue(
      makeController({
        harnessAuthModal: {
          ...makeGeminiSubscriptionModal(),
          subscription_status: "Subscription flow failed. Check error details below.",
          subscription_auth_url: authUrl,
        },
      }),
    );

    render(
      <HarnessAuthenticationSection
        workspaceId="ws-1"
        active
        modalOnly
      />,
    );

    expect(screen.getByText("Subscription flow failed. Check error details below.")).toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "Open sign-in link" })).not.toBeInTheDocument();
    expect(authUrl).toContain("/oauth/");
  });

  it("shows Cursor provider-key help links and hides endpoint-only fields", () => {
    mockUseHarnessAuthenticationController.mockReturnValue(
      makeController({ harnessAuthModal: makeApiKeyModal("cursor") }),
    );

    render(
      <HarnessAuthenticationSection
        workspaceId="ws-1"
        active
        modalOnly
      />,
    );

    const cursorIntegrationsLink = screen.getByRole("link", { name: "Cursor Integrations" });
    expect(cursorIntegrationsLink).toHaveAttribute("href", "https://cursor.com/dashboard?tab=integrations");
    expect(screen.getByText("Label (optional)")).toBeInTheDocument();
    expect(screen.queryByText("Manual model slugs (optional)")).not.toBeInTheDocument();
    expect(screen.queryByText("Base URL (optional)")).not.toBeInTheDocument();
  });

  it("shows Gemini key links/mode selector and hides endpoint-only fields", () => {
    mockUseHarnessAuthenticationController.mockReturnValue(
      makeController({ harnessAuthModal: makeApiKeyModal("gemini") }),
    );

    render(
      <HarnessAuthenticationSection
        workspaceId="ws-1"
        active
        modalOnly
      />,
    );

    expect(screen.getByText("Gemini auth mode")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Google AI Studio" })).toHaveAttribute(
      "href",
      "https://aistudio.google.com/app/apikey",
    );
    expect(screen.getByRole("link", { name: "Google Cloud Credentials" })).toHaveAttribute(
      "href",
      "https://console.cloud.google.com/apis/credentials",
    );
    expect(screen.getByText("Label (optional)")).toBeInTheDocument();
    expect(screen.queryByText("Manual model slugs (optional)")).not.toBeInTheDocument();
    expect(screen.queryByText("Base URL (optional)")).not.toBeInTheDocument();
  });

  it("shows Vertex AI service-account fields when Gemini auth mode is vertex_ai", () => {
    const vertexModal = makeApiKeyModal("gemini");
    vertexModal.gemini_endpoint_auth_type = "vertex_ai";
    vertexModal.endpoint_provider_id = "google_vertex";
    vertexModal.base_url =
      "https://REGION-aiplatform.googleapis.com/v1/projects/PROJECT/locations/REGION/endpoints/openapi";

    mockUseHarnessAuthenticationController.mockReturnValue(
      makeController({ harnessAuthModal: vertexModal }),
    );

    render(
      <HarnessAuthenticationSection
        workspaceId="ws-1"
        active
        modalOnly
      />,
    );

    expect(screen.getByText("Service account JSON")).toBeInTheDocument();
    expect(screen.getByText("Project ID (optional)")).toBeInTheDocument();
    expect(screen.getByText("Location (optional)")).toBeInTheDocument();
  });

  it("renders provider logos in endpoint provider dropdown", () => {
    mockUseHarnessAuthenticationController.mockReturnValue(
      makeController({
        harnessAuthModal: makeEndpointApiKeyModal(),
        harnessEndpointRequiresBaseUrl: () => true,
      }),
    );

    render(
      <HarnessAuthenticationSection
        workspaceId="ws-1"
        active
        modalOnly
      />,
    );

    expect(screen.getByText("Provider")).toBeInTheDocument();
    const providerLogo = document.querySelector(".settings-endpoint-provider-logo") as HTMLImageElement | null;
    expect(providerLogo).not.toBeNull();
    expect(providerLogo?.getAttribute("src")).toContain("OpenAI.svg");
  });

  it("starts Amp browser sign-in when selecting Subscription from chooser", () => {
    const submitHarnessSubscriptionModal = vi.fn(async () => {});
    mockUseHarnessAuthenticationController.mockReturnValue(
      makeController({
        harnessAuthModal: makeChooseModal("amp"),
        submitHarnessSubscriptionModal,
      }),
    );

    render(
      <HarnessAuthenticationSection
        workspaceId="ws-1"
        active
        modalOnly
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Subscription" }));
    expect(submitHarnessSubscriptionModal).toHaveBeenCalledTimes(1);
  });

  it("starts Claude managed sign-in when selecting Subscription from chooser", () => {
    const submitHarnessSubscriptionModal = vi.fn(async () => {});
    mockUseHarnessAuthenticationController.mockReturnValue(
      makeController({
        harnessAuthModal: makeChooseModal("claude-crp"),
        submitHarnessSubscriptionModal,
      }),
    );

    render(
      <HarnessAuthenticationSection
        workspaceId="ws-1"
        active
        modalOnly
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Subscription" }));
    expect(submitHarnessSubscriptionModal).toHaveBeenCalledTimes(1);
  });

  it("shows endpoint provider selector for Pi api key auth", () => {
    mockUseHarnessAuthenticationController.mockReturnValue(
      makeController({
        harnessAuthModal: makeApiKeyModal("pi"),
        harnessEndpointRequiresBaseUrl: (providerId: string) => providerId === "pi",
      }),
    );

    render(
      <HarnessAuthenticationSection
        workspaceId="ws-1"
        active
        modalOnly
      />,
    );

    expect(screen.getByText("Provider")).toBeInTheDocument();
    expect(screen.getByText("Manual model slugs (optional)")).toBeInTheDocument();
    expect(screen.getByText("Base URL")).toBeInTheDocument();
  });

  it("closes API-key-only provider modal on Back instead of reopening chooser", () => {
    const closeHarnessAuthModal = vi.fn();
    mockUseHarnessAuthenticationController.mockReturnValue(
      makeController({
        harnessAuthModal: makeApiKeyModal("opencode"),
        supportsHarnessSubscriptionAuth: (providerId: string) => providerId !== "opencode",
        closeHarnessAuthModal,
      }),
    );

    render(
      <HarnessAuthenticationSection
        workspaceId="ws-1"
        active
        modalOnly
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    expect(closeHarnessAuthModal).toHaveBeenCalledTimes(1);
  });
});

describe("HarnessAuthenticationSection Copilot subscription modal", () => {
  it("shows token-based Copilot subscription fields", () => {
    mockUseHarnessAuthenticationController.mockReturnValue(
      makeController({ harnessAuthModal: makeCopilotSubscriptionModal() }),
    );

    render(
      <HarnessAuthenticationSection
        workspaceId="ws-1"
        active
        modalOnly
      />,
    );

    expect(
      screen.getByText("Paste a GitHub token with Copilot entitlement for the managed Copilot account."),
    ).toBeInTheDocument();
    expect(screen.getByText("Token")).toBeInTheDocument();
    expect(screen.getByPlaceholderText("Copilot subscription")).toBeInTheDocument();
    expect(screen.getByPlaceholderText("you@example.com")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Sign in with GitHub" })).toBeInTheDocument();
  });

  it("does not render device code field even when copilot modal state carries one", () => {
    mockUseHarnessAuthenticationController.mockReturnValue(
      makeController({
        harnessAuthModal: {
          ...makeCopilotSubscriptionModal(),
          subscription_device_code: "ABCD-1234",
          subscription_status: "Waiting for GitHub sign-in to complete in your browser...",
        },
      }),
    );

    render(
      <HarnessAuthenticationSection
        workspaceId="ws-1"
        active
        modalOnly
      />,
    );

    expect(screen.queryByText("GitHub device code")).not.toBeInTheDocument();
    expect(screen.queryByDisplayValue("ABCD-1234")).not.toBeInTheDocument();
  });
});

describe("HarnessAuthenticationSection Kimi subscription modal", () => {
  it("renders Kimi browser sign-in UI with auth link and device code guidance", () => {
    mockUseHarnessAuthenticationController.mockReturnValue(
      makeController({
        harnessAuthModal: {
          ...makeKimiSubscriptionModal(),
          subscription_status: "Open the Kimi sign-in link below and complete authentication in your browser...",
          subscription_auth_url: "https://kimi.example.com/login/device",
          subscription_device_code: "KIMI-1234",
        },
      }),
    );

    render(
      <HarnessAuthenticationSection
        workspaceId="ws-1"
        active
        modalOnly
      />,
    );

    expect(
      screen.getByText(
        "Start Kimi sign-in here, then complete the provider flow in your browser with the link below.",
      ),
    ).toBeInTheDocument();
    expect(screen.queryByText("Kimi credentials JSON")).not.toBeInTheDocument();
    expect(screen.queryByText("Kimi config TOML (optional)")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Start sign-in" })).toBeEnabled();
    expect(screen.getByRole("link", { name: "Open Kimi sign-in" })).toHaveAttribute(
      "href",
      "https://kimi.example.com/login/device",
    );
    expect(screen.getByText("Kimi device code")).toBeInTheDocument();
    expect(screen.getByDisplayValue("KIMI-1234")).toBeInTheDocument();
  });
});

describe("HarnessAuthenticationSection install row rendering", () => {
  it("renders running installs when provider reports install_running without crashing", () => {
    const runningProvider = makeProviderStatus("codex", {
      installed: false,
      health: "unknown",
      usability: {
        usable: false,
        status: "installable",
        blocking_provider_ids: [],
        recommended_action: "install",
      },
      details: {
        install_supported: "true",
        install_running: "true",
        install_target: "host",
      },
    });

    mockUseHarnessAuthenticationController.mockReturnValue(
      makeController({
        harnessAuthModal: null,
        providers: [runningProvider],
        installs: {},
        installBusy: null,
      }),
    );

    render(
      <HarnessAuthenticationSection
        workspaceId="ws-1"
        active
      />,
    );

    const codexRow = screen.getByText("Codex").closest(".settings-harness-row");
    expect(codexRow).not.toBeNull();
    expect(within(codexRow as HTMLElement).getByRole("button", { name: "Installing…" })).toBeInTheDocument();
    expect(within(codexRow as HTMLElement).queryByRole("button", { name: "Cancel" })).not.toBeInTheDocument();
    expect(within(codexRow as HTMLElement).queryByText(/\b(container|host)\b/i)).not.toBeInTheDocument();
  });

  it("renders update action for installed harnesses when matrix update is available", () => {
    const installedProvider = makeProviderStatus("codex", {
      details: {
        install_supported: "true",
        install_target: "host",
        matrix_update_available: "true",
      },
    });

    mockUseHarnessAuthenticationController.mockReturnValue(
      makeController({
        harnessAuthModal: null,
        providers: [installedProvider],
        installs: {},
        installBusy: null,
      }),
    );

    render(
      <HarnessAuthenticationSection
        workspaceId="ws-1"
        active
      />,
    );

    const codexRow = screen.getByText("Codex").closest(".settings-harness-row");
    expect(codexRow).not.toBeNull();
    expect(within(codexRow as HTMLElement).getByRole("button", { name: "Update" })).toBeInTheDocument();
    expect(within(codexRow as HTMLElement).queryByText(/\b(container|host)\b/i)).not.toBeInTheDocument();
  });

  it("hides install action for installed up-to-date harnesses", () => {
    const installedProvider = makeProviderStatus("codex", {
      details: {
        install_supported: "true",
        install_target: "host",
      },
    });

    mockUseHarnessAuthenticationController.mockReturnValue(
      makeController({
        harnessAuthModal: null,
        providers: [installedProvider],
        installs: {},
        installBusy: null,
      }),
    );

    render(
      <HarnessAuthenticationSection
        workspaceId="ws-1"
        active
      />,
    );

    expect(screen.queryByRole("button", { name: "Install" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Update" })).not.toBeInTheDocument();
  });

  it("shows install action for harnesses blocked on repairable dependencies", () => {
    const blockedProvider = makeProviderStatus("qwen", {
      installed: false,
      health: "error",
      details: {
        install_supported: "true",
      },
      usability: {
        usable: false,
        status: "blocked",
        reason_code: "missing_dependency",
        reason: "provider is not ready until required dependencies are installed: acp-crp-bridge",
        blocking_provider_ids: ["acp-crp-bridge"],
        recommended_action: "resolve_dependency",
      },
    });

    mockUseHarnessAuthenticationController.mockReturnValue(
      makeController({
        harnessAuthModal: null,
        providers: [blockedProvider],
        installs: {},
        installBusy: null,
      }),
    );

    render(
      <HarnessAuthenticationSection
        workspaceId="ws-1"
        active
      />,
    );

    const qwenRow = screen.getByText("Qwen Code").closest(".settings-harness-row");
    expect(qwenRow).not.toBeNull();
    expect(within(qwenRow as HTMLElement).getByRole("button", { name: "Install" })).toBeEnabled();
  });

  it("does not render dependency-only provider ids in harness authentication", () => {
    const installedProvider = makeProviderStatus("codex", {
      details: {
        install_supported: "true",
      },
    });
    const dependencyProvider = makeProviderStatus("acp-crp-bridge", {
      details: {
        provider_kind: "dependency",
      },
    });

    mockUseHarnessAuthenticationController.mockReturnValue(
      makeController({
        harnessAuthModal: null,
        providers: [installedProvider, dependencyProvider],
        installs: {},
        installBusy: null,
      }),
    );

    render(
      <HarnessAuthenticationSection
        workspaceId="ws-1"
        active
      />,
    );

    expect(screen.getByText("Codex")).toBeInTheDocument();
    expect(screen.queryByText("acp-crp-bridge")).not.toBeInTheDocument();
  });
});
