import type { HarnessAuthModalState } from "../SettingsPage.types";

export function canSubmitSubscriptionModal(modal: HarnessAuthModalState): boolean {
  if (modal.api_key_busy) return false;
  return !modal.subscription_busy;
}

export function subscriptionPrimaryActionLabel(modal: HarnessAuthModalState): string {
  if (modal.subscription_phase === "finalizing") {
    return "Finalizing...";
  }
  if (modal.provider_id === "claude-crp") {
    if (modal.subscription_busy) {
      return modal.subscription_token.trim() ? "Saving..." : "Waiting...";
    }
    return modal.subscription_token.trim() ? "Save subscription" : "Start sign-in";
  }
  if (modal.subscription_busy && !canSubmitSubscriptionModal(modal)) {
    return "Waiting...";
  }
  if (
    modal.provider_id === "codex"
    || modal.provider_id === "auggie"
    || modal.provider_id === "amp"
    || modal.provider_id === "cursor"
    || modal.provider_id === "gemini"
    || modal.provider_id === "kimi"
    || modal.provider_id === "qwen"
    || modal.provider_id === "mistral"
  ) {
    return modal.provider_id === "gemini" ? "Sign in with Google" : "Start sign-in";
  }
  if (modal.provider_id === "copilot") {
    return "Sign in with GitHub";
  }
  return "Save subscription";
}

export function shouldSubmitClaudeSubscriptionOnEnter(
  modal: HarnessAuthModalState,
  key: string,
): boolean {
  if (key !== "Enter") return false;
  if (modal.provider_id !== "claude-crp") return false;
  if (!modal.subscription_token.trim()) return false;
  return canSubmitSubscriptionModal(modal);
}

export function shouldAutoStartSubscriptionFlow(providerId: string): boolean {
  return providerId === "codex"
    || providerId === "claude-crp"
    || providerId === "gemini"
    || providerId === "qwen"
    || providerId === "kimi"
    || providerId === "cursor"
    || providerId === "amp"
    || providerId === "mistral"
    || providerId === "copilot"
    || providerId === "auggie";
}
