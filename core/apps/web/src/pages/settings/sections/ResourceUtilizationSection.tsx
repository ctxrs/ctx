import { Fragment } from "react";
import { type ResourceUtilization, type Workspace, idToString } from "../../../api/client";
import { Card, Metric } from "../SettingsPage.components";
import { formatAge, formatBytes, formatPct, truncateText } from "../SettingsPage.utils";

export function ResourceUtilizationSection({
  workspaceId,
  workspaces,
  resourceSnapshot,
  resourceLoading,
  resourceError,
  expandedProcessPids,
  onToggleExpanded,
}: {
  workspaceId: string | null;
  workspaces: Workspace[];
  resourceSnapshot: ResourceUtilization | null;
  resourceLoading: boolean;
  resourceError: string | null;
  expandedProcessPids: Record<number, boolean>;
  onToggleExpanded: (pid: number) => void;
}) {
  if (!workspaceId) {
    return <div className="settings-empty">No workspace selected.</div>;
  }

  const snapshot = resourceSnapshot;
  const system = snapshot?.system;
  const disk = snapshot?.workspace?.disk;
  const workspaceName = workspaces.find((workspace) => idToString(workspace.id) === workspaceId)?.name ?? "Workspace";

  const memoryPct =
    system && system.memory_total_bytes > 0 ? (system.memory_used_bytes / system.memory_total_bytes) * 100 : null;
  const swapPct =
    system && system.swap_total_bytes > 0 ? (system.swap_used_bytes / system.swap_total_bytes) * 100 : null;
  const diskPct =
    disk && disk.total_bytes > 0 ? ((disk.total_bytes - disk.available_bytes) / disk.total_bytes) * 100 : null;

  const overviewUpdated =
    snapshot && Number.isFinite(snapshot.cache_age_ms) ? `Updated ${formatAge(snapshot.cache_age_ms)} ago` : "Awaiting resource data…";
  const diskUpdated =
    snapshot && Number.isFinite(snapshot.workspace.size_cache_age_ms)
      ? `Disk scan ${formatAge(snapshot.workspace.size_cache_age_ms)} ago`
      : "Disk scan pending…";

  const processRows = (() => {
    if (!snapshot?.processes) return [];
    const rows = [];
    if (snapshot.processes.daemon) rows.push(snapshot.processes.daemon);
    rows.push(...[...snapshot.processes.providers].sort((a, b) => a.label.localeCompare(b.label)));
    return rows;
  })();

  const worktreeRows = snapshot?.workspace.worktrees ?? [];

  return (
    <>
      <Card title="Overview">
        <div className="settings-card-block">
          <div className="settings-metrics-grid">
            <Metric label="CPU" value={formatPct(system?.cpu_pct)} sublabel="System CPU usage" pct={system?.cpu_pct ?? null} />
            <Metric
              label="Memory"
              value={system ? `${formatBytes(system.memory_used_bytes)} / ${formatBytes(system.memory_total_bytes)}` : "—"}
              sublabel="Physical memory"
              pct={memoryPct}
            />
            <Metric
              label="Swap"
              value={system ? `${formatBytes(system.swap_used_bytes)} / ${formatBytes(system.swap_total_bytes)}` : "—"}
              sublabel="Swap usage"
              pct={swapPct}
            />
            <Metric
              label="Disk"
              value={disk ? `${formatBytes(disk.available_bytes)} free / ${formatBytes(disk.total_bytes)}` : "—"}
              sublabel={disk ? `${disk.mount_point} · ${disk.file_system}` : "Workspace volume"}
              pct={diskPct}
            />
          </div>
          <div className="settings-meta-line">{resourceLoading ? "Refreshing…" : overviewUpdated}</div>
        </div>
      </Card>

      <Card title="Processes">
        <div className="settings-card-block">
          {processRows.length === 0 ? (
            <div className="settings-empty">No process metrics yet.</div>
          ) : (
            <div className="settings-table settings-table-processes">
              <div className="settings-table-head">
                <div>Process</div>
                <div>CPU</div>
                <div>Memory</div>
                <div>PID</div>
              </div>
              {processRows.map((process) => {
                const expanded = !!expandedProcessPids[process.pid];
                const hasChildren = (process.children?.length ?? 0) > 0 || process.child_count > 0;
                return (
                  <Fragment key={`${process.label}-${process.pid}`}>
                    <div className="settings-table-row">
                      <div className="settings-process-cell">
                        <button
                          type="button"
                          className="settings-process-expand"
                          onClick={() => onToggleExpanded(process.pid)}
                          disabled={!hasChildren}
                          aria-label={expanded ? "Collapse process children" : "Expand process children"}
                          aria-expanded={expanded}
                        >
                          {hasChildren ? (expanded ? "▾" : "▸") : "·"}
                        </button>
                        <div>
                          <div className="settings-table-title">{process.label}</div>
                          <div className="settings-table-sub">
                            {process.child_count} child process{process.child_count === 1 ? "" : "es"}
                            {process.children_truncated ? " (truncated)" : ""}
                          </div>
                        </div>
                      </div>
                      <div>{formatPct(process.cpu_pct)}</div>
                      <div>{formatBytes(process.memory_bytes)}</div>
                      <div className="settings-table-mono">{process.pid}</div>
                    </div>
                    {expanded ? (
                      <div className="settings-process-children">
                        {process.child_count === 0 ? (
                          <div className="settings-empty settings-empty-compact">No child processes.</div>
                        ) : (
                          <>
                            <div className="settings-process-children-meta">
                              {process.children_truncated
                                ? `Showing ${process.children.length} of ${process.child_count} descendants (sorted by memory)`
                                : `${process.children.length} descendants`}
                            </div>
                            <div className="settings-table settings-table-process-children">
                              <div className="settings-table-head">
                                <div>Child process</div>
                                <div>CPU</div>
                                <div>Memory</div>
                                <div>PID</div>
                              </div>
                              {process.children.map((child) => (
                                <div key={`${process.pid}-${child.pid}`} className="settings-table-row">
                                  <div>
                                    <div className="settings-table-title">{child.name}</div>
                                    <div className="settings-table-sub">
                                      {child.cmdline
                                        ? `ppid ${child.parent_pid ?? "—"} · ${truncateText(child.cmdline, 120)}`
                                        : `ppid ${child.parent_pid ?? "—"}`}
                                    </div>
                                  </div>
                                  <div>{formatPct(child.cpu_pct)}</div>
                                  <div>{formatBytes(child.memory_bytes)}</div>
                                  <div className="settings-table-mono">{child.pid}</div>
                                </div>
                              ))}
                            </div>
                          </>
                        )}
                      </div>
                    ) : null}
                  </Fragment>
                );
              })}
            </div>
          )}
        </div>
      </Card>

      <Card title="Workspace Disk">
        <div className="settings-card-block">
          <div className="settings-workspace-header">
            <div className="settings-workspace-title">{workspaceName}</div>
            <div className="settings-workspace-path">{snapshot?.workspace.root_path ?? "—"}</div>
            <div className="settings-workspace-meta">
              {snapshot ? `${formatBytes(snapshot.workspace.size_bytes)} total · ${diskUpdated}` : "Sizing…"}
            </div>
          </div>

          {worktreeRows.length === 0 ? (
            <div className="settings-empty">No worktrees found.</div>
          ) : (
            <div className="settings-table settings-table-worktrees">
              <div className="settings-table-head">
                <div>Worktree</div>
                <div>Size</div>
              </div>
              {worktreeRows.map((worktree) => (
                <div key={worktree.worktree_id} className="settings-table-row">
                  <div>
                    <div className="settings-table-title">{worktree.worktree_id}</div>
                    <div className="settings-table-sub">{worktree.root_path}</div>
                  </div>
                  <div>{formatBytes(worktree.size_bytes)}</div>
                </div>
              ))}
            </div>
          )}
        </div>
      </Card>

      {resourceError ? <div className="settings-banner settings-banner-error">{resourceError}</div> : null}
    </>
  );
}
