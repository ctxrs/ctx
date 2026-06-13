import { useEffect, type Dispatch, type MutableRefObject, type SetStateAction } from "react";
import { subscribeInstallProgress, type InstallProgressSnapshot } from "../../state/installProgressMonitor";
import type { LocalInstallState } from "./wizardTypes";

type UseWorkspaceSetupTitlingInstallProgressArgs = {
  titlingLocalInstall: LocalInstallState | null;
  setTitlingLocalInstall: Dispatch<SetStateAction<LocalInstallState | null>>;
  titlingInstallObserverRef: MutableRefObject<{ installId: string; stop: () => void } | null>;
  titlingInstallStateRef: MutableRefObject<LocalInstallState | null>;
  clearTitlingInstallObserver: () => void;
  refreshTitlingLocalStatus: (opts?: { silent?: boolean }) => Promise<unknown>;
};

export function useWorkspaceSetupTitlingInstallProgress({
  titlingLocalInstall,
  setTitlingLocalInstall,
  titlingInstallObserverRef,
  titlingInstallStateRef,
  clearTitlingInstallObserver,
  refreshTitlingLocalStatus,
}: UseWorkspaceSetupTitlingInstallProgressArgs) {
  useEffect(() => {
    titlingInstallStateRef.current = titlingLocalInstall;
  }, [
    titlingInstallStateRef,
    titlingLocalInstall,
  ]);

  useEffect(() => {
    return subscribeInstallProgress((snapshot: InstallProgressSnapshot) => {
      const trackedInstallId =
        titlingInstallObserverRef.current?.installId
        ?? titlingInstallStateRef.current?.installId
        ?? null;
      if (!trackedInstallId) return;
      const entry = snapshot[trackedInstallId];
      if (!entry) return;

      const nextState: LocalInstallState = {
        installId: entry.installId,
        state: entry.state,
        pct: entry.pct,
        errorCode: entry.errorCode,
        error: entry.error,
      };
      const previousState = titlingInstallStateRef.current?.state ?? null;
      titlingInstallStateRef.current = nextState;
      setTitlingLocalInstall((prev) => {
        if (
          prev?.installId === nextState.installId
          && prev.state === nextState.state
          && prev.pct === nextState.pct
          && prev.errorCode === nextState.errorCode
          && prev.error === nextState.error
        ) {
          return prev;
        }
        return nextState;
      });
      if (previousState === "running" && entry.state !== "running") {
        clearTitlingInstallObserver();
        void refreshTitlingLocalStatus({ silent: true });
      }
    });
  }, [
    clearTitlingInstallObserver,
    refreshTitlingLocalStatus,
    setTitlingLocalInstall,
    titlingInstallObserverRef,
    titlingInstallStateRef,
  ]);
}
