import {
  useCallback,
  useMemo,
  useRef,
  useState,
  type MutableRefObject,
} from "react";
import type {
  InstallTarget,
} from "../../api/client";
import {
  cancelInstall,
  installProvider,
  listProviders,
} from "../../api/client";
import { observeInstall } from "../../state/installProgressMonitor";
import { createHostOwnerScope } from "../../state/scopeIdentity";
import {
  upsertProviderInstallProgressForScope,
} from "../../state/providerInstallProgressStore";
import {
  buildHarnessCatalogEntryMap,
} from "../../utils/harnessCatalog";
import {
  computeInstallPct,
} from "../../utils/providerInstallUi";
import type { WizardRoutePlan, WizardStepKey } from "./wizardFlow";
import type { WizardSelections } from "./wizardFlowReducer";
import {
  completeWorkspaceSetupHarnessCandidatesRefresh,
  failWorkspaceSetupHarnessCandidatesRefresh,
  type WorkspaceSetupProvisioningMachineState,
  type WorkspaceSetupProvisioningRequest,
} from "./workspaceSetupProvisioningMachine";
import {
  installTargetForWorkspaceSetupContainerSelection,
  type WorkspaceSetupEffectiveTarget,
} from "./workflowTypes";
import {
  buildRunningHarnessInstallRowPatch,
  deriveHarnessInstallSelectedState,
  getRunningHarnessInstallProviderRows,
  mapHarnessInstallCandidate,
} from "./workspaceSetupHarnessInstallCandidates";
import { deriveHarnessInstallSummary } from "./workspaceSetupHarnessInstallSummary";
import {
  messageFromError,
  type HarnessInstallProviderRow,
  type HarnessInstallRowState,
  type RemoteStatus,
} from "./wizardTypes";
import { useWorkspaceSetupHarnessInstallProgress } from "./useWorkspaceSetupHarnessInstallProgress";

type UseWorkspaceSetupHarnessDownloadsProvisioningArgs = {
  currentStepKeyRef: MutableRefObject<WizardStepKey>;
  selections: WizardSelections;
  effectiveTarget: WorkspaceSetupEffectiveTarget | null;
  desktopApp: boolean;
  parsedRemoteHost: string | undefined;
  remoteStatusRef: MutableRefObject<RemoteStatus>;
  connectDaemonForImport: (locationOverride?: "local" | "remote") => Promise<void>;
  connectDaemonForSpeculativeRefresh: (location: "local" | "remote") => Promise<void>;
  shouldCompleteSpeculativeRefreshAsEmpty: (location: "local" | "remote", error: unknown) => boolean;
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

export function useWorkspaceSetupHarnessDownloadsProvisioning({
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
}: UseWorkspaceSetupHarnessDownloadsProvisioningArgs) {
  const [harnessInstallCandidates, setHarnessInstallCandidates] = useState<HarnessInstallProviderRow[]>([]);
  const [harnessInstallSelected, setHarnessInstallSelected] = useState<Record<string, boolean>>({});
  const [harnessInstallBusy, setHarnessInstallBusy] = useState(false);
  const [harnessInstallError, setHarnessInstallError] = useState<string | null>(null);
  const [harnessInstallRows, setHarnessInstallRows] = useState<Record<string, HarnessInstallRowState>>({});
  const setLocalAdminPasswordInput = useCallback((_value: string) => {}, []);

  const harnessInstallObserversRef = useRef<Record<string, { installId: string; stop: () => void }>>({});
  const harnessByProviderId = useMemo(() => buildHarnessCatalogEntryMap(), []);
  const selectedHarnessInstallTarget: InstallTarget = installTargetForWorkspaceSetupContainerSelection(
    selections.container,
  );
  const providerProgressOwnerScope = useMemo(
    () => (effectiveTarget ? createHostOwnerScope(effectiveTarget.daemonScope) : null),
    [effectiveTarget],
  );

  const clearHarnessInstallObserver = useCallback((providerId?: string) => {
    if (providerId) {
      const active = harnessInstallObserversRef.current[providerId];
      if (!active) return;
      active.stop();
      delete harnessInstallObserversRef.current[providerId];
      return;
    }
    for (const active of Object.values(harnessInstallObserversRef.current)) {
      active.stop();
    }
    harnessInstallObserversRef.current = {};
  }, []);

  const resetHarnessDownloadsProvisioningState = useCallback(() => {
    clearHarnessInstallObserver();
    setHarnessInstallBusy(false);
    setHarnessInstallCandidates([]);
    setHarnessInstallSelected({});
    setHarnessInstallRows({});
    setHarnessInstallError(null);
  }, [clearHarnessInstallObserver]);

  const attachHarnessInstall = useCallback(async (providerId: string, installId: string) => {
    if (!providerId || !installId) return;
    const active = harnessInstallObserversRef.current[providerId];
    if (active?.installId === installId) return;
    active?.stop();
    harnessInstallObserversRef.current[providerId] = {
      installId,
      stop: observeInstall(installId, {
        ownerScope: providerProgressOwnerScope ?? undefined,
        providerId,
        initialState: harnessInstallRows[providerId],
      }),
    };
  }, [harnessInstallRows, providerProgressOwnerScope]);

  const cancelHarnessInstall = useCallback(async (providerId: string) => {
    const installId = harnessInstallRows[providerId]?.installId
      ?? harnessInstallCandidates.find((candidate) => candidate.providerId === providerId)?.installId;
    if (!installId) return;
    try {
      const info = await cancelInstall(installId);
      const fallbackPct = harnessInstallRows[providerId]?.pct ?? null;
      const nextInstallState = {
        installId,
        state: info.state,
        pct: computeInstallPct(info, fallbackPct),
        target: info.target,
        errorCode: info.error_code,
        error: info.error,
      };
      setHarnessInstallRows((prev) => ({
        ...prev,
        [providerId]: {
          ...nextInstallState,
          pct: computeInstallPct(info, prev[providerId]?.pct ?? fallbackPct),
        },
      }));
      setHarnessInstallCandidates((prev) =>
        prev.map((candidate) =>
          candidate.providerId === providerId
            ? {
                ...candidate,
                installRunning: info.state === "running",
                installId,
              }
            : candidate,
        ),
      );
      if (info.state !== "running") {
        clearHarnessInstallObserver(providerId);
      }
      if (providerProgressOwnerScope) {
        upsertProviderInstallProgressForScope(providerProgressOwnerScope, providerId, nextInstallState);
      }
    } catch (error) {
      setHarnessInstallError(messageFromError(error));
    }
  }, [
    clearHarnessInstallObserver,
    harnessInstallCandidates,
    harnessInstallRows,
    providerProgressOwnerScope,
  ]);

  const completeHarnessRefreshWithEmptyCandidates = useCallback((
    request: WorkspaceSetupProvisioningRequest,
  ) => {
    setHarnessInstallCandidates([]);
    setHarnessInstallSelected({});
    setHarnessInstallRows({});
    setHarnessInstallError(null);
    commitProvisioningMachineState((current) => completeWorkspaceSetupHarnessCandidatesRefresh(current, {
      scope: request.scope,
      requestId: request.requestId,
      data: [],
    }));
  }, [commitProvisioningMachineState]);

  const scanHarnessInstallCandidatesForRequest = useCallback(async (
    location: "local" | "remote",
    request: WorkspaceSetupProvisioningRequest,
  ): Promise<void> => {
    if (!desktopApp) {
      if (!isCurrentProvisioningRequest("harnessCandidates", request)) {
        return;
      }
      setHarnessInstallBusy(false);
      completeHarnessRefreshWithEmptyCandidates(request);
      return;
    }
    if (location === "remote") {
      if (!parsedRemoteHost || remoteStatusRef.current !== "connected") {
        return;
      }
    }

    const installTarget = request.scope.installTarget;
    setHarnessInstallBusy(true);
    setHarnessInstallError(null);
    try {
      try {
        await connectDaemonForSpeculativeRefresh(location);
      } catch (error) {
        if (shouldCompleteSpeculativeRefreshAsEmpty(location, error)) {
          if (!isCurrentProvisioningRequest("harnessCandidates", request)) {
            return;
          }
          completeHarnessRefreshWithEmptyCandidates(request);
          return;
        }
        throw error;
      }
      const providers = await listProviders(installTarget);
      if (!isCurrentProvisioningRequest("harnessCandidates", request)) {
        return;
      }
      const rows = providers
        .map((provider) => mapHarnessInstallCandidate(provider, harnessByProviderId, installTarget))
        .filter((row): row is HarnessInstallProviderRow => row !== null)
        .sort((a, b) => a.label.localeCompare(b.label));
      setHarnessInstallCandidates(rows);
      setHarnessInstallSelected((prev) =>
        deriveHarnessInstallSelectedState(rows, prev),
      );
      const runningRows = getRunningHarnessInstallProviderRows(rows);
      if (runningRows.length > 0) {
        setHarnessInstallRows((prev) => ({
          ...prev,
          ...buildRunningHarnessInstallRowPatch(runningRows, prev),
        }));
      }
      for (const row of runningRows) {
        await attachHarnessInstall(row.providerId, row.installId);
      }
      commitProvisioningMachineState((current) => completeWorkspaceSetupHarnessCandidatesRefresh(current, {
        scope: request.scope,
        requestId: request.requestId,
        data: rows,
      }));
    } catch (error) {
      const message = messageFromError(error);
      if (!isCurrentProvisioningRequest("harnessCandidates", request)) {
        return;
      }
      setHarnessInstallCandidates([]);
      setHarnessInstallSelected({});
      setHarnessInstallRows({});
      setHarnessInstallError(message);
      commitProvisioningMachineState((current) => failWorkspaceSetupHarnessCandidatesRefresh(current, {
        scope: request.scope,
        requestId: request.requestId,
        error: message,
      }));
    } finally {
      if (isCurrentProvisioningRequest("harnessCandidates", request)) {
        setHarnessInstallBusy(false);
      }
    }
  }, [
    attachHarnessInstall,
    commitProvisioningMachineState,
    completeHarnessRefreshWithEmptyCandidates,
    connectDaemonForSpeculativeRefresh,
    desktopApp,
    harnessByProviderId,
    isCurrentProvisioningRequest,
    parsedRemoteHost,
    remoteStatusRef,
    shouldCompleteSpeculativeRefreshAsEmpty,
  ]);

  const advanceFromHarnessDownloadsStep = useCallback(async (
    options?: { clearSelections?: boolean },
  ): Promise<WizardRoutePlan | null> => {
    if (harnessInstallBusy) return null;
    const selectionSnapshot = options?.clearSelections ? {} : harnessInstallSelected;
    if (options?.clearSelections) {
      setHarnessInstallSelected({});
    }
    const harnessInstallSummary = deriveHarnessInstallSummary(
      harnessInstallCandidates,
      selectionSnapshot,
      harnessInstallRows,
    );
    const selectedRows = harnessInstallSummary.selectedStatuses;
    const startableRows = harnessInstallSummary.startableRows;
    const blockingRows = harnessInstallSummary.blockingStatuses;
    const runningRows = harnessInstallSummary.runningStatuses;
    const currentRoutePlan = getCurrentRoutePlan();

    if (
      selectedRows.length === 0
      || selectedRows.every(({ status }) => status === "installed" || status === "succeeded")
    ) {
      setHarnessInstallError(null);
      if (currentStepKeyRef.current !== "harness-downloads") return null;
      return currentRoutePlan;
    }
    if (runningRows.length > 0 && startableRows.length === 0) {
      setHarnessInstallError(null);
      if (currentStepKeyRef.current !== "harness-downloads") return null;
      return currentRoutePlan;
    }
    if (blockingRows.length > 0 && startableRows.length === 0) {
      if (currentStepKeyRef.current !== "harness-downloads") return null;
      return currentRoutePlan;
    }

    setHarnessInstallBusy(true);
    setHarnessInstallError(null);
    let shouldAdvance = false;
    try {
      await connectDaemonForImport();
      const startResults = await Promise.all(
        startableRows.map(async (row) => {
          try {
            const started = await installProvider(row.providerId, selectedHarnessInstallTarget);
            const installId = started.install_id;
            const nextInstallState = {
              installId,
              state: "running" as const,
              pct: null,
              target: started.target,
              errorCode: undefined,
              error: undefined,
            };
            setHarnessInstallRows((prev) => ({
              ...prev,
              [row.providerId]: nextInstallState,
            }));
            setHarnessInstallCandidates((prev) =>
              prev.map((candidate) =>
                candidate.providerId === row.providerId
                  ? {
                      ...candidate,
                      installRunning: true,
                      installId,
                    }
                  : candidate,
              ),
            );
            if (providerProgressOwnerScope) {
              upsertProviderInstallProgressForScope(
                providerProgressOwnerScope,
                row.providerId,
                nextInstallState,
              );
            }
            void attachHarnessInstall(row.providerId, installId);
            return {
              providerId: row.providerId,
              ok: true as const,
            };
          } catch (error) {
            return {
              providerId: row.providerId,
              ok: false as const,
              error: messageFromError(error),
            };
          }
        }),
      );

      const failures = startResults
        .filter((result) => !result.ok)
        .map((result) => {
          const label = harnessInstallCandidates.find((candidate) => candidate.providerId === result.providerId)?.label ?? result.providerId;
          return `${label}: ${result.error}`;
        });
      if (failures.length > 0) {
        setHarnessInstallRows((prev) => ({
          ...prev,
          ...Object.fromEntries(
            startResults
              .filter((result) => !result.ok)
              .map((result) => [
                result.providerId,
                {
                  installId: prev[result.providerId]?.installId ?? "",
                  state: "failed" as const,
                  pct: null,
                  target: selectedHarnessInstallTarget,
                  errorCode: undefined,
                  error: result.error,
                },
              ]),
          ),
        }));
        const prefix = failures.length === startableRows.length
          ? "Selected downloads failed to start."
          : "Some selected downloads failed to start.";
        setHarnessInstallError(`${prefix} ${failures.join(" ; ")} Continuing without those downloads.`);
        shouldAdvance = true;
      }

      const startedAny = startResults.some((result) => result.ok);
      if (!shouldAdvance && !startedAny && runningRows.length === 0 && blockingRows.length === 0) {
        return null;
      }
      shouldAdvance = true;
    } catch (error) {
      setHarnessInstallError(`Could not start selected downloads: ${messageFromError(error)} Continuing without those downloads.`);
      shouldAdvance = true;
    } finally {
      setHarnessInstallBusy(false);
    }
    if (!shouldAdvance) return null;
    if (currentStepKeyRef.current !== "harness-downloads") return null;
    return getCurrentRoutePlan();
  }, [
    attachHarnessInstall,
    connectDaemonForImport,
    currentStepKeyRef,
    getCurrentRoutePlan,
    harnessInstallBusy,
    harnessInstallCandidates,
    harnessInstallRows,
    harnessInstallSelected,
    providerProgressOwnerScope,
    selectedHarnessInstallTarget,
  ]);

  const harnessInstallSummary = deriveHarnessInstallSummary(
    harnessInstallCandidates,
    harnessInstallSelected,
    harnessInstallRows,
  );

  useWorkspaceSetupHarnessInstallProgress({
    providerProgressOwnerScope,
    harnessInstallRows,
    setHarnessInstallRows,
    harnessInstallCandidates,
    setHarnessInstallCandidates,
    selectedHarnessInstallTarget,
    harnessInstallObserversRef,
    clearHarnessInstallObserver,
  });

  return {
    harnessInstallCandidates,
    harnessInstallSelected,
    setHarnessInstallSelected,
    harnessInstallBusy,
    harnessInstallError,
    harnessInstallRows,
    localAdminPasswordPromptVisible: false,
    localAdminPasswordInput: "",
    setLocalAdminPasswordInput,
    cancelHarnessInstall,
    advanceFromHarnessDownloadsStep,
    selectedHarnessInstallTarget,
    harnessByProviderId,
    selectedHarnessReadyToStartCount: harnessInstallSummary.selectedHarnessReadyToStartCount,
    selectedHarnessRunningCount: harnessInstallSummary.selectedHarnessRunningCount,
    selectedHarnessBlockedCount: harnessInstallSummary.selectedHarnessBlockedCount,
    selectedHarnessFailedCount: harnessInstallSummary.selectedHarnessFailedCount,
    harnessSummaryValue: harnessInstallSummary.harnessSummaryValue,
    resetHarnessDownloadsProvisioningState,
    scanHarnessInstallCandidatesForRequest,
  };
}
