import {
  useCallback,
  useEffect,
  useMemo,
  useReducer,
  useRef,
  type SetStateAction,
  type MutableRefObject,
} from "react";
import { useWorkspaceSetupCreate } from "./useWorkspaceSetupCreate";
import { useWorkspaceSetupFlow } from "./useWorkspaceSetupFlow";
import { useWorkspaceSetupProvisioning } from "./useWorkspaceSetupProvisioning";
import { useWorkspaceSetupRemote } from "./useWorkspaceSetupRemote";
import {
  applyWorkspaceSetupMachineEffect,
  executeWorkspaceSetupMachineCommand,
} from "./workspaceSetupCoordinator";
import {
  createInitialWorkspaceSetupMachineState,
  workspaceSetupMachineReducer,
  type WorkspaceSetupMachineSnapshot,
} from "./workspaceSetupMachine";
import {
  createInitialWorkflowDraftState,
  makeDraftFieldSetter,
  makeTargetDraftFieldSetter,
} from "./workflowReducer";
import {
  createWorkspaceSetupRouteScope,
  deriveWorkspaceSetupEffectiveTarget,
  serializeWorkspaceSetupRouteScope,
  type WorkspaceSetupDraftState,
  type WorkspaceSetupTargetDraft,
} from "./workflowTypes";
import { workspaceSetupWorkflowReducer } from "./workflowReducer";

type UseWorkspaceSetupWorkflowArgs = {
  navigate: (path: string, opts: { replace: boolean }) => void;
  wizardCompletedRef: MutableRefObject<boolean>;
  wizardKey: string;
  trackWizardCompleted: (payload: { wizardKey: string; workspaceKind: string }) => void;
};

type FieldSetters = {
  [K in keyof WorkspaceSetupDraftState]: (value: SetStateAction<WorkspaceSetupDraftState[K]>) => void;
};

type TargetDraftSetters = {
  [K in keyof WorkspaceSetupTargetDraft]: (value: SetStateAction<WorkspaceSetupTargetDraft[K]>) => void;
};

export function useWorkspaceSetupWorkflow({
  navigate,
  wizardCompletedRef,
  wizardKey,
  trackWizardCompleted,
}: UseWorkspaceSetupWorkflowArgs) {
  const [draft, dispatchDraft] = useReducer(
    workspaceSetupWorkflowReducer,
    undefined,
    createInitialWorkflowDraftState,
  );
  const [machineState, dispatchMachine] = useReducer(
    workspaceSetupMachineReducer,
    undefined,
    createInitialWorkspaceSetupMachineState,
  );
  const handledMachineEffectIdsRef = useRef<Set<number>>(new Set());
  const localAuthWarmupRouteKeyRef = useRef<string | null>(null);
  const localTitlingWarmupRouteKeyRef = useRef<string | null>(null);
  const machineSnapshotRef = useRef<WorkspaceSetupMachineSnapshot | null>(null);

  const setters = useMemo<FieldSetters>(() => ({
    targetDraft: makeDraftFieldSetter(dispatchDraft, "targetDraft"),
    sourcePath: makeDraftFieldSetter(dispatchDraft, "sourcePath"),
    repoUrl: makeDraftFieldSetter(dispatchDraft, "repoUrl"),
    repoBranch: makeDraftFieldSetter(dispatchDraft, "repoBranch"),
    workspaceName: makeDraftFieldSetter(dispatchDraft, "workspaceName"),
    networkAllowlist: makeDraftFieldSetter(dispatchDraft, "networkAllowlist"),
    setupHook: makeDraftFieldSetter(dispatchDraft, "setupHook"),
    targetBranch: makeDraftFieldSetter(dispatchDraft, "targetBranch"),
    targetBranchTouched: makeDraftFieldSetter(dispatchDraft, "targetBranchTouched"),
    verifyCommand: makeDraftFieldSetter(dispatchDraft, "verifyCommand"),
    pushOnSuccess: makeDraftFieldSetter(dispatchDraft, "pushOnSuccess"),
    pushRemote: makeDraftFieldSetter(dispatchDraft, "pushRemote"),
    pushBranch: makeDraftFieldSetter(dispatchDraft, "pushBranch"),
    pushBranchTouched: makeDraftFieldSetter(dispatchDraft, "pushBranchTouched"),
    createError: makeDraftFieldSetter(dispatchDraft, "createError"),
    importRepoStatus: makeDraftFieldSetter(dispatchDraft, "importRepoStatus"),
    importRepoNote: makeDraftFieldSetter(dispatchDraft, "importRepoNote"),
  }), []);

  const targetDraftSetters = useMemo<TargetDraftSetters>(() => ({
    remoteHostInput: makeTargetDraftFieldSetter(dispatchDraft, "remoteHostInput"),
    remotePortInput: makeTargetDraftFieldSetter(dispatchDraft, "remotePortInput"),
    remoteDataDirInput: makeTargetDraftFieldSetter(dispatchDraft, "remoteDataDirInput"),
  }), []);

  const flow = useWorkspaceSetupFlow({
    sourcePath: draft.sourcePath,
    repoUrl: draft.repoUrl,
  });

  const effectiveTarget = useMemo(
    () => deriveWorkspaceSetupEffectiveTarget(flow.selections.location, draft.targetDraft),
    [draft.targetDraft, flow.selections.location],
  );

  const remote = useWorkspaceSetupRemote({
    selections: flow.selections,
    stepKey: flow.currentStepKey,
    needsSourcePath: flow.needsSourcePath,
    sourcePath: draft.sourcePath,
    targetDraft: draft.targetDraft,
    effectiveTarget,
    setRemoteHostInput: targetDraftSetters.remoteHostInput,
    setRemotePortInput: targetDraftSetters.remotePortInput,
    setRemoteDataDirInput: targetDraftSetters.remoteDataDirInput,
    setImportRepoStatus: setters.importRepoStatus,
    setImportRepoNote: setters.importRepoNote,
    setTargetBranch: setters.targetBranch,
    setPushBranch: setters.pushBranch,
    targetBranchTouched: draft.targetBranchTouched,
    pushBranchTouched: draft.pushBranchTouched,
    onRemoteEndpointChanged: flow.invalidateRoutePlan,
  });

  const provisioning = useWorkspaceSetupProvisioning({
    currentStepKeyRef: flow.currentStepKeyRef,
    selections: flow.selections,
    routePlan: flow.routePlan,
    setRoutePlan: flow.setRoutePlan,
    setRoutePlanningBusy: flow.setRoutePlanningBusy,
    invalidateRoutePlan: flow.invalidateRoutePlan,
    desktopApp: remote.desktopApp,
    effectiveTarget,
    remoteStatus: remote.remoteStatus,
    remoteStatusRef: remote.remoteStatusRef,
    connectDaemonForImport: remote.connectDaemonForImport,
  });

  const create = useWorkspaceSetupCreate({
    currentStepKey: flow.currentStepKey,
    intent: {
      selections: flow.selections,
      sourcePath: draft.sourcePath,
      repoUrl: draft.repoUrl,
      repoBranch: draft.repoBranch,
      workspaceName: draft.workspaceName,
      networkAllowlist: draft.networkAllowlist,
      useSandboxStaging: flow.useSandboxStaging,
      importRepoStatus: draft.importRepoStatus,
      importRepoNote: draft.importRepoNote,
      targetBranch: draft.targetBranch,
      verifyCommand: draft.verifyCommand,
      mergeQueueSkipped: flow.mergeQueueSkipped,
      pushOnSuccess: draft.pushOnSuccess,
      pushRemote: draft.pushRemote,
      pushBranch: draft.pushBranch,
      setupHook: draft.setupHook,
      titlingStepVisible: provisioning.titlingStepVisible,
      titlingMode: provisioning.titlingMode,
      titlingRemoteValid: provisioning.titlingRemoteValid,
      titlingPersistError: provisioning.titlingPersistError,
    },
    ensureTitlingPersistedForCurrentTarget: provisioning.ensureTitlingPersistedForCurrentTarget,
    setSourcePath: setters.sourcePath,
    setImportRepoStatus: setters.importRepoStatus,
    setImportRepoNote: setters.importRepoNote,
    onOnboardingInsertionRequested: flow.goToStepKey,
    onCreateErrorStep: flow.goToStepKey,
    navigate,
    wizardCompletedRef,
    wizardKey,
    trackWizardCompleted,
    desktopApp: remote.desktopApp,
    remoteSshPasswordOnce: remote.remoteSshPasswordOnce,
    remoteSshPasswordCandidate: remote.remoteSshPasswordCandidate,
    remoteAdminPasswordOnce: remote.remoteAdminPasswordOnce,
    remoteAdminPasswordCandidate: remote.remoteAdminPasswordCandidate,
    effectiveTarget,
    connectDaemonForImport: remote.connectDaemonForImport,
    ensureOnboardingAfterDaemonConnect: provisioning.ensureOnboardingAfterDaemonConnect,
    waitForDaemonReady: remote.waitForDaemonReady,
    applyConnection: remote.applyConnection,
    rememberRemoteProfile: remote.rememberRemoteProfile,
    requestRemotePasswordPrompt: remote.requestRemotePasswordPrompt,
    setCreateError: setters.createError,
  });

  const machineSnapshot = useMemo<WorkspaceSetupMachineSnapshot>(() => ({
    stepKey: flow.step.key,
    routePlan: flow.routePlan,
    locationSelection: flow.selections.location,
    harnessInstallBusy: provisioning.harnessInstallBusy,
    harnessInstallError: provisioning.harnessInstallError,
    selectedHarnessReadyToStartCount: provisioning.selectedHarnessReadyToStartCount,
    selectedHarnessRunningCount: provisioning.selectedHarnessRunningCount,
    selectedHarnessFailedCount: provisioning.selectedHarnessFailedCount,
    titlingMode: provisioning.titlingMode,
    titlingRemoteValid: provisioning.titlingRemoteValid,
  }), [
    flow.routePlan,
    flow.selections.location,
    flow.step.key,
    provisioning.harnessInstallBusy,
    provisioning.harnessInstallError,
    provisioning.selectedHarnessFailedCount,
    provisioning.selectedHarnessReadyToStartCount,
    provisioning.selectedHarnessRunningCount,
    provisioning.titlingMode,
    provisioning.titlingRemoteValid,
  ]);

  const machineCommandHandlers = useMemo(() => ({
    verifyRemoteConnection: remote.verifyRemoteConnection,
    ensureRoutePlanForSelection: provisioning.ensureRoutePlanForSelection,
    advanceFromAuthImportStep: provisioning.advanceFromAuthImportStep,
    advanceFromHarnessDownloadsStep: provisioning.advanceFromHarnessDownloadsStep,
    onSelectTitlingLocal: provisioning.onSelectTitlingLocal,
    ensureTitlingPersistedForCurrentTarget: provisioning.ensureTitlingPersistedForCurrentTarget,
    preflightSourceStep: create.preflightSourceStep,
  }), [
    create.preflightSourceStep,
    provisioning.advanceFromAuthImportStep,
    provisioning.advanceFromHarnessDownloadsStep,
    provisioning.ensureRoutePlanForSelection,
    provisioning.ensureTitlingPersistedForCurrentTarget,
    provisioning.onSelectTitlingLocal,
    remote.verifyRemoteConnection,
  ]);

  const machineEffectHandlers = useMemo(() => ({
    goToStepKey: flow.goToStepKey,
    goRelativeStep: flow.goRelativeStep,
    setTitlingPersistError: provisioning.setTitlingPersistError,
    setTitlingMode: provisioning.setTitlingMode,
    invalidateTitlingPersisted: provisioning.invalidateTitlingPersisted,
    setRoutePlan: flow.setRoutePlan,
  }), [
    flow.goRelativeStep,
    flow.goToStepKey,
    flow.setRoutePlan,
    provisioning.invalidateTitlingPersisted,
    provisioning.setTitlingMode,
    provisioning.setTitlingPersistError,
  ]);

  useEffect(() => {
    machineSnapshotRef.current = machineSnapshot;
  }, [machineSnapshot]);

  useEffect(() => {
    if (machineState.pendingEffects.length === 0) return;

    const effectIds: number[] = [];
    for (const effect of machineState.pendingEffects) {
      if (handledMachineEffectIdsRef.current.has(effect.id)) {
        continue;
      }
      handledMachineEffectIdsRef.current.add(effect.id);
      effectIds.push(effect.id);
      if (effect.kind === "run_command") {
        void executeWorkspaceSetupMachineCommand(effect.command, machineCommandHandlers)
          .then((result) => {
            const currentSnapshot = machineSnapshotRef.current;
            if (!currentSnapshot) {
              return;
            }
            dispatchMachine({
              type: "command_completed",
              effectId: effect.id,
              result,
              snapshot: currentSnapshot,
            });
          });
        continue;
      }
      applyWorkspaceSetupMachineEffect(effect, machineEffectHandlers);
    }

    if (effectIds.length === 0) return;
    dispatchMachine({
      type: "effects_applied",
      effectIds,
    });
  }, [machineCommandHandlers, machineEffectHandlers, machineState.pendingEffects]);

  const onSelect = useCallback((stepKey: string, optionId: string) => {
    setters.createError(null);
    flow.selectOption(stepKey, optionId);
    if (stepKey === "location" && optionId === "local") {
      remote.resetForLocalSelection();
    }
    if (stepKey === "location") {
      flow.invalidateRoutePlan();
    }
    if (stepKey === "container" && optionId === "host") {
      setters.networkAllowlist("");
    }
    if (stepKey === "network" && optionId !== "allowlist") {
      setters.networkAllowlist("");
    }
    if (stepKey === "source") {
      if (optionId !== "clone") {
        setters.repoUrl("");
        setters.repoBranch("");
      }
      if (optionId !== "new") {
        setters.workspaceName("");
      }
      if (optionId !== "import") {
        setters.importRepoStatus("idle");
        setters.importRepoNote(null);
      }
    }
  }, [flow, remote, setters]);

  const onSelectOption = useCallback((stepKey: string, optionId: string) => {
    onSelect(stepKey, optionId);
    dispatchMachine({
      type: "option_selected",
      stepKey,
      optionId,
      snapshot: machineSnapshot,
    });
  }, [machineSnapshot, onSelect]);

  const onSkipAuthImport = useCallback(() => {
    dispatchMachine({
      type: "skip_auth_import_requested",
    });
  }, []);

  const onSkipHarnessDownloads = useCallback(() => {
    dispatchMachine({
      type: "skip_harness_downloads_requested",
      snapshot: machineSnapshot,
    });
  }, [machineSnapshot]);

  const onSelectTitlingLocal = useCallback(() => {
    dispatchMachine({
      type: "select_titling_local_requested",
    });
  }, []);

  const onSkipTitling = useCallback(() => {
    dispatchMachine({
      type: "skip_titling_requested",
      snapshot: machineSnapshot,
    });
  }, [machineSnapshot]);

  const onNext = useCallback(() => {
    setters.createError(null);
    dispatchMachine({
      type: "next_requested",
      snapshot: machineSnapshot,
    });
  }, [machineSnapshot, setters.createError]);

  useEffect(() => {
    if (draft.pushBranchTouched) return;
    if (!draft.targetBranch.trim()) return;
    setters.pushBranch(draft.targetBranch);
  }, [draft.pushBranchTouched, draft.targetBranch, setters]);

  useEffect(() => {
    if (!remote.desktopApp || flow.selections.location !== "local") {
      localTitlingWarmupRouteKeyRef.current = null;
      return;
    }
    const localTarget = deriveWorkspaceSetupEffectiveTarget("local", draft.targetDraft);
    if (!localTarget) {
      localTitlingWarmupRouteKeyRef.current = null;
      return;
    }
    const routeScope = createWorkspaceSetupRouteScope(localTarget, "host");
    const routeKey = serializeWorkspaceSetupRouteScope(routeScope);
    if (localTitlingWarmupRouteKeyRef.current === routeKey) {
      return;
    }
    localTitlingWarmupRouteKeyRef.current = routeKey;
    void provisioning.prefetchTitlingForCurrentTarget("local").catch(() => {});
  }, [
    draft.targetDraft,
    flow.selections.location,
    provisioning.prefetchTitlingForCurrentTarget,
    remote.desktopApp,
  ]);

  useEffect(() => {
    if (!remote.desktopApp || flow.step.key !== "location" || flow.selections.location) {
      localAuthWarmupRouteKeyRef.current = null;
      return;
    }

    const localTarget = deriveWorkspaceSetupEffectiveTarget("local", draft.targetDraft);
    if (!localTarget) {
      localAuthWarmupRouteKeyRef.current = null;
      return;
    }
    const routeScope = createWorkspaceSetupRouteScope(localTarget, "host");
    const routeKey = serializeWorkspaceSetupRouteScope(routeScope);
    if (localAuthWarmupRouteKeyRef.current === routeKey) {
      return;
    }
    localAuthWarmupRouteKeyRef.current = routeKey;
    void provisioning.refreshAuthImportForRouteScope("local", routeScope).catch(() => {});
  }, [
    draft.targetDraft,
    flow.selections.location,
    flow.step.key,
    provisioning.refreshAuthImportForRouteScope,
    remote.desktopApp,
  ]);

  useEffect(() => {
    dispatchMachine({
      type: "provisioning_snapshot_changed",
      snapshot: machineSnapshot,
    });
  }, [machineSnapshot]);

  const hasAllowlist = flow.step.key !== "network"
    || flow.selections.network !== "allowlist"
    || draft.networkAllowlist.split(/\r?\n/).map((line) => line.trim()).filter(Boolean).length > 0;
  const hasSourceStepInputs = flow.step.key !== "source" || flow.sourceStepValidation.isComplete;
  const hasTargetBranch = flow.step.key !== "merge-queue"
    || flow.mergeQueueSkipped
    || draft.targetBranch.trim() !== "";
  const titlingSelectionComplete = !provisioning.titlingStepVisible
    || provisioning.titlingMode === "skip"
    || (provisioning.titlingMode === "remote" && provisioning.titlingRemoteValid)
    || provisioning.titlingMode === "local";
  const titlingStepCanAdvance = flow.step.key !== "session-titling"
    || (!provisioning.titlingPersistBusy && titlingSelectionComplete);
  const canAdvance = (!flow.requiresSelection || flow.hasSelection)
    && !(flow.step.key === "location" && flow.selections.location === "remote" && (
      !remote.hasRemoteHost
      || remote.remoteStatus === "connecting"
      || effectiveTarget?.kind !== "remote"
    ))
    && !(flow.step.key === "container" && flow.routePlanningBusy)
    && hasSourceStepInputs
    && hasTargetBranch
    && hasAllowlist
    && (flow.step.key !== "auth-import" || !provisioning.authImportBusy)
    && (flow.step.key !== "harness-downloads" || !provisioning.harnessInstallBusy)
    && titlingStepCanAdvance;
  const nextButtonLabel = flow.step.key === "container" && flow.routePlanningBusy
    ? "Working..."
    : flow.step.key === "harness-downloads"
      ? (
        provisioning.harnessInstallBusy
          ? "Working..."
          : provisioning.selectedHarnessReadyToStartCount > 0
            ? "Start and continue"
            : "Continue"
      )
      : "Next";

  return {
    draft,
    setters,
    flow,
    remote,
    provisioning,
    create,
    canAdvance,
    nextButtonLabel,
    onSelect,
    onSelectOption,
    onSkipAuthImport,
    onSkipHarnessDownloads,
    onSelectTitlingLocal,
    onSkipTitling,
    onNext,
  };
}
