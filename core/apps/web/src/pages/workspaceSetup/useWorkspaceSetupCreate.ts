import { type MutableRefObject, useEffect, useState } from "react";
import {
  createWorkspace,
  deleteWorkspace,
  idToString,
  updateWorkspaceExecutionConfig,
  updateWorkspaceMergeQueueConfig,
  updateWorkspaceWorktreeBootstrapConfig,
} from "../../api/client";
import { desktopConnectLocal, desktopConnectSsh } from "../../utils/desktop";
import { trackWorkspaceLaunchCompleted } from "../../utils/analytics";
import { upsertLauncherRecent } from "../../state/launcherRecentsStore";
import {
  buildWorkspaceSetupCreateIntent,
  parseNetworkAllowlist,
  resolveCreateErrorStepKey,
  type WorkspaceSetupCreateIntent,
} from "./createHandoff";
import type { WizardStepKey } from "./wizardFlow";
import { lastPathSegment, messageFromError } from "./wizardTypes";
import { waitForWorkspaceBootstrapBeforeNavigation } from "../workspaceBootstrapGate";
import type {
  RoutePlanInsertionStep,
  WorkspaceSetupEffectiveTarget,
} from "./workflowTypes";
import {
  prepareWorkspaceSetupSandboxRuntime,
  waitForWorkspaceSetupLaunchCompletion,
} from "./workspaceSetupLaunchHelpers";
import { seedDaemonTelemetryPreferenceIfDefault } from "./workspaceSetupTelemetry";
import { useWorkspaceSetupCreateProgress } from "./useWorkspaceSetupCreateProgress";
import { useWorkspaceSetupSourcePreflight } from "./useWorkspaceSetupSourcePreflight";
import { prepareWorkspaceSetupSource } from "./workspaceSetupCreateSource";
import type { WorkspaceSetupProvisioningSource } from "./launchProgress";

type UseWorkspaceSetupCreateArgs = {
  currentStepKey: WizardStepKey;
  intent: WorkspaceSetupCreateIntent;
  ensureTitlingPersistedForCurrentTarget: () => Promise<boolean>;
  setSourcePath: (value: string) => void;
  setImportRepoStatus: (status: "idle" | "checking" | "ok" | "error") => void;
  setImportRepoNote: (note: string | null) => void;
  onOnboardingInsertionRequested: (stepKey: RoutePlanInsertionStep) => void;
  onCreateErrorStep: (stepKey: WizardStepKey) => void;
  navigate: (path: string, opts: { replace: boolean }) => void;
  wizardCompletedRef: MutableRefObject<boolean>;
  wizardKey: string;
  trackWizardCompleted: (payload: { wizardKey: string; workspaceKind: string }) => void;
  desktopApp: boolean;
  remoteSshPasswordOnce: string | null;
  remoteSshPasswordCandidate: string | null;
  remoteAdminPasswordOnce: string | null;
  remoteAdminPasswordCandidate: string | null;
  effectiveTarget: WorkspaceSetupEffectiveTarget | null;
  connectDaemonForImport: (locationOverride?: "local" | "remote") => Promise<void>;
  ensureOnboardingAfterDaemonConnect: (options?: { allowTitlingInsertion?: boolean }) => Promise<{
    insertionStep: RoutePlanInsertionStep | null;
  } | null>;
  waitForDaemonReady: (timeoutMs: number) => Promise<void>;
  applyConnection: (info: Awaited<ReturnType<typeof desktopConnectLocal>>) => void;
  rememberRemoteProfile: (host: string, user: string | null) => void;
  requestRemotePasswordPrompt: (mode?: "ssh" | "admin") => void;
  setCreateError: (message: string | null) => void;
};

export function useWorkspaceSetupCreate({
  currentStepKey,
  intent,
  ensureTitlingPersistedForCurrentTarget,
  setSourcePath,
  setImportRepoStatus,
  setImportRepoNote,
  onOnboardingInsertionRequested,
  onCreateErrorStep,
  navigate,
  wizardCompletedRef,
  wizardKey,
  trackWizardCompleted,
  desktopApp,
  remoteSshPasswordOnce,
  remoteSshPasswordCandidate,
  remoteAdminPasswordOnce,
  remoteAdminPasswordCandidate,
  effectiveTarget,
  connectDaemonForImport,
  ensureOnboardingAfterDaemonConnect,
  waitForDaemonReady,
  applyConnection,
  rememberRemoteProfile,
  requestRemotePasswordPrompt,
  setCreateError,
}: UseWorkspaceSetupCreateArgs) {
  const [creating, setCreating] = useState(false);
  const [localAdminPasswordPromptVisible, setLocalAdminPasswordPromptVisible] = useState(false);
  const [localAdminPasswordInput, setLocalAdminPasswordInput] = useState("");
  const createIntent = buildWorkspaceSetupCreateIntent(intent);
  const {
    selections,
    sourcePath,
    repoUrl,
    workspaceName,
    networkAllowlist,
    useSandboxStaging,
    targetBranch,
    verifyCommand,
    mergeQueueSkipped,
    pushOnSuccess,
    pushRemote,
    pushBranch,
    setupHook,
    titlingStepVisible,
    titlingMode,
    titlingRemoteValid,
    titlingPersistError,
  } = createIntent;
  const remoteTarget = effectiveTarget?.kind === "remote" ? effectiveTarget : null;
  const parsedRemoteHost = remoteTarget?.host;
  const parsedRemoteUser = remoteTarget?.user;
  const parsedRemotePort = remoteTarget?.port ?? null;
  const remoteDataDir = remoteTarget?.dataDir ?? null;
  const provisioningSource = selections.source as WorkspaceSetupProvisioningSource;
  const provisioningExecutionMode = selections.container === "host" ? "host" : "sandbox";
  const localAdminPasswordOnce = localAdminPasswordInput.length > 0 ? localAdminPasswordInput : null;

  const {
    sandboxPrepareMessage,
    setSandboxPrepareMessage,
    launchSnapshot,
    setLaunchSnapshot,
    launchLogs,
    setLaunchLogs,
    appendSyntheticLaunchLog,
    beginProvisioningPhase,
    markProvisioningError,
    setProvisioningWorkspaceId,
    markProvisioningReady,
    resetLaunchProgressState,
    currentLaunchStepLabel,
    currentLaunchElapsed,
    currentLaunchEtaLabel,
    launchCopyLabel,
    showLaunchPanel,
    onCopyLaunchDiagnostics,
  } = useWorkspaceSetupCreateProgress({
    creating,
    provisioningSource,
    provisioningExecutionMode,
  });

  const {
    importInitDialog,
    resolveImportInitDialog,
    confirmInitImportFolder,
    onPickLocalFolder,
    preflightSourceStep,
  } = useWorkspaceSetupSourcePreflight({
    currentStepKey,
    selections,
    desktopApp,
    sourcePath,
    repoUrl,
    useSandboxStaging,
    setSourcePath,
    setImportRepoStatus,
    setImportRepoNote,
    setCreateError,
    connectDaemonForImport,
    ensureOnboardingAfterDaemonConnect,
    onOnboardingInsertionRequested,
  });

  useEffect(() => {
    if (selections.location === "local") {
      return;
    }
    setLocalAdminPasswordPromptVisible(false);
    setLocalAdminPasswordInput("");
  }, [selections.location]);

  const prepareSandboxRuntimeIfNeeded = async () => {
    await prepareWorkspaceSetupSandboxRuntime({
      activationMode: selections.location === "remote" ? "remote" : "local",
      containerSelection: selections.container === "host" ? "host" : "sandbox",
      desktopApp,
      localAdminPasswordOnce,
      remoteAdminPasswordOnce,
      remoteAdminPasswordCandidate,
      requestRemoteAdminPasswordPrompt: () => requestRemotePasswordPrompt("admin"),
      setSandboxPrepareMessage,
      onLocalAdminPasswordRequired: () => {
        setLocalAdminPasswordPromptVisible(true);
        setLocalAdminPasswordInput("");
      },
      onLocalAdminPasswordReady: () => {
        setLocalAdminPasswordPromptVisible(false);
        setLocalAdminPasswordInput("");
      },
      onCreateErrorStep: (stepKey) => onCreateErrorStep(stepKey),
    });
  };

  const onCreate = async () => {
    setCreateError(null);
    resetLaunchProgressState();
    setCreating(true);
    const launchStartedAtMs = Date.now();
    let createdWorkspaceId: string | null = null;
    let shouldCleanupCreatedWorkspace = false;
    try {
      beginProvisioningPhase(
        "connect_daemon",
        selections.location === "remote" ? "Connecting to remote daemon" : "Connecting to local daemon",
        selections.location === "remote"
          ? "Connecting to the selected remote daemon."
          : "Connecting to the local daemon.",
      );
      if (selections.location === "remote" && !desktopApp) {
        throw new Error("Workspace creation from the wizard requires the desktop app.");
      }

      if (selections.location === "remote" && !parsedRemoteHost) {
        throw new Error("Remote host is required (user@host).");
      }

      if (desktopApp) {
        const info = selections.location === "remote"
          ? await desktopConnectSsh({
            host: parsedRemoteHost!,
            user: parsedRemoteUser ?? null,
            password_once: remoteSshPasswordOnce ?? remoteSshPasswordCandidate,
            remote_port: parsedRemotePort,
            start_remote: true,
            remote_data_dir: remoteDataDir,
          })
          : await desktopConnectLocal();
        applyConnection(info);
      }

      if (selections.location === "remote" && parsedRemoteHost) {
        rememberRemoteProfile(parsedRemoteHost, parsedRemoteUser ?? null);
      }

      await waitForDaemonReady(15000);
      await seedDaemonTelemetryPreferenceIfDefault();
      await prepareSandboxRuntimeIfNeeded();
      beginProvisioningPhase(
        "prepare_source",
        "Preparing workspace source",
        "Daemon ready. Preparing workspace source.",
      );
      const onboardingResult = await ensureOnboardingAfterDaemonConnect({
        allowTitlingInsertion: selections.location === "remote",
      });
      const blockingInsertionStep = onboardingResult?.insertionStep === "harness-downloads"
        ? null
        : onboardingResult?.insertionStep;
      if (blockingInsertionStep) {
        // Harness downloads remain optional once Create has started; keep launch moving.
        onOnboardingInsertionRequested(blockingInsertionStep);
        return;
      }
      if (titlingStepVisible && titlingMode !== "skip") {
        if (titlingMode !== "remote" && titlingMode !== "local") {
          throw new Error("Choose a session titling option or skip for now.");
        }
        if (titlingMode === "remote" && !titlingRemoteValid) {
          throw new Error("Remote session titling requires base URL, API key, and model.");
        }
        const persisted = await ensureTitlingPersistedForCurrentTarget();
        if (!persisted) {
          throw new Error(titlingPersistError ?? "Failed to save session titling settings.");
        }
      }

      const preparedSource = await prepareWorkspaceSetupSource({
        intent: createIntent,
        beginProvisioningPhase,
        confirmInitImportFolder,
        setImportRepoStatus,
        setImportRepoNote,
      });
      const { rootPath, name } = preparedSource;
      let { workspaceId } = preparedSource;
      const workspaceKind = selections.location === "remote" ? "remote" : "local";
      const executionMode = selections.container === "host" ? "host" : "sandbox";
      let pendingRouteLaunch: {
        workspaceId: string;
        workspaceKind: "local" | "remote";
        executionMode: "host" | "sandbox";
        source: "wizard";
        startedAtMs: number;
      } | null = null;
      if (!workspaceId) {
        beginProvisioningPhase(
          "register_workspace",
          "Registering workspace",
          "Registering the workspace with the daemon.",
        );
        const created = await createWorkspace(rootPath, name, workspaceKind, "wizard");
        workspaceId = idToString(created.id);
        createdWorkspaceId = workspaceId;
        shouldCleanupCreatedWorkspace = true;
        setProvisioningWorkspaceId(workspaceId);
      }

      beginProvisioningPhase(
        "configure_workspace",
        "Configuring workspace runtime",
        "Saving workspace runtime settings.",
        workspaceId,
      );
      const environment = selections.container === "host"
        ? "host"
        : "sandbox";
      const allowlist = parseNetworkAllowlist(networkAllowlist);
      const netMode = selections.network === "allowlist"
        ? "allowlist"
        : selections.network === "full"
          ? "all"
          : "llm_only";
      await updateWorkspaceExecutionConfig(workspaceId, {
        environment,
        network_mode: selections.container !== "host" ? netMode : null,
        allowlist: selections.container !== "host" && netMode === "allowlist" ? allowlist : null,
      });

      if (selections.container !== "host") {
        appendSyntheticLaunchLog(
          "launch_runtime",
          "Handing off to sandbox launch and waiting for runtime readiness.",
        );
        try {
          await waitForWorkspaceSetupLaunchCompletion(
            workspaceId,
            setLaunchSnapshot,
            setLaunchLogs,
          );
          trackWorkspaceLaunchCompleted({
            workspaceId,
            workspaceKind,
            executionMode,
            source: "wizard",
            startedAtMs: launchStartedAtMs,
            result: "ready",
            persistPendingRoute: false,
          });
          pendingRouteLaunch = {
            workspaceId,
            workspaceKind,
            executionMode,
            source: "wizard",
            startedAtMs: launchStartedAtMs,
          };
        } catch (error) {
          trackWorkspaceLaunchCompleted({
            workspaceId,
            workspaceKind,
            executionMode,
            source: "wizard",
            startedAtMs: launchStartedAtMs,
            result: "error",
          });
          throw error;
        }
      }

      beginProvisioningPhase(
        "bootstrap_workspace",
        "Finalizing workspace setup",
        "Finalizing workspace setup before opening it.",
        workspaceId,
      );

      if (!mergeQueueSkipped) {
        await updateWorkspaceMergeQueueConfig(workspaceId, {
          enabled: true,
          target_branch: targetBranch.trim() || null,
          verify_command: verifyCommand.trim() || null,
          push_on_success: pushOnSuccess ? true : null,
          push_remote: pushOnSuccess ? (pushRemote.trim() || "origin") : null,
          push_branch: pushOnSuccess ? (pushBranch.trim() || targetBranch.trim() || "main") : null,
        });
      }

      if (setupHook.trim()) {
        await updateWorkspaceWorktreeBootstrapConfig(workspaceId, { setup_command: setupHook.trim() });
      }

      await waitForDaemonReady(15000);
      try {
        if (selections.location === "remote" && parsedRemoteHost) {
          await upsertLauncherRecent({
            kind: "ssh",
            label: workspaceName.trim() || lastPathSegment(rootPath) || parsedRemoteHost,
            host: parsedRemoteHost,
            user: parsedRemoteUser ?? null,
            remote_port: parsedRemotePort ?? 4399,
            start_remote: true,
            remote_data_dir: remoteDataDir,
            workspace_root_path: rootPath,
            execution_environment: environment,
            updated_at_ms: Date.now(),
          });
        } else {
          await upsertLauncherRecent({
            kind: "local",
            label: lastPathSegment(rootPath) || rootPath,
            root_path: rootPath,
            execution_environment: environment,
            updated_at_ms: Date.now(),
          });
        }
      } catch {
        // best-effort only; do not block workspace creation if recents persistence fails
      }
      // The workspace is fully created at this point; preserve it if bootstrap gating fails.
      shouldCleanupCreatedWorkspace = false;
      await waitForWorkspaceBootstrapBeforeNavigation(workspaceId);
      markProvisioningReady();
      if (pendingRouteLaunch) {
        trackWorkspaceLaunchCompleted({
          ...pendingRouteLaunch,
          result: "ready",
          emitEvent: false,
        });
      }
      shouldCleanupCreatedWorkspace = false;
      wizardCompletedRef.current = true;
      trackWizardCompleted({
        wizardKey,
        workspaceKind,
      });
      navigate(`/workspaces/${workspaceId}`, { replace: true });
    } catch (error) {
      if (shouldCleanupCreatedWorkspace && createdWorkspaceId) {
        try {
          await deleteWorkspace(createdWorkspaceId);
        } catch {
          // Best-effort rollback only; preserve the original create failure.
        }
      }
      const message = messageFromError(error);
      markProvisioningError(message);
      setCreateError(message);
      const key = resolveCreateErrorStepKey(message);
      if (key) onCreateErrorStep(key);
    } finally {
      setSandboxPrepareMessage(null);
      setCreating(false);
    }
  };

  return {
    creating,
    localAdminPasswordPromptVisible,
    localAdminPasswordInput,
    setLocalAdminPasswordInput,
    setCreateError,
    importInitDialog,
    resolveImportInitDialog,
    onPickLocalFolder,
    preflightSourceStep,
    launchSnapshot,
    launchLogs,
    showLaunchPanel,
    currentLaunchStepLabel,
    currentLaunchElapsed,
    currentLaunchEtaLabel,
    launchCopyLabel,
    createButtonLabel: creating
      ? (sandboxPrepareMessage ?? "Creating…")
      : "Create workspace",
    onCopyLaunchDiagnostics,
    onCreate,
  };
}
