import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useWorkspaceSetupCreate } from "./useWorkspaceSetupCreate";
import type { WorkspaceSetupCreateIntent } from "./createHandoff";
import { deriveWorkspaceSetupEffectiveTarget, type RoutePlanInsertionStep } from "./workflowTypes";

const apiMocks = vi.hoisted(() => ({
  createWorkspace: vi.fn(),
  deleteWorkspace: vi.fn(),
  getSettings: vi.fn(),
  idToString: vi.fn((id: string | number) => String(id)),
  listWorkspaces: vi.fn(),
  prepareLinuxSandboxRuntime: vi.fn(),
  repoClone: vi.fn(),
  repoInit: vi.fn(),
  repoStatus: vi.fn(),
  repoStagingPath: vi.fn(),
  repoValidateDestination: vi.fn(),
  updateSettings: vi.fn(),
  updateWorkspaceExecutionConfig: vi.fn(),
  updateWorkspaceMergeQueueConfig: vi.fn(),
  updateWorkspaceWorktreeBootstrapConfig: vi.fn(),
}));
const clientSettingsMocks = vi.hoisted(() => ({
  state: {
    loaded: true,
    settings: {
      v: 3 as const,
      desktopNotifications: {
        turnCompleted: true,
        turnFailed: true,
        badgeUnreadCount: true,
      },
      telemetry: {
        clientEnabled: true,
      },
    },
  },
  getClientSettingsState: vi.fn(),
  loadClientSettings: vi.fn(),
}));

const desktopMocks = vi.hoisted(() => ({
  desktopConnectLocal: vi.fn(),
  desktopConnectSsh: vi.fn(),
  desktopEnsureLocalLinuxSandboxReady: vi.fn(),
  desktopEnsureRemoteLinuxSandboxReady: vi.fn(),
  desktopPickFolder: vi.fn(),
}));

const launchHandoffMocks = vi.hoisted(() => ({
  startWorkspaceSetupLaunchHandoff: vi.fn(),
  waitForLaunchHandoffTerminal: vi.fn(),
}));

const analyticsMocks = vi.hoisted(() => ({
  trackWorkspaceLaunchCompleted: vi.fn(),
}));

const launcherRecentsMocks = vi.hoisted(() => ({
  upsertLauncherRecent: vi.fn(),
}));

const workspaceBootstrapGateMocks = vi.hoisted(() => ({
  waitForWorkspaceBootstrapBeforeNavigation: vi.fn(async () => undefined),
}));

vi.mock("../../api/client", () => ({
  createWorkspace: apiMocks.createWorkspace,
  deleteWorkspace: apiMocks.deleteWorkspace,
  getSettings: apiMocks.getSettings,
  idToString: apiMocks.idToString,
  listWorkspaces: apiMocks.listWorkspaces,
  prepareLinuxSandboxRuntime: apiMocks.prepareLinuxSandboxRuntime,
  repoClone: apiMocks.repoClone,
  repoInit: apiMocks.repoInit,
  repoStatus: apiMocks.repoStatus,
  repoStagingPath: apiMocks.repoStagingPath,
  repoValidateDestination: apiMocks.repoValidateDestination,
  updateSettings: apiMocks.updateSettings,
  updateWorkspaceExecutionConfig: apiMocks.updateWorkspaceExecutionConfig,
  updateWorkspaceMergeQueueConfig: apiMocks.updateWorkspaceMergeQueueConfig,
  updateWorkspaceWorktreeBootstrapConfig: apiMocks.updateWorkspaceWorktreeBootstrapConfig,
}));

vi.mock("../../state/clientSettings", () => ({
  getClientSettingsState: clientSettingsMocks.getClientSettingsState,
  loadClientSettings: clientSettingsMocks.loadClientSettings,
}));

vi.mock("../../utils/desktop", () => ({
  desktopConnectLocal: desktopMocks.desktopConnectLocal,
  desktopConnectSsh: desktopMocks.desktopConnectSsh,
  desktopEnsureLocalLinuxSandboxReady: desktopMocks.desktopEnsureLocalLinuxSandboxReady,
  desktopEnsureRemoteLinuxSandboxReady: desktopMocks.desktopEnsureRemoteLinuxSandboxReady,
  desktopPickFolder: desktopMocks.desktopPickFolder,
}));

vi.mock("./launchHandoff", () => ({
  mergeWorkspaceSetupLaunchLogs: (prev: unknown[], next: unknown[]) => [...prev, ...next],
  startWorkspaceSetupLaunchHandoff: launchHandoffMocks.startWorkspaceSetupLaunchHandoff,
  waitForLaunchHandoffTerminal: launchHandoffMocks.waitForLaunchHandoffTerminal,
}));

vi.mock("../../utils/analytics", () => ({
  trackWorkspaceLaunchCompleted: analyticsMocks.trackWorkspaceLaunchCompleted,
}));

vi.mock("../../state/launcherRecentsStore", () => ({
  upsertLauncherRecent: launcherRecentsMocks.upsertLauncherRecent,
}));

vi.mock("../workspaceBootstrapGate", () => ({
  waitForWorkspaceBootstrapBeforeNavigation:
    workspaceBootstrapGateMocks.waitForWorkspaceBootstrapBeforeNavigation,
}));

const remoteEffectiveTarget = deriveWorkspaceSetupEffectiveTarget("remote", {
  remoteHostInput: "ctxfixture@127.0.0.1",
  remotePortInput: "44099",
  remoteDataDirInput: "/tmp/ctx-remote",
});
const localEffectiveTarget = deriveWorkspaceSetupEffectiveTarget("local", {
  remoteHostInput: "",
  remotePortInput: "4399",
  remoteDataDirInput: "",
});

if (!remoteEffectiveTarget) {
  throw new Error("expected a remote effective target for create hook tests");
}
if (!localEffectiveTarget) {
  throw new Error("expected a local effective target for create hook tests");
}

const buildIntent = (
  overrides: Partial<WorkspaceSetupCreateIntent> = {},
): WorkspaceSetupCreateIntent => {
  const { selections: selectionOverrides, ...restOverrides } = overrides;
  return {
    sourcePath: "/remote/new-sandbox",
    repoUrl: "",
    repoBranch: "",
    workspaceName: "remote-sandbox",
    networkAllowlist: "",
    useSandboxStaging: false,
    importRepoStatus: "idle",
    importRepoNote: null,
    targetBranch: "",
    verifyCommand: "",
    mergeQueueSkipped: true,
    pushOnSuccess: false,
    pushRemote: "origin",
    pushBranch: "main",
    setupHook: "",
    titlingStepVisible: false,
    titlingMode: "skip" as const,
    titlingRemoteValid: false,
    titlingPersistError: null,
    ...restOverrides,
    selections: {
      location: "remote",
      container: "sandbox",
      source: "new",
      network: "full",
      ...selectionOverrides,
    },
  };
};

const baseLaunchSnapshot = {
  job_id: "launch-1",
  workspace_id: "ws-1",
  kind: "workspace_launch" as const,
  state: "ready" as const,
  created_at: "2026-04-01T00:00:00.000Z",
  updated_at: "2026-04-01T00:00:00.000Z",
  started_at: "2026-04-01T00:00:00.000Z",
  finished_at: "2026-04-01T00:00:01.000Z",
  current_phase: "complete",
  current_step_label: "Ready",
  phases: [],
  logs: [],
  error: null,
};

const createDeferred = <T,>() => {
  let resolve!: (value: T) => void;
  let reject!: (error?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
};

const renderCreateHook = (
  insertionStep: RoutePlanInsertionStep | null,
  {
    intentOverrides,
    effectiveTarget = remoteEffectiveTarget,
    desktopApp = true,
    ...overrides
  }: {
    intentOverrides?: Partial<WorkspaceSetupCreateIntent>;
    effectiveTarget?: ReturnType<typeof deriveWorkspaceSetupEffectiveTarget> | null;
    desktopApp?: boolean;
  } & Partial<Parameters<typeof useWorkspaceSetupCreate>[0]> = {},
) => {
  const onOnboardingInsertionRequested = vi.fn();
  const onCreateErrorStep = vi.fn();
  const navigate = vi.fn();
  const applyConnection = vi.fn();
  const rememberRemoteProfile = vi.fn();
  const requestRemotePasswordPrompt = vi.fn();
  const setCreateError = vi.fn();
  const wizardCompletedRef = { current: false };
  const trackWizardCompleted = vi.fn();

  const hook = renderHook(() =>
    useWorkspaceSetupCreate({
      currentStepKey: "confirm",
      intent: buildIntent(intentOverrides),
      ensureTitlingPersistedForCurrentTarget: vi.fn(async () => true),
      setSourcePath: vi.fn(),
      setImportRepoStatus: vi.fn(),
      setImportRepoNote: vi.fn(),
      onOnboardingInsertionRequested,
      onCreateErrorStep,
      navigate,
      wizardCompletedRef,
      wizardKey: "wizard-remote-sandbox",
      trackWizardCompleted,
      desktopApp,
      remoteSshPasswordOnce: null,
      remoteSshPasswordCandidate: null,
      remoteAdminPasswordOnce: null,
      remoteAdminPasswordCandidate: null,
      effectiveTarget: effectiveTarget ?? null,
      connectDaemonForImport: vi.fn(async () => {}),
      ensureOnboardingAfterDaemonConnect: vi.fn(async () => ({ insertionStep })),
      waitForDaemonReady: vi.fn(async () => {}),
      applyConnection,
      rememberRemoteProfile,
      requestRemotePasswordPrompt,
      setCreateError,
      ...overrides,
    }),
  );

  return {
    hook,
    onOnboardingInsertionRequested,
    onCreateErrorStep,
    navigate,
    applyConnection,
    rememberRemoteProfile,
    requestRemotePasswordPrompt,
    setCreateError,
    wizardCompletedRef,
    trackWizardCompleted,
  };
};

describe("useWorkspaceSetupCreate", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    clientSettingsMocks.state.loaded = true;
    clientSettingsMocks.state.settings.telemetry.clientEnabled = true;
    clientSettingsMocks.getClientSettingsState.mockImplementation(() => clientSettingsMocks.state);
    clientSettingsMocks.loadClientSettings.mockResolvedValue(clientSettingsMocks.state);
    apiMocks.listWorkspaces.mockResolvedValue([]);
    apiMocks.getSettings.mockResolvedValue({
      telemetry: {
        enabled: true,
        endpoint: "",
        source: "default",
      },
    });
    apiMocks.repoInit.mockResolvedValue({ path: "/remote/new-sandbox" });
    apiMocks.createWorkspace.mockResolvedValue({ id: "ws-1" });
    apiMocks.prepareLinuxSandboxRuntime.mockResolvedValue({
      ready: true,
      needs_password: false,
      message: "Linux sandbox runtime is ready.",
      status: {
        state: "ready",
        supported: true,
        cache_root: "/tmp/ctx/linux-sandbox-runtime",
        message: "Linux sandbox runtime is ready.",
      },
    });
    apiMocks.updateWorkspaceExecutionConfig.mockResolvedValue(undefined);
    apiMocks.updateSettings.mockResolvedValue({
      telemetry: {
        enabled: true,
        endpoint: "",
        source: "configured",
      },
    });
    desktopMocks.desktopConnectLocal.mockResolvedValue({
      kind: "local",
      base_url: "http://127.0.0.1:4319",
      token: "local-token",
    });
    desktopMocks.desktopConnectSsh.mockResolvedValue({
      kind: "ssh",
      base_url: "http://127.0.0.1:4399",
      token: "token",
    });
    desktopMocks.desktopEnsureLocalLinuxSandboxReady.mockResolvedValue({ ready: true });
    desktopMocks.desktopEnsureRemoteLinuxSandboxReady.mockResolvedValue({ ready: true });
    launchHandoffMocks.startWorkspaceSetupLaunchHandoff.mockResolvedValue(baseLaunchSnapshot);
    launchHandoffMocks.waitForLaunchHandoffTerminal.mockResolvedValue(undefined);
    launcherRecentsMocks.upsertLauncherRecent.mockResolvedValue(undefined);
    workspaceBootstrapGateMocks.waitForWorkspaceBootstrapBeforeNavigation.mockReset();
    workspaceBootstrapGateMocks.waitForWorkspaceBootstrapBeforeNavigation.mockResolvedValue(undefined);
  });

  it("continues remote create when onboarding refresh discovers optional harness downloads", async () => {
    const {
      hook,
      onOnboardingInsertionRequested,
      navigate,
      wizardCompletedRef,
      trackWizardCompleted,
    } = renderCreateHook("harness-downloads");

    await act(async () => {
      await hook.result.current.onCreate();
    });

    expect(onOnboardingInsertionRequested).not.toHaveBeenCalled();
    expect(apiMocks.createWorkspace).toHaveBeenCalledWith(
      "/remote/new-sandbox",
      "remote-sandbox",
      "remote",
      "wizard",
      "sandbox",
    );
    expect(apiMocks.updateWorkspaceExecutionConfig).toHaveBeenCalledWith("ws-1", {
      environment: "sandbox",
      network_mode: "all",
      allowlist: null,
    });
    expect(desktopMocks.desktopConnectSsh).toHaveBeenCalledWith({
      host: "127.0.0.1",
      user: "ctxfixture",
      password_once: null,
      remote_port: 44099,
      start_remote: true,
      remote_data_dir: "/tmp/ctx-remote",
    });
    expect(
      workspaceBootstrapGateMocks.waitForWorkspaceBootstrapBeforeNavigation,
    ).toHaveBeenCalledWith("ws-1");
    expect(navigate).toHaveBeenCalledWith("/workspaces/ws-1", { replace: true });
    expect(wizardCompletedRef.current).toBe(true);
    expect(trackWizardCompleted).toHaveBeenCalledWith({
      wizardKey: "wizard-remote-sandbox",
      workspaceKind: "remote",
    });
  });

  it("preserves the created workspace when bootstrap fails after create succeeds", async () => {
    workspaceBootstrapGateMocks.waitForWorkspaceBootstrapBeforeNavigation.mockRejectedValueOnce(
      new Error("bootstrap exploded"),
    );
    const { hook, navigate, setCreateError, wizardCompletedRef, trackWizardCompleted } =
      renderCreateHook(null);

    await act(async () => {
      await hook.result.current.onCreate();
    });

    expect(apiMocks.createWorkspace).toHaveBeenCalledWith(
      "/remote/new-sandbox",
      "remote-sandbox",
      "remote",
      "wizard",
      "sandbox",
    );
    expect(
      workspaceBootstrapGateMocks.waitForWorkspaceBootstrapBeforeNavigation,
    ).toHaveBeenCalledWith("ws-1");
    expect(apiMocks.deleteWorkspace).not.toHaveBeenCalled();
    expect(setCreateError).toHaveBeenCalledWith("bootstrap exploded");
    expect(navigate).not.toHaveBeenCalled();
    expect(wizardCompletedRef.current).toBe(false);
    expect(trackWizardCompleted).not.toHaveBeenCalled();
  });

  it("still routes back when remote create discovers a blocking titling step", async () => {
    const { hook, onOnboardingInsertionRequested, navigate } = renderCreateHook("session-titling");

    await act(async () => {
      await hook.result.current.onCreate();
    });

    expect(onOnboardingInsertionRequested).toHaveBeenCalledWith("session-titling");
    expect(apiMocks.createWorkspace).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
  });

  it.each([
    {
      name: "remote sandbox clone",
      intentOverrides: {
        selections: {
          location: "remote",
          container: "sandbox",
          source: "clone",
        },
        sourcePath: "/remote/projects/",
        repoUrl: "https://github.com/contextual-ai/ctx.git",
        repoBranch: "main",
      },
      effectiveTarget: remoteEffectiveTarget,
      pendingMock: () => apiMocks.repoClone,
      resolveValue: { path: "/remote/projects/ctx" },
      expectedStepLabel: "Cloning repository",
    },
    {
      name: "remote host new",
      intentOverrides: {
        selections: {
          location: "remote",
          container: "host",
          source: "new",
        },
      },
      effectiveTarget: remoteEffectiveTarget,
      pendingMock: () => apiMocks.repoInit,
      resolveValue: { path: "/remote/new-sandbox" },
      expectedStepLabel: "Initializing repository",
    },
    {
      name: "local sandbox clone",
      intentOverrides: {
        selections: {
          location: "local",
          container: "sandbox",
          source: "clone",
        },
        sourcePath: "/home/test/projects/",
        repoUrl: "https://github.com/contextual-ai/ctx.git",
        repoBranch: "main",
      },
      effectiveTarget: localEffectiveTarget,
      pendingMock: () => apiMocks.repoClone,
      resolveValue: { path: "/home/test/projects/ctx" },
      expectedStepLabel: "Cloning repository",
    },
    {
      name: "local host new",
      intentOverrides: {
        selections: {
          location: "local",
          container: "host",
          source: "new",
        },
        sourcePath: "/home/test/projects/ctx",
      },
      effectiveTarget: localEffectiveTarget,
      pendingMock: () => apiMocks.repoInit,
      resolveValue: { path: "/home/test/projects/ctx" },
      expectedStepLabel: "Initializing repository",
    },
  ])("shows launch logs immediately for $name", async ({
    intentOverrides,
    effectiveTarget,
    pendingMock,
    resolveValue,
    expectedStepLabel,
  }) => {
    const deferred = createDeferred<typeof resolveValue>();
    pendingMock().mockImplementationOnce(() => deferred.promise);
    const { hook } = renderCreateHook(null, {
      intentOverrides,
      effectiveTarget,
    });

    await act(async () => {
      void hook.result.current.onCreate();
      await Promise.resolve();
    });

    expect(hook.result.current.creating).toBe(true);
    expect(hook.result.current.showLaunchPanel).toBe(true);
    expect(hook.result.current.currentLaunchStepLabel).toBe(expectedStepLabel);
    expect(hook.result.current.currentLaunchEtaLabel).not.toBe("Finishing up…");
    expect(hook.result.current.launchLogs.length).toBeGreaterThan(0);

    await act(async () => {
      deferred.resolve(resolveValue);
      await Promise.resolve();
      await Promise.resolve();
    });
  });

  it("preserves clone-phase attribution when synthetic provisioning fails before sandbox launch", async () => {
    apiMocks.repoClone.mockRejectedValueOnce(new Error("clone exploded"));
    const { hook, setCreateError } = renderCreateHook(null, {
      intentOverrides: {
        selections: {
          location: "remote",
          container: "sandbox",
          source: "clone",
        },
        sourcePath: "/remote/projects/",
        repoUrl: "https://github.com/contextual-ai/ctx.git",
        repoBranch: "main",
      },
      effectiveTarget: remoteEffectiveTarget,
    });

    await act(async () => {
      await hook.result.current.onCreate();
    });

    expect(hook.result.current.launchSnapshot).toMatchObject({
      current_phase: null,
      current_step_label: "Cloning repository",
      error: "clone exploded",
      state: "error",
    });
    expect(hook.result.current.currentLaunchStepLabel).toBe("Cloning repository");
    expect(hook.result.current.launchLogs.at(-1)).toMatchObject({
      level: "error",
      message: "clone exploded",
      phaseLabel: "Clone",
      provisioningPhase: "clone_repo",
    });
    expect(setCreateError).toHaveBeenCalledWith("clone exploded");
  });

  it("uses SSH credentials for connect and admin credentials for remote sandbox prepare", async () => {
    const { hook } = renderCreateHook(null, {
      remoteSshPasswordCandidate: "ssh-password",
      remoteAdminPasswordCandidate: "admin-password",
    });

    await act(async () => {
      await hook.result.current.onCreate();
    });

    expect(desktopMocks.desktopConnectSsh).toHaveBeenCalledWith(
      expect.objectContaining({
        password_once: "ssh-password",
      }),
    );
    expect(desktopMocks.desktopEnsureRemoteLinuxSandboxReady).toHaveBeenCalledWith({
      admin_password_once: "admin-password",
    });
  });

  it("seeds default daemon telemetry from the initiating client preference", async () => {
    clientSettingsMocks.state.settings.telemetry.clientEnabled = true;
    apiMocks.getSettings.mockResolvedValue({
      telemetry: {
        enabled: false,
        endpoint: "",
        source: "default",
      },
    });
    apiMocks.updateSettings.mockResolvedValue({
      telemetry: {
        enabled: true,
        endpoint: "",
        source: "configured",
      },
    });
    const { hook } = renderCreateHook(null);

    await act(async () => {
      await hook.result.current.onCreate();
    });

    expect(apiMocks.updateSettings).toHaveBeenCalledWith({
      telemetry: {
        enabled: true,
        endpoint: "",
      },
    });
  });

  it("does not overwrite an already configured daemon telemetry policy", async () => {
    clientSettingsMocks.state.settings.telemetry.clientEnabled = false;
    apiMocks.getSettings.mockResolvedValue({
      telemetry: {
        enabled: true,
        endpoint: "https://telemetry.example/telemetry",
        source: "configured",
      },
    });
    const { hook } = renderCreateHook(null);

    await act(async () => {
      await hook.result.current.onCreate();
    });

    expect(apiMocks.updateSettings).not.toHaveBeenCalled();
  });

  it("prompts for the local admin password and retries sandbox prepare with it", async () => {
    desktopMocks.desktopEnsureLocalLinuxSandboxReady
      .mockRejectedValueOnce(
        new Error(
          "CTX_LOCAL_ADMIN_PASSWORD_REQUIRED: Local admin password required to prepare sandbox on this machine.",
        ),
      )
      .mockResolvedValueOnce({ ready: true });
    const { hook, onCreateErrorStep, setCreateError } = renderCreateHook(null, {
      intent: {
        ...buildIntent(),
        selections: {
          location: "local",
          container: "sandbox",
          source: "new",
          network: "full",
        },
      },
      effectiveTarget: null,
    });

    await act(async () => {
      await hook.result.current.onCreate();
    });

    expect(hook.result.current.localAdminPasswordPromptVisible).toBe(true);
    expect(onCreateErrorStep).toHaveBeenCalledWith("location");
    expect(setCreateError).toHaveBeenCalledWith(
      "Preparing sandbox needs your Linux admin password. Enter it on the Local step and try again.",
    );
    expect(desktopMocks.desktopEnsureLocalLinuxSandboxReady).toHaveBeenCalledWith({
      admin_password_once: null,
    });

    await act(async () => {
      hook.result.current.setLocalAdminPasswordInput("local-admin");
    });
    await act(async () => {
      await hook.result.current.onCreate();
    });

    expect(desktopMocks.desktopEnsureLocalLinuxSandboxReady).toHaveBeenLastCalledWith({
      admin_password_once: "local-admin",
    });
    expect(hook.result.current.localAdminPasswordPromptVisible).toBe(false);
  });

  it("requests the remote admin password prompt when remote sandbox prepare needs elevation", async () => {
    desktopMocks.desktopEnsureRemoteLinuxSandboxReady.mockRejectedValueOnce(
      new Error(
        "CTX_REMOTE_ADMIN_PASSWORD_REQUIRED: Remote admin password required to prepare sandbox on this host.",
      ),
    );
    const { hook, onCreateErrorStep, requestRemotePasswordPrompt, setCreateError } =
      renderCreateHook(null, {
        remoteSshPasswordCandidate: "ssh-password",
      });

    await act(async () => {
      await hook.result.current.onCreate();
    });

    expect(requestRemotePasswordPrompt).toHaveBeenCalledWith("admin");
    expect(onCreateErrorStep).toHaveBeenCalledWith("location");
    expect(setCreateError).toHaveBeenCalledWith(
      "Preparing sandbox on remote host needs the remote admin password. Enter it on the Remote step and try again.",
    );
    expect(desktopMocks.desktopEnsureRemoteLinuxSandboxReady).toHaveBeenCalledWith({
      admin_password_once: null,
    });
  });
});
