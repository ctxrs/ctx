import { ChevronRight } from "lucide-react";
import { TextInput } from "../../components/ui/text-input";
import type { WorkspaceSetupPageViewProps } from "./WorkspaceSetupPageView.types";

export function WorkspaceSetupTitlingStep({
  titlingProbeBusy,
  titlingProbeError,
  titlingPersistError,
  titlingStatusError,
  titlingMode,
  setTitlingMode,
  titlingLocalInstallBusy,
  titlingPersistBusy,
  onSelectTitlingLocal,
  titlingLocalStatus,
  titlingLocalInstall,
  titlingRemoteBaseUrl,
  setTitlingRemoteBaseUrl,
  titlingRemoteApiKey,
  setTitlingRemoteApiKey,
  titlingRemoteModel,
  setTitlingRemoteModel,
  titlingRemoteAdvancedOpen,
  setTitlingRemoteAdvancedOpen,
  titlingRemoteUseJson,
  setTitlingRemoteUseJson,
  invalidateTitlingPersisted,
  onSkipTitling,
}: WorkspaceSetupPageViewProps) {
  return (
    <div className="wizard-input">
      {titlingProbeBusy ? (
        <div className="wizard-note">Checking session titling configuration on this daemon…</div>
      ) : null}
      {titlingProbeError ? (
        <div className="wizard-error">
          Could not auto-detect titling configuration. You can still configure now or skip. ({titlingProbeError})
        </div>
      ) : null}
      {titlingPersistError ? <div className="wizard-error">{titlingPersistError}</div> : null}
      {titlingStatusError ? <div className="wizard-error">{titlingStatusError}</div> : null}
      <div className="wizard-option-grid wizard-option-grid--two">
        <button
          type="button"
          className={`wizard-option${titlingMode === "remote" ? " is-selected" : ""}`}
          data-testid="wizard-titling-mode-remote"
          onClick={() => {
            invalidateTitlingPersisted();
            setTitlingMode("remote");
          }}
          disabled={titlingLocalInstallBusy || titlingPersistBusy}
          aria-pressed={titlingMode === "remote"}
        >
          <div className="wizard-option-title">
            <span className="wizard-option-title-text">Remote LLM via API Key</span>
          </div>
          <div className="wizard-option-desc">
            Use a cloud endpoint with API key + model for title generation.
          </div>
        </button>
        <button
          type="button"
          className={`wizard-option${titlingMode === "local" ? " is-selected" : ""}`}
          data-testid="wizard-titling-mode-local"
          onClick={onSelectTitlingLocal}
          disabled
          aria-pressed={titlingMode === "local"}
        >
          <div className="wizard-option-title">
            <span className="wizard-option-title-text">Local model</span>
          </div>
          <div className="wizard-option-desc">
            Coming soon: download a small LLM to run locally for generating task titles.
          </div>
        </button>
      </div>
      {titlingMode === "local" ? (
        <div className="wizard-note" data-testid="wizard-titling-local-status">
          {titlingLocalStatus?.ready
            ? "Local model ready."
            : titlingLocalInstallBusy
              ? "Starting local model download…"
              : titlingLocalInstall?.state === "running"
                ? `Installing local model${typeof titlingLocalInstall.pct === "number" ? ` (${titlingLocalInstall.pct}%)` : ""}. This continues in background.`
                : titlingLocalInstall?.state === "cancelled"
                  ? "Local model install cancelled."
                  : titlingLocalInstall?.state === "failed"
                    ? `Local model install failed${titlingLocalInstall.error ? `: ${titlingLocalInstall.error}` : "."}`
                    : "Local model is not ready yet. Titles use fallback until install completes."}
        </div>
      ) : null}
      {titlingMode === "remote" && (
        <div className="wizard-input">
          <label>
            Endpoint base URL
            <TextInput
              data-testid="wizard-titling-remote-base-url"
              placeholder="https://api.your-llm-gateway.example/v1"
              value={titlingRemoteBaseUrl}
              onChange={(event) => {
                invalidateTitlingPersisted();
                setTitlingRemoteBaseUrl(event.target.value);
              }}
            />
          </label>
          <label>
            API key
            <TextInput
              data-testid="wizard-titling-remote-api-key"
              placeholder="sk-..."
              value={titlingRemoteApiKey}
              type="password"
              onChange={(event) => {
                invalidateTitlingPersisted();
                setTitlingRemoteApiKey(event.target.value);
              }}
            />
          </label>
          <label>
            Model
            <TextInput
              data-testid="wizard-titling-remote-model"
              placeholder="model-slug"
              value={titlingRemoteModel}
              onChange={(event) => {
                invalidateTitlingPersisted();
                setTitlingRemoteModel(event.target.value);
              }}
            />
          </label>
          <button
            type="button"
            className="wizard-advanced-link"
            data-testid="wizard-titling-remote-advanced-toggle"
            onClick={() => setTitlingRemoteAdvancedOpen((open) => !open)}
            aria-expanded={titlingRemoteAdvancedOpen}
          >
            <ChevronRight
              size={14}
              className={titlingRemoteAdvancedOpen ? "is-open" : undefined}
              aria-hidden="true"
            />
            Advanced
          </button>
          {titlingRemoteAdvancedOpen && (
            <label className="wizard-checkbox">
              <input
                data-testid="wizard-titling-remote-use-json"
                type="checkbox"
                checked={titlingRemoteUseJson}
                onChange={(event) => {
                  invalidateTitlingPersisted();
                  setTitlingRemoteUseJson(event.target.checked);
                }}
              />
              Prefer JSON response format
            </label>
          )}
        </div>
      )}
      <button
        type="button"
        className="wizard-skip wizard-skip--left wizard-skip--below"
        data-testid="wizard-titling-skip"
        onClick={onSkipTitling}
        disabled={titlingPersistBusy || titlingLocalInstallBusy}
      >
        Skip for now
      </button>
    </div>
  );
}
