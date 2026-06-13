import { render } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { WorkspaceSetupPageController } from "./WorkspaceSetupPage.controller";
import type { WorkspaceSetupPageViewProps } from "./WorkspaceSetupPageView.types";

const controllerState = vi.hoisted(() => ({
  latestProps: null as unknown,
  workflow: null as unknown,
  navigate: vi.fn(),
  createSetLocalAdminPasswordInput: vi.fn(),
  provisioningSetLocalAdminPasswordInput: vi.fn(),
}));

vi.mock("react-router-dom", async () => {
  const actual = await vi.importActual<typeof import("react-router-dom")>("react-router-dom");
  return {
    ...actual,
    useNavigate: () => controllerState.navigate,
  };
});

vi.mock("../../utils/analytics", () => ({
  trackWizardAbandoned: vi.fn(),
  trackWizardCompleted: vi.fn(),
  trackWizardStarted: vi.fn(),
  trackWizardStepCompleted: vi.fn(),
  trackWizardStepViewed: vi.fn(),
}));

vi.mock("./useWorkspaceSetupWorkflow", () => ({
  useWorkspaceSetupWorkflow: () => controllerState.workflow,
}));

vi.mock("./WorkspaceSetupPageView", () => ({
  WorkspaceSetupPageView: (props: WorkspaceSetupPageViewProps) => {
    controllerState.latestProps = props;
    return <div data-testid="workspace-setup-controller-view" />;
  },
}));

const noop = () => undefined;
const asyncNoop = async () => undefined;

function buildWorkflow({
  currentStepKey,
  createPromptVisible,
  provisioningPromptVisible,
}: {
  currentStepKey: "location" | "harness-downloads";
  createPromptVisible: boolean;
  provisioningPromptVisible: boolean;
}) {
  return {
    canAdvance: true,
    nextButtonLabel: "Next",
    onNext: noop,
    onSelect: noop,
    onSelectAuthImportLocal: noop,
    onSelectOption: noop,
    onSelectTitlingLocal: noop,
    onSkipAuthImport: noop,
    onSkipHarnessDownloads: noop,
    onSkipTitling: noop,
    create: {
      createButtonLabel: "Create",
      creating: false,
      currentLaunchElapsed: "",
      currentLaunchEtaLabel: "",
      currentLaunchStepLabel: "",
      importInitDialog: null,
      launchCopyLabel: "Copy",
      launchLogs: [],
      launchSnapshot: null,
      localAdminPasswordInput: "create-password",
      localAdminPasswordPromptVisible: createPromptVisible,
      onCopyLaunchDiagnostics: asyncNoop,
      onCreate: asyncNoop,
      onPickLocalFolder: asyncNoop,
      resolveImportInitDialog: noop,
      setLocalAdminPasswordInput: controllerState.createSetLocalAdminPasswordInput,
      showLaunchPanel: false,
    },
    draft: {
      createError: null,
      importRepoNote: null,
      importRepoStatus: "idle",
      networkAllowlist: "",
      pushBranch: "",
      pushOnSuccess: false,
      pushRemote: "",
      repoBranch: "",
      repoUrl: "",
      setupHook: "",
      sourcePath: "",
      targetBranch: "",
      verifyCommand: "",
      workspaceName: "",
    },
    flow: {
      clearSelection: noop,
      currentStepKey,
      goRelativeStep: noop,
      goToStepKey: noop,
      isFirst: false,
      isLast: false,
      mergeQueueSkipped: false,
      needsSourcePath: false,
      selections: { location: "local" },
      sourceStepValidation: { isComplete: true },
      step: { key: currentStepKey, title: "Step", note: "" },
      stepIndex: 0,
      steps: [{ key: currentStepKey, title: "Step", note: "" }],
      useSandboxStaging: false,
    },
    provisioning: {
      authImportBusy: false,
      authImportCandidates: [],
      authImportError: null,
      authImportSelected: {},
      cancelHarnessInstall: asyncNoop,
      harnessByProviderId: {},
      harnessInstallBusy: false,
      harnessInstallCandidates: [],
      harnessInstallError: null,
      harnessInstallRows: {},
      harnessInstallSelected: {},
      harnessSummaryValue: "Skip",
      invalidateTitlingPersisted: noop,
      localAdminPasswordInput: "harness-password",
      localAdminPasswordPromptVisible: provisioningPromptVisible,
      selectedHarnessBlockedCount: 0,
      selectedHarnessInstallTarget: "container",
      selectedHarnessRunningCount: 0,
      setAuthImportSelected: noop,
      setHarnessInstallSelected: noop,
      setLocalAdminPasswordInput: controllerState.provisioningSetLocalAdminPasswordInput,
      setTitlingMode: noop,
      setTitlingRemoteAdvancedOpen: noop,
      setTitlingRemoteApiKey: noop,
      setTitlingRemoteBaseUrl: noop,
      setTitlingRemoteModel: noop,
      setTitlingRemoteUseJson: noop,
      titlingLocalInstall: null,
      titlingLocalInstallBusy: false,
      titlingLocalStatus: null,
      titlingMode: "skip",
      titlingPersistBusy: false,
      titlingPersistError: null,
      titlingProbeBusy: false,
      titlingProbeError: null,
      titlingRemoteAdvancedOpen: false,
      titlingRemoteApiKey: "",
      titlingRemoteBaseUrl: "",
      titlingRemoteModel: "",
      titlingRemoteUseJson: true,
      titlingRemoteValid: false,
      titlingStatusError: null,
      titlingSummaryValue: "Skip",
    },
    remote: {
      hasRemoteHost: false,
      onRemoteDataDirInputChange: noop,
      onRemoteInputChange: noop,
      onRemotePasswordInputChange: noop,
      onRemotePortInputChange: noop,
      remoteDataDirInput: "",
      remoteError: null,
      remoteHostInput: "",
      remotePasswordInput: "",
      remotePasswordPromptMode: "ssh",
      remotePasswordPromptVisible: false,
      remotePathError: null,
      remotePathStatus: "idle",
      remotePathSuggestions: [],
      remotePortInput: "",
      remoteStatus: "idle",
      setRemoteError: noop,
      setRemoteStatus: noop,
      sshSuggestions: [],
    },
    setters: {
      createError: noop,
      networkAllowlist: noop,
      pushBranch: noop,
      pushBranchTouched: noop,
      pushOnSuccess: noop,
      pushRemote: noop,
      repoBranch: noop,
      repoUrl: noop,
      setupHook: noop,
      sourcePath: noop,
      targetBranch: noop,
      targetBranchTouched: noop,
      verifyCommand: noop,
      workspaceName: noop,
    },
  };
}

function renderControllerWithWorkflow(workflow: unknown) {
  controllerState.workflow = workflow;
  render(<WorkspaceSetupPageController />);
  return controllerState.latestProps as WorkspaceSetupPageViewProps;
}

describe("WorkspaceSetupPageController", () => {
  beforeEach(() => {
    controllerState.latestProps = null;
    controllerState.workflow = null;
    controllerState.navigate.mockReset();
    controllerState.createSetLocalAdminPasswordInput.mockReset();
    controllerState.provisioningSetLocalAdminPasswordInput.mockReset();
  });

  it("routes create-step admin input to create state when a stale harness prompt exists", () => {
    const props = renderControllerWithWorkflow(buildWorkflow({
      createPromptVisible: true,
      currentStepKey: "location",
      provisioningPromptVisible: true,
    }));

    expect(props.localAdminPasswordPromptVisible).toBe(true);
    expect(props.localAdminPasswordInput).toBe("create-password");

    props.setLocalAdminPasswordInput("create-admin");

    expect(controllerState.createSetLocalAdminPasswordInput).toHaveBeenCalledWith("create-admin");
    expect(controllerState.provisioningSetLocalAdminPasswordInput).not.toHaveBeenCalled();
  });

  it("routes harness-step admin input to provisioning state", () => {
    const props = renderControllerWithWorkflow(buildWorkflow({
      createPromptVisible: true,
      currentStepKey: "harness-downloads",
      provisioningPromptVisible: true,
    }));

    expect(props.localAdminPasswordPromptVisible).toBe(true);
    expect(props.localAdminPasswordInput).toBe("harness-password");

    props.setLocalAdminPasswordInput("harness-admin");

    expect(controllerState.provisioningSetLocalAdminPasswordInput).toHaveBeenCalledWith("harness-admin");
    expect(controllerState.createSetLocalAdminPasswordInput).not.toHaveBeenCalled();
  });
});
