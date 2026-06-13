import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { DictationSettings } from "../api/client";
import { desktopListen, isDesktopApp } from "./desktop";
import {
  type SttAvailability,
  type SttDownloadProgress,
  type SttPermission,
  type SttSupportedLanguage,
  loadSttApi,
  normalizeTauriLanguage,
} from "./tauriStt";
import { errorMessage } from "./errorMessage";

export type TauriModelStatus = {
  status: "idle" | "checking" | "ready" | "missing" | "downloading" | "error";
  installed: boolean;
  language: string;
  progress: number | null;
  detail: string | null;
  error: string | null;
};

type UseTauriSttModelStatusArgs = {
  provider: DictationSettings["provider"];
  language: string;
};

export const useTauriSttModelStatus = ({ provider, language }: UseTauriSttModelStatusArgs) => {
  const normalizedLanguage = useMemo(() => normalizeTauriLanguage(language), [language]);
  const [modelStatus, setModelStatus] = useState<TauriModelStatus>(() => ({
    status: "idle",
    installed: false,
    language: normalizedLanguage,
    progress: null,
    detail: null,
    error: null,
  }));
  const downloadListenerRef = useRef<(() => void) | null>(null);
  const downloadPollRef = useRef<number | null>(null);

  const cleanupDownload = useCallback(() => {
    const unlisten = downloadListenerRef.current;
    if (unlisten) {
      try {
        unlisten();
      } catch {
        // ignore
      }
    }
    downloadListenerRef.current = null;
    if (downloadPollRef.current !== null) {
      window.clearInterval(downloadPollRef.current);
    }
    downloadPollRef.current = null;
  }, []);

  const setModelError = useCallback((message: string) => {
    setModelStatus((prev) => ({
      ...prev,
      status: "error",
      error: message,
      detail: null,
      progress: null,
    }));
  }, []);

  const refreshModelStatus = useCallback(async () => {
    if (provider !== "tauri_stt" || !isDesktopApp()) {
      cleanupDownload();
      setModelStatus((prev) => ({
        ...prev,
        status: "idle",
        error: null,
        detail: null,
        progress: null,
      }));
      return;
    }

    setModelStatus((prev) => {
      if (prev.status === "downloading") return prev;
      return {
        ...prev,
        status: "checking",
        error: null,
        detail: null,
      };
    });

    const stt = await loadSttApi();
    if (!stt) {
      setModelError("Tauri dictation is unavailable. Reinstall the desktop app.");
      return;
    }

    const availability = (await stt.isAvailable().catch(() => null)) as SttAvailability | null;
    if (!availability?.available) {
      setModelError(
        availability?.reason ? `Dictation unavailable: ${availability.reason}` : "Dictation is unavailable.",
      );
      return;
    }

    const supported = await stt.getSupportedLanguages().catch(() => null);
    const languages = (supported?.languages ?? []) as SttSupportedLanguage[];
    const match = languages.find(
      (lang) => String(lang?.code ?? "").toLowerCase() === normalizedLanguage.toLowerCase(),
    );
    if (!match) {
      setModelError(`Language ${normalizedLanguage} is not supported by desktop dictation.`);
      return;
    }

    setModelStatus((prev) => {
      if (prev.status === "downloading" && !match.installed) {
        return {
          ...prev,
          installed: false,
          language: match.code ?? normalizedLanguage,
          error: null,
        };
      }
      return {
        status: match.installed ? "ready" : "missing",
        installed: Boolean(match.installed),
        language: match.code ?? normalizedLanguage,
        progress: null,
        detail: null,
        error: null,
      };
    });
  }, [cleanupDownload, normalizedLanguage, provider, setModelError]);

  const startModelDownload = useCallback(async () => {
    if (provider !== "tauri_stt") return;
    if (!isDesktopApp()) {
      setModelError("Desktop STT requires the ctx desktop app.");
      return;
    }

    cleanupDownload();
    setModelStatus((prev) => ({
      ...prev,
      status: "downloading",
      language: normalizedLanguage,
      progress: null,
      detail: "Starting download...",
      error: null,
    }));

    const stt = await loadSttApi();
    if (!stt) {
      setModelError("Tauri dictation is unavailable. Reinstall the desktop app.");
      return;
    }

    const availability = (await stt.isAvailable().catch(() => null)) as SttAvailability | null;
    if (!availability?.available) {
      setModelError(
        availability?.reason ? `Dictation unavailable: ${availability.reason}` : "Dictation is unavailable.",
      );
      return;
    }

    let finished = false;
    const finishDownload = async () => {
      if (finished) return;
      finished = true;
      cleanupDownload();
      try {
        await stt.stopListening();
      } catch {
        // ignore
      }
      await refreshModelStatus();
    };

    try {
      downloadListenerRef.current = await desktopListen<SttDownloadProgress>(
        "stt://download-progress",
        (payload) => {
          const progress = Number.isFinite(payload?.progress) ? payload.progress ?? null : null;
          const status = typeof payload?.status === "string" ? payload.status : null;
          setModelStatus((prev) => {
            if (prev.status !== "downloading") return prev;
            return {
              ...prev,
              progress: progress ?? prev.progress,
              detail: status ?? prev.detail,
            };
          });

          if (progress !== null && progress >= 100) {
            finishDownload().catch(() => {});
            return;
          }
          if (status && /complete|finished|done/i.test(status)) {
            finishDownload().catch(() => {});
          }
        },
      );
    } catch {
      // ignore listener issues
    }

    const permissionOk = (perm: SttPermission | null | undefined) =>
      Boolean(perm && perm.microphone === "granted" && perm.speechRecognition === "granted");
    let perm = await stt.checkPermission().catch(() => null);
    if (!permissionOk(perm)) {
      perm = await stt.requestPermission().catch(() => null);
    }
    if (!permissionOk(perm)) {
      cleanupDownload();
      setModelError("Microphone or speech recognition permission denied.");
      return;
    }

    try {
      await stt.startListening({
        language: normalizedLanguage,
        interimResults: false,
        continuous: false,
        maxDuration: 5000,
      });
    } catch (e: unknown) {
      cleanupDownload();
      setModelError(errorMessage(e));
      return;
    }

    let attempts = 0;
    downloadPollRef.current = window.setInterval(() => {
      attempts += 1;
      if (attempts > 90) {
        cleanupDownload();
        stt
          .stopListening()
          .catch(() => {})
          .finally(() => {
            setModelError("Model download timed out.");
          });
        return;
      }
      stt
        .getSupportedLanguages()
        .then((supported) => {
          const languages = (supported?.languages ?? []) as SttSupportedLanguage[];
          const match = languages.find(
            (lang) => String(lang?.code ?? "").toLowerCase() === normalizedLanguage.toLowerCase(),
          );
          if (match?.installed) {
            finishDownload().catch(() => {});
          }
        })
        .catch(() => {});
    }, 2000);
  }, [cleanupDownload, normalizedLanguage, provider, refreshModelStatus, setModelError]);

  useEffect(() => {
    refreshModelStatus().catch(() => {});
    return () => {
      cleanupDownload();
    };
  }, [cleanupDownload, refreshModelStatus]);

  return {
    modelStatus,
    refreshModelStatus,
    startModelDownload,
  };
};
