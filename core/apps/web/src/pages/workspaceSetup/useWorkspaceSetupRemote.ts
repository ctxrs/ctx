import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type Dispatch,
  type SetStateAction,
} from "react";
import { applyDaemonDesktopConnection, getHealth, repoStatus } from "../../api/client";
import {
  desktopConnectLocal,
  desktopConnectSsh,
  desktopGetGitBranch,
  desktopListSshHosts,
  desktopListSshPaths,
  desktopTestSsh,
  isDesktopApp,
  type DesktopConnectionInfo,
  type DesktopSshHost,
  type DesktopSshPathEntry,
} from "../../utils/desktop";
import {
  loadSshRecents,
  parseUserHost,
  upsertRemoteProfile,
  upsertSshRecent,
  type SshRecent,
} from "./remoteProfiles";
import type { WizardSelections } from "./wizardFlowReducer";
import type { WizardStepKey } from "./wizardFlow";
import {
  looksLikeSshAuthFailure,
  messageFromError,
  type RemoteStatus,
  type SshSuggestion,
} from "./wizardTypes";
import {
  parseWorkspaceSetupRemotePort,
  type WorkspaceSetupEffectiveTarget,
  type WorkspaceSetupTargetDraft,
} from "./workflowTypes";

type ImportRepoStatus = "idle" | "checking" | "ok" | "error";
export type RemotePasswordPromptMode = "ssh" | "admin";

type UseWorkspaceSetupRemoteParams = {
  selections: WizardSelections;
  stepKey: WizardStepKey;
  needsSourcePath: boolean;
  sourcePath: string;
  targetDraft: WorkspaceSetupTargetDraft;
  effectiveTarget: WorkspaceSetupEffectiveTarget | null;
  setRemoteHostInput: Dispatch<SetStateAction<string>>;
  setRemotePortInput: Dispatch<SetStateAction<string>>;
  setRemoteDataDirInput: Dispatch<SetStateAction<string>>;
  setImportRepoStatus: Dispatch<SetStateAction<ImportRepoStatus>>;
  setImportRepoNote: Dispatch<SetStateAction<string | null>>;
  setTargetBranch: Dispatch<SetStateAction<string>>;
  setPushBranch: Dispatch<SetStateAction<string>>;
  targetBranchTouched: boolean;
  pushBranchTouched: boolean;
  onRemoteEndpointChanged: () => void;
};

export type WorkspaceSetupRemoteState = ReturnType<typeof useWorkspaceSetupRemote>;

export function useWorkspaceSetupRemote({
  selections,
  stepKey,
  needsSourcePath,
  sourcePath,
  targetDraft,
  effectiveTarget,
  setRemoteHostInput,
  setRemotePortInput,
  setRemoteDataDirInput,
  setImportRepoStatus,
  setImportRepoNote,
  setTargetBranch,
  setPushBranch,
  targetBranchTouched,
  pushBranchTouched,
  onRemoteEndpointChanged,
}: UseWorkspaceSetupRemoteParams) {
  const [sshHosts, setSshHosts] = useState<DesktopSshHost[]>([]);
  const [sshRecents, setSshRecents] = useState<SshRecent[]>(() => loadSshRecents());
  const [remotePasswordInput, setRemotePasswordInput] = useState("");
  const [remotePasswordPromptVisible, setRemotePasswordPromptVisible] = useState(false);
  const [remotePasswordPromptMode, setRemotePasswordPromptMode] =
    useState<RemotePasswordPromptMode>("ssh");
  const [remoteStatus, setRemoteStatus] = useState<RemoteStatus>("idle");
  const [remoteError, setRemoteError] = useState<string | null>(null);
  const [remotePathSuggestions, setRemotePathSuggestions] = useState<DesktopSshPathEntry[]>([]);
  const [remotePathStatus, setRemotePathStatus] = useState<"idle" | "loading" | "error">("idle");
  const [remotePathError, setRemotePathError] = useState<string | null>(null);
  const remoteStatusRef = useRef<RemoteStatus>("idle");
  const remoteSshPasswordCandidateRef = useRef<string | null>(null);
  const remoteAdminPasswordCandidateRef = useRef<string | null>(null);

  const remoteHostInput = targetDraft.remoteHostInput;
  const remotePortInput = targetDraft.remotePortInput;
  const remoteDataDirInput = targetDraft.remoteDataDirInput;
  const desktopApp = isDesktopApp();
  const parsedRemote = effectiveTarget?.kind === "remote"
    ? { host: effectiveTarget.host, user: effectiveTarget.user }
    : parseUserHost(remoteHostInput);
  const remotePasswordOnce = remotePasswordInput.length > 0 ? remotePasswordInput : null;
  const remoteSshPasswordOnce = remotePasswordPromptMode === "ssh" ? remotePasswordOnce : null;
  const remoteAdminPasswordOnce = remotePasswordPromptMode === "admin" ? remotePasswordOnce : null;
  const parsedRemotePort = effectiveTarget?.kind === "remote"
    ? effectiveTarget.port
    : parseWorkspaceSetupRemotePort(remotePortInput);
  const selectedDaemonTargetKey = effectiveTarget?.targetKey ?? null;
  const hasRemoteHost = Boolean(parsedRemote?.host);

  const applyConnection = useCallback((info: DesktopConnectionInfo) => {
    applyDaemonDesktopConnection(info);
  }, []);

  const resetVerificationState = useCallback(() => {
    if (remoteStatusRef.current !== "idle") {
      remoteStatusRef.current = "idle";
      setRemoteStatus("idle");
    }
    setRemoteError(null);
  }, []);

  const onRemoteInputChange = useCallback((value: string) => {
    setRemoteHostInput(value);
    setRemotePasswordInput("");
    remoteSshPasswordCandidateRef.current = null;
    remoteAdminPasswordCandidateRef.current = null;
    setRemotePasswordPromptVisible(false);
    setRemotePasswordPromptMode("ssh");
    onRemoteEndpointChanged();
    resetVerificationState();
  }, [onRemoteEndpointChanged, resetVerificationState]);

  const onRemotePasswordInputChange = useCallback((value: string) => {
    setRemotePasswordInput(value);
    if (remotePasswordPromptMode === "admin") {
      remoteAdminPasswordCandidateRef.current = value.trim() ? value : null;
    } else {
      remoteSshPasswordCandidateRef.current = value.trim() ? value : null;
    }
    resetVerificationState();
  }, [remotePasswordPromptMode, resetVerificationState]);

  const onRemotePortInputChange = useCallback((value: string) => {
    setRemotePortInput(value);
    onRemoteEndpointChanged();
    resetVerificationState();
  }, [onRemoteEndpointChanged, resetVerificationState]);

  const onRemoteDataDirInputChange = useCallback((value: string) => {
    setRemoteDataDirInput(value);
    onRemoteEndpointChanged();
    resetVerificationState();
  }, [onRemoteEndpointChanged, resetVerificationState]);

  const sleepMs = useCallback(
    (ms: number) => new Promise((resolve) => window.setTimeout(resolve, ms)),
    [],
  );

  const waitForDaemonReady = useCallback(async (timeoutMs: number) => {
    const started = Date.now();
    let lastErr: unknown = null;
    while (Date.now() - started < timeoutMs) {
      try {
        await getHealth();
        return;
      } catch (error) {
        lastErr = error;
      }
      await sleepMs(200);
    }
    throw lastErr ?? new Error("Timed out waiting for daemon health.");
  }, [sleepMs]);

  const connectDaemonForImport = useCallback(async (locationOverride?: "local" | "remote") => {
    const location = locationOverride ?? selections.location;
    if (!isDesktopApp()) {
      throw new Error("Auth import requires the desktop app.");
    }
    if (location === "remote") {
      if (effectiveTarget?.kind !== "remote") {
        throw new Error("Remote host is required before scanning auth.");
      }
      if (remoteStatusRef.current !== "connected") {
        throw new Error("Verify remote host connection before scanning auth.");
      }
      // Wizard scans may reuse an already-running remote daemon, but they must not
      // cold-start it before the explicit Create action.
      const info = await desktopConnectSsh({
        host: effectiveTarget.host,
        user: effectiveTarget.user,
        password_once: remoteSshPasswordOnce ?? remoteSshPasswordCandidateRef.current,
        remote_port: effectiveTarget.port,
        start_remote: false,
        remote_data_dir: effectiveTarget.dataDir,
      });
      applyConnection(info);
      await waitForDaemonReady(15000);
      return;
    }

    const info = await desktopConnectLocal();
    applyConnection(info);
    await waitForDaemonReady(15000);
  }, [
    applyConnection,
    effectiveTarget,
    remoteSshPasswordOnce,
    selections.location,
    waitForDaemonReady,
  ]);

  const rememberCurrentRemoteProfile = useCallback(() => {
    if (effectiveTarget?.kind !== "remote") return;
    upsertRemoteProfile(effectiveTarget.host, effectiveTarget.user, {
      remote_port: effectiveTarget.port,
      remote_data_dir: effectiveTarget.dataDir,
    });
  }, [effectiveTarget]);

  const requestRemotePasswordPrompt = useCallback((mode: RemotePasswordPromptMode = "admin") => {
    setRemotePasswordPromptMode(mode);
    if (mode === "admin") {
      remoteAdminPasswordCandidateRef.current = null;
    }
    setRemotePasswordInput("");
    setRemotePasswordPromptVisible(true);
    setRemoteStatus("idle");
    setRemoteError(null);
  }, []);

  const rememberRemoteProfile = useCallback((host: string, user: string | null) => {
    if (!host) return;
    upsertRemoteProfile(host, user, {
      remote_port: effectiveTarget?.kind === "remote" ? effectiveTarget.port : (parsedRemotePort ?? 4399),
      remote_data_dir: effectiveTarget?.kind === "remote"
        ? effectiveTarget.dataDir
        : (remoteDataDirInput.trim() ? remoteDataDirInput.trim() : null),
    });
  }, [effectiveTarget, parsedRemotePort, remoteDataDirInput]);

  const verifyRemoteConnection = useCallback(async () => {
    if (!parsedRemote?.host) return false;
    if (!isDesktopApp()) {
      setRemoteStatus("error");
      setRemoteError("Remote connections require the desktop app.");
      return false;
    }
    if (remoteStatusRef.current === "connected") return true;

    setRemoteStatus("connecting");
    setRemoteError(null);
    try {
      await desktopTestSsh({
        host: parsedRemote.host,
        user: parsedRemote.user ?? null,
        password_once: remoteSshPasswordOnce,
      });
      remoteStatusRef.current = "connected";
      setRemoteStatus("connected");
      if (remoteSshPasswordOnce) {
        remoteSshPasswordCandidateRef.current = remoteSshPasswordOnce;
      }
      setRemotePasswordInput("");
      setRemotePasswordPromptVisible(false);
      setRemotePasswordPromptMode("ssh");
      rememberCurrentRemoteProfile();
      setSshRecents(upsertSshRecent(parsedRemote.host, parsedRemote.user ?? null));
      return true;
    } catch (error) {
      const detail = messageFromError(error);
      if (!remotePasswordPromptVisible && remoteSshPasswordOnce === null && looksLikeSshAuthFailure(detail)) {
        remoteStatusRef.current = "idle";
        setRemotePasswordPromptMode("ssh");
        setRemotePasswordPromptVisible(true);
        setRemoteStatus("idle");
        setRemoteError(null);
        return false;
      }
      remoteStatusRef.current = "error";
      setRemoteStatus("error");
      setRemoteError(detail);
      return false;
    }
  }, [
    parsedRemote?.host,
    parsedRemote?.user,
    rememberCurrentRemoteProfile,
    remoteSshPasswordOnce,
    remotePasswordPromptVisible,
  ]);

  useEffect(() => {
    remoteStatusRef.current = remoteStatus;
  }, [remoteStatus]);

  useEffect(() => {
    setSshRecents(loadSshRecents());
  }, []);

  useEffect(() => {
    if (!isDesktopApp()) return;
    desktopListSshHosts()
      .then((hosts) => setSshHosts(hosts))
      .catch(() => setSshHosts([]));
  }, []);

  useEffect(() => {
    const shouldSuggest = needsSourcePath
      && selections.location === "remote"
      && remoteStatus === "connected"
      && Boolean(parsedRemote?.host)
      && isDesktopApp();
    if (!shouldSuggest) {
      setRemotePathSuggestions([]);
      setRemotePathStatus("idle");
      setRemotePathError(null);
      return;
    }
    const handle = window.setTimeout(() => {
      setRemotePathStatus("loading");
      setRemotePathError(null);
      desktopListSshPaths({
        host: parsedRemote!.host,
        user: parsedRemote!.user ?? null,
        path: sourcePath,
      })
        .then((entries) => {
          setRemotePathSuggestions(entries);
          setRemotePathStatus("idle");
        })
        .catch((error: unknown) => {
          setRemotePathStatus("error");
          setRemotePathError(messageFromError(error));
        });
    }, 250);
    return () => window.clearTimeout(handle);
  }, [needsSourcePath, parsedRemote?.host, parsedRemote?.user, remoteStatus, selections.location, sourcePath]);

  useEffect(() => {
    if (!isDesktopApp()) return;
    if (selections.location !== "local") return;
    if (selections.source !== "import") {
      setImportRepoStatus("idle");
      setImportRepoNote(null);
      return;
    }
    if (!sourcePath.trim()) {
      setImportRepoStatus("idle");
      setImportRepoNote(null);
      return;
    }
    let cancelled = false;
    setImportRepoStatus("checking");
    setImportRepoNote(null);
    desktopGetGitBranch({ path: sourcePath })
      .then((branch) => {
        if (cancelled) return;
        setImportRepoStatus("ok");
        setImportRepoNote(null);
        if (!branch || targetBranchTouched) return;
        setTargetBranch(branch);
        if (!pushBranchTouched) {
          setPushBranch(branch);
        }
      })
      .catch(() => {
        if (cancelled) return;
        setImportRepoStatus("error");
        setImportRepoNote("Selected folder does not look like a git repo.");
      });
    return () => {
      cancelled = true;
    };
  }, [
    pushBranchTouched,
    selections.location,
    selections.source,
    setImportRepoNote,
    setImportRepoStatus,
    setPushBranch,
    setTargetBranch,
    sourcePath,
    targetBranchTouched,
  ]);

  useEffect(() => {
    const shouldCheck =
      isDesktopApp()
      && stepKey === "source"
      && selections.location === "remote"
      && remoteStatus === "connected"
      && selections.source === "import"
      && Boolean(parseUserHost(remoteHostInput)?.host)
      && Boolean(sourcePath.trim());
    if (!shouldCheck) return;
    const handle = window.setTimeout(() => {
      setImportRepoStatus("ok");
      setImportRepoNote("Remote folder checks run during Create so the remote daemon stays cold until launch.");
    }, 400);

    return () => window.clearTimeout(handle);
  }, [
    remoteHostInput,
    remoteStatus,
    selections.location,
    selections.source,
    setImportRepoNote,
    setImportRepoStatus,
    sourcePath,
    stepKey,
  ]);

  const sshSuggestions = useMemo<SshSuggestion[]>(() => {
    const query = remoteHostInput.trim().toLowerCase();
    const list: SshSuggestion[] = [];
    const seen = new Set<string>();
    for (const recent of sshRecents) {
      const key = `${recent.user ?? ""}@${recent.host}`;
      if (seen.has(key)) continue;
      seen.add(key);
      list.push({ host: recent.host, user: recent.user ?? null });
    }
    for (const host of sshHosts) {
      const key = `${host.user ?? ""}@${host.host}`;
      if (seen.has(key)) continue;
      seen.add(key);
      list.push({ host: host.host, user: host.user ?? null });
    }
    return list.filter((entry) => {
      if (!query) return true;
      const haystack = `${entry.user ?? ""}@${entry.host}`.toLowerCase();
      return haystack.includes(query);
    }).slice(0, 6);
  }, [remoteHostInput, sshHosts, sshRecents]);

  const resetForLocalSelection = useCallback(() => {
    remoteStatusRef.current = "idle";
    setRemoteStatus("idle");
    setRemoteError(null);
    setRemotePasswordInput("");
    setRemotePasswordPromptVisible(false);
    setRemotePasswordPromptMode("ssh");
    remoteSshPasswordCandidateRef.current = null;
    remoteAdminPasswordCandidateRef.current = null;
    setImportRepoStatus("idle");
    setImportRepoNote(null);
  }, [setImportRepoNote, setImportRepoStatus]);

  const onPickLocalFolder = useCallback(async () => {
    if (!isDesktopApp()) return;
    try {
      const { desktopPickFolder } = await import("../../utils/desktop");
      const picked = await desktopPickFolder();
      return picked ?? "";
    } catch {
      return "";
    }
  }, []);

  return {
    desktopApp,
    hasRemoteHost,
    onRemoteDataDirInputChange,
    onRemoteInputChange,
    onRemotePasswordInputChange,
    onRemotePortInputChange,
    parsedRemote,
    parsedRemotePort,
    remoteDataDirInput,
    remoteError,
    remoteHostInput,
    remoteAdminPasswordCandidate: remoteAdminPasswordCandidateRef.current,
    remotePasswordInput,
    remotePasswordPromptMode,
    remotePasswordOnce,
    remoteSshPasswordCandidate: remoteSshPasswordCandidateRef.current,
    remoteSshPasswordOnce,
    remoteAdminPasswordOnce,
    remotePasswordPromptVisible,
    remotePathError,
    remotePathStatus,
    remotePathSuggestions,
    remotePortInput,
    remoteStatus,
    resetForLocalSelection,
    selectedDaemonTargetKey,
    setRemotePasswordInput,
    setRemotePasswordPromptVisible,
    remoteStatusRef,
    setRemoteStatus,
    setRemoteError,
    applyConnection,
    sshSuggestions,
    onPickLocalFolder,
    verifyRemoteConnection,
    waitForDaemonReady,
    connectDaemonForImport,
    rememberCurrentRemoteProfile,
    rememberRemoteProfile,
    requestRemotePasswordPrompt,
  };
}
