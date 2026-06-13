import type { MutableRefObject } from "react";

import {
  importProviderAuthCandidates,
  type ProviderAuthImportCandidate,
} from "../../api/client";
import type { WizardRoutePlan, WizardStepKey } from "./wizardFlow";
import { messageFromError } from "./wizardTypes";

type AdvanceWorkspaceSetupAuthImportStepArgs = {
  authImportBusy: boolean;
  authImportSelected: Record<string, boolean>;
  setAuthImportSelected: (value: Record<string, boolean>) => void;
  authImportCandidates: ProviderAuthImportCandidate[];
  setAuthImportBusy: (busy: boolean) => void;
  setAuthImportError: (error: string | null) => void;
  connectDaemonForImport: () => Promise<void>;
  ensureTitlingProbeForCurrentTarget: () => Promise<boolean | null>;
  currentStepKeyRef: MutableRefObject<WizardStepKey>;
  getCurrentRoutePlan: () => WizardRoutePlan | null;
};

export async function advanceWorkspaceSetupAuthImportStep(
  {
    authImportBusy,
    authImportSelected,
    setAuthImportSelected,
    authImportCandidates,
    setAuthImportBusy,
    setAuthImportError,
    connectDaemonForImport,
    ensureTitlingProbeForCurrentTarget,
    currentStepKeyRef,
    getCurrentRoutePlan,
  }: AdvanceWorkspaceSetupAuthImportStepArgs,
  options?: { clearSelections?: boolean },
): Promise<WizardRoutePlan | null> {
  if (authImportBusy) return null;

  const selectionSnapshot = options?.clearSelections ? {} : authImportSelected;
  if (options?.clearSelections) {
    setAuthImportSelected({});
  }

  const candidateIds = authImportCandidates
    .filter((candidate) => selectionSnapshot[candidate.id])
    .map((candidate) => candidate.id);

  if (candidateIds.length) {
    setAuthImportBusy(true);
    setAuthImportError(null);
    try {
      await connectDaemonForImport();
      const response = await importProviderAuthCandidates(candidateIds);
      const acceptableStatuses = new Set(["imported", "updated", "already_imported"]);
      const failures = (response.results ?? [])
        .filter((result) => !acceptableStatuses.has(result.status))
        .map((result) => {
          const label = authImportCandidates.find((candidate) => candidate.id === result.candidate_id)?.provider_label
            ?? result.provider_id;
          const detail = (result.message ?? `Import status: ${result.status}`).trim();
          return `${label}: ${detail}`;
        });
      if (failures.length > 0) {
        setAuthImportError(`Some auth imports did not apply. ${failures.join(" ; ")}`);
        setAuthImportBusy(false);
        return null;
      }
    } catch (error) {
      setAuthImportError(messageFromError(error));
      setAuthImportBusy(false);
      return null;
    }
    setAuthImportBusy(false);
  }

  await ensureTitlingProbeForCurrentTarget();
  if (currentStepKeyRef.current !== "auth-import") return null;
  return getCurrentRoutePlan();
}
