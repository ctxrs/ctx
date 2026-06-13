import React from "react";
import ReactDOMClient from "react-dom/client";
import { flushSync } from "react-dom";
import type { MessageAttachment } from "../../api/client";
import { SessionThreadPretextVirtualizerList } from "../SessionThreadMessageList.pretextVirtualizer";
import type { WorkbenchMessageListContext } from "../SessionPage.thread";
import {
  AssistantEntry,
  ThreadItemView,
  WorkbenchTurnHeaderView,
} from "../sessionThread/SessionThreadItemViews";
import { SessionThreadMeasurementFrame } from "../sessionThread/SessionThreadMeasurementFrame";
import {
  clearSessionThreadDebugDomAuditFallbacks,
  clearSessionThreadDebugDomAuditCaches,
  measureDebugRenderedSessionAssistantHeight,
} from "../sessionThread/sessionThreadDomMeasurement";
import {
  resetSessionPretextRuntimeCache,
} from "../sessionThread/pretextSessionRuntimeCache";
import { clearTranscriptLayoutPlannerCaches } from "../sessionThread/transcriptLayoutPlanner.app";
import { getWorkbenchTurnHeaderLayoutState } from "../sessionThread/transcriptRowLayoutModel";
import {
  SESSION_THREAD_LAYOUT_STYLE,
} from "../sessionThread/sessionThreadLayoutTokens";
import type { WorkbenchListItem, WorkbenchTurnHeader } from "../sessionView";

export type WorkbenchRowParityMeasurement = {
  planned: number;
  actual: number;
  delta: number;
  viewportWidth?: number;
  rowWidth?: number;
  debugDomMeasured?: number;
  debugDomDelta?: number;
};

export type WorkbenchMessageParityParams = {
  content: string;
  expanded: boolean;
  attachments?: MessageAttachment[];
  viewportWidth?: number;
};

export type WorkbenchAssistantParityParams = {
  content: string;
  isComplete?: boolean;
  viewportWidth?: number;
};

export type WorkbenchAssistantStreamingParityParams = {
  fragments: readonly string[];
  viewportWidth?: number;
};

export type WorkbenchAssistantStreamingParityStep = {
  content: string;
  partial: WorkbenchRowParityMeasurement;
  complete: WorkbenchRowParityMeasurement;
  actualDelta: number;
  plannedDelta: number;
  structureEquivalent: boolean;
};

export type WorkbenchAssistantStreamingParityMeasurement = {
  steps: WorkbenchAssistantStreamingParityStep[];
};

export type WorkbenchTurnHeaderParityParams = {
  content: string;
  viewportWidth?: number;
};

const LIVE_ROW_VIEWPORT_HEIGHT_PX = 1400;
let transcriptProbeCounter = 0;

function applyTranscriptLayoutStyle(host: HTMLElement, viewportWidth: number): void {
  host.style.position = "fixed";
  host.style.left = "-10000px";
  host.style.top = "0";
  host.style.width = `${Math.max(1, Math.round(viewportWidth))}px`;
  host.style.height = `${LIVE_ROW_VIEWPORT_HEIGHT_PX}px`;
  host.style.margin = "0";
  host.style.padding = "0";
  host.style.border = "0";
  host.style.boxSizing = "border-box";
  host.style.visibility = "hidden";
  host.style.pointerEvents = "none";
  for (const [key, value] of Object.entries(SESSION_THREAD_LAYOUT_STYLE)) {
    host.style.setProperty(key, String(value));
  }
}

function makeParityMeasurement(planned: number, actual: number): WorkbenchRowParityMeasurement {
  return {
    planned,
    actual,
    delta: planned - actual,
  };
}

type AssistantRenderSnapshot = {
  measurement: WorkbenchRowParityMeasurement;
  structureSignature: string;
};

function createBaseContext(): WorkbenchMessageListContext {
  return {
    loaded: true,
    loadingOlder: false,
    renderRevision: "e2e-live-row",
    renderRevisionByItemId: {},
    expandedTurnHeaders: {},
    expandedTurnDetailsById: {},
    expandedToolById: {},
    expandedMessageById: {},
    turnToolsLoading: [],
  };
}

function waitForAnimationFrame(): Promise<void> {
  return new Promise((resolve) => {
    requestAnimationFrame(() => resolve());
  });
}

async function waitForTranscriptProbeCommit(): Promise<void> {
  await waitForAnimationFrame();
  await waitForAnimationFrame();
}

async function waitForMountedRow(host: HTMLElement, itemId: string): Promise<{
  row: HTMLElement | null;
  shell: HTMLElement | null;
}> {
  for (let attempt = 0; attempt < 6; attempt += 1) {
    const row = host.querySelector<HTMLElement>(`[role="listitem"][data-thread-item-id="${itemId}"]`);
    const shell = host.querySelector<HTMLElement>(`[data-pretext-virtualizer-item-id="${itemId}"]`);
    if (row && shell) {
      return { row, shell };
    }
    await waitForAnimationFrame();
  }
  return {
    row: host.querySelector<HTMLElement>(`[role="listitem"][data-thread-item-id="${itemId}"]`),
    shell: host.querySelector<HTMLElement>(`[data-pretext-virtualizer-item-id="${itemId}"]`),
  };
}

async function waitForMountedRowToSettle(host: HTMLElement, itemId: string): Promise<void> {
  for (let attempt = 0; attempt < 12; attempt += 1) {
    const row = host.querySelector<HTMLElement>(`[role="listitem"][data-thread-item-id="${itemId}"]`);
    const shell = host.querySelector<HTMLElement>(`[data-pretext-virtualizer-item-id="${itemId}"]`);
    const planned = Number(shell?.getAttribute("data-pretext-virtualizer-planned-height") ?? Number.NaN);
    const actual = row?.getBoundingClientRect().height ?? 0;
    if (row && shell && Number.isFinite(planned) && Math.abs(planned - actual) <= 1) {
      return;
    }
    await waitForAnimationFrame();
  }
}

async function measureMountedTranscriptRowSnapshot<Item extends WorkbenchListItem>(params: {
  item: Item;
  viewportWidth: number;
  context: WorkbenchMessageListContext;
  itemContent: (item: Item) => React.ReactNode;
}): Promise<AssistantRenderSnapshot> {
  const host = document.createElement("div");
  applyTranscriptLayoutStyle(host, params.viewportWidth);
  document.body.appendChild(host);
  const root = ReactDOMClient.createRoot(host);
  const sessionId = `e2e-row-probe-${transcriptProbeCounter += 1}`;
  clearTranscriptLayoutPlannerCaches();
  clearSessionThreadDebugDomAuditFallbacks();
  clearSessionThreadDebugDomAuditCaches();
  resetSessionPretextRuntimeCache();

  try {
    flushSync(() => {
      root.render(
        React.createElement(
          SessionThreadMeasurementFrame,
          { fillHeight: true },
          React.createElement(SessionThreadPretextVirtualizerList, {
            style: { height: `${LIVE_ROW_VIEWPORT_HEIGHT_PX}px`, width: "100%" },
            sessionId,
            isActive: true,
            listItems: [params.item],
            threadProjectionOp: {
              kind: "replace_session",
              projectionRevision: 1,
              changedItemIds: [params.item.id],
              remeasureItemIds: [params.item.id],
            },
            initialLocation: { index: 0, align: "start" },
            itemContent: (_index: number, item: WorkbenchListItem) => params.itemContent(item as Item),
            itemKey: (item: WorkbenchListItem) => item.id,
            context: params.context,
            shortSizeAlign: "top",
          }),
        ),
      );
    });

    await waitForTranscriptProbeCommit();
    await waitForMountedRowToSettle(host, params.item.id);

    const { row, shell } = await waitForMountedRow(host, params.item.id);
    const scroller = host.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    const planned = Number(shell?.getAttribute("data-pretext-virtualizer-planned-height") ?? Number.NaN);
    const actual = row?.getBoundingClientRect().height ?? 0;
    const viewportWidth = scroller?.getBoundingClientRect().width ?? 0;
    const rowWidth = row?.getBoundingClientRect().width ?? 0;
    const structureSignature = row?.querySelector<HTMLElement>(".wb-markdown-root")?.innerHTML ?? "";

    return {
      measurement: {
        ...makeParityMeasurement(
          Number.isFinite(planned) && planned > 0 ? planned : 0,
          actual,
        ),
        viewportWidth,
        rowWidth,
      },
      structureSignature,
    };
  } finally {
    root.unmount();
    host.remove();
    clearSessionThreadDebugDomAuditFallbacks();
    clearSessionThreadDebugDomAuditCaches();
    resetSessionPretextRuntimeCache();
    clearTranscriptLayoutPlannerCaches();
  }
}

export async function measureWorkbenchMessageParity(
  params: WorkbenchMessageParityParams,
): Promise<WorkbenchRowParityMeasurement> {
  const viewportWidth = params.viewportWidth ?? 820;
  const item: Extract<WorkbenchListItem, { kind: "message" }> = {
    kind: "message",
    id: "message-parity",
    role: "user",
    content: params.content,
    attachments: params.attachments ?? [],
    created_at: "2026-04-09T00:00:00Z",
  };

  const context = createBaseContext();
  context.expandedMessageById = { [item.id]: params.expanded };
  return (
    await measureMountedTranscriptRowSnapshot({
      item,
      viewportWidth,
      context,
      itemContent: (currentItem) =>
        React.createElement(
          "div",
          { className: "wb-thread-indent", "data-thread-item-id": currentItem.id },
          React.createElement(ThreadItemView, {
            item: currentItem,
            worktreeId: null,
            onFileOpenError: () => {},
            messageExpanded: params.expanded,
            onToggleMessageExpanded: () => {},
          }),
        ),
    })
  ).measurement;
}

export async function measureWorkbenchAssistantParity(
  params: WorkbenchAssistantParityParams,
): Promise<WorkbenchRowParityMeasurement> {
  const viewportWidth = params.viewportWidth ?? 820;
  const item: Extract<WorkbenchListItem, { kind: "assistant" }> = {
    kind: "assistant",
    id: "assistant-parity",
    turn_id: "turn-1",
    created_at: "2026-04-09T00:00:00Z",
    content: params.content,
    thought: "",
    is_complete: params.isComplete ?? true,
  };

  const snapshot = await measureMountedTranscriptRowSnapshot({
    item,
    viewportWidth,
    context: createBaseContext(),
    itemContent: (currentItem) =>
      React.createElement(
        "div",
        { className: "wb-thread-indent", "data-thread-item-id": currentItem.id },
        React.createElement(AssistantEntry, {
          content: currentItem.content,
          worktreeId: null,
          onFileOpenError: () => {},
        }),
      ),
  });
  const debugDomMeasured = measureDebugRenderedSessionAssistantHeight(
    item,
    snapshot.measurement.viewportWidth ?? viewportWidth,
  );
  return {
    ...snapshot.measurement,
    debugDomMeasured: debugDomMeasured ?? undefined,
    debugDomDelta: debugDomMeasured != null ? debugDomMeasured - snapshot.measurement.actual : undefined,
  };
}

export async function measureWorkbenchAssistantStreamingParity(
  params: WorkbenchAssistantStreamingParityParams,
): Promise<WorkbenchAssistantStreamingParityMeasurement> {
  const viewportWidth = params.viewportWidth ?? 820;
  const steps: WorkbenchAssistantStreamingParityStep[] = [];
  let content = "";

  for (let index = 0; index < params.fragments.length; index += 1) {
    content += params.fragments[index] ?? "";
    if (content.length === 0) {
      continue;
    }
    const partialItem: Extract<WorkbenchListItem, { kind: "assistant" }> = {
      kind: "assistant",
      id: "assistant-streaming-partial",
      turn_id: "turn-1",
      created_at: "2026-04-09T00:00:00Z",
      content,
      thought: "",
      is_complete: false,
    };
    const completeItem: Extract<WorkbenchListItem, { kind: "assistant" }> = {
      ...partialItem,
      id: "assistant-streaming-complete",
      is_complete: true,
    };
    const partial = await measureMountedTranscriptRowSnapshot({
      item: partialItem,
      viewportWidth,
      context: createBaseContext(),
      itemContent: (currentItem) =>
        React.createElement(
          "div",
          { className: "wb-thread-indent", "data-thread-item-id": currentItem.id },
          React.createElement(AssistantEntry, {
            content: currentItem.content,
            worktreeId: null,
            onFileOpenError: () => {},
          }),
        ),
    });
    const complete = await measureMountedTranscriptRowSnapshot({
      item: completeItem,
      viewportWidth,
      context: createBaseContext(),
      itemContent: (currentItem) =>
        React.createElement(
          "div",
          { className: "wb-thread-indent", "data-thread-item-id": currentItem.id },
          React.createElement(AssistantEntry, {
            content: currentItem.content,
            worktreeId: null,
            onFileOpenError: () => {},
          }),
        ),
    });
    steps.push({
      content,
      partial: partial.measurement,
      complete: complete.measurement,
      actualDelta: partial.measurement.actual - complete.measurement.actual,
      plannedDelta: partial.measurement.planned - complete.measurement.planned,
      structureEquivalent: partial.structureSignature === complete.structureSignature,
    });
  }
  return { steps };
}

export async function measureWorkbenchTurnHeaderParity(
  params: WorkbenchTurnHeaderParityParams,
): Promise<WorkbenchRowParityMeasurement> {
  const viewportWidth = params.viewportWidth ?? 820;
  const header: WorkbenchTurnHeader = {
    id: "turn-header-parity",
    content: params.content,
    attachments: [],
    created_at: "2026-04-10T00:00:00Z",
  };
  const item: Extract<WorkbenchListItem, { kind: "turn_header" }> = {
    kind: "turn_header",
    id: "turn-header-parity-row",
    header,
  };
  const layout = getWorkbenchTurnHeaderLayoutState(item, { [header.id]: true });

  const context = createBaseContext();
  context.expandedTurnHeaders = { [header.id]: true };
  return (
    await measureMountedTranscriptRowSnapshot({
      item,
      viewportWidth,
      context,
      itemContent: () =>
        React.createElement(
          "div",
          { style: { display: "contents" }, "data-thread-item-id": item.id },
          React.createElement(WorkbenchTurnHeaderView, {
            header,
            plainText: layout.displayPlainText,
            expanded: layout.expanded,
            onToggle: () => {},
          }),
        ),
    })
  ).measurement;
}
