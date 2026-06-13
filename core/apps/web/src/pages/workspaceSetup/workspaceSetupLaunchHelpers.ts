import { startTransition } from "react";
import type {
  ExecutionLaunchLogLine,
  ExecutionLaunchSnapshot,
} from "../../api/client";
import { prepareLinuxSandboxRuntime } from "../../api/client";
import {
  desktopEnsureLocalLinuxSandboxReady,
  desktopEnsureRemoteLinuxSandboxReady,
} from "../../utils/desktop";
import type { WorkspaceSetupLaunchLogLine } from "./launchProgress";
import {
  mergeWorkspaceSetupLaunchLogs,
  startWorkspaceSetupLaunchHandoff,
  waitForLaunchHandoffTerminal,
} from "./launchHandoff";

const LOCAL_ADMIN_PASSWORD_MESSAGE =
  "Preparing sandbox needs your Linux admin password. Enter it on the Local step and try again.";
const REMOTE_ADMIN_PASSWORD_MESSAGE =
  "Preparing sandbox on remote host needs the remote admin password. Enter it on the Remote step and try again.";

type SetLaunchLogs = (
  update: (prev: WorkspaceSetupLaunchLogLine[]) => WorkspaceSetupLaunchLogLine[],
) => void;

type PrepareWorkspaceSetupSandboxRuntimeArgs = {
  activationMode: "local" | "remote";
  containerSelection: "host" | "sandbox";
  desktopApp: boolean;
  localAdminPasswordOnce: string | null;
  remoteAdminPasswordOnce: string | null;
  remoteAdminPasswordCandidate: string | null;
  requestRemoteAdminPasswordPrompt: () => void;
  setSandboxPrepareMessage: (message: string | null) => void;
  onLocalAdminPasswordRequired: () => void;
  onLocalAdminPasswordReady: () => void;
  onCreateErrorStep: (stepKey: "location") => void;
};

export async function waitForWorkspaceSetupLaunchCompletion(
  workspaceId: string,
  setLaunchSnapshot: (snapshot: ExecutionLaunchSnapshot) => void,
  setLaunchLogs: SetLaunchLogs,
): Promise<void> {
  const initial = await startWorkspaceSetupLaunchHandoff(workspaceId);
  applyWorkspaceSetupLaunchSnapshot(setLaunchSnapshot, setLaunchLogs, initial);
  await waitForLaunchHandoffTerminal(initial, {
    applySnapshot: (snapshot) => applyWorkspaceSetupLaunchSnapshot(setLaunchSnapshot, setLaunchLogs, snapshot),
    appendLines: (lines) => appendWorkspaceSetupLaunchLogs(setLaunchLogs, lines),
  });
}

export async function prepareWorkspaceSetupSandboxRuntime({
  activationMode,
  containerSelection,
  desktopApp,
  localAdminPasswordOnce,
  remoteAdminPasswordOnce,
  remoteAdminPasswordCandidate,
  requestRemoteAdminPasswordPrompt,
  setSandboxPrepareMessage,
  onLocalAdminPasswordRequired,
  onLocalAdminPasswordReady,
  onCreateErrorStep,
}: PrepareWorkspaceSetupSandboxRuntimeArgs): Promise<void> {
  if (containerSelection === "host") {
    return;
  }

  setSandboxPrepareMessage(
    activationMode === "remote"
      ? "Preparing sandbox on remote host…"
      : "Preparing sandbox…",
  );

  if (desktopApp) {
    try {
      if (activationMode === "remote") {
        await desktopEnsureRemoteLinuxSandboxReady({
          admin_password_once: remoteAdminPasswordOnce ?? remoteAdminPasswordCandidate ?? null,
        });
      } else {
        await desktopEnsureLocalLinuxSandboxReady({
          admin_password_once: localAdminPasswordOnce,
        });
        onLocalAdminPasswordReady();
      }
      return;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      if (activationMode === "local" && message.includes("CTX_LOCAL_ADMIN_PASSWORD_REQUIRED")) {
        onLocalAdminPasswordRequired();
        onCreateErrorStep("location");
        throw new Error(LOCAL_ADMIN_PASSWORD_MESSAGE);
      }
      if (activationMode === "remote" && message.includes("CTX_REMOTE_ADMIN_PASSWORD_REQUIRED")) {
        requestRemoteAdminPasswordPrompt();
        onCreateErrorStep("location");
        throw new Error(REMOTE_ADMIN_PASSWORD_MESSAGE);
      }
      throw error;
    }
  }

  const result = await prepareLinuxSandboxRuntime(
    activationMode,
    activationMode === "remote"
      ? (remoteAdminPasswordOnce ?? remoteAdminPasswordCandidate ?? null)
      : localAdminPasswordOnce,
  );
  if (result.ready) {
    if (activationMode === "local") {
      onLocalAdminPasswordReady();
    }
    return;
  }
  if (result.needs_password && activationMode === "local") {
    onLocalAdminPasswordRequired();
    onCreateErrorStep("location");
    throw new Error(LOCAL_ADMIN_PASSWORD_MESSAGE);
  }
  if (result.needs_password && activationMode === "remote") {
    requestRemoteAdminPasswordPrompt();
    onCreateErrorStep("location");
    throw new Error(REMOTE_ADMIN_PASSWORD_MESSAGE);
  }
  throw new Error(result.message);
}

export async function copyWorkspaceSetupLaunchDiagnostics(
  snapshot: ExecutionLaunchSnapshot | null,
  launchLogs: WorkspaceSetupLaunchLogLine[],
  setLaunchCopyState: (state: "copied" | "failed") => void,
): Promise<void> {
  if (!snapshot) {
    return;
  }
  const payload = {
    snapshot,
    logs: launchLogs.map(({ phaseLabel: _phaseLabel, timeLabel: _timeLabel, ...line }) => line),
  };
  try {
    await navigator.clipboard.writeText(JSON.stringify(payload, null, 2));
    setLaunchCopyState("copied");
  } catch {
    setLaunchCopyState("failed");
  }
}

function appendWorkspaceSetupLaunchLogs(
  setLaunchLogs: SetLaunchLogs,
  lines: ExecutionLaunchLogLine[],
): void {
  startTransition(() => {
    setLaunchLogs((prev) => mergeWorkspaceSetupLaunchLogs(prev, lines));
  });
}

function applyWorkspaceSetupLaunchSnapshot(
  setLaunchSnapshot: (snapshot: ExecutionLaunchSnapshot) => void,
  setLaunchLogs: SetLaunchLogs,
  snapshot: ExecutionLaunchSnapshot,
): void {
  setLaunchSnapshot(snapshot);
  appendWorkspaceSetupLaunchLogs(setLaunchLogs, snapshot.logs ?? []);
}
