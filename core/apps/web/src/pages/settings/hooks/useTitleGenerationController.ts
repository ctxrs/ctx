import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  getSettings,
  getTitleGenerationLocalStatus,
  installTitleGenerationLocal,
  updateSettings,
  type TitleGenerationLocalStatus,
  type TitleGenerationSettings,
  type UpdateTitleGenerationSettingsRequest,
} from "../../../api/client";
import { readBoolish } from "../../../utils/boolish";
import type { InstallSession } from "../SettingsPage.types";
import { observeInstall, subscribeInstallProgress } from "../../../state/installProgressMonitor";

type TitleGenerationController = {
  loaded: boolean;
  titleGenMode: TitleGenerationSettings["mode"];
  setTitleGenMode: (mode: TitleGenerationSettings["mode"]) => void;
  titleGenBaseUrl: string;
  setTitleGenBaseUrl: (value: string) => void;
  titleGenApiKey: string;
  setTitleGenApiKey: (value: string) => void;
  titleGenApiKeySet: boolean;
  titleGenModel: string;
  setTitleGenModel: (value: string) => void;
  titleGenUseJson: boolean;
  setTitleGenUseJson: (next: boolean) => void;
  titleGenLocalModelId: string;
  setTitleGenLocalModelId: (value: string) => void;
  titleGenLocalUseJson: boolean;
  setTitleGenLocalUseJson: (next: boolean) => void;
  titleGenLocalStatus: TitleGenerationLocalStatus | null;
  titleGenLocalStatusBusy: boolean;
  titleGenLocalStatusError: string | null;
  titleGenLocalInstallBusy: boolean;
  localInstall: InstallSession | undefined;
  onInstallTitleGenerationLocal: () => Promise<void>;
};

const messageFromError = (error: unknown): string => {
  if (error instanceof Error && error.message) {
    return error.message;
  }
  return String(error);
};

export function useTitleGenerationController(enabled: boolean): TitleGenerationController {
  const [loaded, setLoaded] = useState(false);
  const hydrated = useRef(false);

  const [titleGenMode, setTitleGenMode] = useState<TitleGenerationSettings["mode"]>("remote");
  const [titleGenBaseUrl, setTitleGenBaseUrl] = useState("");
  const [titleGenApiKey, setTitleGenApiKey] = useState("");
  const [titleGenApiKeySet, setTitleGenApiKeySet] = useState(false);
  const [titleGenModel, setTitleGenModel] = useState("");
  const [titleGenUseJson, setTitleGenUseJson] = useState(true);
  const [titleGenLocalModelId, setTitleGenLocalModelId] = useState("ggml-org/Qwen3-1.7B-GGUF");
  const [titleGenLocalUseJson, setTitleGenLocalUseJson] = useState(true);

  const [titleGenLocalStatus, setTitleGenLocalStatus] = useState<TitleGenerationLocalStatus | null>(null);
  const [titleGenLocalStatusBusy, setTitleGenLocalStatusBusy] = useState(false);
  const [titleGenLocalStatusError, setTitleGenLocalStatusError] = useState<string | null>(null);
  const [titleGenLocalInstallBusy, setTitleGenLocalInstallBusy] = useState(false);
  const [localInstall, setLocalInstall] = useState<InstallSession | undefined>(undefined);

  const installObserverRef = useRef<(() => void) | null>(null);
  const localInstallRef = useRef<InstallSession | undefined>(undefined);
  const previousLocalInstallRef = useRef<InstallSession | undefined>(undefined);

  const titleGenerationPayload = useMemo((): UpdateTitleGenerationSettingsRequest => {
    const remote: UpdateTitleGenerationSettingsRequest["remote"] = {
      base_url: titleGenBaseUrl.trim(),
      model: titleGenModel.trim(),
      use_json: titleGenUseJson,
    };
    const apiKey = titleGenApiKey.trim();
    if (apiKey) {
      remote.api_key = apiKey;
    }
    return {
      mode: titleGenMode,
      remote,
      local: {
        model_id: titleGenLocalModelId.trim(),
        use_json: titleGenLocalUseJson,
      },
    };
  }, [
    titleGenApiKey,
    titleGenBaseUrl,
    titleGenLocalModelId,
    titleGenLocalUseJson,
    titleGenMode,
    titleGenModel,
    titleGenUseJson,
  ]);

  const refreshTitleGenLocalStatus = useCallback(async (opts?: { silent?: boolean }) => {
    if (!opts?.silent) {
      setTitleGenLocalStatusBusy(true);
    }
    setTitleGenLocalStatusError(null);
    try {
      const status = await getTitleGenerationLocalStatus();
      setTitleGenLocalStatus(status);
      return status;
    } catch (error) {
      setTitleGenLocalStatusError(messageFromError(error));
      return null;
    } finally {
      if (!opts?.silent) {
        setTitleGenLocalStatusBusy(false);
      }
    }
  }, []);

  const attachInstall = useCallback((installId: string) => {
    const normalizedInstallId = installId.trim();
    if (!normalizedInstallId) return;
    if (localInstallRef.current?.installId === normalizedInstallId && installObserverRef.current) {
      return;
    }

    installObserverRef.current?.();
    setLocalInstall((prev) => ({
      installId: normalizedInstallId,
      state: "running",
      pct: prev?.pct ?? null,
      target: prev?.target,
      errorCode: undefined,
      streamError: prev?.streamError,
      error: undefined,
    }));
    installObserverRef.current = observeInstall(normalizedInstallId, {
      initialState: {
        state: "running",
        pct: localInstallRef.current?.pct ?? null,
      },
    });
  }, []);

  const onInstallTitleGenerationLocal = useCallback(async () => {
    setTitleGenLocalInstallBusy(true);
    setTitleGenLocalStatusError(null);
    try {
      const { install_id } = await installTitleGenerationLocal();
      attachInstall(install_id);
    } catch (error) {
      setTitleGenLocalStatusError(messageFromError(error));
    } finally {
      setTitleGenLocalInstallBusy(false);
    }
  }, [attachInstall]);

  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;
    setLoaded(false);
    getSettings()
      .then((settings) => {
        if (cancelled) return;
        const tg = settings.title_generation ?? null;
        if (tg) {
          setTitleGenMode(tg.mode ?? "remote");
          setTitleGenBaseUrl(tg.remote?.base_url ?? "");
          setTitleGenApiKey("");
          setTitleGenApiKeySet(readBoolish(tg.remote?.api_key_set) ?? false);
          setTitleGenModel(tg.remote?.model ?? "");
          setTitleGenUseJson(readBoolish(tg.remote?.use_json) ?? false);
          setTitleGenLocalModelId(tg.local?.model_id ?? "ggml-org/Qwen3-1.7B-GGUF");
          setTitleGenLocalUseJson(readBoolish(tg.local?.use_json) ?? false);
        }
        setLoaded(true);
      })
      .catch(() => {
        if (cancelled) return;
        setLoaded(true);
      });
    return () => {
      cancelled = true;
    };
  }, [enabled]);

  useEffect(() => {
    if (!enabled || !loaded) return;
    if (!hydrated.current) {
      hydrated.current = true;
      return;
    }
    const timeout = window.setTimeout(() => {
      updateSettings({ title_generation: titleGenerationPayload })
        .then((next) => {
          const remote = next.title_generation?.remote;
          if (readBoolish(remote?.api_key_set) ?? false) {
            setTitleGenApiKey("");
            setTitleGenApiKeySet(true);
          } else {
            setTitleGenApiKeySet(false);
          }
        })
        .catch(() => {});
    }, 450);
    return () => window.clearTimeout(timeout);
  }, [enabled, loaded, titleGenerationPayload]);

  useEffect(() => {
    localInstallRef.current = localInstall;
  }, [localInstall]);

  useEffect(() => {
    return subscribeInstallProgress((snapshot) => {
      const installId = localInstallRef.current?.installId;
      if (!installId) return;
      const entry = snapshot[installId];
      if (!entry) return;
      setLocalInstall((prev) => {
        if (!prev || prev.installId !== installId) return prev;
        const nextInstall: InstallSession = {
          installId,
          state: entry.state,
          pct: entry.pct,
          target: entry.target,
          errorCode: entry.errorCode,
          streamError: prev.streamError,
          error: entry.error,
        };
        if (
          prev.state === nextInstall.state
          && prev.pct === nextInstall.pct
          && prev.target === nextInstall.target
          && prev.errorCode === nextInstall.errorCode
          && prev.error === nextInstall.error
        ) {
          return prev;
        }
        return nextInstall;
      });
    });
  }, []);

  useEffect(() => {
    const previous = previousLocalInstallRef.current;
    previousLocalInstallRef.current = localInstall;
    if (!localInstall || localInstall.state === "running") return;
    if (previous?.installId === localInstall.installId && previous.state === localInstall.state) {
      return;
    }
    installObserverRef.current?.();
    installObserverRef.current = null;
    void refreshTitleGenLocalStatus({ silent: true });
  }, [localInstall, refreshTitleGenLocalStatus]);

  useEffect(() => {
    if (!enabled) return;
    if (titleGenMode !== "local") return;
    refreshTitleGenLocalStatus().then((status) => {
      if (status?.install_running && status.install_id) {
        attachInstall(status.install_id);
      }
    }).catch(() => {});
  }, [attachInstall, enabled, refreshTitleGenLocalStatus, titleGenMode]);

  useEffect(() => {
    return () => {
      installObserverRef.current?.();
      installObserverRef.current = null;
    };
  }, []);

  return {
    loaded,
    titleGenMode,
    setTitleGenMode,
    titleGenBaseUrl,
    setTitleGenBaseUrl,
    titleGenApiKey,
    setTitleGenApiKey,
    titleGenApiKeySet,
    titleGenModel,
    setTitleGenModel,
    titleGenUseJson,
    setTitleGenUseJson,
    titleGenLocalModelId,
    setTitleGenLocalModelId,
    titleGenLocalUseJson,
    setTitleGenLocalUseJson,
    titleGenLocalStatus,
    titleGenLocalStatusBusy,
    titleGenLocalStatusError,
    titleGenLocalInstallBusy,
    localInstall,
    onInstallTitleGenerationLocal,
  };
}
