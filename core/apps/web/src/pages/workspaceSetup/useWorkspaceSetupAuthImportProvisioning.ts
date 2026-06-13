import { useCallback, useState, type MutableRefObject } from "react";
import type { ProviderAuthImportCandidate } from "../../api/client";
import { listProviderAuthImportCandidates } from "../../api/client";
import type { RemoteStatus } from "./wizardTypes";
import {
  completeWorkspaceSetupAuthImportRefresh,
  failWorkspaceSetupAuthImportRefresh,
  type WorkspaceSetupProvisioningMachineState,
  type WorkspaceSetupProvisioningRequest,
} from "./workspaceSetupProvisioningMachine";
import { messageFromError } from "./wizardTypes";

type UseWorkspaceSetupAuthImportProvisioningArgs = {
  desktopApp: boolean;
  parsedRemoteHost: string | undefined;
  remoteStatusRef: MutableRefObject<RemoteStatus>;
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
};

export function useWorkspaceSetupAuthImportProvisioning({
  desktopApp,
  parsedRemoteHost,
  remoteStatusRef,
  connectDaemonForSpeculativeRefresh,
  shouldCompleteSpeculativeRefreshAsEmpty,
  commitProvisioningMachineState,
  isCurrentProvisioningRequest,
}: UseWorkspaceSetupAuthImportProvisioningArgs) {
  const [authImportCandidates, setAuthImportCandidates] = useState<ProviderAuthImportCandidate[]>([]);
  const [authImportSelected, setAuthImportSelected] = useState<Record<string, boolean>>({});
  const [authImportBusy, setAuthImportBusy] = useState(false);
  const [authImportError, setAuthImportError] = useState<string | null>(null);

  const resetAuthImportProvisioningState = useCallback(() => {
    setAuthImportBusy(false);
    setAuthImportCandidates([]);
    setAuthImportSelected({});
    setAuthImportError(null);
  }, []);

  const completeAuthImportRefreshWithEmptyCandidates = useCallback((
    request: WorkspaceSetupProvisioningRequest,
  ) => {
    setAuthImportCandidates([]);
    setAuthImportSelected({});
    setAuthImportError(null);
    commitProvisioningMachineState((current) => completeWorkspaceSetupAuthImportRefresh(current, {
      scope: request.scope,
      requestId: request.requestId,
      data: [],
    }));
  }, [commitProvisioningMachineState]);

  const scanAuthImportCandidatesForRequest = useCallback(async (
    location: "local" | "remote",
    request: WorkspaceSetupProvisioningRequest,
  ): Promise<void> => {
    if (!desktopApp) {
      if (!isCurrentProvisioningRequest("authImport", request)) {
        return;
      }
      setAuthImportBusy(false);
      completeAuthImportRefreshWithEmptyCandidates(request);
      return;
    }
    if (location === "remote") {
      if (!parsedRemoteHost || remoteStatusRef.current !== "connected") {
        return;
      }
    }

    setAuthImportBusy(true);
    setAuthImportError(null);
    try {
      try {
        await connectDaemonForSpeculativeRefresh(location);
      } catch (error) {
        if (shouldCompleteSpeculativeRefreshAsEmpty(location, error)) {
          if (!isCurrentProvisioningRequest("authImport", request)) {
            return;
          }
          completeAuthImportRefreshWithEmptyCandidates(request);
          return;
        }
        throw error;
      }
      const response = await listProviderAuthImportCandidates();
      const candidates = (response.candidates ?? [])
        .filter((candidate) => candidate.parse_status === "parsed");
      if (!isCurrentProvisioningRequest("authImport", request)) {
        return;
      }
      setAuthImportCandidates(candidates);
      setAuthImportSelected((prev) =>
        Object.fromEntries(
          candidates.map((candidate) => [
            candidate.id,
            Object.prototype.hasOwnProperty.call(prev, candidate.id) ? Boolean(prev[candidate.id]) : true,
          ]),
        ),
      );
      commitProvisioningMachineState((current) => completeWorkspaceSetupAuthImportRefresh(current, {
        scope: request.scope,
        requestId: request.requestId,
        data: candidates,
      }));
    } catch (error) {
      const message = messageFromError(error);
      if (!isCurrentProvisioningRequest("authImport", request)) {
        return;
      }
      setAuthImportCandidates([]);
      setAuthImportSelected({});
      setAuthImportError(message);
      commitProvisioningMachineState((current) => failWorkspaceSetupAuthImportRefresh(current, {
        scope: request.scope,
        requestId: request.requestId,
        error: message,
      }));
    } finally {
      if (isCurrentProvisioningRequest("authImport", request)) {
        setAuthImportBusy(false);
      }
    }
  }, [
    commitProvisioningMachineState,
    completeAuthImportRefreshWithEmptyCandidates,
    connectDaemonForSpeculativeRefresh,
    desktopApp,
    isCurrentProvisioningRequest,
    parsedRemoteHost,
    remoteStatusRef,
    shouldCompleteSpeculativeRefreshAsEmpty,
  ]);

  return {
    authImportCandidates,
    authImportSelected,
    setAuthImportSelected,
    authImportBusy,
    authImportError,
    setAuthImportBusy,
    setAuthImportError,
    resetAuthImportProvisioningState,
    scanAuthImportCandidatesForRequest,
  };
}
