import { useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";
import {
  applyDaemonDesktopConnection,
  type ExecutionLaunchSnapshot,
  getHealth,
  listWorkspaces,
} from "../../api/client";
import {
  desktopConnectLocal,
  desktopConnectSsh,
  desktopGetConnection,
  desktopSetDockRecentLocalWorkspaces,
  isDesktopApp,
  type DesktopConnectionInfo,
  type DesktopDockRecentLocalWorkspace,
} from "../../utils/desktop";
import { errorMessage } from "../../utils/errorMessage";
import LauncherBrand from "../../components/LauncherBrand";
import {
  loadLauncherRecents,
  upsertLauncherRecent,
  type LauncherExecutionEnvironment,
  type LauncherRecentEntry,
} from "../../state/launcherRecentsStore";
import {
  startWorkspaceSetupLaunchHandoff,
  waitForLaunchHandoffTerminal,
} from "../workspaceSetup/launchHandoff";
import {
  currentLaunchStepLabel,
  formatLaunchRemaining,
  launchEtaRemainingMs,
} from "../workspaceSetup/launchProgress";
import {
  loadWorkspaceExecutionEnvironment,
  recentLocationDisplay,
  recentRenderKey,
  recentsFromWorkspaces,
  resolveWorkspaceByPathWithRetry,
} from "./launcherRecents";

function applyConnection(info: DesktopConnectionInfo) {
  applyDaemonDesktopConnection(info);
}

const sleepMs = (ms: number) => new Promise((resolve) => window.setTimeout(resolve, ms));

const waitForDaemonReady = async (timeoutMs: number) => {
  const started = Date.now();
  let lastErr: unknown = null;
  while (Date.now() - started < timeoutMs) {
    try {
      await getHealth();
      return;
    } catch (e) {
      lastErr = e;
    }
    await sleepMs(200);
  }
  throw lastErr ?? new Error("Timed out waiting for daemon health.");
};

function withEllipsis(value: string): string {
  const trimmed = String(value || "").trim();
  if (!trimmed) return "";
  if (/[.!?…]$/.test(trimmed)) return trimmed;
  return `${trimmed}...`;
}

function sandboxLaunchProgressLabel(
  snapshot: ExecutionLaunchSnapshot | null,
  nowMs: number,
  fallbackDetail: string | null,
): string {
  if (!snapshot) {
    const fallback = String(fallbackDetail || "").trim();
    return fallback || "Preparing sandbox...";
  }

  if (snapshot.state === "ready") return "Opening workspace...";
  if (snapshot.state === "error") return "Sandbox launch failed";

  let baseLabel: string;
  switch (snapshot.current_phase) {
    case "artifact_download":
      baseLabel = "Downloading runtime...";
      break;
    case "machine_check":
      baseLabel = "Checking sandbox...";
      break;
    case "machine_start_or_init":
      baseLabel = "Restarting VM...";
      break;
    case "image_check":
    case "image_load":
    case "container_check":
    case "container_start_or_create":
    case "runtime_network_setup":
      baseLabel = "Preparing sandbox...";
      break;
    default:
      baseLabel = withEllipsis(currentLaunchStepLabel(snapshot));
      break;
  }

  const etaRemainingMs = launchEtaRemainingMs(snapshot, nowMs);
  if (etaRemainingMs !== null && etaRemainingMs > 0) {
    return `${baseLabel} (${formatLaunchRemaining(etaRemainingMs)})`;
  }
  return baseLabel;
}

export default function LauncherPage() {
  const navigate = useNavigate();
  const [connection, setConnection] = useState<DesktopConnectionInfo | null>(null);
  const [recents, setRecents] = useState<LauncherRecentEntry[]>([]);
  const [busy, setBusy] = useState(false);
  const [openingRecentKey, setOpeningRecentKey] = useState<string | null>(null);
  const [busyDetail, setBusyDetail] = useState<string | null>(null);
  const [launchSnapshot, setLaunchSnapshot] = useState<ExecutionLaunchSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [launchNowMs, setLaunchNowMs] = useState(() => Date.now());

  const isDesktop = isDesktopApp();

  useEffect(() => {
    if (!isDesktop) {
      setConnection({ kind: "none" });
      return;
    }
    desktopGetConnection()
      .then((info) => {
        setConnection(info);
        applyConnection(info);
      })
      .catch(() => setConnection({ kind: "none" }));
  }, [isDesktop, navigate]);

  useEffect(() => {
    let cancelled = false;
    const syncDockRecents = (entries: LauncherRecentEntry[]) => {
      if (!isDesktop) return;
      const localEntries: DesktopDockRecentLocalWorkspace[] = entries
        .filter((entry): entry is Extract<LauncherRecentEntry, { kind: "local" }> => entry.kind === "local")
        .map((entry) => ({
          label: entry.label,
          root_path: entry.root_path,
        }));
      void desktopSetDockRecentLocalWorkspaces(localEntries).catch(() => {});
    };

    const loadRecents = async () => {
      try {
        const persisted = await loadLauncherRecents();
        if (cancelled) return;
        if (persisted.length > 0) {
          setRecents(persisted);
          syncDockRecents(persisted);
          return;
        }

        const workspaces = await listWorkspaces();
        if (cancelled) return;
        const inferred = await recentsFromWorkspaces(workspaces);
        setRecents(inferred);
        syncDockRecents(inferred);
      } catch {
        if (cancelled) return;
        setRecents([]);
        syncDockRecents([]);
      }
    };

    void loadRecents();
    return () => {
      cancelled = true;
    };
  }, [busy, isDesktop]);

  useEffect(() => {
    if (!busy || !openingRecentKey) return undefined;
    setLaunchNowMs(Date.now());
    const intervalId = window.setInterval(() => {
      setLaunchNowMs(Date.now());
    }, 1000);
    return () => {
      window.clearInterval(intervalId);
    };
  }, [busy, openingRecentKey]);

  const prepareSandboxWorkspace = async (workspaceId: string) => {
    setLaunchSnapshot(null);
    setBusyDetail("Preparing sandbox...");
    const initial = await startWorkspaceSetupLaunchHandoff(workspaceId);
    setLaunchSnapshot(initial);
    await waitForLaunchHandoffTerminal(initial, {
      applySnapshot: (snapshot) => {
        setLaunchSnapshot(snapshot);
      },
      appendLines: () => {},
    });
  };

  const resetBusyState = () => {
    setBusy(false);
    setOpeningRecentKey(null);
    setBusyDetail(null);
    setLaunchSnapshot(null);
  };

  const connectLocalAndOpen = async (
    rootPath?: string,
    executionEnvironment?: LauncherExecutionEnvironment,
    openingKey?: string,
  ) => {
    try {
      setBusyDetail("Connecting daemon...");
      const info = await desktopConnectLocal();
      setConnection(info);
      applyConnection(info);
      // Avoid landing on workspaces while the daemon is still booting.
      setBusyDetail("Waiting for daemon...");
      await waitForDaemonReady(15000);
      if (rootPath) {
        setBusyDetail("Finding workspace...");
        const resolvedWorkspace = await resolveWorkspaceByPathWithRetry(rootPath, 5000);
        if (!resolvedWorkspace) {
          setError("Workspace not found for this path. Re-create it from New Workspace.");
          navigate("/workspace-setup");
          return;
        }
        let resolvedExecutionEnvironment = executionEnvironment;
        if (!resolvedExecutionEnvironment) {
          setBusyDetail("Loading settings...");
          resolvedExecutionEnvironment = await loadWorkspaceExecutionEnvironment(resolvedWorkspace.workspaceId)
            .catch(() => undefined);
        }
        if (resolvedExecutionEnvironment === "sandbox") {
          if (openingKey) setOpeningRecentKey(openingKey);
          await prepareSandboxWorkspace(resolvedWorkspace.workspaceId);
        } else {
          setOpeningRecentKey(null);
        }
        try {
          await upsertLauncherRecent({
            kind: "local",
            label: resolvedWorkspace.label,
            root_path: resolvedWorkspace.rootPath,
            execution_environment: resolvedExecutionEnvironment,
            updated_at_ms: Date.now(),
          });
        } catch {
          // best-effort only; do not block workspace open on recents persistence
        }
        navigate(`/workspaces/${resolvedWorkspace.workspaceId}`, { replace: true });
      } else {
        navigate("/", { replace: true });
      }
    } catch (e: unknown) {
      setError(errorMessage(e));
    } finally {
      resetBusyState();
    }
  };

  const onOpenRecent = async (r: LauncherRecentEntry) => {
    const recentKey = recentRenderKey(r);
    setError(null);
    setBusy(true);
    setOpeningRecentKey(r.execution_environment === "sandbox" ? recentKey : null);
    setBusyDetail(r.kind === "local" ? "Connecting daemon..." : "Connecting remote...");
    setLaunchSnapshot(null);
    try {
      if (r.kind === "local") {
        await connectLocalAndOpen(r.root_path, r.execution_environment, recentKey);
        return;
      }
      const info = await desktopConnectSsh({
        host: r.host,
        user: r.user ?? null,
        remote_port: r.remote_port,
        start_remote: Boolean(r.start_remote),
        remote_data_dir: r.remote_data_dir ?? null,
      });
      setConnection(info);
      applyConnection(info);
      // Avoid landing on workspaces while the daemon is still booting / tunnel is coming up.
      setBusyDetail("Waiting for remote...");
      await waitForDaemonReady(15000);
      const targetWorkspaceRootPath = String(r.workspace_root_path ?? "").trim();
      setBusyDetail("Finding workspace...");
      const resolvedWorkspace = targetWorkspaceRootPath
        ? await resolveWorkspaceByPathWithRetry(targetWorkspaceRootPath, 5000)
        : null;
      if (targetWorkspaceRootPath && !resolvedWorkspace) {
        setError("Workspace not found on the connected host for this path. Re-create it from New Workspace.");
        navigate("/workspace-setup");
        return;
      }
      let resolvedExecutionEnvironment = r.execution_environment;
      if (resolvedWorkspace && !resolvedExecutionEnvironment) {
        setBusyDetail("Loading settings...");
        resolvedExecutionEnvironment = await loadWorkspaceExecutionEnvironment(resolvedWorkspace.workspaceId)
          .catch(() => undefined);
      }
      if (resolvedWorkspace && resolvedExecutionEnvironment === "sandbox") {
        setOpeningRecentKey(recentKey);
        await prepareSandboxWorkspace(resolvedWorkspace.workspaceId);
      } else {
        setOpeningRecentKey(null);
      }
      try {
        await upsertLauncherRecent({
          ...r,
          ...(resolvedWorkspace
            ? {
                label: resolvedWorkspace.label,
                workspace_root_path: resolvedWorkspace.rootPath,
                execution_environment: resolvedExecutionEnvironment,
              }
            : {}),
          updated_at_ms: Date.now(),
        });
      } catch {
        // best-effort only; do not block connection flow on recents persistence
      }
      navigate(resolvedWorkspace ? `/workspaces/${resolvedWorkspace.workspaceId}` : "/", { replace: true });
    } catch (e: unknown) {
      setError(errorMessage(e));
    } finally {
      resetBusyState();
    }
  };

  const onNewWorkspace = () => {
    navigate("/workspace-setup");
  };

  const displayRecents = recents;

  return (
    <div className="launcher-shell launcher-shell--crt">
      <LauncherBrand fullScreen>
        <div className="launcher-panel">
          <div className="launcher-actions">
            <button type="button" className="launcher-action" onClick={onNewWorkspace} disabled={busy}>
              <span className="launcher-action-icon" aria-hidden="true">
                <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="2">
                  <path d="M12 5v14" />
                  <path d="M5 12h14" />
                </svg>
              </span>
              <span className="launcher-action-label">New Workspace</span>
            </button>
          </div>

          {error && <div className="launcher-error">{error}</div>}

          <section className="launcher-recents">
            <div className="launcher-recents-header">
              <strong>Recent Workspaces</strong>
            </div>
            <div className="launcher-recents-list" tabIndex={0} aria-label="Recent workspaces list">
              {displayRecents.map((r) => {
                const key = recentRenderKey(r);
                const location = recentLocationDisplay(r);
                const rowOpening = busy && openingRecentKey === key;
                const openingStatus = rowOpening
                  ? sandboxLaunchProgressLabel(launchSnapshot, launchNowMs, busyDetail)
                  : null;
                return (
                  <button
                    type="button"
                    key={key}
                    className="launcher-recent-item"
                    onClick={() => onOpenRecent(r)}
                    disabled={busy}
                  >
                    <span className="launcher-recent-name">{r.label}</span>
                    {rowOpening ? (
                      <span className="launcher-recent-inline-status" role="status" aria-live="polite">
                        <span className="launcher-spinner" aria-hidden="true" />
                        <span>{openingStatus}</span>
                      </span>
                    ) : (
                      <span className="launcher-recent-location" title={location.title}>{location.label}</span>
                    )}
                  </button>
                );
              })}
              {displayRecents.length === 0 && <div className="launcher-empty">No recent workspaces yet.</div>}
            </div>
          </section>
        </div>
      </LauncherBrand>
    </div>
  );
}
