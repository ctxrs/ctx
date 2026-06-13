import type { Dispatch, SetStateAction } from "react";
import { X } from "lucide-react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import type { ExecutionLaunchSnapshot } from "../../api/client";
import type { SessionTitlingMode } from "./WorkspaceSetupPage.logic";
import type { WorkspaceSetupLaunchLogLine } from "./launchProgress";
import type { WizardStepKey } from "./wizardFlow";
import type { ImportInitDialogState, WizardStep } from "./wizardTypes";

type WorkspaceSetupDialogsProps = {
  importInitDialog: ImportInitDialogState | null;
  resolveImportInitDialog: (confirmed: boolean) => void;
  infoStep: WizardStep | null;
  setOpenInfoKey: Dispatch<SetStateAction<string | null>>;
};

export function WorkspaceSetupDialogs({
  importInitDialog,
  resolveImportInitDialog,
  infoStep,
  setOpenInfoKey,
}: WorkspaceSetupDialogsProps) {
  return (
    <>
      {importInitDialog && (
        <div
          className="wizard-modal-backdrop"
          role="dialog"
          aria-modal="true"
          aria-label="Initialize Git repo"
          data-testid="wizard-import-init-modal"
          onClick={() => resolveImportInitDialog(false)}
        >
          <div className="wizard-modal" onClick={(event) => event.stopPropagation()}>
            <div className="wizard-modal-header">
              <div className="wizard-modal-title">Initialize Git repo in this folder?</div>
              <button
                type="button"
                className="wizard-modal-close"
                aria-label="Close"
                onClick={() => resolveImportInitDialog(false)}
              >
                <X size={16} aria-hidden="true" />
              </button>
            </div>
            <div className="wizard-modal-body">
              <div className="wizard-modal-copy">
                The selected folder is not currently a repository.
              </div>
              <div className="wizard-modal-path">
                <code>{importInitDialog.path}</code>
              </div>
              <div className="wizard-modal-note">
                This will run <code>git init</code> and create one empty initial commit. Existing files are not staged or committed.
              </div>
              <div className="wizard-modal-actions">
                <button
                  type="button"
                  className="wizard-secondary"
                  data-testid="wizard-import-init-cancel"
                  onClick={() => resolveImportInitDialog(false)}
                >
                  Cancel
                </button>
                <button
                  type="button"
                  className="wizard-primary"
                  data-testid="wizard-import-init-confirm"
                  onClick={() => resolveImportInitDialog(true)}
                >
                  Initialize Git repo here
                </button>
              </div>
            </div>
          </div>
        </div>
      )}
      {infoStep?.info && (
        <div
          className="wizard-modal-backdrop"
          role="dialog"
          aria-modal="true"
          aria-label={`${infoStep.title} info`}
          onClick={() => setOpenInfoKey(null)}
        >
          <div className="wizard-modal" onClick={(event) => event.stopPropagation()}>
            <div className="wizard-modal-header">
              <div className="wizard-modal-title">{infoStep.title}</div>
              <button
                type="button"
                className="wizard-modal-close"
                aria-label="Close"
                onClick={() => setOpenInfoKey(null)}
              >
                <X size={16} aria-hidden="true" />
              </button>
            </div>
            <div className="wizard-modal-body">
              <div className="wizard-markdown">
                <ReactMarkdown remarkPlugins={[remarkGfm]}>
                  {infoStep.info}
                </ReactMarkdown>
              </div>
            </div>
          </div>
        </div>
      )}
    </>
  );
}

type WorkspaceLaunchLogPanelProps = {
  showLaunchPanel: boolean;
  launchSnapshot: ExecutionLaunchSnapshot | null;
  currentLaunchStepLabel: string;
  currentLaunchElapsed: string;
  currentLaunchEtaLabel: string;
  launchCopyLabel: string;
  onCopyLaunchDiagnostics: () => void;
  launchLogs: WorkspaceSetupLaunchLogLine[];
};

export function WorkspaceLaunchLogPanel({
  showLaunchPanel,
  launchSnapshot,
  currentLaunchStepLabel,
  currentLaunchElapsed,
  currentLaunchEtaLabel,
  launchCopyLabel,
  onCopyLaunchDiagnostics,
  launchLogs,
}: WorkspaceLaunchLogPanelProps) {
  if (!showLaunchPanel || !launchSnapshot) {
    return null;
  }

  return (
    <div className="wizard-launch-log-panel" data-testid="wizard-launch-log-panel">
      <div className="wizard-launch-log-header">
        <div>
          <div className="wizard-launch-log-title">Workspace Launch Logs</div>
          <div className="wizard-launch-log-meta">
            <span>{currentLaunchStepLabel}</span>
            <span>{currentLaunchElapsed} elapsed</span>
            <span>{currentLaunchEtaLabel}</span>
          </div>
        </div>
        <button
          type="button"
          className="wizard-input-button"
          onClick={onCopyLaunchDiagnostics}
          data-testid="wizard-launch-copy"
        >
          {launchCopyLabel}
        </button>
      </div>
      <div className="wizard-launch-log-body">
        {launchLogs.length === 0 ? (
          <div className="wizard-note">Waiting for launch logs…</div>
        ) : (
          launchLogs.map((line) => (
            <div key={line.seq} className="wizard-launch-log-line">
              <span className="wizard-launch-log-ts">{line.timeLabel}</span>
              <span className="wizard-launch-log-phase">{line.phaseLabel}</span>
              <span className={`wizard-launch-log-level wizard-launch-log-level--${line.level}`}>{line.level}</span>
              <span className="wizard-launch-log-msg">{line.message}</span>
            </div>
          ))
        )}
      </div>
    </div>
  );
}

type WorkspaceSetupStepOptionsProps = {
  step: WizardStep;
  selections: Record<string, string>;
  onSelectOption: (stepKey: string, optionId: string) => void;
};

export function WorkspaceSetupStepOptions({
  step,
  selections,
  onSelectOption,
}: WorkspaceSetupStepOptionsProps) {
  if (!step.options) {
    return null;
  }

  return (
    <>
      <div className="wizard-option-grid">
        {step.options.map((option) => {
            const selected = selections[step.key] === option.id;
            return (
              <button
                key={option.id}
                type="button"
                className={`wizard-option${selected ? " is-selected" : ""}`}
                data-testid={`wizard-option-${step.key}-${option.id}`}
                onClick={() => onSelectOption(step.key, option.id)}
                aria-pressed={selected}
              >
                <div className="wizard-option-title">
                  <span className="wizard-option-title-text">{option.title}</span>
                  {option.badge && <span className="wizard-option-badge">{option.badge}</span>}
                </div>
                <div className="wizard-option-desc">{option.desc}</div>
              </button>
            );
        })}
      </div>
    </>
  );
}

type WorkspaceSetupPaginationProps = {
  steps: WizardStep[];
  stepIndex: number;
  selections: Record<string, string>;
  remoteStatus: "idle" | "connecting" | "connected" | "error";
  hasRemoteHost: boolean;
  titlingMode: SessionTitlingMode;
  titlingRemoteValid: boolean;
  sourceStepComplete: boolean;
  mergeQueueSkipped: boolean;
  targetBranch: string;
  goToStepKey: (key: WizardStepKey) => void;
};

export function WorkspaceSetupPagination({
  steps,
  stepIndex,
  selections,
  remoteStatus,
  hasRemoteHost,
  titlingMode,
  titlingRemoteValid,
  sourceStepComplete,
  mergeQueueSkipped,
  targetBranch,
  goToStepKey,
}: WorkspaceSetupPaginationProps) {
  const isStepSatisfied = (key: string): boolean => {
    if (key === "location") {
      if (selections.location === "local") return true;
      if (selections.location !== "remote") return false;
      return remoteStatus === "connected" && hasRemoteHost;
    }
    if (key === "auth-import") return true;
    if (key === "harness-downloads") return true;
    if (key === "session-titling") {
      return titlingMode === "skip"
        || titlingMode === "local"
        || (titlingMode === "remote" && titlingRemoteValid);
    }
    if (key === "source") return sourceStepComplete;
    if (key === "merge-queue") return mergeQueueSkipped || Boolean(targetBranch.trim());
    return true;
  };

  let maxIdx = 0;
  for (let index = 0; index < steps.length; index += 1) {
    if (isStepSatisfied(steps[index].key)) {
      maxIdx = Math.min(steps.length - 1, index + 1);
    } else {
      maxIdx = Math.max(0, index);
      break;
    }
  }

  return (
    <div className="wizard-pagination" role="tablist" aria-label="Setup steps">
      {steps.map((item, index) => {
        const disabled = index > maxIdx;
        return (
          <button
            key={item.key}
            type="button"
            className={`wizard-dot${index === stepIndex ? " is-active" : ""}`}
            aria-label={`Go to step ${index + 1}`}
            aria-current={index === stepIndex ? "true" : undefined}
            disabled={disabled}
            onClick={() => {
              if (!disabled) {
                goToStepKey(item.key);
              }
            }}
          />
        );
      })}
    </div>
  );
}
