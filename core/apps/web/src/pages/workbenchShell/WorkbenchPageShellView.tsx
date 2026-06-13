import React, { useMemo } from "react";
import { X } from "lucide-react";

import { TerminalPanel } from "../../components/TerminalPanel";
import { TitleGenerationInstallBanner } from "../../components/TitleGenerationInstallBanner";
import { WorktreeBootstrapSnackbar } from "../../components/WorktreeBootstrapSnackbar";
import { WorkbenchActiveTaskView } from "./WorkbenchActiveTaskView";
import { WorkbenchEmptyState } from "./WorkbenchEmptyState";
import { WorkbenchSidebar, WorkbenchTopbar } from "./WorkbenchShellChrome";
import { WorkbenchPageMenus } from "./WorkbenchPageMenus";
import { WorkbenchProviderWarningBanner } from "./WorkbenchProviderWarningBanner";
import {
  getWorkbenchRootStyleVars,
  type WorkbenchRootStyleVars,
} from "./workbenchLayoutVars";

type RootStyle = React.CSSProperties & WorkbenchRootStyleVars;

type WorkbenchPageShellViewProps = {
  workspaceId: string;
  activeTaskId: string | null;
  activeSessionId: string | null;
  sidebarCollapsed: boolean;
  sidebarResizing: boolean;
  sidebarWidth: number;
  mobileShell: boolean;
  desktopUi: boolean;
  useHtmlTopbar: boolean;
  desktopStorageNoticeReason: string | null;
  onDismissDesktopStorageNotice: () => void;
  composerHarnessAuthModal: React.ReactNode;
  workspaceBootstrapGateState: "loading" | "error" | "ready";
  providerBootstrapError: string | null;
  onRefreshBootstrap: () => void;
  workbenchWarnings: string[];
  activeTaskController: React.ComponentProps<typeof WorkbenchPageMenus>["activeTaskController"];
  taskListController: React.ComponentProps<typeof WorkbenchPageMenus>["taskListController"];
  topbarProps: React.ComponentProps<typeof WorkbenchTopbar>;
  providerWarningProps: React.ComponentProps<typeof WorkbenchProviderWarningBanner>;
  sidebarProps: React.ComponentProps<typeof WorkbenchSidebar>;
  emptyStateProps: React.ComponentProps<typeof WorkbenchEmptyState>;
  activeTaskViewProps: React.ComponentProps<typeof WorkbenchActiveTaskView> | null;
};

export function WorkbenchPageShellView({
  workspaceId,
  activeTaskId,
  activeSessionId,
  sidebarCollapsed,
  sidebarResizing,
  sidebarWidth,
  mobileShell,
  desktopUi,
  useHtmlTopbar,
  desktopStorageNoticeReason,
  onDismissDesktopStorageNotice,
  composerHarnessAuthModal,
  workspaceBootstrapGateState,
  providerBootstrapError,
  onRefreshBootstrap,
  workbenchWarnings,
  activeTaskController,
  taskListController,
  topbarProps,
  providerWarningProps,
  sidebarProps,
  emptyStateProps,
  activeTaskViewProps,
}: WorkbenchPageShellViewProps) {
  const rootStyle = useMemo<RootStyle>(() => {
    return getWorkbenchRootStyleVars({
      mobileShell,
      sidebarWidth,
      terminalHeight: activeTaskController.terminalHeight,
      terminalOpen: activeTaskController.terminalOpen,
      useHtmlTopbar,
      viewportWidth: window.innerWidth,
    });
  }, [
    activeTaskController.terminalHeight,
    activeTaskController.terminalOpen,
    mobileShell,
    sidebarWidth,
    useHtmlTopbar,
  ]);

  const archiveCleanupSnackbar = taskListController.archiveCleanupNotice ? (
    <div className="wb-snackbar" role="status" aria-live="polite">
      <div className="wb-snackbar-body">
        <div className="wb-snackbar-title">Archived task, but some cleanup failed.</div>
        <div className="wb-snackbar-subtitle">
          Some worktree files were likely root-owned and could not be removed. Fix permissions and delete them manually if
          needed.
        </div>
      </div>
      <button
        type="button"
        className="wb-snackbar-close"
        onClick={taskListController.dismissArchiveCleanupNotice}
        aria-label="Dismiss"
      >
        <X size={14} aria-hidden="true" />
      </button>
    </div>
  ) : null;

  const transcriptNoticeSnackbar = activeTaskController.transcriptNotice ? (
    <div className="wb-snackbar" role="status" aria-live="polite">
      <div className="wb-snackbar-body">
        <div className="wb-snackbar-title">{activeTaskController.transcriptNotice}</div>
      </div>
      <button
        type="button"
        className="wb-snackbar-close"
        onClick={activeTaskController.dismissTranscriptNotice}
        aria-label="Dismiss"
      >
        <X size={14} aria-hidden="true" />
      </button>
    </div>
  ) : null;

  const desktopStorageNoticeSubtitle =
    desktopStorageNoticeReason === "schema_mismatch"
      ? "Desktop detected an outdated local UI state format and reset local UI state."
      : "Desktop detected invalid local UI state data and reset local UI state.";
  const desktopStorageNoticeSnackbar = desktopStorageNoticeReason ? (
    <div className="wb-snackbar" role="status" aria-live="polite">
      <div className="wb-snackbar-body">
        <div className="wb-snackbar-title">Local UI state was reset.</div>
        <div className="wb-snackbar-subtitle">{desktopStorageNoticeSubtitle}</div>
      </div>
      <button
        type="button"
        className="wb-snackbar-close"
        onClick={onDismissDesktopStorageNotice}
        aria-label="Dismiss"
      >
        <X size={14} aria-hidden="true" />
      </button>
    </div>
  ) : null;

  const topbar = useHtmlTopbar ? (
    <div data-tauri-drag-region={desktopUi ? true : undefined}>
      <WorkbenchTopbar {...topbarProps} />
    </div>
  ) : null;

  const rootClassName = `wb-root ${mobileShell ? "wb-root-mobile" : ""} ${sidebarCollapsed ? "wb-root-collapsed" : ""} ${sidebarResizing ? "wb-root-resizing" : ""} ${activeTaskController.diffResizing ? "wb-root-diff-resizing" : ""} ${activeTaskController.terminalResizing ? "wb-root-terminal-resizing" : ""} ${!useHtmlTopbar ? "wb-root-native-titlebar" : ""}`;
  const sharedChrome = (
    <>
      <WorktreeBootstrapSnackbar />
      {composerHarnessAuthModal}
      {archiveCleanupSnackbar}
      {transcriptNoticeSnackbar}
      {desktopStorageNoticeSnackbar}
      {topbar}
    </>
  );

  if (workspaceBootstrapGateState === "loading") {
    return (
      <div className={rootClassName} style={rootStyle}>
        {sharedChrome}
        <div className="wb-main">
          <div className="wb-center">
            <div className="wb-muted" style={{ padding: 16 }}>
              Loading workspace...
            </div>
          </div>
        </div>
      </div>
    );
  }

  if (workspaceBootstrapGateState === "error") {
    return (
      <div className={rootClassName} style={rootStyle}>
        {sharedChrome}
        <div className="wb-main">
          <div className="wb-center">
            <div style={{ maxWidth: 480, padding: 16 }}>
              <div>Failed to load workspace.</div>
              {providerBootstrapError ? (
                <div className="wb-muted" style={{ paddingTop: 8 }}>
                  {providerBootstrapError}
                </div>
              ) : null}
              <button
                style={{ marginTop: 12 }}
                onClick={onRefreshBootstrap}
                type="button"
              >
                Retry workspace load
              </button>
            </div>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className={rootClassName} style={rootStyle}>
      <WorktreeBootstrapSnackbar />
      <TitleGenerationInstallBanner />
      {archiveCleanupSnackbar}
      {transcriptNoticeSnackbar}
      {desktopStorageNoticeSnackbar}
      {topbar}
      {composerHarnessAuthModal}

      {workbenchWarnings.length > 0 && (
        <div className="banner" style={{ margin: "8px 12px 0" }}>
          {workbenchWarnings[0]}
        </div>
      )}
      <WorkbenchProviderWarningBanner {...providerWarningProps} />
      {mobileShell && !sidebarCollapsed ? (
        <button
          type="button"
          className="wb-sidebar-backdrop"
          aria-label="Hide task list"
          onClick={sidebarProps.onCollapseSidebar}
        />
      ) : null}
      <WorkbenchSidebar {...sidebarProps} />

      <div className="wb-main">
        {!activeTaskId ? <WorkbenchEmptyState {...emptyStateProps} /> : null}
        {activeTaskId && activeTaskViewProps ? <WorkbenchActiveTaskView {...activeTaskViewProps} /> : null}
      </div>

      {!mobileShell ? (
        <div className="wb-terminal-shell" aria-hidden={!activeTaskController.terminalOpen}>
          {activeTaskController.terminalOpen && (
            <div className="wb-terminal-resizer" onMouseDown={activeTaskController.onTerminalResizerMouseDown} />
          )}
          <div
            className="wb-terminal-panel"
            style={{
              height: activeTaskController.terminalOpen ? activeTaskController.terminalHeight : 0,
              pointerEvents: activeTaskController.terminalOpen ? "auto" : "none",
            }}
            aria-hidden={!activeTaskController.terminalOpen}
          >
            <TerminalPanel
              ref={activeTaskController.terminalPanelRef}
              workspaceId={workspaceId}
              activeTaskId={activeTaskId}
              activeSessionId={activeSessionId}
              open={activeTaskController.terminalOpen}
              height={activeTaskController.terminalHeight}
              onRequestClose={activeTaskController.closeTerminalPanel}
            />
          </div>
        </div>
      ) : null}

      <WorkbenchPageMenus
        activeTaskController={activeTaskController}
        taskListController={taskListController}
        activeTaskId={activeTaskId}
        activeSessionId={activeSessionId}
      />
    </div>
  );
}
