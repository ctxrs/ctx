import {
  resolveHarnessInstallCandidateStatus,
  type HarnessInstallCandidateStatus,
  type HarnessInstallProviderRow,
  type HarnessInstallRowState,
} from "./wizardTypes";

export type HarnessInstallStatusEntry = {
  candidate: HarnessInstallProviderRow;
  installUi: HarnessInstallRowState | undefined;
  status: HarnessInstallCandidateStatus;
};

export type HarnessInstallSummary = {
  candidateStatuses: HarnessInstallStatusEntry[];
  selectedStatuses: HarnessInstallStatusEntry[];
  startableRows: HarnessInstallProviderRow[];
  blockingStatuses: HarnessInstallStatusEntry[];
  runningStatuses: HarnessInstallStatusEntry[];
  selectedHarnessReadyToStartCount: number;
  selectedHarnessRunningCount: number;
  selectedHarnessBlockedCount: number;
  selectedHarnessFailedCount: number;
  selectedHarnessCompletedCount: number;
  harnessMissingCount: number;
  harnessSummaryValue: string;
};

export function deriveHarnessInstallSummary(
  harnessInstallCandidates: HarnessInstallProviderRow[],
  harnessInstallSelected: Record<string, boolean>,
  harnessInstallRows: Record<string, HarnessInstallRowState>,
): HarnessInstallSummary {
  const candidateStatuses = harnessInstallCandidates.map((candidate) => {
    const installUi = harnessInstallRows[candidate.providerId];
    return {
      candidate,
      installUi,
      status: resolveHarnessInstallCandidateStatus(candidate, installUi),
    };
  });

  const selectedStatuses: HarnessInstallStatusEntry[] = [];
  let harnessMissingCount = 0;

  for (const entry of candidateStatuses) {
    if (
      entry.candidate.installSupported
      && entry.status !== "installed"
      && entry.status !== "succeeded"
    ) {
      harnessMissingCount += 1;
    }
    if (harnessInstallSelected[entry.candidate.providerId] && entry.candidate.installSupported) {
      selectedStatuses.push(entry);
    }
  }

  const startableRows: HarnessInstallProviderRow[] = [];
  const blockingStatuses: HarnessInstallStatusEntry[] = [];
  const runningStatuses: HarnessInstallStatusEntry[] = [];
  let selectedHarnessReadyToStartCount = 0;
  let selectedHarnessRunningCount = 0;
  let selectedHarnessBlockedCount = 0;
  let selectedHarnessFailedCount = 0;
  let selectedHarnessCompletedCount = 0;

  for (const entry of selectedStatuses) {
    switch (entry.status) {
      case "ready_to_start":
        selectedHarnessReadyToStartCount += 1;
        startableRows.push(entry.candidate);
        break;
      case "running":
        selectedHarnessRunningCount += 1;
        runningStatuses.push(entry);
        break;
      case "failed":
        selectedHarnessBlockedCount += 1;
        selectedHarnessFailedCount += 1;
        blockingStatuses.push(entry);
        break;
      case "cancelled":
        selectedHarnessBlockedCount += 1;
        blockingStatuses.push(entry);
        break;
      case "installed":
      case "succeeded":
        selectedHarnessCompletedCount += 1;
        break;
      default:
        break;
    }
  }

  const harnessSummaryValue = harnessMissingCount === 0
    ? "All detectable harnesses are ready"
    : selectedHarnessRunningCount > 0
      ? `${selectedHarnessRunningCount} selected download${selectedHarnessRunningCount === 1 ? "" : "s"} in progress`
      : selectedHarnessBlockedCount > 0
        ? `${selectedHarnessBlockedCount} selected download${selectedHarnessBlockedCount === 1 ? "" : "s"} failed or were canceled`
        : selectedHarnessReadyToStartCount > 0
          ? `${selectedHarnessReadyToStartCount} selected for download`
          : selectedHarnessCompletedCount > 0
            ? `${selectedHarnessCompletedCount} selected download${selectedHarnessCompletedCount === 1 ? "" : "s"} ready`
            : "Skipped for now";

  return {
    candidateStatuses,
    selectedStatuses,
    startableRows,
    blockingStatuses,
    runningStatuses,
    selectedHarnessReadyToStartCount,
    selectedHarnessRunningCount,
    selectedHarnessBlockedCount,
    selectedHarnessFailedCount,
    selectedHarnessCompletedCount,
    harnessMissingCount,
    harnessSummaryValue,
  };
}
