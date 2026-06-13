import { useCallback, useEffect, useRef, useState } from "react";
import { Link } from "react-router-dom";
import {
  InstallInfo,
  InstallProgressEvent,
  ProviderStatus,
  ProviderOptions,
  Workspace,
  getProviderOptions,
  idToString,
  installAllProviders,
  installProvider,
  listProviders,
  listWorkspaces,
} from "../../api/client";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "../../components/ui/select";
import {
  getInstallProgressSnapshot,
  observeInstall,
  subscribeInstallProgress,
  type InstallProgressEntry,
  type InstallProgressSnapshot,
} from "../../state/installProgressMonitor";
import { providerDetailFlag } from "../../utils/boolish";
import { copyTextToClipboard } from "../../utils/clipboard";
import { errorMessage } from "../../utils/errorMessage";
import { PROVIDER_INSTALLS_ENABLED } from "../../utils/providerInstallGate";
import { formatProviderVersionDisplay, getMatrixVersionDisplay } from "../../utils/providerVersionLabel";

type InstallSession = {
  installId: string;
  state: InstallInfo["state"];
  events: InstallProgressEvent[];
  streamError?: string;
  error?: string;
};

const fmtBytes = (n: number): string => {
  if (!Number.isFinite(n)) return "";
  const units = ["B", "KB", "MB", "GB"];
  let v = n;
  let u = 0;
  while (v >= 1024 && u < units.length - 1) {
    v /= 1024;
    u += 1;
  }
  return `${v.toFixed(u === 0 ? 0 : 1)} ${units[u]}`;
};

const asRecord = (value: unknown): Record<string, unknown> => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, unknown>;
};

const toInstallSession = (entry: InstallProgressEntry): InstallSession => ({
  installId: entry.installId,
  state: entry.state,
  events: entry.events,
  error: entry.error,
});

const installsFromSnapshot = (
  snapshot: InstallProgressSnapshot,
): Record<string, InstallSession> =>
  Object.fromEntries(
    Object.values(snapshot)
      .filter((entry) => typeof entry.providerId === "string" && entry.providerId.trim().length > 0)
      .map((entry) => [entry.providerId as string, toInstallSession(entry)]),
  );

export default function ProvidersPage() {
  const [providers, setProviders] = useState<ProviderStatus[]>([]);
  const [workspaces, setWorkspaces] = useState<Workspace[]>([]);
  const [workspaceId, setWorkspaceId] = useState<string | null>(null);
  const [providerOptions, setProviderOptions] = useState<Record<string, ProviderOptions | undefined>>({});
  const [optsBusy, setOptsBusy] = useState<Record<string, boolean>>({});
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [installs, setInstalls] = useState<Record<string, InstallSession>>(
    () => installsFromSnapshot(getInstallProgressSnapshot()),
  );
  const installControlsEnabled = PROVIDER_INSTALLS_ENABLED;

  const installObserversRef = useRef<Record<string, () => void>>({});
  const previousInstallsRef = useRef<Record<string, InstallSession>>({});

  const refresh = useCallback(() =>
    listProviders()
      .then(setProviders)
      .catch((e: unknown) => setError(errorMessage(e))), []);

  useEffect(() => {
    refresh();
    listWorkspaces()
      .then((ws) => {
        setWorkspaces(ws);
        setWorkspaceId((prev) => prev ?? (ws.length > 0 ? idToString(ws[0]?.id ?? "") : null));
      })
      .catch(() => {});
  }, [refresh]);

  useEffect(() => {
    setProviderOptions({});
  }, [workspaceId]);

  useEffect(() => {
    return subscribeInstallProgress((snapshot) => {
      setInstalls(installsFromSnapshot(snapshot));
    });
  }, []);

  const attachInstall = useCallback((providerId: string, installId: string) => {
    if (!providerId || !installId) return;
    const existingInstallId = installs[providerId]?.installId;
    if (existingInstallId === installId && installObserversRef.current[providerId]) {
      return;
    }
    installObserversRef.current[providerId]?.();
    setInstalls((prev) => ({
      ...prev,
      [providerId]: {
        installId,
        state: "running",
        events: prev[providerId]?.events ?? [],
        error: undefined,
      },
    }));
    installObserversRef.current[providerId] = observeInstall(installId, {
      providerId,
      loadHistory: true,
      initialState: {
        state: "running",
      },
    });
  }, [installs]);

  useEffect(() => {
    for (const p of providers) {
      const installId = p.details?.install_id;
      const running = providerDetailFlag(p.details, "install_running");
      const tracked = installs[p.provider_id];
      if (
        running
        && installId
        && (!tracked || tracked.installId !== installId || tracked.state !== "running")
      ) {
        attachInstall(p.provider_id, installId);
      }
    }
  }, [attachInstall, installs, providers]);

  useEffect(() => {
    return () => {
      for (const stop of Object.values(installObserversRef.current)) {
        stop();
      }
      installObserversRef.current = {};
    };
  }, []);

  useEffect(() => {
    let needsRefresh = false;
    for (const [providerId, install] of Object.entries(installs)) {
      const previous = previousInstallsRef.current[providerId];
      if (!install || install.state === "running") continue;
      if (previous?.installId === install.installId && previous.state === install.state) continue;
      needsRefresh = true;
    }
    previousInstallsRef.current = installs;
    if (!needsRefresh) return;
    void refresh();
  }, [installs, refresh]);

  const onInstall = async (id: string) => {
    setBusy(id);
    setError(null);
    try {
      const { install_id } = await installProvider(id);
      attachInstall(id, install_id);
    } catch (e: unknown) {
      setError(errorMessage(e));
    } finally {
      setBusy(null);
    }
  };

  const onInstallAll = async () => {
    setBusy("all");
    setError(null);
    try {
      const installs = await installAllProviders();
      for (const i of installs) attachInstall(i.provider_id, i.install_id);
    } catch (e: unknown) {
      setError(errorMessage(e));
    } finally {
      setBusy(null);
    }
  };

  const ensureProviderOpts = async (providerId: string) => {
    if (!workspaceId) return;
    if (optsBusy[providerId]) return;
    setOptsBusy((prev) => ({ ...prev, [providerId]: true }));
    try {
      const opts = await getProviderOptions(workspaceId, providerId);
      setProviderOptions((prev) => ({ ...prev, [providerId]: opts }));
    } catch (e: unknown) {
      setError(errorMessage(e));
    } finally {
      setOptsBusy((prev) => ({ ...prev, [providerId]: false }));
    }
  };

  return (
    <div className="page">
      <div className="header">
        <Link to="/">← Launcher</Link>
      </div>

      <h1>Providers</h1>

      <div className="card">
        <div className="row">
          <strong>Managed installs</strong>
          {installControlsEnabled ? (
            <button type="button" onClick={onInstallAll} disabled={busy !== null}>
              {busy === "all" ? "Installing…" : "Install all"}
            </button>
          ) : null}
        </div>
        <div className="muted">
          Installs ACP agent servers under <code>~/.ctx/providers/agent-servers</code>.
        </div>
        {error && <div className="error">{error}</div>}
      </div>

      <div className="card">
        <div className="row">
          <strong>Provider status</strong>
          <Select
            value={workspaceId ?? undefined}
            onValueChange={(value) => setWorkspaceId(value || null)}
            disabled={workspaces.length === 0}
          >
            <SelectTrigger>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {workspaces.map((ws) => {
                const id = idToString(ws.id ?? "");
                return (
                  <SelectItem key={id} value={id}>
                    {ws.name}
                  </SelectItem>
                );
              })}
            </SelectContent>
          </Select>
        </div>
        <div className="muted">Provider probes run against the selected workspace root.</div>
      </div>

      <ul className="list">
        {providers.map((p) => {
          const installSupported = providerDetailFlag(p.details, "install_supported");
          const installRunning = installs[p.provider_id]?.state === "running";
          const installDisabled = busy !== null || installRunning || !installSupported;
          const detectedVersionLabel = formatProviderVersionDisplay(p);
          const recommendedVersionLabel = getMatrixVersionDisplay(p.details, "recommended");
          const latestVersionLabel = getMatrixVersionDisplay(p.details, "latest");

          return (
            <li key={p.provider_id} className="card">
            {workspaceId && (
              <>
                {(() => {
                  const opts = providerOptions[p.provider_id];
                  const verifyStatus = String(asRecord(opts?.verify).status ?? "");
                  const statusLine = opts?.auth_required
                    ? "Auth required"
                    : verifyStatus === "ok"
                      ? "Verified"
                      : verifyStatus === "network_error"
                        ? "Offline/unreachable"
                        : verifyStatus === "error"
                          ? "Verify error"
                          : "Unknown";
                  return <div className="muted">Auth: {statusLine}</div>;
                })()}
                <div className="row" style={{ flexWrap: "wrap" }}>
                  <button
                    type="button"
                    onClick={() => ensureProviderOpts(p.provider_id)}
                    disabled={!workspaceId || optsBusy[p.provider_id]}
                    title="Probe session/new (no prompt) to detect auth_required and list models/modes"
                  >
                    {optsBusy[p.provider_id] ? "Checking…" : "Check"}
                  </button>
                </div>
              </>
            )}

            {installs[p.provider_id] && (
              <div className="muted">
                Install: {installs[p.provider_id].state}
                {installs[p.provider_id].streamError ? ` · ${installs[p.provider_id].streamError}` : ""}
              </div>
            )}
            <div className="row">
              <strong>{p.provider_id}</strong>
              <span className="muted">{p.health}</span>
            </div>
            <div className="muted">
              {p.installed ? "Installed" : "Not installed"}
              {p.detected_path ? ` · ${p.detected_path}` : ""}
            </div>

            {(p.details?.managed_package || p.details?.managed_version || p.details?.managed_install_dir) && (
              <div className="muted">
                Managed: {p.details?.managed_package ?? "provider"}
                {p.details?.managed_version ? `@${p.details.managed_version}` : ""}
                {p.details?.managed_install_dir ? ` · ${p.details.managed_install_dir}` : ""}
              </div>
            )}

            {(p.details?.managed_last_success_at || p.details?.managed_last_error) && (
              <div className="muted">
                {p.details?.managed_last_success_at ? `Last success: ${p.details.managed_last_success_at}` : ""}
                {p.details?.managed_last_error
                  ? `${p.details?.managed_last_success_at ? " · " : ""}Last error: ${p.details.managed_last_error}`
                  : ""}
              </div>
            )}

            {(detectedVersionLabel ||
              recommendedVersionLabel ||
              latestVersionLabel ||
              providerDetailFlag(p.details, "matrix_update_available") ||
              providerDetailFlag(p.details, "matrix_update_requires_context")) && (
              <div className="muted">
                {detectedVersionLabel ? `Detected: ${detectedVersionLabel}` : "Detected: unknown"}
                {recommendedVersionLabel ? ` · Recommended: ${recommendedVersionLabel}` : ""}
                {providerDetailFlag(p.details, "matrix_update_available") ? " · Update available" : ""}
                {providerDetailFlag(p.details, "managed_dependency_update_available") ? " · Dependency update available" : ""}
                {providerDetailFlag(p.details, "matrix_update_requires_context") ? " · Requires ctx update" : ""}
              </div>
            )}

            {p.diagnostics?.length > 0 && (
              <ul className="sublist">
                {p.diagnostics.map((d, i) => (
                  <li key={i} className="muted">
                    {d}
                  </li>
                ))}
              </ul>
            )}

            {installs[p.provider_id]?.events?.length > 0 && (
              <div className="card" style={{ marginTop: 12 }}>
                {(() => {
                  const ev = installs[p.provider_id].events[installs[p.provider_id].events.length - 1];
                  const pct =
                    typeof ev.bytes === "number" && typeof ev.total_bytes === "number" && ev.total_bytes > 0
                      ? Math.round((ev.bytes / ev.total_bytes) * 100)
                      : null;
                  return (
                    <>
                      <div className="muted">
                        {ev.stage}: {ev.message}
                        {pct !== null ? ` · ${pct}% (${fmtBytes(ev.bytes!)} / ${fmtBytes(ev.total_bytes!)})` : ""}
                      </div>
                      {pct !== null && (
                        <progress value={pct} max={100} style={{ width: "100%", marginTop: 8 }} />
                      )}
                    </>
                  );
                })()}
                {installs[p.provider_id].error && (
                  <div className="error">
                    <div className="row">
                      <strong>Install error</strong>
                      <button
                        type="button"
                        onClick={() => void copyTextToClipboard(installs[p.provider_id].error ?? "")}
                      >
                        Copy
                      </button>
                    </div>
                    <pre style={{ whiteSpace: "pre-wrap", margin: "8px 0 0" }}>
                      {installs[p.provider_id].error}
                    </pre>
                  </div>
                )}
                <ul className="sublist">
                  {installs[p.provider_id].events.slice(-6).map((ev, i) => (
                    <li key={i} className="muted">
                      {ev.stage}: {ev.message}
                    </li>
                  ))}
                </ul>
                {(() => {
                  if (installs[p.provider_id].error) return null;
                  const lastErr = [...installs[p.provider_id].events]
                    .reverse()
                    .find((e) => e.level === "error");
                  if (!lastErr) return null;
                  const text = `[${lastErr.at}] ${p.provider_id} ${lastErr.stage}: ${lastErr.message}`;
                  return (
                    <div className="row">
                      <button type="button" onClick={() => void copyTextToClipboard(text)}>
                        Copy error
                      </button>
                    </div>
                  );
                })()}
              </div>
            )}

            {installControlsEnabled ? (
              <div className="row">
                <button
                  type="button"
                  onClick={() => onInstall(p.provider_id)}
                  disabled={installDisabled}
                  title={installSupported ? "Install this provider" : "Install not supported yet"}
                >
                  {busy === p.provider_id || installRunning
                    ? "Installing…"
                    : p.installed && p.health === "unsupported_version"
                      ? "Update"
                      : p.installed
                        ? "Reinstall"
                        : "Install"}
                </button>
              </div>
            ) : null}
          </li>
          );
        })}
        {providers.length === 0 && <li className="muted">No providers.</li>}
      </ul>
    </div>
  );
}
