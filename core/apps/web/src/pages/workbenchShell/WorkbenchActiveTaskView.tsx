import { useEffect, useState, type ComponentProps, type MouseEvent } from "react";

import { ArtifactsPane } from "../../components/ArtifactsPane";
import { DiffReviewPane } from "../../components/DiffReviewPane";
import { SessionsPane } from "../../components/SessionsPane";
import { WorkbenchSessionSlot } from "./WorkbenchPage.sessionSlot";
import { WorkbenchSessionHeader } from "./WorkbenchSessionHeader";
import { WorkbenchSessionLoadIssues, type WorkbenchSessionLoadIssue } from "./WorkbenchSessionLoadIssues";
import type { AgentWorkTaskSummary } from "./agentWorkProjection";
import type { GitPaneModel } from "./worktreeGitPaneModel";

type WorktreeChip = ComponentProps<typeof WorkbenchSessionHeader>["worktreeChip"];
type SessionLoadIssues = WorkbenchSessionLoadIssue[];
type OptimisticFailure = ComponentProps<typeof WorkbenchSessionSlot>["optimisticFailure"];
type SessionSections = ComponentProps<typeof SessionsPane>["sections"];
type Artifacts = ComponentProps<typeof ArtifactsPane>["artifacts"];

type WorkbenchActiveTaskViewProps = {
  sessionsCount: number;
  showSingleSessionHeader: boolean;
  singleSessionTitle: string;
  worktreeChip: WorktreeChip;
  worktreeCopied: boolean;
  showArtifactsPane: boolean;
  showReviewPane: boolean;
  terminalOpen: boolean;
  artifactsCount: number;
  diffBadgeCount: number;
  agentWorkSummary?: AgentWorkTaskSummary | null;
  onCopyWorktreeLocation: () => void;
  onOpenWorktreeTerminal: () => void;
  onToggleArtifactsPane: () => void;
  onToggleDiffPane: () => void;
  onToggleTerminalPanel: () => void;
  onOpenConvoMenu: (triggerEl: HTMLElement) => void;
  sessionLoadIssues: SessionLoadIssues;
  onRetrySessionLoads: () => void;
  activeSessionId: string | null;
  activeSessionRenderable: boolean;
  optimisticFailure: OptimisticFailure;
  rightPaneOpen: boolean;
  onSplitterMouseDown: (event: MouseEvent<HTMLDivElement>) => void;
  diffWidth: number;
  showSessionsPane: boolean;
  sessionSections: SessionSections;
  activeSessionKind: string;
  onSectionChange: (key: string) => void;
  activeWebSessionId: string | null;
  onSelectWebSession: (id: string) => void;
  daemonBaseUrl: string;
  webSessionsLoading: boolean;
  hasDiff: boolean;
  gitPaneModel: GitPaneModel;
  diffLoading: boolean;
  diffSummaryError: string | null;
  diffTooLarge: boolean;
  diffTooLargeLabel: string | null;
  activeSessionDiff: string;
  activeDiffContentError: string | null;
  diffEmptyLabel: string;
  artifacts: Artifacts;
  artifactsLoading: boolean;
  artifactsError: string | null;
  onRetryArtifactsLoad: () => void;
  mobileMode?: boolean;
};

export function WorkbenchActiveTaskView({
  sessionsCount,
  showSingleSessionHeader,
  singleSessionTitle,
  worktreeChip,
  worktreeCopied,
  showArtifactsPane,
  showReviewPane,
  terminalOpen,
  artifactsCount,
  diffBadgeCount,
  agentWorkSummary,
  onCopyWorktreeLocation,
  onOpenWorktreeTerminal,
  onToggleArtifactsPane,
  onToggleDiffPane,
  onToggleTerminalPanel,
  onOpenConvoMenu,
  sessionLoadIssues,
  onRetrySessionLoads,
  activeSessionId,
  activeSessionRenderable,
  optimisticFailure,
  rightPaneOpen,
  onSplitterMouseDown,
  diffWidth,
  showSessionsPane,
  sessionSections,
  activeSessionKind,
  onSectionChange,
  activeWebSessionId,
  onSelectWebSession,
  daemonBaseUrl,
  webSessionsLoading,
  hasDiff,
  gitPaneModel,
  diffLoading,
  diffSummaryError,
  diffTooLarge,
  diffTooLargeLabel,
  activeSessionDiff,
  activeDiffContentError,
  diffEmptyLabel,
  artifacts,
  artifactsLoading,
  artifactsError,
  onRetryArtifactsLoad,
  mobileMode = false,
}: WorkbenchActiveTaskViewProps) {
  const [stickyRenderableSessionId, setStickyRenderableSessionId] = useState<string | null>(null);

  useEffect(() => {
    if (!activeSessionId) {
      setStickyRenderableSessionId(null);
      return;
    }
    if (activeSessionRenderable) {
      setStickyRenderableSessionId(activeSessionId);
      return;
    }
    setStickyRenderableSessionId((current) => (current === activeSessionId ? current : null));
  }, [activeSessionId, activeSessionRenderable]);

  const showSessionSlot = Boolean(activeSessionId) && (
    activeSessionRenderable || stickyRenderableSessionId === activeSessionId
  );

  return (
    <div className="wb-body">
      <div className="wb-convo">
        {showSingleSessionHeader ? (
          <WorkbenchSessionHeader
            busy={sessionsCount === 0}
            title={singleSessionTitle}
            worktreeChip={worktreeChip}
            worktreeCopied={worktreeCopied}
            showArtifactsPane={showArtifactsPane}
            showReviewPane={showReviewPane}
            terminalOpen={terminalOpen}
            artifactsCount={artifactsCount}
            diffBadgeCount={diffBadgeCount}
            agentWorkSummary={agentWorkSummary}
            onCopyWorktreeLocation={onCopyWorktreeLocation}
            onOpenWorktreeTerminal={onOpenWorktreeTerminal}
            onToggleArtifactsPane={onToggleArtifactsPane}
            onToggleDiffPane={onToggleDiffPane}
            onToggleTerminalPanel={onToggleTerminalPanel}
            onOpenConvoMenu={onOpenConvoMenu}
            showAuxiliaryActions={!mobileMode}
          />
        ) : null}

        <div className="wb-session">
          <WorkbenchSessionLoadIssues issues={sessionLoadIssues} onRetry={onRetrySessionLoads} />
          {activeSessionId && showSessionSlot ? (
            <WorkbenchSessionSlot
              sessionId={activeSessionId}
              optimisticFailure={optimisticFailure}
            />
          ) : null}
          {activeSessionId && !showSessionSlot ? (
            <div className="wb-session-slot wb-session-slot--hydrating">
              <div className="wb-session-slot-body wb-muted" style={{ padding: 16 }}>
                Loading conversation...
              </div>
            </div>
          ) : null}
          {!activeSessionId ? (
            <div className="wb-muted" style={{ padding: 16 }}>
              Select a session to view this task.
            </div>
          ) : null}
        </div>
      </div>

      {!mobileMode && rightPaneOpen ? (
        <>
          <div className="wb-splitter" onMouseDown={onSplitterMouseDown} />
          <div className="wb-right" style={{ width: diffWidth, maxWidth: "100%" }}>
            {showSessionsPane ? (
              <div className="wb-right-pane">
                <SessionsPane
                  sections={sessionSections}
                  activeSection={activeSessionKind}
                  onSectionChange={onSectionChange}
                  selectedSessionId={activeWebSessionId}
                  onSelectSession={onSelectWebSession}
                  daemonBaseUrl={daemonBaseUrl}
                  loading={webSessionsLoading}
                />
              </div>
            ) : showReviewPane ? (
              <div className="wb-right-pane wb-diff">
                {hasDiff ? (
                  <DiffReviewPane
                    diff={activeSessionDiff}
                    inventory={gitPaneModel}
                    detail={{
                      loading: diffLoading,
                      error: activeDiffContentError ?? diffSummaryError,
                      tooLarge: diffTooLarge,
                      tooLargeLabel: diffTooLargeLabel,
                    }}
                    labels={activeDiffContentError ? { empty: activeDiffContentError } : undefined}
                  />
                ) : (
                  <div className="wb-diff-empty">
                    <div className="wb-muted">{diffEmptyLabel}</div>
                  </div>
                )}
              </div>
            ) : showArtifactsPane ? (
              <div className="wb-right-pane">
                <ArtifactsPane
                  sessionId={activeSessionId ?? ""}
                  artifacts={artifacts}
                  loading={artifactsLoading}
                  error={artifactsError}
                  onRetry={onRetryArtifactsLoad}
                />
              </div>
            ) : null}
          </div>
        </>
      ) : null}
    </div>
  );
}
