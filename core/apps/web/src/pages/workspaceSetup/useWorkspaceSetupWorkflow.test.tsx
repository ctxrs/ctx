import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useWorkspaceSetupCreate } from "./useWorkspaceSetupCreate";
import { useWorkspaceSetupFlow } from "./useWorkspaceSetupFlow";
import { useWorkspaceSetupProvisioning } from "./useWorkspaceSetupProvisioning";
import { useWorkspaceSetupRemote } from "./useWorkspaceSetupRemote";
import { useWorkspaceSetupWorkflow } from "./useWorkspaceSetupWorkflow";

vi.mock("./useWorkspaceSetupCreate", () => ({
  useWorkspaceSetupCreate: vi.fn(),
}));

vi.mock("./useWorkspaceSetupFlow", () => ({
  useWorkspaceSetupFlow: vi.fn(),
}));

vi.mock("./useWorkspaceSetupProvisioning", () => ({
  useWorkspaceSetupProvisioning: vi.fn(),
}));

vi.mock("./useWorkspaceSetupRemote", () => ({
  useWorkspaceSetupRemote: vi.fn(),
}));

const renderWorkflowHarness = () => {
  const navigate = vi.fn();
  const wizardCompletedRef = { current: false };
  const trackWizardCompleted = vi.fn();

  const Harness = () => {
    const workflow = useWorkspaceSetupWorkflow({
      navigate,
      wizardCompletedRef,
      wizardKey: "wizard-test",
      trackWizardCompleted,
    });

    return (
      <button data-testid="workflow-next" onClick={() => { void workflow.onNext(); }}>
        Next
      </button>
    );
  };

  return render(<Harness />);
};

describe("useWorkspaceSetupWorkflow", () => {
  beforeEach(() => {
    const goToStepKey = vi.fn();

    vi.mocked(useWorkspaceSetupFlow).mockReturnValue({
      currentStepKey: "harness-downloads",
      currentStepKeyRef: { current: "harness-downloads" },
      selections: {
        location: "local",
        container: "sandbox",
      },
      routePlan: {
        targetKey: "local|sandbox",
        containerSelection: "sandbox",
        includeHarnessDownloads: true,
        includeAuthImport: false,
        includeTitling: false,
      },
      routePlanningBusy: false,
      stepKeys: ["location", "container", "harness-downloads", "source", "setup", "merge-queue", "confirm"],
      steps: [],
      step: {
        key: "harness-downloads",
        title: "Harness Downloads",
        note: "",
      },
      stepIndex: 2,
      isFirst: false,
      isLast: false,
      requiresSelection: false,
      hasSelection: true,
      mergeQueueSkipped: false,
      useSandboxStaging: false,
      sourceStepValidation: {
        isComplete: true,
        needsSourcePath: false,
      },
      needsSourcePath: false,
      goToStepKey,
      goRelativeStep: vi.fn(),
      selectOption: vi.fn(),
      clearSelection: vi.fn(),
      invalidateRoutePlan: vi.fn(),
      setRoutePlan: vi.fn(),
      setRoutePlanningBusy: vi.fn(),
    } as never);

    vi.mocked(useWorkspaceSetupRemote).mockReturnValue({
      desktopApp: true,
      selectedDaemonTargetKey: "local",
      parsedRemote: null,
      parsedRemotePort: null,
      remoteDataDirInput: "",
      remoteStatus: "connected",
      remoteStatusRef: { current: "connected" },
      connectDaemonForImport: vi.fn(),
      waitForDaemonReady: vi.fn(),
      applyConnection: vi.fn(),
      rememberRemoteProfile: vi.fn(),
      verifyRemoteConnection: vi.fn().mockResolvedValue(true),
      hasRemoteHost: true,
      remotePasswordOnce: null,
      remoteSshPasswordOnce: null,
      remoteSshPasswordCandidate: null,
      remoteAdminPasswordOnce: null,
      remoteAdminPasswordCandidate: null,
      remotePasswordPromptMode: "ssh",
      resetForLocalSelection: vi.fn(),
      setImportRepoStatus: vi.fn(),
      setImportRepoNote: vi.fn(),
      setTargetBranch: vi.fn(),
      setPushBranch: vi.fn(),
    } as never);

    vi.mocked(useWorkspaceSetupProvisioning).mockReturnValue({
      titlingStepVisible: false,
      titlingMode: "unset",
      titlingRemoteValid: false,
      titlingPersistError: null,
      prefetchTitlingForCurrentTarget: vi.fn().mockResolvedValue(undefined),
      ensureTitlingPersistedForCurrentTarget: vi.fn().mockResolvedValue(true),
      ensureOnboardingAfterDaemonConnect: vi.fn().mockResolvedValue(null),
      refreshAuthImportForRouteScope: vi.fn().mockResolvedValue(undefined),
      ensureRoutePlanForSelection: vi.fn().mockResolvedValue(null),
      advanceFromAuthImportStep: vi.fn().mockResolvedValue(null),
      advanceFromHarnessDownloadsStep: vi.fn().mockResolvedValue({
        targetKey: "local-route",
        containerSelection: "sandbox",
        includeHarnessDownloads: true,
        includeAuthImport: false,
        includeTitling: false,
      }),
      onSelectTitlingLocal: vi.fn().mockReturnValue(false),
      invalidateTitlingPersisted: vi.fn(),
      setTitlingMode: vi.fn(),
      setTitlingPersistError: vi.fn(),
      authImportBusy: false,
      harnessInstallBusy: false,
      selectedHarnessReadyToStartCount: 0,
      selectedHarnessBlockedCount: 1,
    } as never);

    vi.mocked(useWorkspaceSetupCreate).mockReturnValue({
      preflightSourceStep: vi.fn().mockResolvedValue(true),
    } as never);
  });

  it("advances past harness downloads when blocked selected installs are terminal", async () => {
    renderWorkflowHarness();

    fireEvent.click(screen.getByTestId("workflow-next"));

    await waitFor(() => {
      expect(vi.mocked(useWorkspaceSetupProvisioning).mock.results[0]?.value.advanceFromHarnessDownloadsStep)
        .toHaveBeenCalled();
    });
    await waitFor(() => {
      expect(vi.mocked(useWorkspaceSetupFlow).mock.results[0]?.value.goToStepKey)
        .toHaveBeenCalledWith("source");
    });
  });
});
