import { TextInput } from "../../../components/ui/text-input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "../../../components/ui/select";
import type { TitleGenerationSettings } from "../../../api/client";
import { clampPct } from "../SettingsPage.utils";
import { Row, Toggle } from "../SettingsPage.components";
import { useTitleGenerationController } from "../hooks/useTitleGenerationController";
import { GeneralSection } from "./GeneralSection";

type TitleGenerationSectionProps = {
  active: boolean;
};

export function TitleGenerationSection({ active }: TitleGenerationSectionProps) {
  const {
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
  } = useTitleGenerationController(active);

  const localInstallRunning = localInstall?.state === "running" || titleGenLocalStatus?.install_running === true;
  const localInstalled = titleGenLocalStatus?.ready === true;
  const localStatusLabel = titleGenLocalStatusBusy
    ? "Loading…"
    : titleGenLocalStatusError
      ? "Status unavailable"
      : localInstallRunning
        ? "Installing…"
        : localInstalled
          ? "Installed"
          : "Not installed";
  const localStatusClass = titleGenLocalStatusError
    ? "settings-pill settings-pill-err"
    : localInstallRunning
      ? "settings-pill settings-pill-warn"
      : localInstalled
        ? "settings-pill settings-pill-ok"
        : "settings-pill settings-pill-warn";
  const installLabel =
    localInstallRunning && localInstall?.pct != null
      ? `${clampPct(localInstall.pct)}%`
      : localInstallRunning
        ? "Installing…"
        : localInstalled
          ? "Installed"
          : "Install";

  return (
    <>
      <GeneralSection>
        <div className="settings-preferences-flat">
          <div className="settings-preferences-group">
            <Row
              title="Mode"
              description="Choose between remote API or local model for session titles."
              control={
                <Select value={titleGenMode} onValueChange={(value) => setTitleGenMode(value as TitleGenerationSettings["mode"])}>
                  <SelectTrigger className="settings-control settings-select tw-min-w-[10rem]">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="remote">Remote</SelectItem>
                    <SelectItem value="local">Local</SelectItem>
                  </SelectContent>
                </Select>
              }
            />
          </div>
          <div className="settings-preferences-group">
            {titleGenMode === "remote" ? (
              <>
                <Row
                  title="Base URL"
                  description="OpenAI-compatible endpoint for title generation (best-effort; falls back to truncating the prompt)."
                  control={
                    <TextInput
                      className="settings-control settings-control-wide"
                      value={titleGenBaseUrl}
                      onChange={(e) => setTitleGenBaseUrl(e.target.value)}
                      placeholder="https://api.your-llm-gateway.example/v1"
                    />
                  }
                />
                <Row
                  title="API key"
                  description={titleGenApiKeySet ? "Key is stored; enter a new value to rotate." : "Stored locally in your ctx data dir."}
                  control={
                    <TextInput
                      className="settings-control settings-control-wide"
                      value={titleGenApiKey}
                      onChange={(e) => setTitleGenApiKey(e.target.value)}
                      placeholder={titleGenApiKeySet ? "(already set, leave blank to keep)" : "sk-..."}
                      type="password"
                    />
                  }
                />
                <Row
                  title="Model"
                  description="Model used for generating session titles."
                  control={
                    <TextInput
                      className="settings-control settings-control-wide"
                      value={titleGenModel}
                      onChange={(e) => setTitleGenModel(e.target.value)}
                      placeholder="model-slug"
                    />
                  }
                />
                <Row
                  title="Structured output (JSON)"
                  description="Enable when the model supports JSON schema output."
                  control={
                    <Toggle
                      checked={titleGenUseJson}
                      disabled={!loaded}
                      onChange={setTitleGenUseJson}
                      ariaLabel="Structured output"
                    />
                  }
                />
              </>
            ) : (
              <>
                <Row
                  title="Local model id"
                  description="Model id for the local title generator."
                  control={
                    <TextInput
                      className="settings-control settings-control-wide"
                      value={titleGenLocalModelId}
                      onChange={(e) => setTitleGenLocalModelId(e.target.value)}
                      placeholder="ggml-org/Qwen3-1.7B-GGUF"
                    />
                  }
                />
                <Row
                  title="Structured output (JSON)"
                  description="Enable when the model supports JSON schema output."
                  control={
                    <Toggle
                      checked={titleGenLocalUseJson}
                      disabled={!loaded}
                      onChange={setTitleGenLocalUseJson}
                      ariaLabel="Structured output"
                    />
                  }
                />
                <Row
                  title="Local install"
                  description="Downloads the llama.cpp runtime and Qwen3 1.7B model to the daemon host."
                  control={
                    <div className="row" style={{ gap: 8, alignItems: "center", justifyContent: "flex-end" }}>
                      <span className={localStatusClass}>{localStatusLabel}</span>
                      <button
                        type="button"
                        className="settings-btn"
                        onClick={() => {
                          void onInstallTitleGenerationLocal();
                        }}
                        disabled={titleGenLocalInstallBusy || localInstallRunning || localInstalled}
                      >
                        {installLabel}
                      </button>
                    </div>
                  }
                />
              </>
            )}
          </div>
        </div>
      </GeneralSection>
      {titleGenMode === "local" && titleGenLocalStatusError ? (
        <div className="settings-banner settings-banner-error">{titleGenLocalStatusError}</div>
      ) : null}
    </>
  );
}
