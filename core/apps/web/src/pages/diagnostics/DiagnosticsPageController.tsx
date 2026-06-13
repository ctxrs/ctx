import { useCallback, useEffect, useMemo, useState } from "react";
import { Textarea } from "../../components/ui/text-input";
import { Link, useLocation } from "react-router-dom";
import {
  applyDaemonDesktopConnection,
  appendDesktopLog,
  ApplyAppImageUpdateResp,
  checkUpdates,
  Diagnostics,
  DownloadAppImageUpdateResp,
  getDiagnostics,
  UpdateCheck,
  applyAppImageUpdate,
  downloadAppImageUpdate,
  openLogsFolder,
} from "../../api/client";
import { copyTextToClipboard } from "../../utils/clipboard";
import {
  desktopGetConnection,
  getDesktopPlatform,
  openExternalLink,
  desktopRestartLocalDaemon,
  desktopApplyAppUpdate,
  desktopCheckAppUpdate,
  desktopGetLastAppUpdateAttempt,
  desktopUpdateRemoteDaemon,
  isDesktopApp,
  type DesktopAppUpdateCheckResp,
  type DesktopAppUpdateAttemptResp,
  type DesktopPlatform,
  type DesktopConnectionInfo,
} from "../../utils/desktop";
import { errorMessage } from "../../utils/errorMessage";
import {
  appendDownloadAttributionIdToUrl,
  clearPendingDownloadAttributionId,
  createDownloadAttributionId,
  setPendingDownloadAttributionId,
} from "../../utils/analytics";
import {
  joinBaseAndPath,
  preferredDesktopArtifactFromManifest,
} from "../../utils/diagnosticsReleaseArtifacts";

export default function DiagnosticsPage() {
  const location = useLocation();
  const [diagnostics, setDiagnostics] = useState<Diagnostics | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [updateInfo, setUpdateInfo] = useState<UpdateCheck | null>(null);
  const [updateBusy, setUpdateBusy] = useState(false);
  const [desktopAppUpdateBusy, setDesktopAppUpdateBusy] = useState(false);
  const [daemonUpdateBusy, setDaemonUpdateBusy] = useState(false);
  const [downloadResp, setDownloadResp] = useState<DownloadAppImageUpdateResp | null>(null);
  const [applyResp, setApplyResp] = useState<ApplyAppImageUpdateResp | null>(null);
  const [desktopAppUpdateInfo, setDesktopAppUpdateInfo] = useState<DesktopAppUpdateCheckResp | null>(null);
  const [desktopLastUpdateAttempt, setDesktopLastUpdateAttempt] = useState<DesktopAppUpdateAttemptResp | null>(null);
  const [desktopConnection, setDesktopConnection] = useState<DesktopConnectionInfo | null>(null);
  const [desktopPlatform, setDesktopPlatform] = useState<DesktopPlatform>("unknown");
  const desktop = isDesktopApp();

  const refresh = () => {
    setError(null);
    return getDiagnostics()
      .then((d) => {
        setDiagnostics(d);
        setNotice(null);
      })
      .catch((e) => setError(e.message));
  };

  useEffect(() => {
    appendDesktopLog("ui: opened Diagnostics page").catch(() => {});
    refresh();
    if (desktop) {
      desktopGetConnection()
        .then((info) => {
          setDesktopConnection(info);
          applyDaemonDesktopConnection(info);
        })
        .catch(() => setDesktopConnection({ kind: "none" }));
      getDesktopPlatform().then((platform) => setDesktopPlatform(platform));
    }
  }, [desktop]);

  const pretty = useMemo(
    () => (diagnostics ? JSON.stringify(diagnostics, null, 2) : ""),
    [diagnostics],
  );

  const onCopy = async () => {
    if (!diagnostics) return;
    const ok = await copyTextToClipboard(pretty);
    if (ok) {
      setNotice("Copied diagnostics JSON to clipboard.");
    } else {
      setNotice("Copy failed. Clipboard access may be blocked; copy manually or use HTTPS.");
    }
  };

  const onOpenLogs = async () => {
    setError(null);
    setNotice(null);
    try {
      await openLogsFolder();
      appendDesktopLog("ui: requested open logs folder").catch(() => {});
    } catch (e: unknown) {
      setError(errorMessage(e));
    }
  };

  const onCheckUpdates = useCallback(async () => {
    setError(null);
    setNotice(null);
    setUpdateBusy(true);
    setDownloadResp(null);
    setApplyResp(null);
    try {
      const info = await checkUpdates();
      setUpdateInfo(info);
      if (info.platform_supported === false) {
        setNotice("Update metadata found, but this platform currently has no desktop artifact.");
      } else if (info.update_available) {
        setNotice(`Update available: ${info.latest_version}`);
      } else {
        setNotice("No update available.");
      }
      const nextIsLinux = (info.platform ?? "").startsWith("linux-") || desktopPlatform === "linux";
      if (desktop && !nextIsLinux) {
        try {
          const [nativeInfo, attempt] = await Promise.all([
            desktopCheckAppUpdate(),
            desktopGetLastAppUpdateAttempt().catch(() => null),
          ]);
          setDesktopAppUpdateInfo(nativeInfo);
          setDesktopLastUpdateAttempt(attempt);
        } catch {
          setDesktopAppUpdateInfo(null);
          setDesktopLastUpdateAttempt(null);
        }
      }
    } catch (e: unknown) {
      setError(errorMessage(e));
    } finally {
      setUpdateBusy(false);
    }
  }, [desktop, desktopPlatform]);

  useEffect(() => {
    const shouldAutoCheck = new URLSearchParams(location.search).get("check_updates") === "1";
    if (!shouldAutoCheck) return;
    void onCheckUpdates();
  }, [location.search, onCheckUpdates]);

  const onDownloadUpdate = async () => {
    setError(null);
    setNotice(null);
    setUpdateBusy(true);
    setApplyResp(null);
    try {
      const resp = await downloadAppImageUpdate();
      setDownloadResp(resp);
      setNotice(`Downloaded update to ${resp.downloaded_path}`);
    } catch (e: unknown) {
      setError(errorMessage(e));
    } finally {
      setUpdateBusy(false);
    }
  };

  const onApplyUpdate = async () => {
    setError(null);
    setNotice(null);
    setUpdateBusy(true);
    try {
      const resp = await applyAppImageUpdate();
      setApplyResp(resp);
      setNotice(resp.message);
    } catch (e: unknown) {
      setError(errorMessage(e));
    } finally {
      setUpdateBusy(false);
    }
  };

  const onApplyDesktopAppUpdate = async () => {
    setError(null);
    setNotice(null);
    setDesktopAppUpdateBusy(true);
    const downloadId = createDownloadAttributionId();
    try {
      await setPendingDownloadAttributionId(downloadId);
      const resp = await desktopApplyAppUpdate({ downloadId });
      if (!resp.applied) {
        await clearPendingDownloadAttributionId();
      }
      setNotice(resp.message);
      try {
        const [nativeInfo, attempt] = await Promise.all([
          desktopCheckAppUpdate(),
          desktopGetLastAppUpdateAttempt().catch(() => null),
        ]);
        setDesktopAppUpdateInfo(nativeInfo);
        setDesktopLastUpdateAttempt(attempt);
      } catch {
        // ignore re-check failures
      }
    } catch (e: unknown) {
      await clearPendingDownloadAttributionId();
      setError(errorMessage(e));
    } finally {
      setDesktopAppUpdateBusy(false);
    }
  };

  const desktopArtifact = useMemo(
    () => preferredDesktopArtifactFromManifest(updateInfo?.manifest, updateInfo?.platform),
    [updateInfo?.manifest, updateInfo?.platform],
  );

  const desktopArtifactUrl = useMemo(() => {
    if (!updateInfo?.base_url) return null;
    const urlPath = String(desktopArtifact?.url_path ?? "").trim();
    if (!urlPath) return null;
    return joinBaseAndPath(updateInfo.base_url, urlPath);
  }, [desktopArtifact?.url_path, updateInfo?.base_url]);

  const onOpenDesktopDownload = async () => {
    if (!desktopArtifactUrl) return;
    setError(null);
    setNotice(null);
    const downloadId = createDownloadAttributionId();
    const attributedUrl = appendDownloadAttributionIdToUrl(desktopArtifactUrl, downloadId);
    if (desktop) {
      await setPendingDownloadAttributionId(downloadId);
    }
    const opened = await openExternalLink(attributedUrl);
    if (opened) {
      setNotice("Opened desktop update download.");
    } else {
      if (desktop) {
        await clearPendingDownloadAttributionId();
      }
      setError("Unable to open desktop update URL.");
    }
  };

  const onUpdateConnectedDaemon = async () => {
    if (!desktop) return;
    const kind = desktopConnection?.kind ?? "none";
    const label = kind === "ssh" ? "update and restart the remote daemon" : "restart the local daemon";
    if (!window.confirm(`This will ${label} and may interrupt active agent activity. Continue?`)) {
      return;
    }
    setError(null);
    setNotice(null);
    setDaemonUpdateBusy(true);
    try {
      if (kind === "ssh") {
        const resp = await desktopUpdateRemoteDaemon();
        setNotice(resp.message);
      } else {
        await desktopRestartLocalDaemon();
        setNotice("Local daemon restarted with the app-managed binary.");
      }
      const info = await desktopGetConnection().catch(() => null);
      if (info) {
        setDesktopConnection(info);
        applyDaemonDesktopConnection(info);
      }
    } catch (e: unknown) {
      setError(errorMessage(e));
    } finally {
      setDaemonUpdateBusy(false);
    }
  };

  const isLinuxPlatform = (updateInfo?.platform ?? "").startsWith("linux-") || desktopPlatform === "linux";
  const useNativeDesktopUpdater =
    desktop &&
    !isLinuxPlatform &&
    desktopAppUpdateInfo?.configured === true &&
    (desktopAppUpdateInfo.available || !updateInfo?.update_available);

  return (
    <div className="page">
      <div className="row">
        <h1 style={{ marginRight: "auto" }}>Diagnostics</h1>
        <Link to="/">Launcher</Link>
      </div>

      <div className="row" style={{ gap: 8, flexWrap: "wrap" }}>
        <button onClick={() => refresh()}>Refresh</button>
        <button onClick={onCopy} disabled={!diagnostics}>
          Copy diagnostics
        </button>
        <button onClick={onOpenLogs} disabled={!diagnostics}>
          Open logs folder
        </button>
      </div>

      {notice && <div className="banner">{notice}</div>}
      {error && <div className="error">{error}</div>}

      <div className="card">
        <h2 style={{ marginTop: 0 }}>Updates</h2>
        <div className="muted">
          Uses <code>CTX_DOWNLOAD_BASE_URL</code> to fetch{" "}
          <code>/releases/&lt;channel&gt;/latest.json</code>.
        </div>
        <div className="row" style={{ gap: 8, flexWrap: "wrap", marginTop: 8 }}>
          <button onClick={onCheckUpdates} disabled={updateBusy}>
            {updateBusy ? "Checking…" : "Check updates"}
          </button>
          {desktop && (
            <button onClick={onUpdateConnectedDaemon} disabled={daemonUpdateBusy}>
              {daemonUpdateBusy
                ? "Applying daemon update..."
                : desktopConnection?.kind === "ssh"
                  ? "Update connected remote daemon"
                  : "Restart local daemon"}
            </button>
          )}
          {isLinuxPlatform && (
            <>
              <button
                onClick={onDownloadUpdate}
                disabled={updateBusy || !updateInfo?.update_available}
              >
                Download AppImage update
              </button>
              <button
                onClick={onApplyUpdate}
                disabled={updateBusy || !downloadResp?.can_apply_in_place}
              >
                Apply in place
              </button>
            </>
          )}
          {!isLinuxPlatform && (
            <>
              {useNativeDesktopUpdater ? (
                <button
                  onClick={onApplyDesktopAppUpdate}
                  disabled={desktopAppUpdateBusy || !desktopAppUpdateInfo?.available}
                >
                  {desktopAppUpdateBusy ? "Installing desktop update..." : "Install desktop update"}
                </button>
              ) : (
                <button
                  onClick={onOpenDesktopDownload}
                  disabled={updateBusy || !updateInfo?.update_available || !desktopArtifactUrl}
                >
                  Open latest desktop download
                </button>
              )}
            </>
          )}
        </div>
        <div className="muted" style={{ marginTop: 8 }}>
          <div>
            <b>Current:</b>{" "}
            <span className="muted">{updateInfo?.current_version ?? "Unknown"}</span>
          </div>
          <div>
            <b>Latest:</b>{" "}
            <span className="muted">{updateInfo?.latest_version ?? "Unknown"}</span>
          </div>
          <div>
            <b>Platform:</b>{" "}
            <span className="muted">{updateInfo?.platform ?? desktopPlatform}</span>
          </div>
          {updateInfo?.platform_supported === false && (
            <div className="muted">
              No desktop artifact is published for this platform in the current release manifest.
            </div>
          )}
          {!isLinuxPlatform && desktopArtifactUrl && (
            <div>
              <b>Download URL:</b> <span className="muted">{desktopArtifactUrl}</span>
            </div>
          )}
          {!isLinuxPlatform && desktopAppUpdateInfo?.configured === false && desktopAppUpdateInfo?.message && (
            <div className="muted">{desktopAppUpdateInfo.message}</div>
          )}
          {!isLinuxPlatform && desktopAppUpdateInfo?.configured && (
            <div>
              <b>Native updater:</b>{" "}
              <span className="muted">
                {desktopAppUpdateInfo.available
                  ? `Update available (${desktopAppUpdateInfo.latest_version ?? "unknown"})`
                  : "No update available"}
              </span>
            </div>
          )}
          {!isLinuxPlatform && desktopAppUpdateInfo?.configured && (
            <div>
              <b>Native updater phase:</b>{" "}
              <span className="muted">
                {String(desktopAppUpdateInfo.phase ?? "unknown")}
                {desktopAppUpdateInfo.last_error ? ` · ${desktopAppUpdateInfo.last_error}` : ""}
              </span>
            </div>
          )}
          {desktopLastUpdateAttempt && (
            <div>
              <b>Last native attempt:</b>{" "}
              <span className="muted">
                {desktopLastUpdateAttempt.attempt_id} · {desktopLastUpdateAttempt.result}
                {desktopLastUpdateAttempt.target_version
                  ? ` · target ${desktopLastUpdateAttempt.target_version}`
                  : ""}
              </span>
            </div>
          )}
          {desktop && (
            <div>
              <b>Connected daemon:</b>{" "}
              <span className="muted">{desktopConnection?.kind ?? "unknown"}</span>
            </div>
          )}
          {downloadResp?.downloaded_path && (
            <div>
              <b>Downloaded:</b>{" "}
              <span className="muted">{downloadResp.downloaded_path}</span>
            </div>
          )}
          {applyResp?.target_path && (
            <div>
              <b>Applied to:</b>{" "}
              <span className="muted">{applyResp.target_path}</span>
            </div>
          )}
          {!downloadResp?.can_apply_in_place && downloadResp && (
            <div className="muted">
              Cannot apply in place (not running as AppImage / missing{" "}
              <code>CTX_APPIMAGE_PATH</code>). Downloaded file can be applied manually.
            </div>
          )}
        </div>
      </div>

      {diagnostics && (
        <div className="card">
          <div style={{ marginBottom: 8 }}>
            <div>
              <b>Daemon URL:</b>{" "}
              <span className="muted">{diagnostics.daemon.daemon_url}</span>
            </div>
            <div>
              <b>Data root:</b>{" "}
              <span className="muted">{diagnostics.daemon.data_root}</span>
            </div>
            <div>
              <b>Logs:</b> <span className="muted">{diagnostics.logs.dir}</span>
            </div>
          </div>

          <label>
            Diagnostics JSON
            <Textarea
              readOnly
              value={pretty}
              style={{ width: "100%", height: 340, fontFamily: "monospace" }}
            />
          </label>
        </div>
      )}

      {diagnostics?.logs?.files?.length ? (
        <div className="card">
          <h2 style={{ marginTop: 0 }}>Log files</h2>
          <ul className="list">
            {diagnostics.logs.files.map((f) => (
              <li key={f.name}>
                <div>{f.name}</div>
                <div className="muted">
                  {f.bytes} bytes
                  {f.modified_utc ? ` • modified ${f.modified_utc}` : ""}
                </div>
              </li>
            ))}
          </ul>
        </div>
      ) : null}
    </div>
  );
}
