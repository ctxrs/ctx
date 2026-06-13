import type {
  DesktopAppRestartResp,
  DesktopAppUpdateApplyReq,
  DesktopAppUpdateApplyResp,
  DesktopAppUpdateAttemptResp,
  DesktopAppUpdateCheckReq,
  DesktopAppUpdateCheckResp,
  DesktopAppUpdateStateResp,
  DesktopConnectionInfo,
  DesktopGitBranchReq,
  DesktopLinuxSandboxEnsureResp,
  DesktopLocalLinuxSandboxEnsureReq,
  DesktopRemoteDaemonUpdateReq,
  DesktopRemoteDaemonUpdateResp,
  DesktopRemotePrewarmReq,
  DesktopRemoteLinuxSandboxEnsureReq,
  DesktopRestartLocalDaemonReq,
  DesktopSshConnectJobStatus,
  DesktopSshConnectPollReq,
  DesktopSshHost,
  DesktopSshPathEntry,
  DesktopSshPathReq,
  DesktopSshTestReq,
  SshConnectReq,
} from "../generated/desktop-ipc";
import { invoke, invokeDesktopReq } from "./desktopCore";

const DESKTOP_SSH_CONNECT_POLL_MS = 500;
const DESKTOP_SSH_CONNECT_TIMEOUT_MS = 4 * 60_000;

const sleep = (ms: number) =>
  new Promise<void>((resolve) => {
    globalThis.setTimeout(resolve, ms);
  });

type DesktopUpdateChannelOverride = {
  channel?: string | null;
};

type DesktopAppUpdateApplyOptions = DesktopUpdateChannelOverride & {
  downloadId?: string | null;
};

const optionalText = (value: string | null | undefined): string | undefined => {
  const normalized = String(value ?? "").trim();
  return normalized.length > 0 ? normalized : undefined;
};

const consumeDesktopSshConnectJob = async (jobId: string) => {
  try {
    const req: DesktopSshConnectPollReq = { job_id: jobId, consume: true };
    await invokeDesktopReq<DesktopSshConnectPollReq, DesktopSshConnectJobStatus>(
      "desktop_connect_ssh_poll",
      req,
    );
  } catch {
    // Ignore cleanup failures so the primary connect result surfaces cleanly.
  }
};

export const desktopGetConnection = async (): Promise<DesktopConnectionInfo> =>
  invoke<DesktopConnectionInfo>("desktop_get_connection");

export const desktopDisconnect = async (): Promise<void> =>
  invoke<void>("desktop_disconnect");

export const desktopConnectLocal = async (): Promise<DesktopConnectionInfo> =>
  invoke<DesktopConnectionInfo>("desktop_connect_local");

export const desktopRestartLocalDaemon = async (): Promise<DesktopConnectionInfo> =>
  invokeDesktopReq<DesktopRestartLocalDaemonReq, DesktopConnectionInfo>(
    "desktop_restart_local_daemon",
    { confirm: true },
  );

export const desktopConnectSsh = async (req: SshConnectReq): Promise<DesktopConnectionInfo> => {
  const jobId = String(await invokeDesktopReq<SshConnectReq, string>("desktop_connect_ssh_begin", req)).trim();
  if (!jobId) {
    throw new Error("desktop_connect_ssh_begin returned empty job id");
  }

  const startedAt = Date.now();
  while (Date.now() - startedAt < DESKTOP_SSH_CONNECT_TIMEOUT_MS) {
    const snapshot = await invokeDesktopReq<DesktopSshConnectPollReq, DesktopSshConnectJobStatus>(
      "desktop_connect_ssh_poll",
      { job_id: jobId, consume: false },
    );
    const status = String(snapshot.status || "").trim().toLowerCase();
    if (status === "succeeded") {
      await consumeDesktopSshConnectJob(jobId);
      if (!snapshot.info) {
        throw new Error("desktop_connect_ssh succeeded without connection info");
      }
      return snapshot.info;
    }
    if (status === "failed") {
      await consumeDesktopSshConnectJob(jobId);
      throw new Error(String(snapshot.error || "desktop_connect_ssh failed"));
    }
    await sleep(DESKTOP_SSH_CONNECT_POLL_MS);
  }

  await consumeDesktopSshConnectJob(jobId);
  throw new Error("desktop_connect_ssh timed out waiting for completion");
};

export const desktopUpdateRemoteDaemon = async (
  options?: DesktopUpdateChannelOverride,
): Promise<DesktopRemoteDaemonUpdateResp> => {
  const channel = optionalText(options?.channel);
  return invokeDesktopReq<DesktopRemoteDaemonUpdateReq, DesktopRemoteDaemonUpdateResp>(
    "desktop_update_remote_daemon",
    {
      confirm: true,
      ...(channel ? { channel } : {}),
    },
  );
};

export const desktopCheckAppUpdate = async (
  options?: DesktopUpdateChannelOverride,
): Promise<DesktopAppUpdateCheckResp> => {
  const channel = optionalText(options?.channel);
  return invokeDesktopReq<DesktopAppUpdateCheckReq, DesktopAppUpdateCheckResp>(
    "desktop_check_app_update",
    channel ? { channel } : {},
  );
};

export const desktopGetAppUpdateState = async (
  options?: DesktopUpdateChannelOverride,
): Promise<DesktopAppUpdateStateResp> => {
  const channel = optionalText(options?.channel);
  return invokeDesktopReq<DesktopAppUpdateCheckReq, DesktopAppUpdateStateResp>(
    "desktop_get_app_update_state",
    channel ? { channel } : {},
  );
};

export const desktopApplyAppUpdate = async (
  options?: DesktopAppUpdateApplyOptions,
): Promise<DesktopAppUpdateApplyResp> => {
  const channel = optionalText(options?.channel);
  const downloadId = optionalText(options?.downloadId);
  return invokeDesktopReq<DesktopAppUpdateApplyReq, DesktopAppUpdateApplyResp>(
    "desktop_apply_app_update",
    {
      confirm: true,
      ...(channel ? { channel } : {}),
      ...(downloadId ? { download_id: downloadId } : {}),
    },
  );
};

export const desktopRestartApp = async (): Promise<DesktopAppRestartResp> =>
  invoke<DesktopAppRestartResp>("desktop_restart_app");

export const desktopGetLastAppUpdateAttempt = async (): Promise<DesktopAppUpdateAttemptResp | null> =>
  invoke<DesktopAppUpdateAttemptResp | null>("desktop_get_last_app_update_attempt");

export const desktopListSshHosts = async (): Promise<DesktopSshHost[]> =>
  invoke<DesktopSshHost[]>("desktop_list_ssh_hosts");

export const desktopTestSsh = async (req: { host: string; user?: string | null; password_once?: string | null }): Promise<void> =>
  invokeDesktopReq<DesktopSshTestReq, void>("desktop_test_ssh", req);

export const desktopKickoffRemotePrewarm = async (req: DesktopRemotePrewarmReq): Promise<void> =>
  invokeDesktopReq<DesktopRemotePrewarmReq, void>("desktop_kickoff_remote_prewarm", req);

export const desktopEnsureLocalLinuxSandboxReady = async (
  req?: DesktopLocalLinuxSandboxEnsureReq,
): Promise<DesktopLinuxSandboxEnsureResp> =>
  invokeDesktopReq<DesktopLocalLinuxSandboxEnsureReq, DesktopLinuxSandboxEnsureResp>(
    "desktop_ensure_local_linux_sandbox_ready",
    req ?? {},
  );

export const desktopEnsureRemoteLinuxSandboxReady = async (
  req?: DesktopRemoteLinuxSandboxEnsureReq,
): Promise<DesktopLinuxSandboxEnsureResp> =>
  invokeDesktopReq<DesktopRemoteLinuxSandboxEnsureReq, DesktopLinuxSandboxEnsureResp>(
    "desktop_ensure_remote_linux_sandbox_ready",
    req ?? {},
  );

export const desktopListSshPaths = async (req: DesktopSshPathReq): Promise<DesktopSshPathEntry[]> =>
  invokeDesktopReq<DesktopSshPathReq, DesktopSshPathEntry[]>("desktop_list_ssh_paths", req);

export const desktopGetGitBranch = async (req: DesktopGitBranchReq): Promise<string | null> =>
  invokeDesktopReq<DesktopGitBranchReq, string | null>("desktop_get_git_branch", req);
