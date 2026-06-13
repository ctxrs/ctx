import { describe, expect, it } from "vitest";
import type { HarnessAuthModalState } from "../SettingsPage.types";
import {
  canSubmitSubscriptionModal,
  shouldAutoStartSubscriptionFlow,
  shouldSubmitClaudeSubscriptionOnEnter,
  subscriptionPrimaryActionLabel,
} from "./HarnessAuthenticationSection";

function baseModal(overrides: Partial<HarnessAuthModalState> = {}): HarnessAuthModalState {
  return {
    provider_id: "claude-crp",
    stage: "subscription",
    endpoint_id: null,
    endpoint_provider_id: "openai",
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
    ...overrides,
  };
}

describe("HarnessAuthenticationSection Claude subscription submit", () => {
  it("allows starting Claude sign-in while no token is entered", () => {
    const modal = baseModal({ subscription_busy: true });

    expect(canSubmitSubscriptionModal(modal)).toBe(false);
    expect(subscriptionPrimaryActionLabel(modal)).toBe("Waiting...");
  });

  it("allows submit for Claude when setup token is provided and idle", () => {
    const modal = baseModal({
      subscription_busy: false,
      subscription_token: "sk-ant-oat01-token",
    });

    expect(canSubmitSubscriptionModal(modal)).toBe(true);
    expect(subscriptionPrimaryActionLabel(modal)).toBe("Save subscription");
    expect(shouldSubmitClaudeSubscriptionOnEnter(modal, "Enter")).toBe(true);
  });

  it("keeps Claude action as save even when token text is invalid", () => {
    const modal = baseModal({
      subscription_busy: false,
      subscription_token: "ePBMdWetJlSbZ0aR#state",
    });

    expect(canSubmitSubscriptionModal(modal)).toBe(true);
    expect(subscriptionPrimaryActionLabel(modal)).toBe("Save subscription");
  });

  it("does not submit Claude subscription for non-enter keys", () => {
    const modal = baseModal({
      subscription_busy: true,
      subscription_token: "sk-ant-oat01-token",
    });

    expect(shouldSubmitClaudeSubscriptionOnEnter(modal, "Tab")).toBe(false);
  });

  it("keeps codex subscription action as start sign-in when idle", () => {
    const modal = baseModal({
      provider_id: "codex",
      subscription_busy: false,
      subscription_token: "",
    });

    expect(subscriptionPrimaryActionLabel(modal)).toBe("Start sign-in");
  });

  it("uses save labeling for Claude when no setup token is entered", () => {
    const modal = baseModal({
      provider_id: "claude-crp",
      subscription_busy: false,
      subscription_token: "",
    });

    expect(subscriptionPrimaryActionLabel(modal)).toBe("Start sign-in");
    expect(canSubmitSubscriptionModal(modal)).toBe(true);
  });

  it("shows Kimi subscription action as start sign-in when idle", () => {
    const modal = baseModal({
      provider_id: "kimi",
      subscription_busy: false,
      subscription_token: "",
    });

    expect(subscriptionPrimaryActionLabel(modal)).toBe("Start sign-in");
  });

  it("uses GitHub sign-in action for Copilot when no fallback token is entered", () => {
    const modal = baseModal({
      provider_id: "copilot",
      subscription_busy: false,
      subscription_token: "",
    });

    expect(subscriptionPrimaryActionLabel(modal)).toBe("Sign in with GitHub");
  });

  it("keeps Copilot action as GitHub sign-in when token text is present", () => {
    const modal = baseModal({
      provider_id: "copilot",
      subscription_busy: false,
      subscription_token: "gho_abc123",
    });

    expect(subscriptionPrimaryActionLabel(modal)).toBe("Sign in with GitHub");
  });

  it("keeps submit disabled for Copilot while sign-in is busy", () => {
    const modal = baseModal({
      provider_id: "copilot",
      subscription_busy: true,
      subscription_token: "gho_abc123",
    });

    expect(canSubmitSubscriptionModal(modal)).toBe(false);
    expect(subscriptionPrimaryActionLabel(modal)).toBe("Waiting...");
  });

  it("keeps auggie subscription action as start sign-in when idle", () => {
    const modal = baseModal({
      provider_id: "auggie",
      subscription_busy: false,
      subscription_token: "",
    });

    expect(subscriptionPrimaryActionLabel(modal)).toBe("Start sign-in");
  });

  it("shows finalizing state for browser-auth providers during reconciliation", () => {
    const modal = baseModal({
      provider_id: "cursor",
      subscription_busy: true,
      subscription_phase: "finalizing",
    });

    expect(canSubmitSubscriptionModal(modal)).toBe(false);
    expect(subscriptionPrimaryActionLabel(modal)).toBe("Finalizing...");
  });

  it("auto-starts browser sign-in from stage 1 for codex, claude, gemini, kimi, cursor, amp, copilot, and auggie", () => {
    expect(shouldAutoStartSubscriptionFlow("codex")).toBe(true);
    expect(shouldAutoStartSubscriptionFlow("claude-crp")).toBe(true);
    expect(shouldAutoStartSubscriptionFlow("gemini")).toBe(true);
    expect(shouldAutoStartSubscriptionFlow("kimi")).toBe(true);
    expect(shouldAutoStartSubscriptionFlow("cursor")).toBe(true);
    expect(shouldAutoStartSubscriptionFlow("amp")).toBe(true);
    expect(shouldAutoStartSubscriptionFlow("copilot")).toBe(true);
    expect(shouldAutoStartSubscriptionFlow("auggie")).toBe(true);
  });
});
