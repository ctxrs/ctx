import type { MergeQueueEntry } from "@ctx/types";
import { idToString } from "../../../api/client";
import { Card, Row } from "../SettingsPage.components";
import { formatAge, truncateText } from "../SettingsPage.utils";

type MergeQueueLivePanelOrphanedProps = {
  workspaceId: string | null;
  entries: MergeQueueEntry[];
  loading: boolean;
  error: string | null;
  actionBusyById: Record<string, boolean>;
  logBusyById: Record<string, boolean>;
  onRefresh: () => Promise<void>;
  onRetry: (entryId: string) => Promise<void>;
  onCancel: (entryId: string) => Promise<void>;
  onLogs: (entryId: string) => Promise<void>;
};

// Orphaned on purpose: preserved live queue operations UI extracted from SettingsPage.
export function MergeQueueLivePanelOrphaned(props: MergeQueueLivePanelOrphanedProps) {
  const {
    workspaceId,
    entries,
    loading,
    error,
    actionBusyById,
    logBusyById,
    onRefresh,
    onRetry,
    onCancel,
    onLogs,
  } = props;

  return (
    <>
      <Card title="Merge Queue">
        <Row
          title="Actions"
          control={
            <button
              type="button"
              className="settings-btn"
              onClick={() => onRefresh().catch(() => {})}
              disabled={!workspaceId || loading}
            >
              {loading ? "Refreshing…" : "Refresh"}
            </button>
          }
        />
      </Card>

      <Card title="Queue Entries">
        <div className="settings-card-block">
          {loading ? <div className="settings-empty-compact">Loading merge queue…</div> : null}
          {!loading && entries.length === 0 ? (
            <div className="settings-empty-compact">No merge queue entries.</div>
          ) : null}
          {!loading && entries.length > 0 ? (
            <div className="settings-table">
              <div className="settings-table-head">
                <div>Entry</div>
                <div>Status</div>
                <div>Target</div>
                <div>Updated</div>
                <div />
              </div>
              {entries.map((entry) => {
                const entryId = idToString(entry.id);
                const updatedMs = Date.parse(entry.updated_at);
                const updatedLabel = Number.isFinite(updatedMs)
                  ? `${formatAge(Date.now() - updatedMs)} ago`
                  : "—";
                const actionBusy = actionBusyById[entryId] ?? false;
                const logBusy = logBusyById[entryId] ?? false;
                const canCancel = entry.status === "queued";
                const canRetry = entry.status === "failed" || entry.status === "conflict";
                const subtitle = entry.error_message
                  ? truncateText(entry.error_message, 64)
                  : entry.result_commit_sha
                    ? `commit ${entry.result_commit_sha.slice(0, 8)}`
                    : entryId;
                return (
                  <div key={entryId} className="settings-table-row">
                    <div>
                      <div className="settings-table-title">
                        {entry.message?.trim() ? truncateText(entry.message, 48) : "Merge queue entry"}
                      </div>
                      <div className="settings-table-sub">{subtitle}</div>
                    </div>
                    <div className="settings-table-sub">{entry.status}</div>
                    <div className="settings-table-mono">{entry.target_branch}</div>
                    <div className="settings-table-sub">{updatedLabel}</div>
                    <div className="settings-row-right">
                      <button
                        type="button"
                        className="settings-btn settings-btn-secondary settings-btn-compact"
                        onClick={() => onRetry(entryId).catch(() => {})}
                        disabled={!canRetry || actionBusy}
                      >
                        {actionBusy && canRetry ? "Retrying…" : "Retry"}
                      </button>
                      <button
                        type="button"
                        className="settings-btn settings-btn-secondary settings-btn-compact"
                        onClick={() => onCancel(entryId).catch(() => {})}
                        disabled={!canCancel || actionBusy}
                      >
                        {actionBusy && canCancel ? "Cancelling…" : "Cancel"}
                      </button>
                      <button
                        type="button"
                        className="settings-btn settings-btn-secondary settings-btn-compact"
                        onClick={() => onLogs(entryId).catch(() => {})}
                        disabled={logBusy}
                      >
                        {logBusy ? "Downloading…" : "Logs"}
                      </button>
                    </div>
                  </div>
                );
              })}
            </div>
          ) : null}
        </div>
      </Card>
      {error ? <div className="settings-banner settings-banner-error">{error}</div> : null}
    </>
  );
}
