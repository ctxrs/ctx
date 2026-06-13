import {
  desktopGetConnection,
  desktopStartCodexLoginRelay,
  isDesktopApp,
  openExternalLink,
} from "../../../../utils/desktop";

type CodexDesktopRelayParams = {
  accountId: string;
  expectedCallbackUrl?: string | null;
  completionToken?: string | null;
};

async function tryStartCodexDesktopRelay(params: CodexDesktopRelayParams) {
  if (!isDesktopApp()) return false;
  const connection = await desktopGetConnection().catch(() => null);
  const relayRequired = connection?.kind === "ssh";
  if (!params.expectedCallbackUrl || !params.completionToken) {
    if (relayRequired) {
      throw new Error(
        "Codex sign-in is missing remote callback metadata. Update the remote daemon and retry.",
      );
    }
    return false;
  }
  try {
    const started = await desktopStartCodexLoginRelay({
      login_id: params.accountId,
      callback_url: params.expectedCallbackUrl,
      completion_token: params.completionToken,
    });
    if (!started && relayRequired) {
      throw new Error(
        "Codex sign-in could not start the remote callback relay. Reconnect the remote daemon and retry.",
      );
    }
    return started;
  } catch (error) {
    if (relayRequired) {
      throw error;
    }
    return false;
  }
}

export async function openCodexAuthUrlWithDesktopRelay(
  url: string,
  params?: CodexDesktopRelayParams,
) {
  if (!url) return;
  if (params) {
    await tryStartCodexDesktopRelay(params);
  }
  await openExternalLink(url);
}
