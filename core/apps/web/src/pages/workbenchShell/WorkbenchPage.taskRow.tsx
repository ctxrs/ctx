import { useEffect, useRef, useState } from "react";
import React from "react";
import { Archive, Ellipsis, LayersPlus, X } from "lucide-react";
import { TextInput } from "../../components/ui/text-input";
import { HARNESS_CATALOG } from "../../utils/harnessCatalog";
import { shouldSendOnEnter } from "../../utils/keyboard";
import { formatRelativeAgeShort } from "../../utils/relativeTime";
import { useRelativeNowMs } from "../../utils/useRelativeNowMs";
import { getLoadTestTelemetry } from "../../utils/loadTestTelemetry";
import { noteVisibleSessionSwitchStarted } from "../../state/visibleSessionSwitchState";
import { spinnerDelayForNow } from "./WorkbenchPage.utils";
import type { AnchorRect } from "./WorkbenchPage.types";
import { formatAgentWorkSummaryChips, type AgentWorkTaskSummary } from "./agentWorkProjection";

type TaskRowProps = {
  taskId: string;
  sessionId?: string | null;
  activeSessionId?: string | null;
  taskIndex?: number;
  subscribedAtClick?: boolean;
  authoritativeAtClick?: boolean;
  title: string;
  archived: boolean;
  archivePending: boolean;
  archivePendingAction: "archive" | "unarchive" | null;
  statusKind: "archive" | "error" | "working" | "unread" | "idle";
  selected: boolean;
  hovered: boolean;
  isRenaming: boolean;
  ageIso: string | null | undefined;
  providerCount: number;
  harnesses: Array<(typeof HARNESS_CATALOG)[number]>;
  agentWorkSummary?: AgentWorkTaskSummary | null;
  getRenameDraft: (taskId: string, fallback: string) => string;
  setRenameDraft: (taskId: string, nextValue: string) => void;
  onFocusTask: (taskId: string, sessionId?: string | null) => void;
  onOpenMenu: (taskId: string, opts: { triggerEl: HTMLElement } | { x: number; y: number }) => void;
  menuEnabled?: boolean;
  archiveEnabled?: boolean;
  onDismiss?: (taskId: string) => void;
  dismissLabel?: string;
  onToggleArchive: (taskId: string, nextArchived: boolean, anchor?: AnchorRect | null) => Promise<void>;
  onHoverEnter: (taskId: string) => void;
  onHoverLeave: (taskId: string) => void;
  onCancelRename: () => void;
  onCommitRename: (taskId: string, nextValue: string) => void;
};

function RelativeAgeLabel({
  iso,
  fallback = "Now",
}: {
  iso: string | null | undefined;
  fallback?: string;
}) {
  const nowMs = useRelativeNowMs();
  const age = formatRelativeAgeShort(iso, nowMs);
  return <>{age || fallback}</>;
}

export const TaskRow = React.memo(function TaskRow({
  taskId,
  sessionId,
  activeSessionId,
  taskIndex,
  subscribedAtClick,
  authoritativeAtClick,
  title,
  archived,
  archivePending,
  archivePendingAction,
  statusKind,
  selected,
  hovered,
  isRenaming,
  ageIso,
  providerCount,
  harnesses,
  agentWorkSummary,
  getRenameDraft,
  setRenameDraft,
  onFocusTask,
  onOpenMenu,
  menuEnabled,
  archiveEnabled,
  onDismiss,
  dismissLabel,
  onToggleArchive,
  onHoverEnter,
  onHoverLeave,
  onCancelRename,
  onCommitRename,
}: TaskRowProps) {
  const [renameDraft, setRenameDraftState] = useState(() => getRenameDraft(taskId, title));
  const renameInputRef = useRef<HTMLInputElement | null>(null);
  const spinnerDelayRef = useRef<number>(spinnerDelayForNow());
  const wasRenamingRef = useRef(false);
  const ignoreBlurRef = useRef(false);
  const clickOutsideRef = useRef(false);

  useEffect(() => {
    if (isRenaming && !wasRenamingRef.current) {
      const initialDraft = getRenameDraft(taskId, title);
      setRenameDraftState(initialDraft);
      setRenameDraft(taskId, initialDraft);
      requestAnimationFrame(() => {
        const el = renameInputRef.current;
        if (!el) return;
        el.focus();
        el.select();
      });
    }
    wasRenamingRef.current = isRenaming;
    if (isRenaming) {
      ignoreBlurRef.current = false;
      clickOutsideRef.current = false;
    }
  }, [getRenameDraft, isRenaming, setRenameDraft, taskId, title]);

  useEffect(() => {
    if (!isRenaming) return;
    const onPointerDown = (e: PointerEvent) => {
      const input = renameInputRef.current;
      const target = e.target as Node | null;
      if (!input || !target) return;
      clickOutsideRef.current = !input.contains(target);
    };
    window.addEventListener("pointerdown", onPointerDown);
    return () => {
      window.removeEventListener("pointerdown", onPointerDown);
    };
  }, [isRenaming]);

  const archiveLabel = archivePending
    ? archivePendingAction === "archive"
      ? "Archiving..."
      : "Unarchiving..."
    : archived
      ? "Unarchive"
      : "Archive";

  const showMenu = menuEnabled !== false;
  const showArchive = archiveEnabled !== false && !onDismiss;
  const showDismiss = typeof onDismiss === "function";
  const resolvedDismissLabel = dismissLabel || "Dismiss";
  const graphChips = formatAgentWorkSummaryChips(agentWorkSummary);
  const recordSwitchStart = (source: "pointer" | "keyboard") => {
    if (!sessionId || sessionId === activeSessionId) return;
    noteVisibleSessionSwitchStarted(sessionId);
    getLoadTestTelemetry()?.startVisibleSessionSwitch({
      fromSessionId: activeSessionId ?? null,
      toSessionId: sessionId ?? null,
      taskId,
      targetIndex: taskIndex,
      source,
      subscribedAtClick,
      authoritativeAtClick,
    });
  };

  return (
    <div
      className={`wb-task-row ${archived ? "wb-task-row-archived" : ""} ${selected ? "wb-task-row-active" : ""} ${
        hovered ? "wb-task-row-hovered" : ""
      }`}
      role="listitem"
      onContextMenuCapture={(e) => {
        if (showMenu) return;
        e.preventDefault();
        e.stopPropagation();
      }}
      onClick={() => {
        recordSwitchStart("pointer");
        onFocusTask(taskId, sessionId ?? null);
      }}
      onPointerEnter={() => onHoverEnter(taskId)}
      onPointerLeave={() => onHoverLeave(taskId)}
      onContextMenu={(e) => {
        e.preventDefault();
        e.stopPropagation();
        if (!showMenu) return;
        onOpenMenu(taskId, { x: e.clientX, y: e.clientY });
      }}
      onKeyDown={(e) => {
        const target = e.target as HTMLElement | null;
        if (target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable)) {
          return;
        }
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          recordSwitchStart("keyboard");
          onFocusTask(taskId, sessionId ?? null);
        }
      }}
      tabIndex={0}
      aria-label={title}
    >
      <div className="wb-task-leading" aria-hidden="true">
        {providerCount > 1 ? (
          <LayersPlus className="wb-task-harness-multi" size={16} />
        ) : harnesses.length > 0 ? (
          <img
            className={`wb-task-harness-logo ${harnesses[0].invertInDark ? "wb-invert" : ""} ${
              harnesses[0].invertInLight ? "wb-invert-light" : ""
            }`}
            src={harnesses[0].logoSrc}
            alt=""
          />
        ) : (
          <span className="wb-task-harness-fallback" aria-hidden="true" />
        )}
      </div>
      <div className="wb-task-body">
        {isRenaming ? (
          <TextInput
            ref={renameInputRef}
            className="wb-task-rename"
            value={renameDraft}
            onChange={(e) => {
              const nextValue = e.target.value;
              setRenameDraftState(nextValue);
              setRenameDraft(taskId, nextValue);
            }}
            onClick={(e) => e.stopPropagation()}
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                e.preventDefault();
                e.stopPropagation();
                ignoreBlurRef.current = true;
                onCancelRename();
              }
              if (shouldSendOnEnter(e)) {
                e.preventDefault();
                e.stopPropagation();
                ignoreBlurRef.current = true;
                onCommitRename(taskId, e.currentTarget.value);
              }
            }}
            onBlur={(e) => {
              if (ignoreBlurRef.current) {
                ignoreBlurRef.current = false;
                return;
              }
              if (!clickOutsideRef.current) return;
              clickOutsideRef.current = false;
              onCommitRename(taskId, e.currentTarget.value);
            }}
            aria-label="Rename task"
          />
        ) : (
          <>
            <div className="wb-task-title">{title}</div>
            {graphChips.length > 0 ? (
              <div className="wb-task-agent-work" aria-label="Agent work summary">
                {graphChips.map((chip) => (
                  <span key={chip} className="wb-task-agent-work-chip">{chip}</span>
                ))}
              </div>
            ) : null}
          </>
        )}
      </div>
      <div className="wb-task-meta">
        <div className="wb-task-meta-status" aria-hidden="true">
          <div className="wb-task-age">
            <RelativeAgeLabel iso={ageIso} />
          </div>
          <span className="wb-task-status-slot">
            {statusKind === "archive" && (
              <span
                className="wb-task-spinner wb-task-spinner-archive"
                style={{ animationDelay: `${spinnerDelayRef.current}ms` }}
              />
            )}
            {statusKind === "working" && (
              <span className="wb-task-spinner" style={{ animationDelay: `${spinnerDelayRef.current}ms` }} />
            )}
            {statusKind === "unread" && <span className="wb-task-status-dot wb-task-status-dot-unread" />}
            {statusKind === "error" && <span className="wb-task-status-dot wb-task-status-dot-error" />}
          </span>
        </div>
        <div className="wb-task-actions" aria-label="Task actions">
          {showMenu ? (
            <button
              type="button"
              className="wb-icon wb-task-action wb-task-menu-trigger"
              onClick={(e) => {
                e.stopPropagation();
                onOpenMenu(taskId, { triggerEl: e.currentTarget });
              }}
              aria-label="More actions"
              title="More actions"
            >
              <Ellipsis size={14} />
            </button>
          ) : null}
          {showDismiss ? (
            <button
              type="button"
              className="wb-icon wb-task-action"
              onClick={(e) => {
                e.stopPropagation();
                onDismiss?.(taskId);
              }}
              aria-label={resolvedDismissLabel}
              title={resolvedDismissLabel}
            >
              <X size={14} />
            </button>
          ) : showArchive ? (
            <button
              type="button"
              className="wb-icon wb-task-action wb-archive-confirm-trigger"
              disabled={archivePending}
              onClick={(e) => {
                e.stopPropagation();
                const anchor = e.currentTarget.getBoundingClientRect();
                onToggleArchive(taskId, !archived, anchor).catch(() => {});
              }}
              aria-label={archiveLabel}
              title={archiveLabel}
            >
              <Archive size={14} />
            </button>
          ) : null}
        </div>
      </div>
    </div>
  );
});
