import type { WizardRoutePlan } from "./wizardFlow";
import type {
  EnsureOnboardingAfterDaemonConnectResult,
  RoutePlanInsertionStep,
  WorkspaceSetupProvisioningSnapshot,
} from "./workflowTypes";
import { serializeWorkspaceSetupRouteScope } from "./workflowTypes";

type ResolveRoutePlanInsertionOptions = {
  allowTitlingInsertion?: boolean;
};

export const buildWizardRoutePlan = (
  snapshot: WorkspaceSetupProvisioningSnapshot,
): WizardRoutePlan => ({
  targetKey: serializeWorkspaceSetupRouteScope(snapshot.routeScope),
  containerSelection: snapshot.routeScope.containerSelection,
  includeHarnessDownloads:
    snapshot.harnessCandidatesStatus === "error" || snapshot.missingHarnessCount > 0,
  includeAuthImport:
    snapshot.authImportStatus === "error" || snapshot.authImportCandidateCount > 0,
  includeTitling: snapshot.titlingMode !== "skip" && snapshot.titlingRequired,
});

export const resolveRoutePlanInsertionStep = (
  routePlan: WizardRoutePlan,
  previousPlan: WizardRoutePlan | null,
  options?: ResolveRoutePlanInsertionOptions,
): RoutePlanInsertionStep | null => {
  const allowTitlingInsertion = options?.allowTitlingInsertion ?? true;
  const reusablePreviousPlan =
    previousPlan?.targetKey === routePlan.targetKey ? previousPlan : null;
  const shouldInsertHarnessDownloads =
    routePlan.includeHarnessDownloads && reusablePreviousPlan?.includeHarnessDownloads !== true;
  if (shouldInsertHarnessDownloads) {
    return "harness-downloads";
  }

  const shouldInsertAuthImport =
    routePlan.includeAuthImport && reusablePreviousPlan?.includeAuthImport !== true;
  if (shouldInsertAuthImport) {
    return "auth-import";
  }

  const shouldInsertTitling =
    allowTitlingInsertion
    && routePlan.includeTitling
    && reusablePreviousPlan?.includeTitling !== true;
  if (shouldInsertTitling) {
    return "session-titling";
  }

  return null;
};

export const buildOnboardingAfterConnectResult = (
  snapshot: WorkspaceSetupProvisioningSnapshot,
  previousPlan: WizardRoutePlan | null,
  options?: ResolveRoutePlanInsertionOptions,
): EnsureOnboardingAfterDaemonConnectResult => {
  const routePlan = buildWizardRoutePlan(snapshot);
  return {
    routePlan,
    insertionStep: resolveRoutePlanInsertionStep(routePlan, previousPlan, options),
  };
};
