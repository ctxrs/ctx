import { useCallback, type MutableRefObject } from "react";
import type { WizardRoutePlan, WizardStepKey } from "./wizardFlow";
import type { WizardSelections } from "./wizardFlowReducer";
import type {
  WorkspaceSetupProvisioningMachineState,
  WorkspaceSetupProvisioningRequest,
} from "./workspaceSetupProvisioningMachine";
import type {
  WorkspaceSetupEffectiveTarget,
} from "./workflowTypes";
import {
  messageFromError,
  type RemoteStatus,
} from "./wizardTypes";
import { withTimeout } from "./promiseTimeout";
import { useWorkspaceSetupAuthImportProvisioning } from "./useWorkspaceSetupAuthImportProvisioning";
import { useWorkspaceSetupHarnessDownloadsProvisioning } from "./useWorkspaceSetupHarnessDownloadsProvisioning";

type UseWorkspaceSetupProviderProvisioningArgs = {
  currentStepKeyRef: MutableRefObject<WizardStepKey>;
  selections: WizardSelections;
  effectiveTarget: WorkspaceSetupEffectiveTarget | null;
  desktopApp: boolean;
  parsedRemoteHost: string | undefined;
  remoteStatusRef: MutableRefObject<RemoteStatus>;
  connectDaemonForImport: (locationOverride?: "local" | "remote") => Promise<void>;
  shouldDeferSpeculativeLocalRefresh: (location: "local" | "remote") => boolean;
  commitProvisioningMachineState: (
    updater:
      | WorkspaceSetupProvisioningMachineState
      | ((current: WorkspaceSetupProvisioningMachineState) => WorkspaceSetupProvisioningMachineState),
  ) => WorkspaceSetupProvisioningMachineState;
  isCurrentProvisioningRequest: (
    resource: WorkspaceSetupProvisioningRequest["resource"],
    request: WorkspaceSetupProvisioningRequest,
  ) => boolean;
  getCurrentRoutePlan: () => WizardRoutePlan | null;
};

const REMOTE_SPECULATIVE_DAEMON_PROBE_TIMEOUT_MS = 5_000;

export function useWorkspaceSetupProviderProvisioning({
  currentStepKeyRef,
  selections,
  effectiveTarget,
  desktopApp,
  parsedRemoteHost,
  remoteStatusRef,
  connectDaemonForImport,
  shouldDeferSpeculativeLocalRefresh,
  commitProvisioningMachineState,
  isCurrentProvisioningRequest,
  getCurrentRoutePlan,
}: UseWorkspaceSetupProviderProvisioningArgs) {
  const shouldTreatRemoteSpeculativeConnectFailureAsEmpty = useCallback((
    location: "local" | "remote",
    error: unknown,
  ): boolean => {
    if (location !== "remote") {
      return false;
    }
    const detail = messageFromError(error).toLowerCase();
    return detail.includes("failed to reach remote daemon")
      || detail.includes("remote start skipped")
      || detail.includes("timed out waiting for daemon health")
      || detail.includes("timed out probing remote daemon")
      || (detail.includes("sending request get") && detail.includes("/api/health"));
  }, []);

  const connectDaemonForSpeculativeRefresh = useCallback(async (
    location: "local" | "remote",
  ): Promise<void> => {
    if (location === "remote") {
      await withTimeout(
        connectDaemonForImport(location),
        REMOTE_SPECULATIVE_DAEMON_PROBE_TIMEOUT_MS,
        "Timed out probing remote daemon.",
      );
      return;
    }
    await connectDaemonForImport(location);
  }, [connectDaemonForImport]);

  const shouldCompleteSpeculativeRefreshAsEmpty = useCallback((
    location: "local" | "remote",
    error: unknown,
  ): boolean => (
    shouldDeferSpeculativeLocalRefresh(location)
    || shouldTreatRemoteSpeculativeConnectFailureAsEmpty(location, error)
  ), [
    shouldDeferSpeculativeLocalRefresh,
    shouldTreatRemoteSpeculativeConnectFailureAsEmpty,
  ]);

  const authImportProvisioning = useWorkspaceSetupAuthImportProvisioning({
    desktopApp,
    parsedRemoteHost,
    remoteStatusRef,
    connectDaemonForSpeculativeRefresh,
    shouldCompleteSpeculativeRefreshAsEmpty,
    commitProvisioningMachineState,
    isCurrentProvisioningRequest,
  });

  const harnessDownloadsProvisioning = useWorkspaceSetupHarnessDownloadsProvisioning({
    currentStepKeyRef,
    selections,
    effectiveTarget,
    desktopApp,
    parsedRemoteHost,
    remoteStatusRef,
    connectDaemonForImport,
    connectDaemonForSpeculativeRefresh,
    shouldCompleteSpeculativeRefreshAsEmpty,
    commitProvisioningMachineState,
    isCurrentProvisioningRequest,
    getCurrentRoutePlan,
  });
  const { resetAuthImportProvisioningState } = authImportProvisioning;
  const { resetHarnessDownloadsProvisioningState } = harnessDownloadsProvisioning;

  const resetProviderProvisioningState = useCallback(() => {
    resetAuthImportProvisioningState();
    resetHarnessDownloadsProvisioningState();
  }, [
    resetAuthImportProvisioningState,
    resetHarnessDownloadsProvisioningState,
  ]);

  return {
    authImportCandidates: authImportProvisioning.authImportCandidates,
    authImportSelected: authImportProvisioning.authImportSelected,
    setAuthImportSelected: authImportProvisioning.setAuthImportSelected,
    authImportBusy: authImportProvisioning.authImportBusy,
    authImportError: authImportProvisioning.authImportError,
    setAuthImportBusy: authImportProvisioning.setAuthImportBusy,
    setAuthImportError: authImportProvisioning.setAuthImportError,
    harnessInstallCandidates: harnessDownloadsProvisioning.harnessInstallCandidates,
    harnessInstallSelected: harnessDownloadsProvisioning.harnessInstallSelected,
    setHarnessInstallSelected: harnessDownloadsProvisioning.setHarnessInstallSelected,
    harnessInstallBusy: harnessDownloadsProvisioning.harnessInstallBusy,
    harnessInstallError: harnessDownloadsProvisioning.harnessInstallError,
    harnessInstallRows: harnessDownloadsProvisioning.harnessInstallRows,
    localAdminPasswordPromptVisible: harnessDownloadsProvisioning.localAdminPasswordPromptVisible,
    localAdminPasswordInput: harnessDownloadsProvisioning.localAdminPasswordInput,
    setLocalAdminPasswordInput: harnessDownloadsProvisioning.setLocalAdminPasswordInput,
    cancelHarnessInstall: harnessDownloadsProvisioning.cancelHarnessInstall,
    advanceFromHarnessDownloadsStep: harnessDownloadsProvisioning.advanceFromHarnessDownloadsStep,
    selectedHarnessInstallTarget: harnessDownloadsProvisioning.selectedHarnessInstallTarget,
    harnessByProviderId: harnessDownloadsProvisioning.harnessByProviderId,
    selectedHarnessReadyToStartCount: harnessDownloadsProvisioning.selectedHarnessReadyToStartCount,
    selectedHarnessRunningCount: harnessDownloadsProvisioning.selectedHarnessRunningCount,
    selectedHarnessBlockedCount: harnessDownloadsProvisioning.selectedHarnessBlockedCount,
    selectedHarnessFailedCount: harnessDownloadsProvisioning.selectedHarnessFailedCount,
    harnessSummaryValue: harnessDownloadsProvisioning.harnessSummaryValue,
    resetProviderProvisioningState,
    scanAuthImportCandidatesForRequest: authImportProvisioning.scanAuthImportCandidatesForRequest,
    scanHarnessInstallCandidatesForRequest: harnessDownloadsProvisioning.scanHarnessInstallCandidatesForRequest,
  };
}
