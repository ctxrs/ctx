import type { MutableRefObject } from "react";
import type { WizardRoutePlan, WizardStepKey } from "./wizardFlow";
import type { WizardSelections } from "./wizardFlowReducer";
import type { RemoteStatus } from "./wizardTypes";
import type { WorkspaceSetupEffectiveTarget } from "./workflowTypes";

export const TITLING_PROBE_TIMEOUT_MS = 2_000;

export type UseWorkspaceSetupProvisioningArgs = {
  currentStepKeyRef: MutableRefObject<WizardStepKey>;
  selections: WizardSelections;
  routePlan: WizardRoutePlan | null;
  setRoutePlan: (routePlan: WizardRoutePlan | null) => void;
  setRoutePlanningBusy: (busy: boolean) => void;
  invalidateRoutePlan: () => void;
  desktopApp: boolean;
  effectiveTarget: WorkspaceSetupEffectiveTarget | null;
  remoteStatus: RemoteStatus;
  remoteStatusRef: MutableRefObject<RemoteStatus>;
  connectDaemonForImport: (locationOverride?: "local" | "remote") => Promise<void>;
};
