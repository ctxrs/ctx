import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { sameProvisioningScope } from "../../state/scopeIdentity";
import {
  beginWorkspaceSetupProvisioningRefresh,
  createInitialWorkspaceSetupProvisioningMachineState,
  hasReadyWorkspaceSetupProvisioningStateForRouteScope,
  type WorkspaceSetupProvisioningMachineState,
  type WorkspaceSetupProvisioningRefreshReason,
  type WorkspaceSetupProvisioningRequest,
} from "./workspaceSetupProvisioningMachine";
import { messageFromError } from "./wizardTypes";
import type {
  EnsureOnboardingAfterDaemonConnectResult,
  WorkspaceSetupRouteScope,
} from "./workflowTypes";
import {
  createWorkspaceSetupRouteScope,
  sameWorkspaceSetupRouteScope,
  serializeWorkspaceSetupRouteScope,
} from "./workflowTypes";
import { useWorkspaceSetupProviderProvisioning } from "./useWorkspaceSetupProviderProvisioning";
import { advanceWorkspaceSetupAuthImportStep } from "./advanceWorkspaceSetupAuthImportStep";
import { ensureWorkspaceSetupSandboxWarmup } from "./ensureWorkspaceSetupSandboxWarmup";
import {
  type UseWorkspaceSetupProvisioningArgs,
} from "./useWorkspaceSetupProvisioning.types";
import type { WizardRoutePlan } from "./wizardFlow";
import { useWorkspaceSetupTitlingProvisioning } from "./useWorkspaceSetupTitlingProvisioning";

export function useWorkspaceSetupProvisioning({
  currentStepKeyRef,
  selections,
  routePlan,
  setRoutePlan,
  setRoutePlanningBusy,
  invalidateRoutePlan,
  desktopApp,
  effectiveTarget,
  remoteStatus,
  remoteStatusRef,
  connectDaemonForImport,
}: UseWorkspaceSetupProvisioningArgs) {
  const provisioningMachineStateRef = useRef<WorkspaceSetupProvisioningMachineState>(
    createInitialWorkspaceSetupProvisioningMachineState(),
  );
  const [, setProvisioningMachineState] = useState<WorkspaceSetupProvisioningMachineState>(
    () => provisioningMachineStateRef.current,
  );
  const previousTargetKeyRef = useRef<string | null>(null);
  const sandboxWarmupTargetKeyRef = useRef<string | null>(null);

  const selectedDaemonTargetKey = effectiveTarget?.targetKey ?? null;
  const remoteTarget = effectiveTarget?.kind === "remote" ? effectiveTarget : null;
  const parsedRemoteHost = remoteTarget?.host;
  const currentRouteScope = useMemo<WorkspaceSetupRouteScope | null>(() => {
    const containerSelection = (selections.container ?? "").trim();
    if (!effectiveTarget || !containerSelection) {
      return null;
    }
    return createWorkspaceSetupRouteScope(effectiveTarget, containerSelection);
  }, [effectiveTarget, selections.container]);
  const authImportStepVisible = Boolean(routePlan?.includeAuthImport);
  const harnessInstallStepVisible = Boolean(routePlan?.includeHarnessDownloads);
  const titlingStepVisible = Boolean(routePlan?.includeTitling);
  const canProbeTitling = desktopApp && (
    selections.location === "local"
    || (
      selections.location === "remote"
      && Boolean(parsedRemoteHost)
      && remoteStatus === "connected"
    )
  );

  const commitProvisioningMachineState = useCallback((
    updater:
      | WorkspaceSetupProvisioningMachineState
      | ((current: WorkspaceSetupProvisioningMachineState) => WorkspaceSetupProvisioningMachineState),
  ): WorkspaceSetupProvisioningMachineState => {
    const nextState = typeof updater === "function"
      ? updater(provisioningMachineStateRef.current)
      : updater;
    provisioningMachineStateRef.current = nextState;
    setProvisioningMachineState(nextState);
    return nextState;
  }, []);

  const isCurrentProvisioningRequest = useCallback((
    resource: WorkspaceSetupProvisioningRequest["resource"],
    request: WorkspaceSetupProvisioningRequest,
  ): boolean => {
    const current = provisioningMachineStateRef.current[resource];
    return current.requestId === request.requestId
      && Boolean(current.scope)
      && sameProvisioningScope(current.scope!, request.scope);
  }, []);

  const getCurrentRoutePlan = useCallback(
    (): WizardRoutePlan | null => provisioningMachineStateRef.current.routePlan ?? routePlan,
    [routePlan],
  );

  const shouldDeferSpeculativeLocalRefresh = useCallback((location: "local" | "remote"): boolean => {
    if (location !== "local") {
      return false;
    }
    const refreshReason = provisioningMachineStateRef.current.refreshReason;
    return refreshReason === "refresh_auth_import" || refreshReason === "ensure_route_plan";
  }, []);

  const shouldTreatRemoteSpeculativeConnectFailureAsSkipped = useCallback((error: unknown): boolean => {
    if (selections.location !== "remote") {
      return false;
    }
    const detail = messageFromError(error).toLowerCase();
    return detail.includes("failed to reach remote daemon")
      || detail.includes("remote start skipped")
      || detail.includes("timed out waiting for daemon health")
      || detail.includes("timed out loading daemon settings.")
      || detail.includes("timed out probing remote daemon")
      || (detail.includes("sending request get") && detail.includes("/api/health"));
  }, [selections.location]);

  const titlingProvisioning = useWorkspaceSetupTitlingProvisioning({
    selections,
    titlingStepVisible,
    desktopApp,
    parsedRemoteHost,
    selectedDaemonTargetKey,
    canProbeTitling,
    remoteStatusRef,
    connectDaemonForImport,
    commitProvisioningMachineState,
    isCurrentProvisioningRequest,
    shouldTreatRemoteSpeculativeConnectFailureAsSkipped,
  });

  const providerProvisioning = useWorkspaceSetupProviderProvisioning({
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
  });

  const {
    resetProviderProvisioningState,
    scanAuthImportCandidatesForRequest,
    scanHarnessInstallCandidatesForRequest,
  } = providerProvisioning;
  const {
    resetTitlingProvisioningState,
    scanTitlingProbeForRequest,
    titlingMode,
  } = titlingProvisioning;

  const resetProvisioningState = useCallback(() => {
    resetProviderProvisioningState();
    resetTitlingProvisioningState();
    sandboxWarmupTargetKeyRef.current = null;
    commitProvisioningMachineState(createInitialWorkspaceSetupProvisioningMachineState());
  }, [
    commitProvisioningMachineState,
    resetProviderProvisioningState,
    resetTitlingProvisioningState,
  ]);

  const refreshProvisioningForRouteScope = useCallback(async (
    location: "local" | "remote",
    routeScope: WorkspaceSetupRouteScope,
    refreshReason: WorkspaceSetupProvisioningRefreshReason,
    options?: {
      allowTitlingInsertion?: boolean;
      resources?: WorkspaceSetupProvisioningRequest["resource"][];
      force?: boolean;
    },
  ): Promise<WorkspaceSetupProvisioningMachineState | null> => {
    const refreshStart = beginWorkspaceSetupProvisioningRefresh(provisioningMachineStateRef.current, {
      routeScope,
      refreshReason,
      titlingMode,
      previousPlan: routePlan,
      allowTitlingInsertion: options?.allowTitlingInsertion,
      resources: options?.resources,
      force: options?.force,
    });
    const started = commitProvisioningMachineState(refreshStart.state);
    const { requests } = refreshStart;
    if (requests.length === 0) {
      if (started.routeScope && sameWorkspaceSetupRouteScope(started.routeScope, routeScope)) {
        setRoutePlan(started.routePlan);
        return started;
      }
      return null;
    }

    await Promise.all(requests.map(async (request) => {
      switch (request.resource) {
        case "authImport":
          await scanAuthImportCandidatesForRequest(location, request);
          return;
        case "harnessCandidates":
          await scanHarnessInstallCandidatesForRequest(location, request);
          return;
        case "titlingProbe":
          await scanTitlingProbeForRequest(request);
          return;
        default: {
          const exhaustiveCheck: never = request.resource;
          return exhaustiveCheck;
        }
      }
    }));

    const current = provisioningMachineStateRef.current;
    if (!current.routeScope || !sameWorkspaceSetupRouteScope(current.routeScope, routeScope)) {
      return null;
    }
    setRoutePlan(current.routePlan);
    return current;
  }, [
    commitProvisioningMachineState,
    routePlan,
    scanAuthImportCandidatesForRequest,
    scanHarnessInstallCandidatesForRequest,
    scanTitlingProbeForRequest,
    setRoutePlan,
    titlingMode,
  ]);

  const refreshAuthImportForRouteScope = useCallback(async (
    location: "local" | "remote",
    routeScope: WorkspaceSetupRouteScope,
    options?: { force?: boolean },
  ): Promise<void> => {
    await refreshProvisioningForRouteScope(
      location,
      routeScope,
      "refresh_auth_import",
      {
        force: options?.force,
        resources: ["authImport"],
      },
    );
  }, [refreshProvisioningForRouteScope]);

  const ensureTitlingProbeForCurrentTarget = useCallback(async (
    options?: { force?: boolean },
  ): Promise<boolean | null> => {
    const location = selections.location;
    if (!currentRouteScope || (location !== "local" && location !== "remote")) {
      return null;
    }
    if (location === "remote") {
      if (!parsedRemoteHost || remoteStatusRef.current !== "connected") {
        return null;
      }
    }
    const nextState = await refreshProvisioningForRouteScope(
      location,
      currentRouteScope,
      "refresh_titling_probe",
      {
        force: options?.force,
        resources: ["titlingProbe"],
      },
    );
    if (!nextState) return null;
    return nextState.titlingProbe.status === "error"
      ? true
      : nextState.titlingProbe.data?.required === true;
  }, [
    currentRouteScope,
    parsedRemoteHost,
    refreshProvisioningForRouteScope,
    remoteStatusRef,
    selections.location,
  ]);

  const resetRoutePlan = useCallback(() => {
    invalidateRoutePlan();
  }, [invalidateRoutePlan]);

  const ensureOnboardingAfterDaemonConnect = useCallback(async (
    options?: { allowTitlingInsertion?: boolean },
  ): Promise<EnsureOnboardingAfterDaemonConnectResult | null> => {
    const location = selections.location;
    if (!currentRouteScope || (location !== "local" && location !== "remote")) {
      return null;
    }
    const nextState = await refreshProvisioningForRouteScope(
      location,
      currentRouteScope,
      "ensure_onboarding_after_connect",
      {
        allowTitlingInsertion: options?.allowTitlingInsertion,
        force: true,
      },
    );
    if (!nextState?.routePlan) {
      return null;
    }
    return {
      routePlan: nextState.routePlan,
      insertionStep: nextState.insertionStep,
    };
  }, [
    currentRouteScope,
    refreshProvisioningForRouteScope,
    selections.location,
  ]);

  const ensureSandboxWarmupForRouteScope = useCallback(async (
    location: "local" | "remote",
    routeScope: WorkspaceSetupRouteScope,
  ): Promise<void> => ensureWorkspaceSetupSandboxWarmup(
    { desktopApp, location, routeScope, sandboxWarmupTargetKeyRef, connectDaemonForImport },
  ), [
    connectDaemonForImport,
    desktopApp,
  ]);

  const ensureRoutePlanForSelection = useCallback(async (
    containerSelectionOverride?: string,
  ): Promise<WizardRoutePlan | null> => {
    const location = selections.location;
    const containerSelection = (containerSelectionOverride ?? selections.container ?? "").trim();
    if (!effectiveTarget || !containerSelection || (location !== "local" && location !== "remote")) {
      return null;
    }
    const requestedRouteScope = createWorkspaceSetupRouteScope(effectiveTarget, containerSelection);
    const requestedRouteKey = serializeWorkspaceSetupRouteScope(requestedRouteScope);
    const currentPlan = provisioningMachineStateRef.current.routePlan ?? routePlan;
    if (
      currentPlan?.targetKey === requestedRouteKey
      && hasReadyWorkspaceSetupProvisioningStateForRouteScope(
        provisioningMachineStateRef.current,
        requestedRouteScope,
      )
    ) {
      await ensureSandboxWarmupForRouteScope(location, requestedRouteScope);
      return currentPlan;
    }

    setRoutePlanningBusy(true);
    try {
      const nextState = await refreshProvisioningForRouteScope(location, requestedRouteScope, "ensure_route_plan");
      const nextRoutePlan = nextState?.routePlan ?? null;
      if (nextRoutePlan) {
        await ensureSandboxWarmupForRouteScope(location, requestedRouteScope);
      }
      return nextRoutePlan;
    } finally {
      setRoutePlanningBusy(false);
    }
  }, [
    ensureSandboxWarmupForRouteScope,
    effectiveTarget,
    refreshProvisioningForRouteScope,
    routePlan,
    selections.container,
    selections.location,
    setRoutePlanningBusy,
  ]);

  const advanceFromAuthImportStep = async (
    options?: { clearSelections?: boolean },
  ): Promise<WizardRoutePlan | null> => advanceWorkspaceSetupAuthImportStep(
    {
      authImportBusy: providerProvisioning.authImportBusy,
      authImportSelected: providerProvisioning.authImportSelected,
      setAuthImportSelected: providerProvisioning.setAuthImportSelected,
      authImportCandidates: providerProvisioning.authImportCandidates,
      setAuthImportBusy: providerProvisioning.setAuthImportBusy,
      setAuthImportError: providerProvisioning.setAuthImportError,
      connectDaemonForImport,
      ensureTitlingProbeForCurrentTarget,
      currentStepKeyRef,
      getCurrentRoutePlan,
    },
    options,
  );

  useEffect(() => {
    const targetChanged = previousTargetKeyRef.current !== selectedDaemonTargetKey;
    previousTargetKeyRef.current = selectedDaemonTargetKey;
    if (!targetChanged) return;
    resetProvisioningState();
    resetRoutePlan();
  }, [resetProvisioningState, resetRoutePlan, selectedDaemonTargetKey]);

  useEffect(() => {
    if (selections.location !== "local" || (selections.container ?? "").trim() !== "sandbox") {
      sandboxWarmupTargetKeyRef.current = null;
    }
  }, [selections.container, selections.location]);

  useEffect(() => {
    if (!routePlan) return;
    const activeTargetKey = currentRouteScope
      ? serializeWorkspaceSetupRouteScope(currentRouteScope)
      : null;
    if (activeTargetKey === routePlan.targetKey) return;
    resetRoutePlan();
  }, [
    currentRouteScope,
    resetRoutePlan,
    routePlan,
  ]);

  return {
    authImportStepVisible,
    authImportCandidates: providerProvisioning.authImportCandidates,
    authImportSelected: providerProvisioning.authImportSelected,
    setAuthImportSelected: providerProvisioning.setAuthImportSelected,
    authImportBusy: providerProvisioning.authImportBusy,
    authImportError: providerProvisioning.authImportError,
    advanceFromAuthImportStep,
    harnessInstallStepVisible,
    harnessInstallCandidates: providerProvisioning.harnessInstallCandidates,
    harnessInstallSelected: providerProvisioning.harnessInstallSelected,
    setHarnessInstallSelected: providerProvisioning.setHarnessInstallSelected,
    harnessInstallBusy: providerProvisioning.harnessInstallBusy,
    harnessInstallError: providerProvisioning.harnessInstallError,
    harnessInstallRows: providerProvisioning.harnessInstallRows,
    localAdminPasswordPromptVisible: providerProvisioning.localAdminPasswordPromptVisible,
    localAdminPasswordInput: providerProvisioning.localAdminPasswordInput,
    setLocalAdminPasswordInput: providerProvisioning.setLocalAdminPasswordInput,
    cancelHarnessInstall: providerProvisioning.cancelHarnessInstall,
    advanceFromHarnessDownloadsStep: providerProvisioning.advanceFromHarnessDownloadsStep,
    selectedHarnessInstallTarget: providerProvisioning.selectedHarnessInstallTarget,
    harnessByProviderId: providerProvisioning.harnessByProviderId,
    selectedHarnessReadyToStartCount: providerProvisioning.selectedHarnessReadyToStartCount,
    selectedHarnessRunningCount: providerProvisioning.selectedHarnessRunningCount,
    selectedHarnessBlockedCount: providerProvisioning.selectedHarnessBlockedCount,
    selectedHarnessFailedCount: providerProvisioning.selectedHarnessFailedCount,
    harnessSummaryValue: providerProvisioning.harnessSummaryValue,
    titlingStepVisible: titlingProvisioning.titlingStepVisible,
    titlingProbeBusy: titlingProvisioning.titlingProbeBusy,
    titlingProbeError: titlingProvisioning.titlingProbeError,
    titlingConfiguredReady: titlingProvisioning.titlingConfiguredReady,
    titlingMode: titlingProvisioning.titlingMode,
    setTitlingMode: titlingProvisioning.setTitlingMode,
    titlingRemoteBaseUrl: titlingProvisioning.titlingRemoteBaseUrl,
    setTitlingRemoteBaseUrl: titlingProvisioning.setTitlingRemoteBaseUrl,
    titlingRemoteApiKey: titlingProvisioning.titlingRemoteApiKey,
    setTitlingRemoteApiKey: titlingProvisioning.setTitlingRemoteApiKey,
    titlingRemoteModel: titlingProvisioning.titlingRemoteModel,
    setTitlingRemoteModel: titlingProvisioning.setTitlingRemoteModel,
    titlingRemoteUseJson: titlingProvisioning.titlingRemoteUseJson,
    setTitlingRemoteUseJson: titlingProvisioning.setTitlingRemoteUseJson,
    titlingRemoteAdvancedOpen: titlingProvisioning.titlingRemoteAdvancedOpen,
    setTitlingRemoteAdvancedOpen: titlingProvisioning.setTitlingRemoteAdvancedOpen,
    titlingLocalStatus: titlingProvisioning.titlingLocalStatus,
    titlingStatusError: titlingProvisioning.titlingStatusError,
    titlingLocalInstallBusy: titlingProvisioning.titlingLocalInstallBusy,
    titlingLocalInstall: titlingProvisioning.titlingLocalInstall,
    titlingPersistBusy: titlingProvisioning.titlingPersistBusy,
    titlingPersistError: titlingProvisioning.titlingPersistError,
    setTitlingPersistError: titlingProvisioning.setTitlingPersistError,
    titlingRemoteValid: titlingProvisioning.titlingRemoteValid,
    titlingSummaryValue: titlingProvisioning.titlingSummaryValue,
    invalidateTitlingPersisted: titlingProvisioning.invalidateTitlingPersisted,
    ensureTitlingProbeForCurrentTarget,
    ensureTitlingPersistedForCurrentTarget: titlingProvisioning.ensureTitlingPersistedForCurrentTarget,
    ensureOnboardingAfterDaemonConnect,
    refreshAuthImportForRouteScope,
    prefetchTitlingForCurrentTarget: titlingProvisioning.prefetchTitlingForCurrentTarget,
    onSelectTitlingLocal: titlingProvisioning.onSelectTitlingLocal,
    ensureRoutePlanForSelection,
    resetRoutePlan,
  };
}
