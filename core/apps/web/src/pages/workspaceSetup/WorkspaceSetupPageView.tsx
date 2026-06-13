import { Link } from "react-router-dom";
import LauncherBrand from "../../components/LauncherBrand";
import {
  WorkspaceSetupDialogs,
} from "./WorkspaceSetupChrome";
import { WorkspaceSetupPagination } from "./WorkspaceSetupPagination";
import { WorkspaceSetupStepHeader } from "./WorkspaceSetupStepHeader";
import type { WorkspaceSetupPageViewProps } from "./WorkspaceSetupPageView.types";
import { WorkspaceSetupPageStepBody } from "./WorkspaceSetupPageStepBody";

export function WorkspaceSetupPageView(props: WorkspaceSetupPageViewProps) {
  const {
    importInitDialog,
    resolveImportInitDialog,
    infoStep,
    setOpenInfoKey,
    step,
    steps,
    stepIndex,
    selections,
    remoteStatus,
    titlingMode,
    titlingRemoteValid,
    sourceStepComplete,
    mergeQueueSkipped,
    targetBranch,
    hasRemoteHost,
    goToStepKey,
    isFirst,
    isLast,
    canAdvance,
    creating,
    onCreate,
    onNext,
    goRelativeStep,
    createButtonLabel,
    nextButtonLabel,
  } = props;

  return (
    <div className="launcher-shell launcher-shell--crt">
      <LauncherBrand fullScreen>
        <div className="wizard-panel" data-testid="workspace-setup" data-step-key={step.key}>
          <WorkspaceSetupDialogs
            importInitDialog={importInitDialog}
            resolveImportInitDialog={resolveImportInitDialog}
            infoStep={infoStep}
            setOpenInfoKey={setOpenInfoKey}
          />
          <div className="wizard-steps">
            <div className="wizard-step" data-testid="wizard-step" data-step-key={step.key}>
              <WorkspaceSetupStepHeader step={step} setOpenInfoKey={setOpenInfoKey} />
              <WorkspaceSetupPageStepBody {...props} />
            </div>
          </div>
          <WorkspaceSetupPagination
            steps={steps}
            stepIndex={stepIndex}
            selections={selections}
            remoteStatus={remoteStatus}
            hasRemoteHost={hasRemoteHost}
            titlingMode={titlingMode}
            titlingRemoteValid={titlingRemoteValid}
            sourceStepComplete={sourceStepComplete}
            mergeQueueSkipped={mergeQueueSkipped}
            targetBranch={targetBranch}
            goToStepKey={goToStepKey}
          />
          <div className="wizard-actions">
            {isFirst ? (
              <Link to="/" className="wizard-secondary" data-testid="wizard-back-link">
                Back
              </Link>
            ) : (
              <button
                type="button"
                className="wizard-secondary"
                data-testid="wizard-back"
                onClick={() => goRelativeStep(-1)}
              >
                Back
              </button>
            )}
            {isLast ? (
              <button
                type="button"
                className="wizard-primary"
                data-testid="wizard-create"
                disabled={!canAdvance || creating}
                onClick={onCreate}
              >
                {createButtonLabel}
              </button>
            ) : (
              <button
                type="button"
                className="wizard-primary"
                data-testid="wizard-next"
                disabled={!canAdvance || creating}
                onClick={onNext}
              >
                {nextButtonLabel}
              </button>
            )}
          </div>
        </div>
      </LauncherBrand>
    </div>
  );
}
