import type { DictationSettings, UpdateDictationSettingsRequest } from "../api/client";
import { readBoolish } from "./boolish";
import { isDesktopApp } from "./desktop";
import type { TauriModelStatus } from "./useTauriSttModelStatus";

export const DEFAULT_LIVEKIT_BASE_URL = "https://agent-gateway.livekit.cloud/v1";
export const DEFAULT_LIVEKIT_MODEL = "auto";
export const DEFAULT_LIVEKIT_LANGUAGE = "en";
export const ONBOARDING_REOPEN_COOLDOWN_MS = 400;

export type DictationOnboardingStage = "choose" | "local_setup" | "cloud_setup";

export type DictationOnboardingCloudDraft = {
  baseUrl: string;
  apiKey: string;
  apiKeySet: boolean;
  apiSecret: string;
  apiSecretSet: boolean;
  model: string;
  language: string;
};

export type DictationOnboardingState = {
  open: boolean;
  stage: DictationOnboardingStage;
  busy: boolean;
  error: string | null;
  cloud: DictationOnboardingCloudDraft;
  localModelStatus: TauriModelStatus;
  localPendingStart: boolean;
  localOptionDisabledReason: string | null;
};

export const seedCloudDraft = (settings: DictationSettings | null | undefined): DictationOnboardingCloudDraft => {
  const livekit = settings?.livekit;
  return {
    baseUrl: String(livekit?.base_url ?? DEFAULT_LIVEKIT_BASE_URL),
    apiKey: "",
    apiKeySet: readBoolish(livekit?.api_key_set) ?? false,
    apiSecret: "",
    apiSecretSet: readBoolish(livekit?.api_secret_set) ?? false,
    model: String(livekit?.model ?? DEFAULT_LIVEKIT_MODEL),
    language: String(livekit?.language ?? DEFAULT_LIVEKIT_LANGUAGE),
  };
};

export const normalizeCloudDraft = (draft: DictationOnboardingCloudDraft) => {
  const baseUrl = draft.baseUrl.trim() || DEFAULT_LIVEKIT_BASE_URL;
  const apiKey = draft.apiKey.trim();
  const apiSecret = draft.apiSecret.trim();
  const model = draft.model.trim() || DEFAULT_LIVEKIT_MODEL;
  const language = draft.language.trim() || DEFAULT_LIVEKIT_LANGUAGE;
  return { baseUrl, apiKey, apiSecret, model, language };
};

const livekitSettingsFromDraft = (draft: DictationOnboardingCloudDraft): UpdateDictationSettingsRequest["livekit"] => {
  const normalized = normalizeCloudDraft(draft);
  const livekit: UpdateDictationSettingsRequest["livekit"] = {
    base_url: normalized.baseUrl,
    model: normalized.model,
    language: normalized.language,
  };
  if (normalized.apiKey) {
    livekit.api_key = normalized.apiKey;
  }
  if (normalized.apiSecret) {
    livekit.api_secret = normalized.apiSecret;
  }
  return livekit;
};

export const cloudSettingsFromDraft = (draft: DictationOnboardingCloudDraft): UpdateDictationSettingsRequest => ({
  enabled: true,
  provider: "livekit_inference",
  livekit: livekitSettingsFromDraft(draft),
});

export const localSettingsFromDraft = (draft: DictationOnboardingCloudDraft): UpdateDictationSettingsRequest => ({
  enabled: true,
  provider: "tauri_stt",
  livekit: livekitSettingsFromDraft(draft),
});

export const runtimeSettingsFromDraft = (
  provider: DictationSettings["provider"],
  draft: DictationOnboardingCloudDraft,
): DictationSettings => {
  const normalized = normalizeCloudDraft(draft);
  return {
    enabled: true,
    provider,
    livekit: {
      base_url: normalized.baseUrl,
      api_key_set: draft.apiKeySet || Boolean(normalized.apiKey),
      api_secret_set: draft.apiSecretSet || Boolean(normalized.apiSecret),
      model: normalized.model,
      language: normalized.language,
    },
  };
};

export const needsDictationOnboarding = (settings: DictationSettings | null | undefined): boolean => {
  if (!settings) return true;
  if (!settings.enabled) return true;

  const provider = settings.provider ?? "livekit_inference";
  if (provider === "disabled") return true;

  if (provider === "tauri_stt") {
    return !isDesktopApp();
  }

  if (provider === "livekit_inference") {
    const livekit = settings.livekit;
    const hasKey = readBoolish(livekit?.api_key_set) ?? false;
    const hasSecret = readBoolish(livekit?.api_secret_set) ?? false;
    return !hasKey || !hasSecret;
  }

  return true;
};
