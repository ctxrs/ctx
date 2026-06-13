import type { SessionTitlingMode } from "./WorkspaceSetupPage.logic";
import type { RemoteStatus, WizardStep } from "./wizardTypes";
import type { WizardStepKey } from "./wizardFlow";

type WorkspaceSetupPaginationProps = {
  steps: WizardStep[];
  stepIndex: number;
  selections: Record<string, string>;
  remoteStatus: RemoteStatus;
  hasRemoteHost: boolean;
  titlingMode: SessionTitlingMode;
  titlingRemoteValid: boolean;
  sourceStepComplete: boolean;
  mergeQueueSkipped: boolean;
  targetBranch: string;
  goToStepKey: (key: WizardStepKey) => void;
};

const isStepSatisfied = (
  key: string,
  params: Omit<WorkspaceSetupPaginationProps, "steps" | "stepIndex" | "goToStepKey">,
): boolean => {
  if (key === "location") {
    if (params.selections.location === "local") return true;
    if (params.selections.location !== "remote") return false;
    return params.remoteStatus === "connected" && params.hasRemoteHost;
  }
  if (key === "auth-import") return true;
  if (key === "harness-downloads") return true;
  if (key === "session-titling") {
    return params.titlingMode === "skip"
      || params.titlingMode === "local"
      || (params.titlingMode === "remote" && params.titlingRemoteValid);
  }
  if (key === "source") return params.sourceStepComplete;
  if (key === "merge-queue") return params.mergeQueueSkipped || Boolean(params.targetBranch.trim());
  return true;
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
  let maxIdx = 0;
  for (let index = 0; index < steps.length; index += 1) {
    if (isStepSatisfied(steps[index].key, {
      selections,
      remoteStatus,
      hasRemoteHost,
      titlingMode,
      titlingRemoteValid,
      sourceStepComplete,
      mergeQueueSkipped,
      targetBranch,
    })) {
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
