import { Cloud, Download, KeyRound, Laptop, X } from "lucide-react";
import { TextInput } from "../ui/text-input";
import type {
  DictationOnboardingCloudDraft,
  DictationOnboardingState,
} from "../../utils/useDictationController";

type DictationOnboardingModalProps = {
  state: DictationOnboardingState | null;
  onClose: () => void;
  onBack: () => void;
  onChooseLocal: () => void;
  onChooseCloud: () => void;
  onCloudChange: (patch: Partial<DictationOnboardingCloudDraft>) => void;
  onSubmitCloud: () => void;
  onSubmitLocal: () => void;
};

const localStatusUi = (state: DictationOnboardingState) => {
  const status = state.localModelStatus;
  if (status.status === "checking") {
    return { label: "Checking...", className: "settings-pill settings-pill-warn" };
  }
  if (status.status === "downloading") {
    if (typeof status.progress === "number") {
      return {
        label: `Downloading ${Math.max(0, Math.min(100, Math.round(status.progress)))}%`,
        className: "settings-pill settings-pill-warn",
      };
    }
    return { label: "Downloading...", className: "settings-pill settings-pill-warn" };
  }
  if (status.status === "ready" || status.installed) {
    return { label: "Installed", className: "settings-pill settings-pill-ok" };
  }
  if (status.status === "error") {
    return { label: "Not installed", className: "settings-pill settings-pill-err" };
  }
  return { label: "Not installed", className: "settings-pill settings-pill-warn" };
};

export function DictationOnboardingModal({
  state,
  onClose,
  onBack,
  onChooseLocal,
  onChooseCloud,
  onCloudChange,
  onSubmitCloud,
  onSubmitLocal,
}: DictationOnboardingModalProps) {
  if (!state?.open) return null;

  const localUi = localStatusUi(state);
  const localDisabled = Boolean(state.localOptionDisabledReason);
  const localActionDisabled =
    localDisabled
    || state.busy
    || state.localPendingStart
    || state.localModelStatus.status === "checking"
    || state.localModelStatus.status === "idle"
    || state.localModelStatus.status === "downloading";

  const localActionLabel = (() => {
    if (state.busy && state.localPendingStart) return "Preparing...";
    if (state.localPendingStart) return "Finishing setup...";
    if (state.localModelStatus.status === "checking" || state.localModelStatus.status === "idle") {
      return "Checking...";
    }
    if (state.localModelStatus.status === "downloading") {
      const progress = state.localModelStatus.progress;
      if (typeof progress === "number") {
        return `${Math.max(0, Math.min(100, Math.round(progress)))}%`;
      }
      return "Downloading...";
    }
    if (state.localModelStatus.status === "ready" || state.localModelStatus.installed) {
      return "Use local model";
    }
    return "Download now";
  })();

  return (
    <div className="modal-overlay" role="dialog" aria-modal="true" onClick={onClose}>
      <div className="modal settings-harness-modal" onClick={(e) => e.stopPropagation()}>
        <div className="settings-harness-modal-header">
          <div className="settings-harness-title">
            <span className="settings-harness-logo-fallback" aria-hidden="true" />
            <span className="settings-harness-name">Enable Dictation</span>
          </div>
          <button
            type="button"
            className="settings-harness-modal-close"
            onClick={onClose}
            aria-label="Close"
          >
            <X size={16} aria-hidden="true" />
          </button>
        </div>

        {state.stage === "choose" ? (
          <div className="settings-harness-modal-choice-stack">
            <div className="settings-row-desc">
              Choose how you want speech-to-text to run when you use the microphone.
            </div>
            <div className="settings-harness-modal-choice-grid">
              <button
                type="button"
                className="settings-btn settings-harness-modal-choice-btn"
                onClick={onChooseLocal}
                disabled={localDisabled}
                title={state.localOptionDisabledReason ?? "Use desktop local model"}
              >
                <Laptop size={18} className="settings-harness-modal-choice-icon" aria-hidden="true" />
                <span>Local model</span>
              </button>
              <button
                type="button"
                className="settings-btn settings-harness-modal-choice-btn"
                onClick={onChooseCloud}
              >
                <Cloud size={18} className="settings-harness-modal-choice-icon" aria-hidden="true" />
                <span>Cloud provider</span>
              </button>
            </div>
            {state.localOptionDisabledReason ? (
              <div className="settings-row-desc">{state.localOptionDisabledReason}</div>
            ) : null}
          </div>
        ) : state.stage === "local_setup" ? (
          <div className="settings-harness-modal-fields">
            <div className="settings-row-desc">
              Uses on-device desktop speech recognition for dictation. Download the language model once, then start speaking.
            </div>
            <div className="row" style={{ gap: 8, alignItems: "center", justifyContent: "space-between" }}>
              <span className={localUi.className}>{localUi.label}</span>
            </div>
            {state.localModelStatus.detail ? (
              <div className="settings-row-desc">{state.localModelStatus.detail}</div>
            ) : null}
            {state.localModelStatus.error ? (
              <div className="settings-row-desc settings-banner-error">{state.localModelStatus.error}</div>
            ) : null}
            <div className="modal-actions settings-harness-modal-actions">
              <button type="button" className="settings-btn settings-btn-secondary" onClick={onBack} disabled={state.busy}>
                Back
              </button>
              <button
                type="button"
                className="settings-btn"
                onClick={onSubmitLocal}
                disabled={localActionDisabled}
              >
                {state.localModelStatus.status === "ready" || state.localModelStatus.installed ? null : (
                  <Download size={14} style={{ marginRight: 6, verticalAlign: "text-bottom" }} aria-hidden="true" />
                )}
                {localActionLabel}
              </button>
            </div>
          </div>
        ) : (
          <div className="settings-harness-modal-fields">
            <div className="settings-row-desc">
              Enter your LiveKit Inference credentials to enable cloud dictation.
            </div>
            <label className="settings-harness-modal-label">
              API key
              <TextInput
                className="settings-control settings-control-wide"
                value={state.cloud.apiKey}
                onChange={(e) => onCloudChange({ apiKey: e.target.value })}
                placeholder={state.cloud.apiKeySet ? "(already set, leave blank to keep)" : "lk..."}
                type="password"
              />
            </label>
            <label className="settings-harness-modal-label">
              API secret
              <TextInput
                className="settings-control settings-control-wide"
                value={state.cloud.apiSecret}
                onChange={(e) => onCloudChange({ apiSecret: e.target.value })}
                placeholder={state.cloud.apiSecretSet ? "(already set, leave blank to keep)" : "secret"}
                type="password"
              />
            </label>
            <label className="settings-harness-modal-label">
              Base URL
              <TextInput
                className="settings-control settings-control-wide"
                value={state.cloud.baseUrl}
                onChange={(e) => onCloudChange({ baseUrl: e.target.value })}
                placeholder="https://agent-gateway.livekit.cloud/v1"
              />
            </label>
            <label className="settings-harness-modal-label">
              Model
              <TextInput
                className="settings-control settings-control-wide"
                value={state.cloud.model}
                onChange={(e) => onCloudChange({ model: e.target.value })}
                placeholder="auto"
              />
            </label>
            <label className="settings-harness-modal-label">
              Language
              <TextInput
                className="settings-control settings-control-wide"
                value={state.cloud.language}
                onChange={(e) => onCloudChange({ language: e.target.value })}
                placeholder="en"
              />
            </label>
            <div className="modal-actions settings-harness-modal-actions">
              <button type="button" className="settings-btn settings-btn-secondary" onClick={onBack} disabled={state.busy}>
                Back
              </button>
              <button type="button" className="settings-btn" onClick={onSubmitCloud} disabled={state.busy}>
                <KeyRound size={14} style={{ marginRight: 6, verticalAlign: "text-bottom" }} aria-hidden="true" />
                {state.busy ? "Saving..." : "Save and start"}
              </button>
            </div>
          </div>
        )}

        {state.error ? <div className="settings-banner settings-banner-error">{state.error}</div> : null}
      </div>
    </div>
  );
}
