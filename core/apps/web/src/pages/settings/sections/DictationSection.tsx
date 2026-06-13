import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "../../../components/ui/select";
import type { DictationSettings } from "../../../api/client";
import { TextInput } from "../../../components/ui/text-input";
import { MODEL_OPTIONS } from "../SettingsPage.constants";
import { Row, Toggle } from "../SettingsPage.components";
import { clampPct } from "../SettingsPage.utils";
import { useDictationController } from "../hooks/useDictationController";
import { GeneralSection } from "./GeneralSection";

type DictationSectionProps = {
  active: boolean;
};

export function DictationSection({ active }: DictationSectionProps) {
  const {
    loaded,
    dictationEnabled,
    setDictationEnabled,
    dictationProvider,
    setDictationProvider,
    model,
    setModel,
    language,
    setLanguage,
    baseUrl,
    setBaseUrl,
    apiKey,
    setApiKey,
    apiKeySet,
    apiSecret,
    setApiSecret,
    apiSecretSet,
    dictationCanSave,
    tauriModelStatus,
    startTauriModelDownload,
    isDesktopApp,
    saveError,
  } = useDictationController(active);

  const tauriStatusLabel = (() => {
    switch (tauriModelStatus.status) {
      case "ready":
        return "Installed";
      case "missing":
        return "Not installed";
      case "downloading":
        return "Downloading";
      case "checking":
        return "Checking";
      case "error":
        return "Error";
      default:
        return "Unavailable";
    }
  })();
  const tauriStatusClass = (() => {
    switch (tauriModelStatus.status) {
      case "ready":
        return "settings-pill-ok";
      case "missing":
      case "downloading":
        return "settings-pill-warn";
      case "error":
        return "settings-pill-err";
      default:
        return "";
    }
  })();
  const tauriDownloadPct =
    tauriModelStatus.status === "downloading" && tauriModelStatus.progress !== null
      ? clampPct(tauriModelStatus.progress)
      : null;
  const tauriDownloadLabel = (() => {
    if (tauriModelStatus.status === "downloading") {
      return tauriDownloadPct !== null ? `Downloading ${Math.round(tauriDownloadPct)}%` : "Downloading...";
    }
    if (tauriModelStatus.status === "error") return "Retry download";
    return "Download model";
  })();
  const showTauriDownload = dictationProvider === "tauri_stt" && isDesktopApp;
  const tauriDownloadDisabled =
    !dictationEnabled
    || tauriModelStatus.status === "checking"
    || tauriModelStatus.status === "downloading"
    || tauriModelStatus.status === "ready";
  const tauriStatusTitle = tauriModelStatus.error ?? tauriModelStatus.detail ?? undefined;

  return (
    <GeneralSection>
      <div className="settings-preferences-flat">
        <div className="settings-preferences-group">
          <Row
            title="Enable dictation"
            description="Enable speech-to-text dictation in the composer."
            control={
              <Toggle
                checked={dictationEnabled}
                disabled={!loaded}
                onChange={setDictationEnabled}
                ariaLabel="Enable dictation"
              />
            }
          />
          <Row
            title="Provider"
            description="Choose the dictation backend."
            control={
              <Select
                value={dictationProvider}
                onValueChange={(value) => setDictationProvider(value as DictationSettings["provider"])}
                disabled={!dictationEnabled}
              >
                <SelectTrigger className="settings-control settings-select tw-min-w-[10rem]">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="livekit_inference">LiveKit Inference (cloud)</SelectItem>
                  <SelectItem value="tauri_stt">Desktop STT (Tauri)</SelectItem>
                </SelectContent>
              </Select>
            }
          />
          {dictationProvider === "livekit_inference" ? (
            <Row
              title="Model"
              description="Transcription model used by LiveKit."
              control={
                <Select value={model} onValueChange={setModel} disabled={!dictationEnabled}>
                  <SelectTrigger className="settings-control settings-select tw-min-w-[10rem]">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {MODEL_OPTIONS.map((option) => (
                      <SelectItem key={option.value} value={option.value}>
                        {option.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              }
            />
          ) : null}
          <Row
            title="Language"
            description={
              dictationProvider === "tauri_stt"
                ? "Locale code for desktop dictation (e.g. en-US, es-ES)."
                : "BCP-47 code (e.g. en, es, multi)."
            }
            control={
              <TextInput
                className="settings-control"
                value={language}
                onChange={(e) => setLanguage(e.target.value)}
                disabled={!dictationEnabled}
                placeholder="en"
              />
            }
          />
        </div>

        <div className="settings-preferences-group">
          {dictationProvider === "livekit_inference" ? (
            <>
              <Row
                title="Inference base URL"
                description="LiveKit Agent Gateway endpoint."
                control={
                  <TextInput
                    className="settings-control settings-control-wide"
                    value={baseUrl}
                    onChange={(e) => setBaseUrl(e.target.value)}
                    disabled={!dictationEnabled}
                    placeholder="https://agent-gateway.livekit.cloud/v1"
                  />
                }
              />
              <Row
                title="LiveKit API key"
                description={apiKeySet ? "Key is stored; enter a new value to rotate." : "Stored locally in your ctx data dir."}
                control={
                  <TextInput
                    className="settings-control settings-control-wide"
                    value={apiKey}
                    onChange={(e) => setApiKey(e.target.value)}
                    disabled={!dictationEnabled}
                    placeholder={apiKeySet ? "(set)" : "APIK…"}
                    type="password"
                  />
                }
              />
              <Row
                title="LiveKit API secret"
                description={apiSecretSet ? "Secret is stored; enter a new value to rotate." : "Required."}
                control={
                  <TextInput
                    className="settings-control settings-control-wide"
                    value={apiSecret}
                    onChange={(e) => setApiSecret(e.target.value)}
                    disabled={!dictationEnabled}
                    placeholder={apiSecretSet ? "(set)" : "MAB…"}
                    type="password"
                  />
                }
              />
            </>
          ) : (
            <Row
              title="Desktop models"
              description="Download the Vosk model for the selected language."
              control={
                <div className="row" style={{ gap: 8, flexWrap: "wrap", justifyContent: "flex-end" }}>
                  <span className={`settings-pill ${tauriStatusClass}`} title={tauriStatusTitle}>
                    {tauriStatusLabel}
                  </span>
                  {showTauriDownload ? (
                    <button
                      type="button"
                      className="settings-btn settings-btn-secondary"
                      onClick={() => {
                        void startTauriModelDownload();
                      }}
                      disabled={tauriDownloadDisabled}
                      style={
                        tauriDownloadPct !== null
                          ? ({ ["--settings-install-pct" as string]: `${tauriDownloadPct}%` } as React.CSSProperties)
                          : undefined
                      }
                    >
                      {tauriDownloadLabel}
                    </button>
                  ) : null}
                </div>
              }
            />
          )}
        </div>
      </div>
      {dictationProvider === "tauri_stt" && !isDesktopApp ? (
        <div className="settings-banner">Desktop STT requires the ctx desktop app.</div>
      ) : null}
      {dictationProvider === "tauri_stt" && tauriModelStatus.error ? (
        <div className="settings-banner settings-banner-error">{tauriModelStatus.error}</div>
      ) : null}
      {!dictationCanSave && dictationEnabled && dictationProvider === "livekit_inference" ? (
        <div className="settings-banner settings-banner-error">Enter an API key and secret to enable dictation.</div>
      ) : null}
      {saveError ? <div className="settings-banner settings-banner-error">{saveError}</div> : null}
    </GeneralSection>
  );
}
