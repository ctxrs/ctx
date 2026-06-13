import { useEffect, type Dispatch, type MutableRefObject, type SetStateAction } from "react";
import type { InstallTarget } from "../../api/client";
import { subscribeInstallProgress, type InstallProgressSnapshot } from "../../state/installProgressMonitor";
import type { HostOwnerScope } from "../../state/scopeIdentity";
import {
  resolveProviderInstallProgressSession,
  subscribeProviderInstallProgressForScope,
} from "../../state/providerInstallProgressStore";
import type {
  HarnessInstallProviderRow,
  HarnessInstallRowState,
} from "./wizardTypes";

type HarnessInstallObserver = {
  installId: string;
  stop: () => void;
};

type UseWorkspaceSetupHarnessInstallProgressArgs = {
  providerProgressOwnerScope: HostOwnerScope | null;
  harnessInstallRows: Record<string, HarnessInstallRowState>;
  setHarnessInstallRows: Dispatch<SetStateAction<Record<string, HarnessInstallRowState>>>;
  harnessInstallCandidates: HarnessInstallProviderRow[];
  setHarnessInstallCandidates: Dispatch<SetStateAction<HarnessInstallProviderRow[]>>;
  selectedHarnessInstallTarget: InstallTarget;
  harnessInstallObserversRef: MutableRefObject<Record<string, HarnessInstallObserver>>;
  clearHarnessInstallObserver: (providerId?: string) => void;
};

export function useWorkspaceSetupHarnessInstallProgress({
  providerProgressOwnerScope,
  harnessInstallRows,
  setHarnessInstallRows,
  harnessInstallCandidates,
  setHarnessInstallCandidates,
  selectedHarnessInstallTarget,
  harnessInstallObserversRef,
  clearHarnessInstallObserver,
}: UseWorkspaceSetupHarnessInstallProgressArgs) {
  useEffect(() => {
    if (!providerProgressOwnerScope) {
      return () => {};
    }
    return subscribeProviderInstallProgressForScope(providerProgressOwnerScope, (snapshot) => {
      const providerIds = new Set([
        ...Object.keys(harnessInstallRows),
        ...harnessInstallCandidates.map((candidate) => candidate.providerId),
      ]);
      if (providerIds.size === 0) return;

      setHarnessInstallRows((prev) => {
        let changed = false;
        const next = { ...prev };
        for (const providerId of providerIds) {
          const session = resolveProviderInstallProgressSession(snapshot, providerId, selectedHarnessInstallTarget);
          if (!session) continue;
          const nextRow: HarnessInstallRowState = {
            installId: session.installId,
            state: session.state,
            pct: session.pct,
            target: session.target,
            errorCode: session.errorCode,
            error: session.error,
          };
          const current = prev[providerId];
          if (
            current?.installId === nextRow.installId
            && current.state === nextRow.state
            && current.pct === nextRow.pct
            && current.target === nextRow.target
            && current.errorCode === nextRow.errorCode
            && current.error === nextRow.error
          ) {
            continue;
          }
          next[providerId] = nextRow;
          changed = true;
        }
        return changed ? next : prev;
      });

      setHarnessInstallCandidates((prev) => {
        let changed = false;
        const next = prev.map((candidate) => {
          const session = resolveProviderInstallProgressSession(
            snapshot,
            candidate.providerId,
            selectedHarnessInstallTarget,
          );
          if (!session) return candidate;
          const installRunning = session.state === "running";
          if (candidate.installRunning === installRunning && candidate.installId === session.installId) {
            return candidate;
          }
          changed = true;
          return {
            ...candidate,
            installRunning,
            installId: session.installId,
          };
        });
        return changed ? next : prev;
      });

      const terminalProviderIds = Array.from(providerIds).filter((providerId) => {
        const session = resolveProviderInstallProgressSession(snapshot, providerId, selectedHarnessInstallTarget);
        return session ? session.state !== "running" : false;
      });
      if (terminalProviderIds.length === 0) return;
      for (const providerId of terminalProviderIds) {
        clearHarnessInstallObserver(providerId);
      }
    });
  }, [
    clearHarnessInstallObserver,
    harnessInstallCandidates,
    harnessInstallRows,
    providerProgressOwnerScope,
    selectedHarnessInstallTarget,
    setHarnessInstallCandidates,
    setHarnessInstallRows,
  ]);

  useEffect(() => {
    return subscribeInstallProgress((snapshot: InstallProgressSnapshot) => {
      const providerIds = Object.keys(harnessInstallObserversRef.current);
      if (providerIds.length === 0) return;
      for (const providerId of providerIds) {
        const trackedInstallId = harnessInstallObserversRef.current[providerId]?.installId ?? null;
        if (!trackedInstallId) continue;
        const entry = snapshot[trackedInstallId];
        if (!entry) continue;

        const nextState: HarnessInstallRowState = {
          installId: entry.installId,
          state: entry.state,
          pct: entry.pct,
          target: entry.target,
          errorCode: entry.errorCode,
          error: entry.error,
        };
        setHarnessInstallRows((prev) => {
          const current = prev[providerId];
          if (
            current?.installId === nextState.installId
            && current.state === nextState.state
            && current.pct === nextState.pct
            && current.target === nextState.target
            && current.errorCode === nextState.errorCode
            && current.error === nextState.error
          ) {
            return prev;
          }
          return {
            ...prev,
            [providerId]: nextState,
          };
        });
        if (entry.state !== "running") {
          clearHarnessInstallObserver(providerId);
        }
      }
    });
  }, [
    clearHarnessInstallObserver,
    harnessInstallObserversRef,
    setHarnessInstallRows,
  ]);

  useEffect(() => {
    return () => {
      clearHarnessInstallObserver();
    };
  }, [clearHarnessInstallObserver]);
}
