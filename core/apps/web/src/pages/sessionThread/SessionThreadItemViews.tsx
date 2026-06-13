import {
  Fragment,
  memo,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type MouseEvent,
  type ReactNode,
} from "react";
import { Check, Copy } from "lucide-react";
import { type MessageAttachment } from "../../api/client";
import { MessageAttachmentImage } from "../../components/MessageAttachmentImage";
import { type SessionViewVerbosity } from "../../state/uiStateStore";
import { copyTextToClipboard } from "../../utils/clipboard";
import { useRelativeNowMs } from "../../utils/useRelativeNowMs";
import { MemoMarkdown } from "../sessionView/SessionPage.markdown";
import {
  attachmentDisplayName,
  formatElapsedMs,
  formatToolInput,
  humanToolStatus,
  humanTurnStatus,
  looksLikeMarkdown,
  parseIsoMs,
  toolKindIcon,
  toolSummaryLine,
} from "../sessionView/SessionPage.helpers";
import type { ThreadItem, WorkbenchTurnHeader } from "../sessionView/SessionPage.types";
import { buildWorkbenchToolLabel } from "./sessionThreadToolLabel";
import { getWorkbenchMessageCollapseState, getWorkbenchMessageLayoutState } from "./transcriptRowLayoutModel";

type SelectionSnapshot = {
  text: string;
  anchorNode: Node | null;
  anchorOffset: number;
  focusNode: Node | null;
  focusOffset: number;
};

function readSelectionSnapshot(): SelectionSnapshot {
  const selection = window.getSelection();
  return {
    text: selection?.toString().trim() ?? "",
    anchorNode: selection?.anchorNode ?? null,
    anchorOffset: selection?.anchorOffset ?? -1,
    focusNode: selection?.focusNode ?? null,
    focusOffset: selection?.focusOffset ?? -1,
  };
}

function selectionChangedDuringInteraction(
  before: SelectionSnapshot | null,
  after: SelectionSnapshot,
): boolean {
  if (!after.text) return false;
  if (!before) return true;
  return (
    before.text !== after.text ||
    before.anchorNode !== after.anchorNode ||
    before.anchorOffset !== after.anchorOffset ||
    before.focusNode !== after.focusNode ||
    before.focusOffset !== after.focusOffset
  );
}

export const ThreadItemView = memo(function ThreadItemView({
  item,
  worktreeId,
  onFileOpenError,
  messageExpanded,
  onToggleMessageExpanded,
}: {
  item: ThreadItem;
  worktreeId: string | null;
  onFileOpenError: (message: string | null) => void;
  messageExpanded?: boolean;
  onToggleMessageExpanded?: (expanded: boolean) => void;
}) {
  switch (item.kind) {
    case "message":
      return (
        <CollapsibleMessage
          id={item.id}
          role={item.role}
          content={item.content}
          attachments={item.attachments}
          worktreeId={worktreeId}
          onFileOpenError={onFileOpenError}
          expanded={messageExpanded ?? !getWorkbenchMessageCollapseState(item.content).canCollapse}
          onToggleExpanded={onToggleMessageExpanded}
        />
      );
    case "assistant":
    case "tool":
      return null;
    case "tool_group":
    case "turn_status":
      return null;
    default:
      return null;
  }
});

export function WorkbenchTurnHeaderView({
  header,
  plainText,
  expanded,
  onToggle,
}: {
  header: WorkbenchTurnHeader;
  plainText: string;
  expanded: boolean;
  onToggle: () => void;
}) {
  const [copied, setCopied] = useState(false);
  const resetTimerRef = useRef<number | null>(null);
  const pointerSelectionRef = useRef<SelectionSnapshot | null>(null);

  useEffect(() => {
    if (!copied) return;
    if (resetTimerRef.current) window.clearTimeout(resetTimerRef.current);
    resetTimerRef.current = window.setTimeout(() => {
      setCopied(false);
      resetTimerRef.current = null;
    }, 1000);
    return () => {
      if (resetTimerRef.current) window.clearTimeout(resetTimerRef.current);
    };
  }, [copied]);

  const handleCopy = useCallback(
    async (e: MouseEvent) => {
      e.stopPropagation();
      const content = header.content ?? "";
      if (!content.trim()) return;
      const ok = await copyTextToClipboard(content);
      if (!ok) return;
      setCopied(true);
    },
    [header.content],
  );

  const handleCopyMouseDown = useCallback((e: MouseEvent<HTMLButtonElement>) => {
    e.stopPropagation();
  }, []);

  const handleClick = (event: MouseEvent<HTMLDivElement>) => {
    const target = event.target;
    if (target instanceof Element && target.closest(".wb-turn-header-copy")) return;
    const selection = readSelectionSnapshot();
    if (selectionChangedDuringInteraction(pointerSelectionRef.current, selection)) return;
    onToggle();
  };

  const handleKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key !== "Enter" && event.key !== " ") return;
    event.preventDefault();
    onToggle();
  };

  const hasContent = (header.content ?? "").trim().length > 0;

  return (
    <div
      className={`wb-turn-header ${expanded ? "wb-turn-header-expanded" : "wb-turn-header-collapsed"}`}
      role="button"
      tabIndex={0}
      onMouseDown={() => {
        pointerSelectionRef.current = readSelectionSnapshot();
      }}
      onClick={handleClick}
      onKeyDown={handleKeyDown}
      aria-expanded={expanded}
    >
      <div className="wb-turn-header-bubble">
        {hasContent && (
          <button
            type="button"
            className="wb-turn-header-copy"
            aria-label={copied ? "Copied" : "Copy message"}
            title={copied ? "Copied" : "Copy message"}
            onMouseDown={handleCopyMouseDown}
            onClick={handleCopy}
          >
            {copied ? <Check size={12} aria-hidden="true" /> : <Copy size={12} aria-hidden="true" />}
          </button>
        )}
        <div className="wb-turn-header-content">{plainText}</div>
        {expanded && header.attachments.length > 0 && (
          <div className="wb-turn-header-attachments" aria-label="Attachments">
            {header.attachments.map((a, idx) => {
              if (a.kind !== "image" && a.kind !== "image_ref") return null;
              const name = attachmentDisplayName(a.name);
              return (
                <MessageAttachmentImage
                  key={idx}
                  attachment={a}
                  className="wb-turn-header-attachment-img"
                  alt={name}
                  title={name}
                />
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}

function CollapsibleMessage({
  id,
  role,
  content,
  attachments,
  worktreeId,
  onFileOpenError,
  expanded,
  onToggleExpanded,
}: {
  id: string;
  role: "user" | "assistant" | "system";
  content: string;
  attachments: MessageAttachment[];
  worktreeId: string | null;
  onFileOpenError: (message: string | null) => void;
  expanded: boolean;
  onToggleExpanded?: (expanded: boolean) => void;
}) {
  const layoutState = getWorkbenchMessageLayoutState(
    {
      kind: "message",
      id,
      role,
      content,
      attachments,
      created_at: "",
    },
    expanded ? { [id]: true } : {},
  );
  const canCollapse = layoutState.expandable;
  const canToggle = canCollapse && typeof onToggleExpanded === "function";
  const shown = layoutState.shownContent;

  return (
    <div className="wb-message-row">
      <div className={`msg ${role}`}>
        <div className="role">{role}</div>
        <div id={`msg-${id}`}>
          {layoutState.renderMode === "plain_text" ? (
            <div className="wb-markdown-root wb-message-plain-text">{shown}</div>
          ) : (
            <MemoMarkdown
              content={shown}
              linkifyFiles={role === "assistant"}
              worktreeId={worktreeId}
              onFileOpenError={onFileOpenError}
            />
          )}
        </div>
        {attachments?.length > 0 && (
          <div className="attachments">
            {attachments.map((a, idx) => {
              if (a.kind !== "image" && a.kind !== "image_ref") return null;
              return (
                <MessageAttachmentImage
                  key={idx}
                  attachment={a}
                  className="attachment-img"
                  alt={a.name ?? `image-${idx}`}
                />
              );
            })}
          </div>
        )}
        {canToggle && (
          <button
            type="button"
            className="link"
            aria-expanded={expanded}
            aria-controls={`msg-${id}`}
            onClick={() => onToggleExpanded?.(!expanded)}
          >
            {expanded ? "Show less" : "Show more"}
          </button>
        )}
      </div>
    </div>
  );
}

export const AssistantEntry = memo(function AssistantEntry({
  content,
  worktreeId,
  onFileOpenError,
}: {
  content: string;
  worktreeId: string | null;
  onFileOpenError: (message: string | null) => void;
}) {
  return (
    <div className="wb-assistant-entry">
      <div className="wb-assistant-body">
        <MemoMarkdown
          content={content}
          linkifyFiles
          worktreeId={worktreeId}
          onFileOpenError={onFileOpenError}
        />
      </div>
    </div>
  );
});

export function WorkbenchToolRow({
  item,
  verbosity,
  expanded,
  onToggle,
}: {
  item: Extract<ThreadItem, { kind: "tool" }>;
  verbosity: SessionViewVerbosity;
  expanded: boolean;
  onToggle: () => void;
}) {
  void verbosity;
  void expanded;
  void onToggle;
  const { verb, inlineTail } = buildWorkbenchToolLabel(item);
  return (
    <div className="wb-tool-row">
      <div className="wb-event-row wb-event-row-static">
        <span className="wb-event-text wb-tool-text">
          <span className="wb-tool-mainline">
            <span className="wb-tool-verb">{verb}</span>
            {inlineTail ? (
              <>
                <span className="wb-tool-sep" aria-hidden="true">
                  ·
                </span>
                <span className="wb-tool-rest">{inlineTail}</span>
              </>
            ) : null}
          </span>
        </span>
      </div>
    </div>
  );
}

export const WorkbenchThoughtRow = memo(function WorkbenchThoughtRow({
  item,
}: {
  item: Extract<ThreadItem, { kind: "thought" }>;
}) {
  return <div className="wb-thought-row">{item.content}</div>;
});

export const WorkbenchTurnStatusRow = memo(function WorkbenchTurnStatusRow({
  item,
}: {
  item: Extract<ThreadItem, { kind: "turn_status" }>;
}) {
  const isRunning = item.status === "running" || item.status === "starting" || item.status === "queued";
  const nowMs = useRelativeNowMs(1000, isRunning);
  const isCompleted = item.status === "completed";
  const customStatus = item.custom_status?.trim();
  const statusLabel = isRunning && customStatus ? customStatus : humanTurnStatus(item.status);
  const startMs = parseIsoMs(item.started_at);
  const endMs = isRunning ? nowMs : parseIsoMs(item.updated_at) ?? nowMs;
  const elapsedMs = startMs != null && endMs != null ? Math.max(0, endMs - startMs) : 0;
  const elapsedLabel = formatElapsedMs(elapsedMs);

  const [copied, setCopied] = useState(false);
  const resetTimerRef = useRef<number | null>(null);

  useEffect(() => {
    if (!copied) return;
    if (resetTimerRef.current) window.clearTimeout(resetTimerRef.current);
    resetTimerRef.current = window.setTimeout(() => {
      setCopied(false);
      resetTimerRef.current = null;
    }, 1000);
    return () => {
      if (resetTimerRef.current) window.clearTimeout(resetTimerRef.current);
    };
  }, [copied]);

  const handleCopy = useCallback(async () => {
    const content = item.assistant_messages_content ?? "";
    if (!content.trim()) return;
    const ok = await copyTextToClipboard(content);
    if (!ok) return;
    setCopied(true);
  }, [item.assistant_messages_content]);

  const hasContent = (item.assistant_messages_content ?? "").trim().length > 0;
  const showCopyButton = isCompleted && hasContent;

  return (
    <div className="wb-turn-status">
      <span className="wb-turn-status-label">{statusLabel}</span>
      <span className="wb-turn-status-dot" aria-hidden="true">
        ·
      </span>
      <span className="wb-turn-status-time">{elapsedLabel}</span>
      {showCopyButton && (
        <>
          <span className="wb-turn-status-dot" aria-hidden="true">
            ·
          </span>
          <button
            type="button"
            className="wb-turn-status-copy"
            aria-label={copied ? "Copied" : "Copy response"}
            title={copied ? "Copied" : "Copy response"}
            onClick={() => void handleCopy()}
          >
            {copied ? <Check size={12} aria-hidden="true" /> : <Copy size={12} aria-hidden="true" />}
          </button>
        </>
      )}
    </div>
  );
});

export const WorkbenchToolGroupRow = memo(function WorkbenchToolGroupRow({
  item,
  verbosity,
  expanded,
  onToggle,
  toolsLoading,
  onRequestTools,
  onToggleTool,
  expandedToolById,
}: {
  item: Extract<ThreadItem, { kind: "tool_group" }>;
  verbosity: SessionViewVerbosity;
  expanded: boolean;
  onToggle: () => void;
  toolsLoading: boolean;
  onRequestTools: () => void;
  onToggleTool: (id: string) => void;
  expandedToolById: Record<string, boolean>;
}) {
  const total = Math.max(item.tool_total ?? 0, item.tools.length);
  const parts: string[] = [];
  if (total > 0) {
    parts.push(`${total} tool${total === 1 ? "" : "s"}`);
  }
  if ((item.tool_running ?? 0) > 0) {
    parts.push(`${item.tool_running} running`);
  }
  if ((item.tool_failed ?? 0) > 0) {
    parts.push(`${item.tool_failed} failed`);
  }
  if (parts.length === 0 && item.thought.trim()) {
    parts.push("Thought");
  }
  const label = parts.join(" · ") || "Activity";
  const hasDetails = total > 0 || item.thought.trim().length > 0;

  useEffect(() => {
    if (expanded && total > 0 && item.tools.length === 0 && !toolsLoading) {
      onRequestTools();
    }
  }, [expanded, total, item.tools.length, toolsLoading, onRequestTools]);

  return (
    <div className="wb-tool-group">
      <button
        type="button"
        className="wb-event-row"
        aria-expanded={expanded}
        onClick={onToggle}
      >
        <span className="wb-event-text">{label}</span>
        <span className="wb-event-chev" aria-hidden="true">
          {expanded ? "▴" : "▾"}
        </span>
      </button>

      {expanded && hasDetails && (
        <div className="wb-tool-group-body">
          {total > 0 && item.tools.length === 0 && toolsLoading && <div className="wb-tool-loading">Loading tools...</div>}
          {item.tools.map((tool) => (
            <WorkbenchToolRow
              key={tool.id}
              item={tool}
              verbosity={verbosity}
              expanded={expandedToolById[tool.id] ?? false}
              onToggle={() => onToggleTool(tool.id)}
            />
          ))}
          {item.thought.trim() && (
            <div className="wb-tool-thought">
              <div className="wb-tool-section-title">Thought</div>
              <pre className="wb-tool-pre">{item.thought}</pre>
            </div>
          )}
        </div>
      )}
    </div>
  );
});

function ToolCard({ item }: { item: Extract<ThreadItem, { kind: "tool" }> }) {
  const [expanded, setExpanded] = useState(false);
  const isRunning = item.status === "in_progress" || item.status === "pending";
  const isFailed = item.status === "failed";
  const hasOutput = item.output_text.trim().length > 0;
  const shouldDefaultOpen = item.tool_kind === "execute" && isRunning;
  const isOpen = expanded || shouldDefaultOpen;
  const summary = useMemo(
    () => String(item.subtitle ?? "").trim() || toolSummaryLine(item.tool_kind, item.input),
    [item.input, item.subtitle, item.tool_kind],
  );
  const path = item.locations?.length === 1 ? item.locations[0]?.path : null;
  const subtitleItems: ReactNode[] = [
    <span key="status" className={`pill ${isFailed ? "err" : isRunning ? "run" : "ok"}`}>
      {humanToolStatus(item.status)}
    </span>,
  ];
  if (path && !summary) subtitleItems.push(<span key="path" className="muted tool-path">{path}</span>);
  if (summary) subtitleItems.push(<span key="summary" className="muted tool-summary">{summary}</span>);

  return (
    <div className={`tool-card ${isOpen ? "expanded" : ""}`}>
      <button
        type="button"
        className="tool-header"
        onClick={() => setExpanded((e) => !e)}
        aria-expanded={isOpen}
        aria-controls={`tool-${item.id}`}
      >
        <div className="tool-header-left">
          <span className={`tool-icon kind-${item.tool_kind}`}>{toolKindIcon(item.tool_kind)}</span>
          <div className="tool-title-wrap">
            <div className="tool-title">{item.title}</div>
            <div className="tool-subtitle">
              {subtitleItems.map((node, idx) => (
                <Fragment key={idx}>
                  {idx > 0 && (
                    <span className="tool-status-dot" aria-hidden="true">
                      ·
                    </span>
                  )}
                  {node}
                </Fragment>
              ))}
            </div>
          </div>
        </div>
        <div className="tool-header-right">
          <span className="muted">{new Date(item.updated_at).toLocaleTimeString()}</span>
          <span className="thinking-chev">{isOpen ? "▴" : "▾"}</span>
        </div>
      </button>

      {isOpen && (
        <div id={`tool-${item.id}`} className="tool-body">
          {Boolean(item.input) && (
            <div className="tool-section">
              <div className="tool-section-title">Input</div>
              <pre className="tool-pre">{formatToolInput(item.tool_kind, item.input)}</pre>
            </div>
          )}
          {hasOutput && (
            <div className="tool-section">
              <div className="tool-section-title">Output</div>
              {looksLikeMarkdown(item.output_text) ? (
                <div className="tool-markdown">
                  <MemoMarkdown content={item.output_text} />
                </div>
              ) : (
                <pre className="tool-pre tool-output">{item.output_text}</pre>
              )}
            </div>
          )}
          <details className="tool-raw">
            <summary className="link">
              Raw event {item.updates_seen > 1 ? `(updated ${item.updates_seen}×)` : ""}
            </summary>
            <pre className="json">{JSON.stringify(item.raw, null, 2)}</pre>
          </details>
        </div>
      )}
    </div>
  );
}
