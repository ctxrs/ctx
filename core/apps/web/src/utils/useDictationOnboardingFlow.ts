import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { DictationSettings, UpdateDictationSettingsRequest } from "../api/client";
import { errorMessage } from "./errorMessage";
import { isDesktopApp } from "./desktop";
import {
  cloudSettingsFromDraft,
  DictationOnboardingCloudDraft,
  DictationOnboardingState,
  DictationOnboardingStage,
  localSettingsFromDraft,
  normalizeCloudDraft,
  ONBOARDING_REOPEN_COOLDOWN_MS,
  runtimeSettingsFromDraft,
  seedCloudDraft,
} from "./dictationControllerSettings";
import { useTauriSttModelStatus } from "./useTauriSttModelStatus";

type UseDictationOnboardingFlowOptions = {
  startDictationWithSettings: (settings: DictationSettings) => Promise<boolean>;
  persistDictationSettings: (settings: UpdateDictationSettingsRequest) => Promise<DictationSettings>;
  stopDictation: () => Promise<string>;
};

type UseDictationOnboardingFlowResult = {
  dictationOnboarding: DictationOnboardingState | null;
  openDictationOnboarding: (settings: DictationSettings | null | undefined) => void;
  dismissDictationOnboarding: () => void;
  backDictationOnboarding: () => void;
  chooseDictationOnboardingLocal: () => void;
  chooseDictationOnboardingCloud: () => void;
  updateDictationOnboardingCloud: (patch: Partial<DictationOnboardingCloudDraft>) => void;
  submitDictationOnboardingLocal: () => Promise<void>;
  submitDictationOnboardingCloud: () => Promise<void>;
};

export const useDictationOnboardingFlow = ({
  startDictationWithSettings,
  persistDictationSettings,
  stopDictation,
}: UseDictationOnboardingFlowOptions): UseDictationOnboardingFlowResult => {
  const [onboardingOpen, setOnboardingOpen] = useState(false);
  const [onboardingStage, setOnboardingStage] = useState<DictationOnboardingStage>("choose");
  const [onboardingBusy, setOnboardingBusy] = useState(false);
  const [onboardingError, setOnboardingError] = useState<string | null>(null);
  const [onboardingCloud, setOnboardingCloud] = useState<DictationOnboardingCloudDraft>(() => seedCloudDraft(null));
  const [onboardingLocalPendingStart, setOnboardingLocalPendingStart] = useState(false);
  const onboardingReopenCooldownUntilRef = useRef(0);
  const onboardingOpenRef = useRef(false);
  const onboardingSubmissionIdRef = useRef(0);

  useEffect(() => {
    onboardingOpenRef.current = onboardingOpen;
  }, [onboardingOpen]);

  const localOptionDisabledReason = useMemo(
    () => (isDesktopApp() ? null : "Local model is only available in the desktop app."),
    [],
  );

  const onboardingModelProvider: DictationSettings["provider"] =
    onboardingOpen && onboardingStage === "local_setup" ? "tauri_stt" : "livekit_inference";
  const {
    modelStatus: onboardingLocalModelStatus,
    startModelDownload: startOnboardingLocalModelDownload,
  } = useTauriSttModelStatus({
    provider: onboardingModelProvider,
    language: onboardingCloud.language,
  });

  const beginOnboardingSubmission = useCallback((): number => {
    const nextId = onboardingSubmissionIdRef.current + 1;
    onboardingSubmissionIdRef.current = nextId;
    return nextId;
  }, []);

  const isOnboardingSubmissionActive = useCallback((submissionId: number): boolean => {
    return onboardingSubmissionIdRef.current === submissionId && onboardingOpenRef.current;
  }, []);

  const dismissDictationOnboarding = useCallback(() => {
    onboardingSubmissionIdRef.current += 1;
    onboardingOpenRef.current = false;
    setOnboardingOpen(false);
    setOnboardingStage("choose");
    setOnboardingBusy(false);
    setOnboardingError(null);
    setOnboardingLocalPendingStart(false);
    onboardingReopenCooldownUntilRef.current = Date.now() + ONBOARDING_REOPEN_COOLDOWN_MS;
  }, []);

  const openDictationOnboarding = useCallback(
    (settings: DictationSettings | null | undefined) => {
      if (onboardingOpen) return;
      if (Date.now() < onboardingReopenCooldownUntilRef.current) return;
      onboardingSubmissionIdRef.current += 1;
      onboardingOpenRef.current = true;
      setOnboardingCloud(seedCloudDraft(settings));
      setOnboardingStage("choose");
      setOnboardingBusy(false);
      setOnboardingError(null);
      setOnboardingLocalPendingStart(false);
      setOnboardingOpen(true);
    },
    [onboardingOpen],
  );

  const chooseDictationOnboardingLocal = useCallback(() => {
    if (localOptionDisabledReason) {
      setOnboardingError(localOptionDisabledReason);
      return;
    }
    setOnboardingError(null);
    setOnboardingStage("local_setup");
  }, [localOptionDisabledReason]);

  const chooseDictationOnboardingCloud = useCallback(() => {
    setOnboardingError(null);
    setOnboardingStage("cloud_setup");
  }, []);

  const backDictationOnboarding = useCallback(() => {
    onboardingSubmissionIdRef.current += 1;
    setOnboardingError(null);
    setOnboardingBusy(false);
    setOnboardingLocalPendingStart(false);
    setOnboardingStage("choose");
  }, []);

  const updateDictationOnboardingCloud = useCallback((patch: Partial<DictationOnboardingCloudDraft>) => {
    setOnboardingCloud((prev) => ({ ...prev, ...patch }));
  }, []);

  const finishLocalOnboardingStart = useCallback(
    async (submissionId: number) => {
      const localSettings = localSettingsFromDraft(onboardingCloud);
      const started = await startDictationWithSettings(runtimeSettingsFromDraft("tauri_stt", onboardingCloud));
      if (!isOnboardingSubmissionActive(submissionId)) {
        if (started) {
          await stopDictation().catch(() => {});
        }
        return;
      }
      if (!started) {
        setOnboardingError("Failed to start local dictation.");
        return;
      }

      try {
        await persistDictationSettings(localSettings);
      } catch (e: unknown) {
        if (!isOnboardingSubmissionActive(submissionId)) {
          await stopDictation().catch(() => {});
          return;
        }
        await stopDictation().catch(() => {});
        setOnboardingError(errorMessage(e) || "Failed to save local dictation settings.");
        return;
      }

      if (!isOnboardingSubmissionActive(submissionId)) {
        await stopDictation().catch(() => {});
        return;
      }

      dismissDictationOnboarding();
    },
    [
      dismissDictationOnboarding,
      isOnboardingSubmissionActive,
      onboardingCloud,
      persistDictationSettings,
      startDictationWithSettings,
      stopDictation,
    ],
  );

  const submitDictationOnboardingCloud = useCallback(async () => {
    if (!onboardingOpen || onboardingBusy) return;

    const normalized = normalizeCloudDraft(onboardingCloud);
    if (!onboardingCloud.apiKeySet && !normalized.apiKey) {
      setOnboardingError("API key is required.");
      return;
    }
    if (!onboardingCloud.apiSecretSet && !normalized.apiSecret) {
      setOnboardingError("API secret is required.");
      return;
    }

    const submissionId = beginOnboardingSubmission();
    setOnboardingBusy(true);
    setOnboardingError(null);
    try {
      const persisted = await persistDictationSettings(cloudSettingsFromDraft(onboardingCloud));
      if (!isOnboardingSubmissionActive(submissionId)) return;
      const started = await startDictationWithSettings(persisted);
      if (!isOnboardingSubmissionActive(submissionId)) {
        if (started) {
          await stopDictation().catch(() => {});
        }
        return;
      }
      if (!started) {
        setOnboardingError("Failed to start cloud dictation.");
        return;
      }
      dismissDictationOnboarding();
    } catch (e: unknown) {
      if (isOnboardingSubmissionActive(submissionId)) {
        setOnboardingError(errorMessage(e) || "Failed to configure cloud dictation.");
      }
    } finally {
      if (onboardingSubmissionIdRef.current === submissionId) {
        setOnboardingBusy(false);
      }
    }
  }, [
    beginOnboardingSubmission,
    dismissDictationOnboarding,
    isOnboardingSubmissionActive,
    onboardingBusy,
    onboardingCloud,
    onboardingOpen,
    persistDictationSettings,
    startDictationWithSettings,
    stopDictation,
  ]);

  const submitDictationOnboardingLocal = useCallback(async () => {
    if (!onboardingOpen || onboardingBusy) return;
    if (localOptionDisabledReason) {
      setOnboardingError(localOptionDisabledReason);
      return;
    }
    if (onboardingLocalModelStatus.status === "checking" || onboardingLocalModelStatus.status === "idle") {
      setOnboardingError("Checking local model status. Please wait.");
      return;
    }

    const submissionId = beginOnboardingSubmission();
    setOnboardingBusy(true);
    setOnboardingError(null);

    try {
      if (onboardingLocalModelStatus.installed || onboardingLocalModelStatus.status === "ready") {
        await finishLocalOnboardingStart(submissionId);
        return;
      }

      setOnboardingLocalPendingStart(true);
      await startOnboardingLocalModelDownload();
    } catch (e: unknown) {
      if (isOnboardingSubmissionActive(submissionId)) {
        setOnboardingLocalPendingStart(false);
        setOnboardingError(errorMessage(e) || "Failed to configure local dictation.");
      }
    } finally {
      if (onboardingSubmissionIdRef.current === submissionId) {
        setOnboardingBusy(false);
      }
    }
  }, [
    beginOnboardingSubmission,
    finishLocalOnboardingStart,
    isOnboardingSubmissionActive,
    localOptionDisabledReason,
    onboardingBusy,
    onboardingLocalModelStatus.installed,
    onboardingLocalModelStatus.status,
    onboardingOpen,
    startOnboardingLocalModelDownload,
  ]);

  useEffect(() => {
    if (!onboardingOpen) return;
    if (onboardingStage !== "local_setup") return;
    if (!onboardingLocalPendingStart) return;

    if (onboardingLocalModelStatus.status === "error") {
      setOnboardingLocalPendingStart(false);
      setOnboardingError(onboardingLocalModelStatus.error ?? "Local model download failed.");
      return;
    }

    if (!onboardingLocalModelStatus.installed && onboardingLocalModelStatus.status !== "ready") {
      return;
    }

    const submissionId = beginOnboardingSubmission();
    setOnboardingLocalPendingStart(false);
    setOnboardingBusy(true);
    setOnboardingError(null);

    void (async () => {
      try {
        await finishLocalOnboardingStart(submissionId);
      } catch (e: unknown) {
        if (!isOnboardingSubmissionActive(submissionId)) return;
        setOnboardingError(errorMessage(e) || "Failed to configure local dictation.");
      } finally {
        if (onboardingSubmissionIdRef.current === submissionId) {
          setOnboardingBusy(false);
        }
      }
    })();
  }, [
    beginOnboardingSubmission,
    finishLocalOnboardingStart,
    isOnboardingSubmissionActive,
    onboardingLocalModelStatus.error,
    onboardingLocalModelStatus.installed,
    onboardingLocalModelStatus.status,
    onboardingLocalPendingStart,
    onboardingOpen,
    onboardingStage,
  ]);

  const dictationOnboarding = useMemo<DictationOnboardingState | null>(() => {
    if (!onboardingOpen) return null;
    return {
      open: onboardingOpen,
      stage: onboardingStage,
      busy: onboardingBusy,
      error: onboardingError,
      cloud: onboardingCloud,
      localModelStatus: onboardingLocalModelStatus,
      localPendingStart: onboardingLocalPendingStart,
      localOptionDisabledReason,
    };
  }, [
    localOptionDisabledReason,
    onboardingBusy,
    onboardingCloud,
    onboardingError,
    onboardingLocalModelStatus,
    onboardingLocalPendingStart,
    onboardingOpen,
    onboardingStage,
  ]);

  return {
    dictationOnboarding,
    openDictationOnboarding,
    dismissDictationOnboarding,
    backDictationOnboarding,
    chooseDictationOnboardingLocal,
    chooseDictationOnboardingCloud,
    updateDictationOnboardingCloud,
    submitDictationOnboardingLocal,
    submitDictationOnboardingCloud,
  };
};
