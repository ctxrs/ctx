import { useCallback, useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import { WorkspaceSetupPageView } from "./WorkspaceSetupPageView";
import { useWorkspaceSetupWorkflow } from "./useWorkspaceSetupWorkflow";
import {
  trackWizardAbandoned,
  trackWizardCompleted,
  trackWizardStarted,
  trackWizardStepCompleted,
  trackWizardStepViewed,
} from "../../utils/analytics";

export function WorkspaceSetupPageController() {
  const navigate = useNavigate();
  const [openInfoKey, setOpenInfoKey] = useState<string | null>(null);
  const [mergeAdvancedOpen, setMergeAdvancedOpen] = useState(false);
  const [harnessDownloadsCanScroll, setHarnessDownloadsCanScroll] = useState(false);
  const [harnessDownloadsAtBottom, setHarnessDownloadsAtBottom] = useState(true);

  const wizardKey = "workspace_setup" as const;
  const wizardCompletedRef = useRef(false);
  const wizardStartedRef = useRef(false);
  const harnessDownloadsScrollRef = useRef<HTMLDivElement | null>(null);

  const workflow = useWorkspaceSetupWorkflow({
    navigate: (path, opts) => navigate(path, opts),
    wizardCompletedRef,
    wizardKey,
    trackWizardCompleted: (payload: { wizardKey: string; workspaceKind: string }) => {
      trackWizardCompleted({
        wizardKey: payload.wizardKey as "workspace_setup",
        workspaceKind: payload.workspaceKind as "local" | "remote" | "unknown",
      });
    },
  });
  const lastWizardStepViewedRef = useRef<{ key: string; index: number }>({
    key: workflow.flow.step.key,
    index: workflow.flow.stepIndex,
  });
  const lastWizardStepCompletedRef = useRef<{ key: string; index: number } | null>(null);

  const infoStep = openInfoKey
    ? workflow.flow.steps.find((step) => step.key === openInfoKey) ?? null
    : null;
  const harnessDownloadsAdminPromptActive =
    workflow.flow.currentStepKey === "harness-downloads"
    && workflow.provisioning.localAdminPasswordPromptVisible;

  const updateHarnessDownloadsScrollState = useCallback(() => {
    const node = harnessDownloadsScrollRef.current;
    if (!node) {
      setHarnessDownloadsCanScroll(false);
      setHarnessDownloadsAtBottom(true);
      return;
    }
    const canScroll = node.scrollHeight - node.clientHeight > 2;
    const atBottom = !canScroll || node.scrollTop + node.clientHeight >= node.scrollHeight - 2;
    setHarnessDownloadsCanScroll(canScroll);
    setHarnessDownloadsAtBottom(atBottom);
  }, []);

  useEffect(() => {
    if (workflow.flow.currentStepKey !== "harness-downloads") {
      setHarnessDownloadsCanScroll(false);
      setHarnessDownloadsAtBottom(true);
      return;
    }
    const handle = window.requestAnimationFrame(() => {
      updateHarnessDownloadsScrollState();
    });
    window.addEventListener("resize", updateHarnessDownloadsScrollState);
    return () => {
      window.cancelAnimationFrame(handle);
      window.removeEventListener("resize", updateHarnessDownloadsScrollState);
    };
  }, [
    updateHarnessDownloadsScrollState,
    workflow.flow.currentStepKey,
    workflow.provisioning.harnessInstallBusy,
    workflow.provisioning.harnessInstallCandidates,
    workflow.provisioning.harnessInstallRows,
  ]);

  useEffect(() => {
    if (wizardStartedRef.current) return;
    wizardStartedRef.current = true;
    trackWizardStarted({ wizardKey });
    return () => {
      if (wizardCompletedRef.current) return;
      const last = lastWizardStepViewedRef.current;
      trackWizardAbandoned({
        wizardKey,
        lastStepKey: last.key,
        lastStepIndex: last.index,
      });
    };
  }, [wizardKey]);

  useEffect(() => {
    const previous = lastWizardStepViewedRef.current;
    if (workflow.flow.stepIndex > previous.index) {
      const alreadyCompleted = lastWizardStepCompletedRef.current;
      if (!alreadyCompleted || alreadyCompleted.key !== previous.key || alreadyCompleted.index !== previous.index) {
        trackWizardStepCompleted({
          wizardKey,
          stepKey: previous.key,
          stepIndex: previous.index,
        });
        lastWizardStepCompletedRef.current = previous;
      }
    }
    lastWizardStepViewedRef.current = { key: workflow.flow.step.key, index: workflow.flow.stepIndex };
    trackWizardStepViewed({
      wizardKey,
      stepKey: workflow.flow.step.key,
      stepIndex: workflow.flow.stepIndex,
    });
  }, [wizardKey, workflow.flow.step.key, workflow.flow.stepIndex]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      if (workflow.create.importInitDialog) {
        workflow.create.resolveImportInitDialog(false);
        return;
      }
      setOpenInfoKey(null);
    };
    if (openInfoKey || workflow.create.importInitDialog) {
      window.addEventListener("keydown", onKeyDown);
      return () => window.removeEventListener("keydown", onKeyDown);
    }
    return;
  }, [openInfoKey, workflow.create.importInitDialog, workflow.create.resolveImportInitDialog]);

  return (
    <WorkspaceSetupPageView
      importInitDialog={workflow.create.importInitDialog}
      resolveImportInitDialog={workflow.create.resolveImportInitDialog}
      infoStep={infoStep}
      openInfoKey={openInfoKey}
      setOpenInfoKey={setOpenInfoKey}
      step={workflow.flow.step}
      steps={workflow.flow.steps}
      stepIndex={workflow.flow.stepIndex}
      selections={workflow.flow.selections}
      createError={workflow.draft.createError}
      setCreateError={workflow.setters.createError}
      showLaunchPanel={workflow.create.showLaunchPanel}
      launchSnapshot={workflow.create.launchSnapshot}
      currentLaunchStepLabel={workflow.create.currentLaunchStepLabel}
      currentLaunchElapsed={workflow.create.currentLaunchElapsed}
      currentLaunchEtaLabel={workflow.create.currentLaunchEtaLabel}
      launchCopyLabel={workflow.create.launchCopyLabel}
      onCopyLaunchDiagnostics={() => {
        void workflow.create.onCopyLaunchDiagnostics();
      }}
      launchLogs={workflow.create.launchLogs}
      onSelectOption={workflow.onSelectOption}
      networkAllowlist={workflow.draft.networkAllowlist}
      setNetworkAllowlist={workflow.setters.networkAllowlist}
      remoteHostInput={workflow.remote.remoteHostInput}
      onRemoteInputChange={workflow.remote.onRemoteInputChange}
      remotePortInput={workflow.remote.remotePortInput}
      onRemotePortInputChange={workflow.remote.onRemotePortInputChange}
      remoteDataDirInput={workflow.remote.remoteDataDirInput}
      onRemoteDataDirInputChange={workflow.remote.onRemoteDataDirInputChange}
      localAdminPasswordPromptVisible={
        workflow.create.localAdminPasswordPromptVisible
        || harnessDownloadsAdminPromptActive
      }
      localAdminPasswordInput={
        harnessDownloadsAdminPromptActive
          ? workflow.provisioning.localAdminPasswordInput
          : workflow.create.localAdminPasswordInput
      }
      setLocalAdminPasswordInput={(value) => {
        if (harnessDownloadsAdminPromptActive) {
          workflow.provisioning.setLocalAdminPasswordInput(value);
          return;
        }
        workflow.create.setLocalAdminPasswordInput(value);
      }}
      remotePasswordPromptVisible={workflow.remote.remotePasswordPromptVisible}
      remotePasswordPromptMode={workflow.remote.remotePasswordPromptMode}
      remotePasswordInput={workflow.remote.remotePasswordInput}
      onRemotePasswordInputChange={workflow.remote.onRemotePasswordInputChange}
      remoteStatus={workflow.remote.remoteStatus}
      setRemoteStatus={workflow.remote.setRemoteStatus}
      remoteError={workflow.remote.remoteError}
      setRemoteError={workflow.remote.setRemoteError}
      sshSuggestions={workflow.remote.sshSuggestions}
      authImportBusy={workflow.provisioning.authImportBusy}
      authImportError={workflow.provisioning.authImportError}
      authImportCandidates={workflow.provisioning.authImportCandidates}
      authImportSelected={workflow.provisioning.authImportSelected}
      setAuthImportSelected={workflow.provisioning.setAuthImportSelected}
      harnessByProviderId={workflow.provisioning.harnessByProviderId}
      onSkipAuthImport={workflow.onSkipAuthImport}
      harnessInstallBusy={workflow.provisioning.harnessInstallBusy}
      harnessInstallError={workflow.provisioning.harnessInstallError}
      selectedHarnessRunningCount={workflow.provisioning.selectedHarnessRunningCount}
      selectedHarnessBlockedCount={workflow.provisioning.selectedHarnessBlockedCount}
      harnessInstallCandidates={workflow.provisioning.harnessInstallCandidates}
      harnessDownloadsCanScroll={harnessDownloadsCanScroll}
      harnessDownloadsAtBottom={harnessDownloadsAtBottom}
      harnessDownloadsScrollRef={harnessDownloadsScrollRef}
      updateHarnessDownloadsScrollState={updateHarnessDownloadsScrollState}
      harnessInstallSelected={workflow.provisioning.harnessInstallSelected}
      setHarnessInstallSelected={workflow.provisioning.setHarnessInstallSelected}
      harnessInstallRows={workflow.provisioning.harnessInstallRows}
      selectedHarnessInstallTarget={workflow.provisioning.selectedHarnessInstallTarget}
      cancelHarnessInstall={(providerId) => {
        void workflow.provisioning.cancelHarnessInstall(providerId);
      }}
      onSkipHarnessDownloads={workflow.onSkipHarnessDownloads}
      titlingProbeBusy={workflow.provisioning.titlingProbeBusy}
      titlingProbeError={workflow.provisioning.titlingProbeError}
      titlingPersistError={workflow.provisioning.titlingPersistError}
      titlingStatusError={workflow.provisioning.titlingStatusError}
      titlingMode={workflow.provisioning.titlingMode}
      setTitlingMode={workflow.provisioning.setTitlingMode}
      titlingLocalInstallBusy={workflow.provisioning.titlingLocalInstallBusy}
      titlingPersistBusy={workflow.provisioning.titlingPersistBusy}
      onSelectTitlingLocal={workflow.onSelectTitlingLocal}
      titlingLocalStatus={workflow.provisioning.titlingLocalStatus}
      titlingLocalInstall={workflow.provisioning.titlingLocalInstall}
      titlingRemoteBaseUrl={workflow.provisioning.titlingRemoteBaseUrl}
      setTitlingRemoteBaseUrl={workflow.provisioning.setTitlingRemoteBaseUrl}
      titlingRemoteApiKey={workflow.provisioning.titlingRemoteApiKey}
      setTitlingRemoteApiKey={workflow.provisioning.setTitlingRemoteApiKey}
      titlingRemoteModel={workflow.provisioning.titlingRemoteModel}
      setTitlingRemoteModel={workflow.provisioning.setTitlingRemoteModel}
      titlingRemoteAdvancedOpen={workflow.provisioning.titlingRemoteAdvancedOpen}
      setTitlingRemoteAdvancedOpen={workflow.provisioning.setTitlingRemoteAdvancedOpen}
      titlingRemoteUseJson={workflow.provisioning.titlingRemoteUseJson}
      setTitlingRemoteUseJson={workflow.provisioning.setTitlingRemoteUseJson}
      invalidateTitlingPersisted={workflow.provisioning.invalidateTitlingPersisted}
      onSkipTitling={workflow.onSkipTitling}
      needsSourcePath={workflow.flow.needsSourcePath}
      sourcePath={workflow.draft.sourcePath}
      setSourcePath={workflow.setters.sourcePath}
      onPickLocalFolder={() => {
        void workflow.create.onPickLocalFolder();
      }}
      importRepoStatus={workflow.draft.importRepoStatus}
      importRepoNote={workflow.draft.importRepoNote}
      remotePathSuggestions={workflow.remote.remotePathSuggestions}
      remotePathStatus={workflow.remote.remotePathStatus}
      remotePathError={workflow.remote.remotePathError}
      repoUrl={workflow.draft.repoUrl}
      setRepoUrl={workflow.setters.repoUrl}
      repoBranch={workflow.draft.repoBranch}
      setRepoBranch={workflow.setters.repoBranch}
      useSandboxStaging={workflow.flow.useSandboxStaging}
      setupHook={workflow.draft.setupHook}
      setSetupHook={workflow.setters.setupHook}
      workspaceName={workflow.draft.workspaceName}
      setWorkspaceName={workflow.setters.workspaceName}
      mergeQueueSkipped={workflow.flow.mergeQueueSkipped}
      targetBranch={workflow.draft.targetBranch}
      setTargetBranch={workflow.setters.targetBranch}
      setTargetBranchTouched={workflow.setters.targetBranchTouched}
      verifyCommand={workflow.draft.verifyCommand}
      setVerifyCommand={workflow.setters.verifyCommand}
      mergeAdvancedOpen={mergeAdvancedOpen}
      setMergeAdvancedOpen={setMergeAdvancedOpen}
      pushOnSuccess={workflow.draft.pushOnSuccess}
      setPushOnSuccess={workflow.setters.pushOnSuccess}
      pushRemote={workflow.draft.pushRemote}
      setPushRemote={workflow.setters.pushRemote}
      pushBranch={workflow.draft.pushBranch}
      setPushBranch={workflow.setters.pushBranch}
      setPushBranchTouched={workflow.setters.pushBranchTouched}
      enableMergeQueueIfSkipped={() => {
        if (!workflow.flow.mergeQueueSkipped) return;
        workflow.flow.clearSelection("merge-queue");
      }}
      onMergeSkip={() => {
        workflow.onSelect("merge-queue", "skip");
        setMergeAdvancedOpen(false);
        workflow.setters.pushOnSuccess(false);
        workflow.flow.goRelativeStep(1);
      }}
      harnessSummaryValue={workflow.provisioning.harnessSummaryValue}
      titlingSummaryValue={workflow.provisioning.titlingSummaryValue}
      sourceStepComplete={workflow.flow.sourceStepValidation.isComplete}
      titlingRemoteValid={workflow.provisioning.titlingRemoteValid}
      hasRemoteHost={workflow.remote.hasRemoteHost}
      goToStepKey={workflow.flow.goToStepKey}
      isFirst={workflow.flow.isFirst}
      isLast={workflow.flow.isLast}
      canAdvance={workflow.canAdvance}
      creating={workflow.create.creating}
      onCreate={() => {
        void workflow.create.onCreate();
      }}
      onNext={() => {
        void workflow.onNext();
      }}
      goRelativeStep={workflow.flow.goRelativeStep}
      createButtonLabel={workflow.create.createButtonLabel}
      nextButtonLabel={workflow.nextButtonLabel}
    />
  );
}
