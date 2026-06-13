// Shared browser-authoritative measurement helpers for transcript parity,
// exact text measurement, and mounted row correction.
import React, { type ReactNode } from "react";
import ReactDOMClient from "react-dom/client";
import { flushSync } from "react-dom";
import type { MessageAttachment } from "../../api/client";
import { MemoMarkdown } from "../sessionView/SessionPage.markdown";
import type { WorkbenchListItem, WorkbenchTurnHeader } from "../sessionView/SessionPage.types";
import {
  AssistantEntry,
  ThreadItemView,
  WorkbenchTurnHeaderView,
} from "./SessionThreadItemViews";
import { SessionThreadMeasurementFrame } from "./SessionThreadMeasurementFrame";
import {
  SESSION_THREAD_LAYOUT_STYLE,
} from "./sessionThreadLayoutTokens";
import { SESSION_THREAD_ROW_MEASUREMENT_CONTRACT } from "./sessionThreadMeasurementContract";

const MEASUREMENT_CACHE_LIMIT = 4000;
const TURN_HEADER_PREVIEW_OUTER_WIDTH_CHROME_PX =
  SESSION_THREAD_ROW_MEASUREMENT_CONTRACT.turnHeader.copyGutterPx +
  SESSION_THREAD_ROW_MEASUREMENT_CONTRACT.turnHeader.bubblePaddingInlinePx * 2 +
  SESSION_THREAD_ROW_MEASUREMENT_CONTRACT.turnHeader.bubbleBorderWidthPx * 2;

type MeasurementSurfaceRecord = {
  container: HTMLDivElement;
  root: ReactDOMClient.Root;
};

let measurementSurface: MeasurementSurfaceRecord | null = null;

const markdownHeightCache = new Map<string, number>();
const rowHeightCache = new Map<string, number>();
const pendingDomMeasurementFallbackItemIds = new Set<string>();

const normalizeHeight = (value: number): number =>
  Number.isFinite(value) && value > 0 ? Math.max(1, Math.round(value * 16) / 16) : 0;

function pruneCache<T>(cache: Map<string, T>, limit: number): void {
  while (cache.size > limit) {
    const oldestKey = cache.keys().next().value;
    if (typeof oldestKey !== "string") break;
    cache.delete(oldestKey);
  }
}

function fingerprintString(value: string): string {
  const normalized = String(value ?? "");
  let hash = 2166136261;
  for (let index = 0; index < normalized.length; index += 1) {
    hash ^= normalized.charCodeAt(index);
    hash = Math.imul(hash, 16777619);
  }
  return `${normalized.length}:${(hash >>> 0).toString(36)}`;
}

function fingerprintAttachments(attachments: readonly MessageAttachment[] | undefined): string {
  return fingerprintString(
    JSON.stringify(
      (attachments ?? []).map((attachment) => ({
        kind: attachment.kind ?? "",
        name: attachment.name ?? "",
        mimeType: attachment.mime_type ?? "",
        blobId: "blob_id" in attachment ? attachment.blob_id ?? "" : "",
        dataLength: "data_base64" in attachment ? attachment.data_base64?.length ?? 0 : 0,
      })),
    ) ?? "",
  );
}

function ensureMeasurementSurface(): MeasurementSurfaceRecord | null {
  if (typeof document === "undefined" || !document.body) {
    return null;
  }
  if (measurementSurface) {
    return measurementSurface;
  }
  const container = document.createElement("div");
  container.setAttribute("data-session-thread-measurement-surface", "1");
  container.style.position = "fixed";
  container.style.left = "-10000px";
  container.style.top = "0";
  container.style.margin = "0";
  container.style.padding = "0";
  container.style.border = "0";
  container.style.visibility = "hidden";
  container.style.pointerEvents = "none";
  container.style.boxSizing = "border-box";
  document.body.appendChild(container);
  measurementSurface = {
    container,
    root: ReactDOMClient.createRoot(container),
  };
  return measurementSurface;
}

function renderIntoMeasurementSurface(params: {
  width: number;
  className?: string;
  content: ReactNode;
  measureSelector?: string;
}): number | null {
  const surface = ensureMeasurementSurface();
  if (!surface) {
    return null;
  }

  const wrapperStyle: React.CSSProperties = {
    width: `${Math.max(1, Math.round(params.width))}px`,
    margin: 0,
    padding: 0,
    border: 0,
    boxSizing: "border-box",
  };

  flushSync(() => {
    surface.root.render(
      React.createElement(
        "div",
        {
          className: params.className,
          style: wrapperStyle,
          ref: (element: HTMLDivElement | null) => {
            if (!element) return;
            for (const [key, value] of Object.entries(SESSION_THREAD_LAYOUT_STYLE)) {
              element.style.setProperty(key, String(value));
            }
          },
        },
        params.content,
      ),
    );
  });

  const target =
    (params.measureSelector
      ? surface.container.querySelector<HTMLElement>(params.measureSelector)
      : surface.container.firstElementChild) ?? null;
  const measuredHeight = target instanceof HTMLElement ? normalizeHeight(target.getBoundingClientRect().height) : 0;

  flushSync(() => {
    surface.root.render(null);
  });

  return measuredHeight > 0 ? measuredHeight : null;
}

function renderTranscriptRowIntoMeasurementSurface(params: {
  viewportWidth: number;
  itemId: string;
  content: ReactNode;
}): number | null {
  const normalizedViewportWidth = Math.max(1, Math.round(params.viewportWidth));
  return renderIntoMeasurementSurface({
    width: normalizedViewportWidth,
    content: React.createElement(
      SessionThreadMeasurementFrame,
      null,
      React.createElement(
        "div",
        {
          className: "wb-pretext-transcript-shell wb-thread-stack wb-thread-scroller--message-list",
          style: { position: "relative", minWidth: 0 },
        },
        React.createElement(
          "div",
          {
            className: "wb-thread-scroller",
            role: "list",
            "data-pretext-virtualizer-list": "1",
            style: {
              position: "relative",
              width: "auto",
              minWidth: 0,
              overflowX: "hidden",
              overflowY: "auto",
            },
          },
          React.createElement(
            "div",
            {
              className: "wb-thread-list",
              "data-pretext-virtualizer-content": "1",
              style: { position: "relative", height: "auto" },
            },
            React.createElement(
              "div",
              { "data-pretext-virtualizer-row-shell": "1", style: { position: "relative", width: "100%" } },
              React.createElement(
                "div",
                {
                  className: "wb-pretext-virtualizer-row",
                  "data-pretext-virtualizer-row": "1",
                  "data-pretext-virtualizer-item-id": params.itemId,
                },
                React.createElement(
                  "div",
                  { role: "listitem", "data-thread-item-id": params.itemId },
                  params.content,
                ),
              ),
            ),
          ),
        ),
      ),
    ),
    measureSelector: `[role="listitem"][data-thread-item-id="${params.itemId}"]`,
  });
}

function readCachedMeasurement(
  cache: Map<string, number>,
  cacheKey: string,
  measure: () => number | null,
): number | null {
  const cached = cache.get(cacheKey);
  if (cached != null) {
    return cached;
  }
  const measured = measure();
  if (measured == null) {
    return null;
  }
  cache.set(cacheKey, measured);
  pruneCache(cache, MEASUREMENT_CACHE_LIMIT);
  return measured;
}

export function clearSessionThreadDebugDomAuditCaches(): void {
  markdownHeightCache.clear();
  rowHeightCache.clear();
  if (measurementSurface) {
    flushSync(() => {
      measurementSurface?.root.render(null);
    });
    measurementSurface.container.remove();
    measurementSurface = null;
  }
}

export function clearSessionThreadExactMeasurementCaches(): void {
  clearSessionThreadDebugDomAuditCaches();
}

export function clearSessionThreadDebugDomAuditFallbacks(): void {
  pendingDomMeasurementFallbackItemIds.clear();
}

export function consumeSessionThreadDebugDomAuditItemIds(
  candidateItemIds?: readonly string[],
): string[] {
  if (!candidateItemIds) {
    const pending = [...pendingDomMeasurementFallbackItemIds];
    pendingDomMeasurementFallbackItemIds.clear();
    return pending;
  }
  const pending: string[] = [];
  for (const itemId of candidateItemIds) {
    if (!pendingDomMeasurementFallbackItemIds.delete(itemId)) continue;
    pending.push(itemId);
  }
  return pending;
}

export function measureDebugRenderedSessionMarkdownHeight(markdown: string, width: number): number | null {
  const normalizedWidth = Math.max(1, Math.round(width));
  const cacheKey = `markdown:${normalizedWidth}:${fingerprintString(markdown)}`;
  return readCachedMeasurement(markdownHeightCache, cacheKey, () =>
    renderIntoMeasurementSurface({
      width: normalizedWidth,
      className: "wb-assistant-body",
      content: React.createElement(MemoMarkdown, { content: markdown }),
      measureSelector: ".wb-markdown-root",
    }),
  );
}

export function measureRenderedSessionMarkdownHeight(markdown: string, width: number): number | null {
  return measureDebugRenderedSessionMarkdownHeight(markdown, width);
}

export function measureRenderedSessionPlainTextBlockHeight(text: string, width: number): number | null {
  const normalizedWidth = Math.max(1, Math.round(width));
  const cacheKey = `plain-text:${normalizedWidth}:${fingerprintString(text)}`;
  return readCachedMeasurement(markdownHeightCache, cacheKey, () =>
    renderIntoMeasurementSurface({
      width: normalizedWidth,
      content: React.createElement(
        "div",
        {
          className: "wb-markdown-root wb-message-plain-text",
        },
        text,
      ),
      measureSelector: ".wb-message-plain-text",
    }),
  );
}

export function measureRenderedSessionTurnHeaderPreviewTextHeight(params: {
  text: string;
  width: number;
  collapsedMaxHeightPx: number;
  expanded: boolean;
}): number | null {
  const normalizedTextWidth = Math.max(1, Math.round(params.width));
  const outerWidth = normalizedTextWidth + TURN_HEADER_PREVIEW_OUTER_WIDTH_CHROME_PX;
  const cacheKey = [
    "turn-header-preview",
    outerWidth,
    params.expanded ? "expanded" : "collapsed",
    params.collapsedMaxHeightPx,
    fingerprintString(params.text),
  ].join(":");
  return readCachedMeasurement(markdownHeightCache, cacheKey, () =>
    renderIntoMeasurementSurface({
      width: outerWidth,
      content: React.createElement(
        SessionThreadMeasurementFrame,
        null,
        React.createElement(
          "div",
          {
            className: `wb-turn-header ${params.expanded ? "wb-turn-header-expanded" : "wb-turn-header-collapsed"}`,
            style: {
              ["--wb-turn-header-collapsed-max-height" as "--wb-turn-header-collapsed-max-height"]:
                `${params.collapsedMaxHeightPx}px`,
            } as React.CSSProperties,
          },
          React.createElement(
            "div",
            { className: "wb-turn-header-bubble" },
            React.createElement(
              "button",
              {
                type: "button",
                className: "wb-turn-header-copy",
                "aria-hidden": "true",
                tabIndex: -1,
              },
            ),
            React.createElement("div", { className: "wb-turn-header-content" }, params.text),
          ),
        ),
      ),
      measureSelector: ".wb-turn-header-content",
    }),
  );
}

export function measureRenderedSessionMessageHeight(
  item: Extract<WorkbenchListItem, { kind: "message" }>,
  viewportWidth: number,
  expanded: boolean,
): number | null {
  const normalizedViewportWidth = Math.max(1, Math.round(viewportWidth));
  const cacheKey = [
    "message",
    normalizedViewportWidth,
    expanded ? "expanded" : "collapsed",
    item.role,
    fingerprintString(item.content),
    fingerprintAttachments(item.attachments),
  ].join(":");
  return readCachedMeasurement(rowHeightCache, cacheKey, () => {
    const measured = renderTranscriptRowIntoMeasurementSurface({
      viewportWidth: normalizedViewportWidth,
      itemId: item.id,
      content: React.createElement(
        "div",
        { className: "wb-thread-indent" },
        React.createElement(ThreadItemView, {
          item,
          worktreeId: null,
          onFileOpenError: () => {},
          messageExpanded: expanded,
          onToggleMessageExpanded: () => {},
        }),
      ),
    });
    if (measured == null) {
      pendingDomMeasurementFallbackItemIds.add(item.id);
    }
    return measured;
  });
}

export function measureDebugRenderedSessionAssistantHeight(
  item: Extract<WorkbenchListItem, { kind: "assistant" }>,
  viewportWidth: number,
): number | null {
  const normalizedViewportWidth = Math.max(1, Math.round(viewportWidth));
  const cacheKey = [
    "assistant",
    normalizedViewportWidth,
    item.is_complete ? "complete" : "partial",
    fingerprintString(item.content),
  ].join(":");
  return readCachedMeasurement(rowHeightCache, cacheKey, () => {
    const measured = renderTranscriptRowIntoMeasurementSurface({
      viewportWidth: normalizedViewportWidth,
      itemId: item.id,
      content: React.createElement(
        "div",
        { className: "wb-thread-indent" },
        React.createElement(AssistantEntry, {
          content: item.content,
          worktreeId: null,
          onFileOpenError: () => {},
        }),
      ),
    });
    if (measured == null) {
      pendingDomMeasurementFallbackItemIds.add(item.id);
    }
    return measured;
  });
}

export function measureRenderedSessionAssistantHeight(
  item: Extract<WorkbenchListItem, { kind: "assistant" }>,
  viewportWidth: number,
): number | null {
  return measureDebugRenderedSessionAssistantHeight(item, viewportWidth);
}

export function measureRenderedSessionTurnHeaderHeight(
  header: WorkbenchTurnHeader,
  plainText: string,
  expanded: boolean,
  viewportWidth: number,
): number | null {
  const normalizedViewportWidth = Math.max(1, Math.round(viewportWidth));
  const cacheKey = [
    "turn-header",
    normalizedViewportWidth,
    expanded ? "expanded" : "collapsed",
    fingerprintString(plainText),
    fingerprintAttachments(header.attachments),
  ].join(":");
  return readCachedMeasurement(rowHeightCache, cacheKey, () => {
    const itemId = `turn-header-${header.id}`;
    const measured = renderTranscriptRowIntoMeasurementSurface({
      viewportWidth: normalizedViewportWidth,
      itemId,
      content: React.createElement(
        "div",
        { style: { display: "contents" } },
        React.createElement(WorkbenchTurnHeaderView, {
          header,
          plainText,
          expanded,
          onToggle: () => {},
        }),
      ),
    });
    if (measured == null) {
      pendingDomMeasurementFallbackItemIds.add(itemId);
    }
    return measured;
  });
}
