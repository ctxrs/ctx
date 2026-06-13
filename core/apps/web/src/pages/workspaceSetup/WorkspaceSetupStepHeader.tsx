import type { Dispatch, SetStateAction } from "react";
import { Info } from "lucide-react";
import type { WizardStep } from "./wizardTypes";

type WorkspaceSetupStepHeaderProps = {
  step: WizardStep;
  setOpenInfoKey: Dispatch<SetStateAction<string | null>>;
};

export function WorkspaceSetupStepHeader({
  step,
  setOpenInfoKey,
}: WorkspaceSetupStepHeaderProps) {
  return (
    <div className="wizard-step-header">
      <div className="wizard-step-title-row">
        <div className="wizard-step-title">{step.title}</div>
        {step.info && (
          <button
            type="button"
            className="wizard-info-toggle"
            onClick={() => setOpenInfoKey((prev) => (prev === step.key ? null : step.key))}
            aria-label="Info"
          >
            <Info size={16} aria-hidden="true" />
          </button>
        )}
      </div>
      <div className="wizard-step-note">{step.note}</div>
    </div>
  );
}
