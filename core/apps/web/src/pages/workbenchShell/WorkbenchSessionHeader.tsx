import { Check, Copy, Ellipsis, GitBranch, Image, Terminal } from "lucide-react";

type WorktreeChip = {
  worktreeLabel: string;
  worktreePath: string;
  canCopyWorktree: boolean;
  canOpenTerminal: boolean;
};

type WorkbenchSessionHeaderProps = {
  busy: boolean;
  title: string;
  worktreeChip: WorktreeChip;
  worktreeCopied: boolean;
  showArtifactsPane: boolean;
  showReviewPane: boolean;
  terminalOpen: boolean;
  artifactsCount: number;
  diffBadgeCount: number;
  onCopyWorktreeLocation: () => void;
  onOpenWorktreeTerminal: () => void;
  onToggleArtifactsPane: () => void;
  onToggleDiffPane: () => void;
  onToggleTerminalPanel: () => void;
  onOpenConvoMenu: (triggerEl: HTMLElement) => void;
  showAuxiliaryActions?: boolean;
};

export function WorkbenchSessionHeader({
  busy,
  title,
  worktreeChip,
  worktreeCopied,
  showArtifactsPane,
  showReviewPane,
  terminalOpen,
  artifactsCount,
  diffBadgeCount,
  onCopyWorktreeLocation,
  onOpenWorktreeTerminal,
  onToggleArtifactsPane,
  onToggleDiffPane,
  onToggleTerminalPanel,
  onOpenConvoMenu,
  showAuxiliaryActions = true,
}: WorkbenchSessionHeaderProps) {
  return (
    <div className="wb-single-track-header" aria-busy={busy ? "true" : undefined}>
      <div className="wb-single-track-row">
        <div className="wb-single-track-title-row">
          <div className="wb-single-track-title">{title}</div>
          {worktreeChip.worktreeLabel && (
            <>
              <span className="wb-single-track-dot" aria-hidden="true">
                ·
              </span>
              <span className="wb-worktree-actions">
                <button
                  type="button"
                  className={`wb-worktree-chip ${worktreeCopied ? "wb-worktree-chip-copied" : ""}`}
                  disabled={!worktreeChip.canCopyWorktree}
                  onClick={onCopyWorktreeLocation}
                  title="Copy worktree location"
                  aria-label="Copy worktree location"
                >
                  <span className="wb-worktree-chip-slug">{worktreeChip.worktreeLabel}</span>
                  <span className="wb-worktree-chip-copy" aria-hidden="true">
                    {worktreeCopied ? <Check size={12} /> : <Copy size={12} />}
                  </span>
                </button>
                <button
                  type="button"
                  className="wb-worktree-action"
                  disabled={!worktreeChip.canOpenTerminal}
                  onClick={onOpenWorktreeTerminal}
                  title="Open worktree terminal"
                  aria-label="Open worktree terminal"
                >
                  <Terminal size={13} />
                </button>
              </span>
              {worktreeCopied && (
                <span className="sr-only" aria-live="polite">
                  Copied worktree location to clipboard.
                </span>
              )}
            </>
          )}
        </div>
        <div className="wb-icon-row">
          {showAuxiliaryActions ? (
            <>
              <button
                type="button"
                className={`wb-icon ${showArtifactsPane ? "wb-icon-active" : ""}`}
                aria-label="Toggle artifacts"
                aria-pressed={showArtifactsPane}
                title={showArtifactsPane ? "Hide artifacts" : "Show artifacts"}
                onClick={onToggleArtifactsPane}
              >
                <Image size={14} />
                {artifactsCount > 0 && <span className="wb-icon-badge">{artifactsCount}</span>}
              </button>
              <button
                type="button"
                className={`wb-icon ${showReviewPane ? "wb-icon-active" : ""}`}
                aria-label="Toggle diff view"
                aria-pressed={showReviewPane}
                title={showReviewPane ? "Hide diff view" : "Show diff view"}
                onClick={onToggleDiffPane}
              >
                <GitBranch size={14} />
                {diffBadgeCount > 0 && <span className="wb-icon-badge">{diffBadgeCount}</span>}
              </button>
              <button
                type="button"
                className={`wb-icon ${terminalOpen ? "wb-icon-active" : ""}`}
                aria-label="Toggle terminal panel"
                aria-pressed={terminalOpen}
                title={terminalOpen ? "Hide terminal" : "Show terminal"}
                onClick={onToggleTerminalPanel}
              >
                <Terminal size={14} />
              </button>
            </>
          ) : null}
          <button
            type="button"
            className="wb-icon wb-convo-menu-trigger"
            aria-label="Conversation options"
            title="Conversation options"
            onClick={(event) => onOpenConvoMenu(event.currentTarget)}
          >
            <Ellipsis size={14} />
          </button>
        </div>
      </div>
    </div>
  );
}
