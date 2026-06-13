import { useCallback, useMemo, useRef, useState } from "react";
import type { WorktreeBootstrapNotice, WorkspaceActiveSnapshotEvent } from "@ctx/types";
import { getWorktreeBootstrapLogs, idToString } from "../api/client";
import { useWorkspaceActiveSnapshotEvents } from "../state/workspaceActiveSnapshotStore";
import { desktopSaveTextFile, isDesktopApp } from "../utils/desktop";
import { errorMessage } from "../utils/errorMessage";

const buildNoticeKey = (notice: WorktreeBootstrapNotice): string =>
  `${idToString(notice.worktree_id)}:${notice.finished_at}`;

const labelForNotice = (notice: WorktreeBootstrapNotice): string => {
  return notice.command || notice.script_path || "worktree bootstrap";
};

const messageForNotice = (notice: WorktreeBootstrapNotice): string => {
  const label = labelForNotice(notice);
  if (notice.status === "timeout") {
    const seconds = notice.timeout_sec ?? 60;
    return `Worktree bootstrap timed out after ${seconds}s: ${label}`;
  }
  if (notice.status === "failed") {
    if (typeof notice.exit_code === "number") {
      return `Worktree bootstrap failed (exit ${notice.exit_code}): ${label}`;
    }
    return `Worktree bootstrap failed: ${label}`;
  }
  return `Worktree bootstrap finished: ${label}`;
};

const saveTextFile = async (name: string, contents: string) => {
  if (isDesktopApp()) {
    await desktopSaveTextFile({ suggested_name: name, contents });
    return;
  }
  const blob = new Blob([contents], { type: "text/plain" });
  const url = URL.createObjectURL(blob);
  try {
    const a = document.createElement("a");
    a.href = url;
    a.download = name;
    a.rel = "noopener";
    a.click();
  } finally {
    window.setTimeout(() => URL.revokeObjectURL(url), 1000);
  }
};

export function WorktreeBootstrapSnackbar() {
  const [notice, setNotice] = useState<WorktreeBootstrapNotice | null>(null);
  const [downloadError, setDownloadError] = useState<string | null>(null);
  const [downloading, setDownloading] = useState(false);
  const lastKeyRef = useRef<string | null>(null);

  useWorkspaceActiveSnapshotEvents((evt: WorkspaceActiveSnapshotEvent) => {
    if (evt.type !== "worktree_bootstrap") return;
    const next = evt.notice;
    const key = buildNoticeKey(next);
    if (key === lastKeyRef.current) return;
    lastKeyRef.current = key;
    setNotice(next);
    setDownloadError(null);
  });

  const message = useMemo(() => (notice ? messageForNotice(notice) : ""), [notice]);
  const subtitle = notice?.worktree_root ?? "";

  const onDismiss = useCallback(() => {
    setNotice(null);
    setDownloadError(null);
  }, []);

  const onDownload = useCallback(async () => {
    if (!notice || downloading) return;
    setDownloading(true);
    setDownloadError(null);
    try {
      const worktreeId = idToString(notice.worktree_id);
      if (!worktreeId) throw new Error("Missing worktree id.");
      const contents = await getWorktreeBootstrapLogs(worktreeId);
      const name = `worktree-bootstrap-${worktreeId}.log`;
      await saveTextFile(name, contents);
    } catch (err: unknown) {
      setDownloadError(errorMessage(err) || "Failed to download logs.");
    } finally {
      setDownloading(false);
    }
  }, [notice, downloading]);

  const onOpenSettings = useCallback(() => {
    window.location.assign("/settings#worktree_bootstrap");
  }, []);

  if (!notice) return null;

  return (
    <div className="wb-snackbar" role="status" aria-live="polite">
      <div className="wb-snackbar-body">
        <div className="wb-snackbar-title">{message}</div>
        {subtitle ? <div className="wb-snackbar-subtitle">{subtitle}</div> : null}
        {downloadError ? <div className="wb-snackbar-error">{downloadError}</div> : null}
      </div>
      <div className="wb-snackbar-actions">
        <button type="button" className="wb-snackbar-btn" onClick={onDownload} disabled={downloading}>
          {downloading ? "Downloading..." : "Download Logs"}
        </button>
        <button type="button" className="wb-snackbar-btn wb-snackbar-btn-secondary" onClick={onOpenSettings}>
          Open in Settings
        </button>
      </div>
      <button type="button" className="wb-snackbar-close" onClick={onDismiss} aria-label="Dismiss">
        X
      </button>
    </div>
  );
}
