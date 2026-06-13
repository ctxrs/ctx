import { useCallback, useEffect, useState, type Dispatch, type SetStateAction } from "react";
import {
  completeCodexLogin,
  deleteCodexAccount,
  getCodexAccountUsage,
  importCodexHostAuth,
  listCodexAccounts,
  listProviders,
  probeCodexHostImport,
  setCodexActiveAccount,
  startCodexLogin,
  type CodexAccountUsageResponse,
  type CodexAccountsResponse,
  type CodexHostImportProbe,
  type ProviderStatus,
} from "../../../api/client";
import {
  desktopGetConnection,
  desktopStartCodexLoginRelay,
  isDesktopApp,
  openExternalLink,
} from "../../../utils/desktop";

type CodexAccountsController = {
  providers: ProviderStatus[];
  codexAccounts: CodexAccountsResponse | null;
  codexAccountsBusy: boolean;
  codexAccountsError: string | null;
  codexUsage: CodexAccountUsageResponse | null;
  codexUsageBusy: boolean;
  codexUsageError: string | null;
  codexImportProbe: CodexHostImportProbe | null;
  codexImportBusy: boolean;
  codexCallbackBusy: Record<string, boolean>;
  codexCallbackUrls: Record<string, string>;
  setCodexCallbackUrls: Dispatch<SetStateAction<Record<string, string>>>;
  codexNewLabel: string;
  setCodexNewLabel: Dispatch<SetStateAction<string>>;
  refreshCodexUsage: (opts?: { refresh?: boolean; silent?: boolean }) => Promise<CodexAccountUsageResponse | null>;
  onCodexDelete: (accountId: string) => Promise<void>;
  onCodexSetActive: (accountId: string | null) => Promise<void>;
  onCodexLogin: () => Promise<void>;
  onCodexImportHost: () => Promise<void>;
  openCodexAuthUrl: (
    url: string,
    manual?: { accountId: string; expectedCallbackUrl: string | null; completionToken: string | null },
  ) => Promise<void>;
  onCodexCompleteCallback: (login: CodexAccountsResponse["logins"][number]) => Promise<void>;
};

const messageFromError = (error: unknown): string => {
  if (error instanceof Error && error.message) {
    return error.message;
  }
  return String(error);
};

export function useCodexAccountsController(enabled: boolean): CodexAccountsController {
  const [providers, setProviders] = useState<ProviderStatus[]>([]);
  const [codexAccounts, setCodexAccounts] = useState<CodexAccountsResponse | null>(null);
  const [codexAccountsBusy, setCodexAccountsBusy] = useState(false);
  const [codexAccountsError, setCodexAccountsError] = useState<string | null>(null);
  const [codexUsage, setCodexUsage] = useState<CodexAccountUsageResponse | null>(null);
  const [codexUsageBusy, setCodexUsageBusy] = useState(false);
  const [codexUsageError, setCodexUsageError] = useState<string | null>(null);
  const [codexNewLabel, setCodexNewLabel] = useState("");
  const [codexImportProbe, setCodexImportProbe] = useState<CodexHostImportProbe | null>(null);
  const [codexImportBusy, setCodexImportBusy] = useState(false);
  const [codexCallbackUrls, setCodexCallbackUrls] = useState<Record<string, string>>({});
  const [codexCallbackBusy, setCodexCallbackBusy] = useState<Record<string, boolean>>({});

  const refreshProviders = useCallback(async () => {
    try {
      const next = await listProviders();
      setProviders(next);
    } catch {
      // noop; codex section will render unavailable state when provider missing
    }
  }, []);

  const refreshCodexAccounts = useCallback(async (opts?: { silent?: boolean }) => {
    if (!opts?.silent) {
      setCodexAccountsBusy(true);
      setCodexAccountsError(null);
    }
    try {
      const next = await listCodexAccounts();
      setCodexAccounts(next);
      return next;
    } catch (error) {
      if (!opts?.silent) {
        setCodexAccountsError(messageFromError(error));
      }
      return null;
    } finally {
      if (!opts?.silent) {
        setCodexAccountsBusy(false);
      }
    }
  }, []);

  const refreshCodexUsage = useCallback(async (opts?: { refresh?: boolean; silent?: boolean }) => {
    if (!opts?.silent) {
      setCodexUsageBusy(true);
    }
    setCodexUsageError(null);
    try {
      const next = await getCodexAccountUsage(opts?.refresh);
      setCodexUsage(next);
      return next;
    } catch (error) {
      setCodexUsageError(messageFromError(error));
      return null;
    } finally {
      if (!opts?.silent) {
        setCodexUsageBusy(false);
      }
    }
  }, []);

  const refreshCodexImportProbe = useCallback(async () => {
    try {
      const probe = await probeCodexHostImport();
      setCodexImportProbe(probe);
      return probe;
    } catch (error) {
      setCodexImportProbe({
        available: false,
        error: messageFromError(error),
      });
      return null;
    }
  }, []);

  const tryStartCodexDesktopRelay = useCallback(async (params: {
    accountId: string;
    expectedCallbackUrl?: string | null;
    completionToken?: string | null;
  }) => {
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
  }, []);

  const openCodexAuthUrl = useCallback(
    async (
      url: string,
      params?: {
        accountId: string;
        expectedCallbackUrl?: string | null;
        completionToken?: string | null;
      },
    ) => {
      if (!url) return;
      if (params) {
        await tryStartCodexDesktopRelay(params);
      }
      await openExternalLink(url);
    },
    [tryStartCodexDesktopRelay],
  );

  const onCodexLogin = useCallback(async () => {
    setCodexAccountsBusy(true);
    setCodexAccountsError(null);
    try {
      const label = codexNewLabel.trim();
      const res = await startCodexLogin(label ? label : undefined);
      await openCodexAuthUrl(res.auth_url, {
        accountId: res.account_id,
        expectedCallbackUrl: res.expected_callback_url ?? null,
        completionToken: res.completion_token,
      });
      setCodexNewLabel("");
      await refreshCodexAccounts();
    } catch (error) {
      setCodexAccountsError(messageFromError(error));
    } finally {
      setCodexAccountsBusy(false);
    }
  }, [codexNewLabel, openCodexAuthUrl, refreshCodexAccounts]);

  const onCodexImportHost = useCallback(async () => {
    setCodexImportBusy(true);
    setCodexAccountsBusy(true);
    setCodexAccountsError(null);
    try {
      const label = codexNewLabel.trim();
      const next = await importCodexHostAuth(label ? label : undefined);
      setCodexAccounts(next);
      setCodexNewLabel("");
      await refreshCodexImportProbe();
      refreshCodexUsage({ refresh: true, silent: true }).catch(() => {});
    } catch (error) {
      setCodexAccountsError(messageFromError(error));
    } finally {
      setCodexImportBusy(false);
      setCodexAccountsBusy(false);
    }
  }, [codexNewLabel, refreshCodexImportProbe, refreshCodexUsage]);

  const onCodexSetActive = useCallback(async (accountId: string | null) => {
    setCodexAccountsBusy(true);
    setCodexAccountsError(null);
    try {
      const next = await setCodexActiveAccount(accountId);
      setCodexAccounts(next);
      refreshCodexUsage({ refresh: true, silent: true }).catch(() => {});
    } catch (error) {
      setCodexAccountsError(messageFromError(error));
    } finally {
      setCodexAccountsBusy(false);
    }
  }, [refreshCodexUsage]);

  const onCodexDelete = useCallback(async (accountId: string) => {
    setCodexAccountsBusy(true);
    setCodexAccountsError(null);
    try {
      const next = await deleteCodexAccount(accountId);
      setCodexAccounts(next);
      refreshCodexUsage({ refresh: true, silent: true }).catch(() => {});
    } catch (error) {
      setCodexAccountsError(messageFromError(error));
    } finally {
      setCodexAccountsBusy(false);
    }
  }, [refreshCodexUsage]);

  const onCodexCompleteCallback = useCallback(async (login: CodexAccountsResponse["logins"][number]) => {
    const callbackUrl = (codexCallbackUrls[login.account_id] ?? "").trim();
    if (!callbackUrl) {
      setCodexAccountsError("Callback URL is required.");
      return;
    }
    const token = login.completion_token?.trim();
    if (!token) {
      setCodexAccountsError("Completion token is missing for this pending login.");
      return;
    }
    setCodexCallbackBusy((prev) => ({ ...prev, [login.account_id]: true }));
    setCodexAccountsError(null);
    try {
      await completeCodexLogin(login.account_id, callbackUrl, token);
      setCodexCallbackUrls((prev) => ({ ...prev, [login.account_id]: "" }));
      await refreshCodexAccounts();
      refreshCodexUsage({ refresh: true, silent: true }).catch(() => {});
    } catch (error) {
      setCodexAccountsError(messageFromError(error));
    } finally {
      setCodexCallbackBusy((prev) => ({ ...prev, [login.account_id]: false }));
    }
  }, [codexCallbackUrls, refreshCodexAccounts, refreshCodexUsage]);

  useEffect(() => {
    if (!enabled) return;
    refreshProviders().catch(() => {});
    refreshCodexAccounts().catch(() => {});
    refreshCodexUsage({ refresh: false, silent: true }).catch(() => {});
    refreshCodexImportProbe().catch(() => {});
  }, [enabled, refreshCodexAccounts, refreshCodexImportProbe, refreshCodexUsage, refreshProviders]);

  useEffect(() => {
    if (!enabled) return;
    const pending = codexAccounts?.logins?.some((login) => login.status === "pending");
    if (!pending) return;
    const interval = window.setInterval(() => {
      refreshCodexAccounts({ silent: true }).catch(() => {});
    }, 2000);
    return () => window.clearInterval(interval);
  }, [codexAccounts, enabled, refreshCodexAccounts]);

  useEffect(() => {
    if (!enabled || !codexAccounts) return;
    refreshCodexUsage({ refresh: true, silent: true }).catch(() => {});
  }, [codexAccounts?.active_account_id, codexAccounts?.accounts?.length, enabled, refreshCodexUsage]);

  return {
    providers,
    codexAccounts,
    codexAccountsBusy,
    codexAccountsError,
    codexUsage,
    codexUsageBusy,
    codexUsageError,
    codexImportProbe,
    codexImportBusy,
    codexCallbackBusy,
    codexCallbackUrls,
    setCodexCallbackUrls,
    codexNewLabel,
    setCodexNewLabel,
    refreshCodexUsage,
    onCodexDelete,
    onCodexSetActive,
    onCodexLogin,
    onCodexImportHost,
    openCodexAuthUrl,
    onCodexCompleteCallback,
  };
}
