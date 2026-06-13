import type { Workspace } from "@ctx/types";
import { RefreshCw, Settings } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import { idToString, listWorkspaces } from "../../api/client";
import { useDaemonConnection } from "../../api/useDaemonConnection";
import { errorMessage } from "../../utils/errorMessage";
import { MobileShellChrome } from "./MobileShellChrome";

export function MobileHomePage() {
  const navigate = useNavigate();
  const connection = useDaemonConnection();
  const [workspaces, setWorkspaces] = useState<Workspace[]>([]);
  const [busy, setBusy] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      setWorkspaces(await listWorkspaces());
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load, connection.authToken, connection.baseUrl]);

  return (
    <MobileShellChrome
      actions={
        <>
          <button type="button" className="mobile-shell-icon-btn" onClick={() => void load()} disabled={busy} aria-label="Refresh workspaces">
            <RefreshCw size={16} className={busy ? "mobile-shell-spin" : undefined} aria-hidden="true" />
          </button>
          <Link className="mobile-shell-icon-btn" to="/mobile/connect" aria-label="Connection settings">
            <Settings size={16} aria-hidden="true" />
          </Link>
        </>
      }
    >
      {error ? <div className="mobile-shell-error">{error}</div> : null}

      <div className="mobile-shell-list" aria-busy={busy}>
        {workspaces.map((workspace) => {
          const workspaceId = idToString(workspace.id);
          if (!workspaceId) return null;
          const title = workspace.name || workspaceId;
          const rootPath = workspace.root_path || "No root path";
          return (
            <button
              key={workspaceId}
              type="button"
              className="mobile-shell-list-item"
              onClick={() => navigate(`/workspaces/${encodeURIComponent(workspaceId)}`)}
            >
              <span className="mobile-shell-list-title">{title}</span>
              <span className="mobile-shell-list-subtitle">{rootPath}</span>
            </button>
          );
        })}

        {!busy && workspaces.length === 0 ? (
          <div className="mobile-shell-empty">
            No workspaces found on this daemon.
          </div>
        ) : null}

        {busy ? <div className="mobile-shell-empty">Loading workspaces...</div> : null}
      </div>
    </MobileShellChrome>
  );
}
