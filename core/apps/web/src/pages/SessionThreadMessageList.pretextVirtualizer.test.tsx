// @vitest-environment jsdom

import { act, fireEvent, render, screen } from "@testing-library/react";
import { createRef, useEffect, useState } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { PretextVirtualizerListMethods } from "@pretext-virtualizer/interface";
import { SessionThreadPretextVirtualizerList } from "./SessionThreadMessageList.pretextVirtualizer";
import type { WorkbenchListItem } from "./SessionPage.types";
import type { WorkbenchMessageListContext } from "./SessionPage.thread";
import type { WorkbenchThreadProjectionOp } from "./sessionThreadProjection";
import { classifyWorkbenchThreadProjectionOp } from "./sessionThreadProjection";
import { WorkbenchTurnHeaderView } from "./sessionThread/SessionThreadItemViews";
import {
  createDefaultSessionTranscriptUiState,
  getOrCreateSessionPretextRuntime,
  noteSessionPretextRuntimeSnapshot,
  primeSessionPretextRuntime,
  readSessionPretextRuntimePreparedState,
  resetSessionPretextRuntimeCache,
} from "./sessionThread/pretextSessionRuntimeCache";
import {
  noteSessionTranscriptWarmViewport,
  resetSessionTranscriptWarmStateForTests,
} from "./sessionThread/sessionTranscriptWarmState";
import { defaultTranscriptLayoutPlanner } from "./sessionThread/transcriptLayoutPlanner.app";
import { getWorkbenchTurnHeaderLayoutState } from "./sessionThread/transcriptRowLayoutModel";

const resizeObserverInstances: Array<{ callback: ResizeObserverCallback }> = [];

class ResizeObserverStub {
  callback: ResizeObserverCallback;

  constructor(callback: ResizeObserverCallback) {
    this.callback = callback;
    resizeObserverInstances.push({ callback });
  }

  observe(): void {}

  disconnect(): void {}
}

const context: WorkbenchMessageListContext = {
  loaded: true,
  loadingOlder: false,
  expandedTurnHeaders: {},
  expandedTurnDetailsById: {},
  expandedToolById: {},
  expandedMessageById: {},
  turnToolsLoading: [],
  verbosity: "default",
};

const makeItems = (count = 2): WorkbenchListItem[] =>
  Array.from({ length: count }, (_, index) => ({
    kind: "message" as const,
    id: `message-${index + 1}`,
    role: index % 2 === 0 ? ("user" as const) : ("assistant" as const),
    content: `message ${index + 1}`,
    attachments: [],
    created_at: `2026-03-17T00:0${index}:00Z`,
  }));

const makeWrappingItems = (count = 12): WorkbenchListItem[] =>
  Array.from({ length: count }, (_, index) => ({
    kind: "message" as const,
    id: `wrapping-message-${index + 1}`,
    role: index % 2 === 0 ? ("user" as const) : ("assistant" as const),
    content: `${`wrapping message ${index + 1} `.repeat(32)}end`,
    attachments: [],
    created_at: `2026-03-18T00:${String(index).padStart(2, "0")}:00Z`,
  }));

const makeMessageRange = (prefix: string, count: number, startAt = 1): WorkbenchListItem[] =>
  Array.from({ length: count }, (_, index) => ({
    kind: "message" as const,
    id: `${prefix}-${startAt + index}`,
    role: index % 2 === 0 ? ("user" as const) : ("assistant" as const),
    content: `${prefix} message ${startAt + index}`,
    attachments: [],
    created_at: `2026-03-18T01:${String(index).padStart(2, "0")}:00Z`,
  }));

const EXPANDABLE_TURN_HEADER_TEXT = [
  "oh my god i have another idea, might be great",
  "",
  "we could ask the model to read the article and then write 10 comments as HN users.",
  "",
  "and then we model “were these comments the same type of comments you get on front page or are they not”",
  "",
  "for example if comments are like “wow thanks for promotional garbage” or “this was a very poor article” that might be super useful to us.",
  "",
  "what do you think about other creative ideas like that",
].join("\n");

const makePendingAssistantItem = (content: string): WorkbenchListItem => ({
  kind: "assistant",
  id: "assistant-turn-1-pending",
  turn_id: "turn-1",
  created_at: "2026-04-08T00:00:00Z",
  content,
  thought: "",
  is_complete: false,
});

const makeTurnStatusItem = (): WorkbenchListItem => ({
  kind: "turn_status",
  id: "turn-status-turn-1",
  turn_id: "turn-1",
  created_at: "2026-04-08T00:00:01Z",
  started_at: "2026-04-08T00:00:00Z",
  updated_at: "2026-04-08T00:00:01Z",
  status: "running",
  assistant_messages_content: "",
});

function InteractiveTurnHeaderHarness({
  sessionId,
  listItems,
  projectionKind = "toggle_expansion",
}: {
  sessionId: string;
  listItems: WorkbenchListItem[];
  projectionKind?: WorkbenchThreadProjectionOp["kind"];
}) {
  const [expandedTurnHeaders, setExpandedTurnHeaders] = useState<Record<string, boolean>>({});
  const projectionOp: WorkbenchThreadProjectionOp = {
    kind: projectionKind,
    projectionRevision: Object.keys(expandedTurnHeaders).length,
    changedItemIds: ["turn-header-live-regression"],
    remeasureItemIds: [
      "turn-header-live-regression",
      "assistant-live-regression",
      "turn-status-live-regression",
    ],
  };
  return (
    <SessionThreadPretextVirtualizerList
      style={{ height: 400 }}
      sessionId={sessionId}
      isActive
      listItems={listItems}
      threadProjectionOp={projectionOp}
      itemContent={(_, item) => {
        if (item.kind === "turn_header") {
          const layout = getWorkbenchTurnHeaderLayoutState(item, expandedTurnHeaders);
          return (
            <WorkbenchTurnHeaderView
              header={item.header}
              plainText={layout.displayPlainText}
              expanded={layout.expanded}
              onToggle={() =>
                setExpandedTurnHeaders((prev) => ({ ...prev, [item.header.id]: !layout.expanded }))
              }
            />
          );
        }
        return <div>{item.id}</div>;
      }}
      itemKey={(item) => item.id}
      context={{ ...context, expandedTurnHeaders }}
    />
  );
}

const noopProjectionOp: WorkbenchThreadProjectionOp = {
  kind: "noop",
  projectionRevision: 0,
  changedItemIds: [],
  remeasureItemIds: [],
};

function defineScrollerMetrics(scroller: HTMLElement, metrics: { clientHeight: number; clientWidth: number; scrollHeight: number }) {
  Object.defineProperty(scroller, "clientHeight", { configurable: true, value: metrics.clientHeight });
  Object.defineProperty(scroller, "clientWidth", { configurable: true, value: metrics.clientWidth });
  Object.defineProperty(scroller, "scrollHeight", { configurable: true, value: metrics.scrollHeight, writable: true });
}

function defineTrackMetrics(track: HTMLElement, metrics: { clientHeight: number; top?: number }) {
  Object.defineProperty(track, "clientHeight", { configurable: true, value: metrics.clientHeight });
  Object.defineProperty(track, "getBoundingClientRect", {
    configurable: true,
    value: () =>
      ({
        top: metrics.top ?? 0,
        left: 0,
        right: 0,
        bottom: (metrics.top ?? 0) + metrics.clientHeight,
        width: 6,
        height: metrics.clientHeight,
        x: 0,
        y: metrics.top ?? 0,
        toJSON: () => undefined,
      }) satisfies DOMRect,
  });
}

function getRenderedItemIds(container: HTMLElement): string[] {
  return Array.from(
    container.querySelectorAll<HTMLElement>("[data-pretext-virtualizer-row='1'][data-pretext-virtualizer-item-id]"),
  )
    .map((row) => row.getAttribute("data-pretext-virtualizer-item-id"))
    .filter((id): id is string => Boolean(id));
}

describe("SessionThreadPretextVirtualizerList", () => {
  beforeEach(() => {
    resizeObserverInstances.length = 0;
    resetSessionPretextRuntimeCache();
    resetSessionTranscriptWarmStateForTests();
    Object.defineProperty(globalThis, "requestAnimationFrame", {
      configurable: true,
      value: (callback: FrameRequestCallback) => {
        callback(0);
        return 1;
      },
    });
    Object.defineProperty(globalThis, "cancelAnimationFrame", {
      configurable: true,
      value: () => undefined,
    });
    Object.defineProperty(globalThis, "ResizeObserver", {
      configurable: true,
      value: ResizeObserverStub,
    });
  });

  it("renders a deterministic transcript surface and exposes imperative methods", () => {
    const methodsRef = createRef<PretextVirtualizerListMethods<WorkbenchListItem, WorkbenchMessageListContext> | null>();

    const { container } = render(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId="session-1"
        isActive
        listItems={makeItems()}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
        methodsRef={methodsRef}
      />,
    );

    const scroller = container.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    expect(scroller).not.toBeNull();
    expect(methodsRef.current).not.toBeNull();
    expect(typeof methodsRef.current?.scrollToBottom).toBe("function");
    expect(container.querySelectorAll("[data-pretext-virtualizer-row='1']").length).toBeGreaterThan(0);
  });

  it("keeps normal runtime free of per-row mismatch observers and bounds the rendered window", () => {
    const { container } = render(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId="session-bounded-window"
        isActive
        listItems={makeItems(200)}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    expect(resizeObserverInstances).toHaveLength(1);

    const scroller = container.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    expect(scroller).not.toBeNull();
    defineScrollerMetrics(scroller!, { clientHeight: 400, clientWidth: 900, scrollHeight: 16000 });

    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
      scroller!.scrollTop = 6000;
      fireEvent.scroll(scroller!);
    });

    expect(resizeObserverInstances).toHaveLength(1);
    expect(getRenderedItemIds(container).length).toBeLessThan(40);
  });

  it("hydrates the initial snapshot from the warm viewport when the prepared runtime is still empty", () => {
    const sessionId = "session-warm-open";
    const runtime = getOrCreateSessionPretextRuntime(sessionId);
    noteSessionTranscriptWarmViewport({ width: 900, height: 300 });
    noteSessionPretextRuntimeSnapshot(runtime, runtime.core.getSnapshot(), []);

    const { container } = render(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId={sessionId}
        isActive
        listItems={makeWrappingItems(6)}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    const shells = container.querySelectorAll("[data-pretext-virtualizer-row-shell='1']");
    expect(shells.length).toBeGreaterThan(0);
  });

  it("restores the current bottom snapshot when an inactive session becomes active", () => {
    const sessionId = "session-activation-refresh";
    const initialItems = makeItems(4);
    const updatedItems: WorkbenchListItem[] = initialItems.map((item) =>
      item.kind === "message" && item.id === "message-4"
        ? { ...item, content: `${item.content} marker` }
        : item,
    );
    const uiState = createDefaultSessionTranscriptUiState();

    function Harness({
      isActive,
      listItems,
    }: {
      isActive: boolean;
      listItems: WorkbenchListItem[];
    }) {
      return (
        <SessionThreadPretextVirtualizerList
          style={{ height: 240 }}
          sessionId={sessionId}
          isActive={isActive}
          listItems={listItems}
          threadProjectionOp={noopProjectionOp}
          itemContent={(_, item) => <div>{item.kind === "message" ? item.content : item.id}</div>}
          itemKey={(item) => item.id}
          context={context}
        />
      );
    }

    const { container, rerender } = render(<Harness isActive={false} listItems={initialItems} />);
    const scroller = container.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    expect(scroller).not.toBeNull();
    defineScrollerMetrics(scroller!, { clientHeight: 240, clientWidth: 900, scrollHeight: 240 });

    act(() => {
      primeSessionPretextRuntime({
        sessionId,
        listItems: updatedItems,
        uiState,
        viewportWidth: 900,
        viewportHeight: 240,
      });
    });

    rerender(<Harness isActive listItems={updatedItems} />);

    expect(container.textContent ?? "").toContain("marker");
  });

  it("shows the jump-to-latest control when detached from bottom", () => {
    const { container } = render(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId="session-1"
        isActive
        listItems={makeItems(20)}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    const scroller = container.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    if (!scroller) throw new Error("Expected transcript scroller");
    defineScrollerMetrics(scroller, { clientHeight: 300, clientWidth: 900, scrollHeight: 1400 });

    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
      scroller.scrollTop = 900;
      fireEvent.scroll(scroller);
      fireEvent.wheel(scroller, { deltaY: -120 });
      scroller.scrollTop = 420;
      fireEvent.scroll(scroller);
    });

    expect(screen.getByRole("button", { name: "Jump to latest" })).toBeInTheDocument();
  });

  it("restores the custom scrollbar when the transcript overflows", () => {
    const { container } = render(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId="session-1"
        isActive
        listItems={makeItems(20)}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    const scroller = container.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    const track = container.querySelector<HTMLElement>(".wb-scrollbar-track");
    const scrollbar = container.querySelector<HTMLElement>(".wb-scrollbar");
    const thumb = container.querySelector<HTMLElement>(".wb-scrollbar-thumb");
    if (!scroller || !track || !scrollbar || !thumb) {
      throw new Error("Expected transcript scrollbar elements");
    }

    defineScrollerMetrics(scroller, { clientHeight: 300, clientWidth: 900, scrollHeight: 1400 });
    defineTrackMetrics(track, { clientHeight: 300, top: 0 });

    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
      fireEvent.wheel(scroller, { deltaY: 120 });
      scroller.scrollTop = 420;
      fireEvent.scroll(scroller);
    });

    expect(scrollbar.classList.contains("is-hidden")).toBe(false);
    expect(scrollbar.classList.contains("is-active")).toBe(true);
    expect(thumb.style.height).not.toBe("");
    expect(thumb.style.transform).toContain("translateY");
  });

  it("scrolls when clicking the restored scrollbar track", () => {
    const { container } = render(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId="session-1"
        isActive
        listItems={makeItems(20)}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    const scroller = container.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    const track = container.querySelector<HTMLElement>(".wb-scrollbar-track");
    if (!scroller || !track) {
      throw new Error("Expected transcript scrollbar elements");
    }

    defineScrollerMetrics(scroller, { clientHeight: 300, clientWidth: 900, scrollHeight: 1400 });
    defineTrackMetrics(track, { clientHeight: 300, top: 0 });

    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
    });

    act(() => {
      fireEvent.pointerDown(track, { button: 0, clientY: 150 });
    });

    expect(scroller.scrollTop).toBeGreaterThan(0);
  });

  it("does not snap back to bottom after appending while detached", () => {
    const { container, rerender } = render(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId="session-1"
        isActive
        listItems={makeItems(20)}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    const scroller = container.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    if (!scroller) throw new Error("Expected transcript scroller");
    defineScrollerMetrics(scroller, { clientHeight: 300, clientWidth: 900, scrollHeight: 1400 });

    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
      scroller.scrollTop = 920;
      fireEvent.scroll(scroller);
      fireEvent.wheel(scroller, { deltaY: -120 });
      scroller.scrollTop = 500;
      fireEvent.scroll(scroller);
    });

    const detachedScrollTop = scroller.scrollTop;

    rerender(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId="session-1"
        isActive
        listItems={makeItems(21)}
        threadProjectionOp={{ ...noopProjectionOp, kind: "append_stream", changedItemIds: ["message-21"], remeasureItemIds: ["message-20", "message-21"] }}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    expect(scroller.scrollTop).toBeLessThan(900);
    expect(Math.abs(scroller.scrollTop - detachedScrollTop)).toBeLessThan(80);
  });

  it("detaches from bottom on direct scroll without wheel and stays detached on append", () => {
    const { container, rerender } = render(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId="session-1"
        isActive
        listItems={makeItems(20)}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    const scroller = container.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    if (!scroller) throw new Error("Expected transcript scroller");
    defineScrollerMetrics(scroller, { clientHeight: 300, clientWidth: 900, scrollHeight: 1400 });

    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
      scroller.scrollTop = 920;
      fireEvent.scroll(scroller);
      scroller.scrollTop = 500;
      fireEvent.scroll(scroller);
    });

    const detachedScrollTop = scroller.scrollTop;

    rerender(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId="session-1"
        isActive
        listItems={makeItems(21)}
        threadProjectionOp={{ ...noopProjectionOp, kind: "append_stream", changedItemIds: ["message-21"], remeasureItemIds: ["message-20", "message-21"] }}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    expect(Math.abs(scroller.scrollTop - detachedScrollTop)).toBeLessThan(80);
  });

  it("stays detached when expanding a turn header away from the bottom", () => {
    const listItems: WorkbenchListItem[] = [
      ...makeMessageRange("before", 12),
      {
        kind: "turn_header",
        id: "turn-header-live-regression",
        header: {
          id: "header-live-regression",
          content: EXPANDABLE_TURN_HEADER_TEXT,
          attachments: [],
          created_at: "2026-04-09T22:00:56.315133Z",
        },
      },
      {
        kind: "assistant",
        id: "assistant-live-regression",
        turn_id: "turn-live-regression",
        created_at: "2026-04-09T22:01:00.000Z",
        content: "Yes. That is one of the best ideas so far.",
        thought: "",
        is_complete: true,
      },
      {
        kind: "turn_status",
        id: "turn-status-live-regression",
        turn_id: "turn-live-regression",
        created_at: "2026-04-09T22:01:01.000Z",
        started_at: "2026-04-09T22:00:56.315133Z",
        updated_at: "2026-04-09T22:01:17.596920Z",
        status: "completed",
        assistant_messages_content: "Yes. That is one of the best ideas so far.",
      },
      ...makeMessageRange("after", 12),
    ];

    const { container, rerender } = render(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId="session-1"
        isActive
        listItems={listItems}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    const scroller = container.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    if (!scroller) throw new Error("Expected transcript scroller");
    defineScrollerMetrics(scroller, { clientHeight: 300, clientWidth: 900, scrollHeight: 2400 });

    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
      scroller.scrollTop = 920;
      fireEvent.scroll(scroller);
      scroller.scrollTop = 500;
      fireEvent.scroll(scroller);
    });

    const detachedScrollTop = scroller.scrollTop;

    rerender(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId="session-1"
        isActive
        listItems={listItems}
        threadProjectionOp={{
          kind: "toggle_expansion",
          projectionRevision: 1,
          changedItemIds: ["turn-header-live-regression"],
          remeasureItemIds: [
            "turn-header-live-regression",
            "assistant-live-regression",
            "turn-status-live-regression",
          ],
        }}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={{ ...context, expandedTurnHeaders: { "header-live-regression": true } }}
      />,
    );

    expect(Math.abs(scroller.scrollTop - detachedScrollTop)).toBeLessThan(80);
  });

  it("does not jump to the bottom when a visible turn header is expanded while detached", () => {
    const listItems: WorkbenchListItem[] = [
      {
        kind: "turn_header",
        id: "turn-header-live-regression",
        header: {
          id: "header-live-regression",
          content: EXPANDABLE_TURN_HEADER_TEXT,
          attachments: [],
          created_at: "2026-04-09T22:00:56.315133Z",
        },
      },
      {
        kind: "assistant",
        id: "assistant-live-regression",
        turn_id: "turn-live-regression",
        created_at: "2026-04-09T22:01:00.000Z",
        content: "Yes. That is one of the best ideas so far.",
        thought: "",
        is_complete: true,
      },
      {
        kind: "turn_status",
        id: "turn-status-live-regression",
        turn_id: "turn-live-regression",
        created_at: "2026-04-09T22:01:01.000Z",
        started_at: "2026-04-09T22:00:56.315133Z",
        updated_at: "2026-04-09T22:01:17.596920Z",
        status: "completed",
        assistant_messages_content: "Yes. That is one of the best ideas so far.",
      },
      ...makeMessageRange("after", 24),
    ];

    const { container } = render(
      <InteractiveTurnHeaderHarness sessionId="session-turn-header-click" listItems={listItems} />,
    );

    const scroller = container.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    if (!scroller) throw new Error("Expected transcript scroller");
    defineScrollerMetrics(scroller, { clientHeight: 300, clientWidth: 900, scrollHeight: 2400 });

    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
      scroller.scrollTop = 920;
      fireEvent.scroll(scroller);
      scroller.scrollTop = 80;
      fireEvent.scroll(scroller);
    });

    const detachedScrollTop = scroller.scrollTop;
    const header = container.querySelector<HTMLElement>(".wb-turn-header");
    if (!header) throw new Error("Expected visible turn header");

    act(() => {
      fireEvent.mouseDown(header);
      fireEvent.click(header);
    });

    expect(header).toHaveAttribute("aria-expanded", "true");
    expect(Math.abs(scroller.scrollTop - detachedScrollTop)).toBeLessThan(80);
  });

  it("does not jump when expansion is merged into a reconcile update while detached", () => {
    const listItems: WorkbenchListItem[] = [
      {
        kind: "turn_header",
        id: "turn-header-live-regression",
        header: {
          id: "header-live-regression",
          content: EXPANDABLE_TURN_HEADER_TEXT,
          attachments: [],
          created_at: "2026-04-09T22:00:56.315133Z",
        },
      },
      {
        kind: "assistant",
        id: "assistant-live-regression",
        turn_id: "turn-live-regression",
        created_at: "2026-04-09T22:01:00.000Z",
        content: "Yes. That is one of the best ideas so far.",
        thought: "",
        is_complete: true,
      },
      {
        kind: "turn_status",
        id: "turn-status-live-regression",
        turn_id: "turn-live-regression",
        created_at: "2026-04-09T22:01:01.000Z",
        started_at: "2026-04-09T22:00:56.315133Z",
        updated_at: "2026-04-09T22:01:17.596920Z",
        status: "completed",
        assistant_messages_content: "Yes. That is one of the best ideas so far.",
      },
      ...makeMessageRange("after", 24),
    ];

    const { container } = render(
      <InteractiveTurnHeaderHarness
        sessionId="session-turn-header-click-reconcile"
        listItems={listItems}
        projectionKind="reconcile"
      />,
    );

    const scroller = container.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    if (!scroller) throw new Error("Expected transcript scroller");
    defineScrollerMetrics(scroller, { clientHeight: 300, clientWidth: 900, scrollHeight: 2400 });

    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
      scroller.scrollTop = 920;
      fireEvent.scroll(scroller);
      scroller.scrollTop = 80;
      fireEvent.scroll(scroller);
    });

    const detachedScrollTop = scroller.scrollTop;
    const header = container.querySelector<HTMLElement>(".wb-turn-header");
    if (!header) throw new Error("Expected visible turn header");

    act(() => {
      fireEvent.mouseDown(header);
      fireEvent.click(header);
    });

    expect(header).toHaveAttribute("aria-expanded", "true");
    expect(Math.abs(scroller.scrollTop - detachedScrollTop)).toBeLessThan(80);
  });

  it("keeps the interacted row anchored when reconcile changes include streaming rows too", () => {
    const listItems: WorkbenchListItem[] = [
      {
        kind: "turn_header",
        id: "turn-header-live-regression",
        header: {
          id: "header-live-regression",
          content: EXPANDABLE_TURN_HEADER_TEXT,
          attachments: [],
          created_at: "2026-04-09T22:00:56.315133Z",
        },
      },
      {
        kind: "assistant",
        id: "assistant-live-regression",
        turn_id: "turn-live-regression",
        created_at: "2026-04-09T22:01:00.000Z",
        content: "Yes. That is one of the best ideas so far.",
        thought: "",
        is_complete: true,
      },
      {
        kind: "turn_status",
        id: "turn-status-live-regression",
        turn_id: "turn-live-regression",
        created_at: "2026-04-09T22:01:01.000Z",
        started_at: "2026-04-09T22:00:56.315133Z",
        updated_at: "2026-04-09T22:01:17.596920Z",
        status: "completed",
        assistant_messages_content: "Yes. That is one of the best ideas so far.",
      },
      ...makeMessageRange("after", 24),
    ];

    function MixedReconcileHarness() {
      const [expandedTurnHeaders, setExpandedTurnHeaders] = useState<Record<string, boolean>>({});
      const projectionOp: WorkbenchThreadProjectionOp = {
        kind: "reconcile",
        projectionRevision: Object.keys(expandedTurnHeaders).length,
        changedItemIds: ["turn-header-live-regression", "assistant-live-regression"],
        remeasureItemIds: [
          "turn-header-live-regression",
          "assistant-live-regression",
          "turn-status-live-regression",
        ],
      };
      return (
        <SessionThreadPretextVirtualizerList
          style={{ height: 400 }}
          sessionId="session-turn-header-click-reconcile-streaming"
          isActive
          listItems={listItems}
          threadProjectionOp={projectionOp}
          itemContent={(_, item) => {
            if (item.kind === "turn_header") {
              const layout = getWorkbenchTurnHeaderLayoutState(item, expandedTurnHeaders);
              return (
                <WorkbenchTurnHeaderView
                  header={item.header}
                  plainText={layout.displayPlainText}
                  expanded={layout.expanded}
                  onToggle={() =>
                    setExpandedTurnHeaders((prev) => ({ ...prev, [item.header.id]: !layout.expanded }))
                  }
                />
              );
            }
            return <div>{item.id}</div>;
          }}
          itemKey={(item) => item.id}
          context={{ ...context, expandedTurnHeaders }}
        />
      );
    }

    const { container } = render(<MixedReconcileHarness />);

    const scroller = container.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    if (!scroller) throw new Error("Expected transcript scroller");
    defineScrollerMetrics(scroller, { clientHeight: 300, clientWidth: 900, scrollHeight: 2400 });

    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
      scroller.scrollTop = 920;
      fireEvent.scroll(scroller);
      scroller.scrollTop = 80;
      fireEvent.scroll(scroller);
    });

    const detachedScrollTop = scroller.scrollTop;
    const header = container.querySelector<HTMLElement>(".wb-turn-header");
    if (!header) throw new Error("Expected visible turn header");

    act(() => {
      fireEvent.mouseDown(header);
      fireEvent.click(header);
    });

    expect(header).toHaveAttribute("aria-expanded", "true");
    expect(Math.abs(scroller.scrollTop - detachedScrollTop)).toBeLessThan(80);
  });

  it("does not remount stable rows when appending new rows", () => {
    const mounts = new Map<string, number>();

    function Row({ item }: { item: WorkbenchListItem }) {
      useEffect(() => {
        mounts.set(item.id, (mounts.get(item.id) ?? 0) + 1);
      }, [item.id]);
      return <div>{item.id}</div>;
    }

    const { rerender } = render(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId="session-1"
        isActive
        listItems={makeItems(3)}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <Row item={item} />}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    expect(mounts.get("message-1")).toBe(1);
    expect(mounts.get("message-2")).toBe(1);
    expect(mounts.get("message-3")).toBe(1);

    rerender(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId="session-1"
        isActive
        listItems={makeItems(4)}
        threadProjectionOp={{ ...noopProjectionOp, kind: "append_stream", changedItemIds: ["message-4"], remeasureItemIds: ["message-3", "message-4"] }}
        itemContent={(_, item) => <Row item={item} />}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    expect(mounts.get("message-1")).toBe(1);
    expect(mounts.get("message-2")).toBe(1);
    expect(mounts.get("message-3")).toBe(1);
    expect(mounts.get("message-4")).toBe(1);
  });

  it("reveals the earliest prepended history row after scrolling to the top", () => {
    const sessionId = "session-prepend-history-top";
    const initialItems = makeMessageRange("message", 24);
    const olderItems = makeMessageRange("older", 4);
    const nextItems = [...olderItems, ...initialItems];

    const { container, rerender } = render(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId={sessionId}
        isActive
        listItems={initialItems}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    const scroller = container.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    if (!scroller) throw new Error("Expected transcript scroller");
    defineScrollerMetrics(scroller, { clientHeight: 300, clientWidth: 900, scrollHeight: 3200 });

    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
      scroller.scrollTop = 640;
      fireEvent.scroll(scroller);
      fireEvent.wheel(scroller, { deltaY: -120 });
      scroller.scrollTop = 320;
      fireEvent.scroll(scroller);
    });

    rerender(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId={sessionId}
        isActive
        listItems={nextItems}
        threadProjectionOp={{
          kind: "prepend_history",
          projectionRevision: 1,
          changedItemIds: olderItems.map((item) => item.id),
          remeasureItemIds: olderItems.map((item) => item.id),
        }}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    act(() => {
      scroller.scrollTop = 0;
      fireEvent.scroll(scroller);
    });

    expect(getRenderedItemIds(container)[0]).toBe("older-1");
  });

  it("keeps the viewport pinned to bottom when the viewport height shrinks while attached", () => {
    const { container } = render(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId="session-1"
        isActive
        listItems={makeItems(24)}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <div style={{ height: 48 }}>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    const scroller = container.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    if (!scroller) throw new Error("Expected transcript scroller");

    defineScrollerMetrics(scroller, { clientHeight: 400, clientWidth: 900, scrollHeight: 1800 });
    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
    });

    const beforeResizeScrollTop = scroller.scrollTop;

    defineScrollerMetrics(scroller, { clientHeight: 300, clientWidth: 900, scrollHeight: 1800 });
    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
    });

    expect(scroller.scrollTop).toBeGreaterThan(beforeResizeScrollTop);
  });

  it("does not report a short bottom-aligned thread as detached while resize metrics settle", () => {
    const onAtBottomChange = vi.fn();
    const { container } = render(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId="session-short-bottom"
        isActive
        listItems={makeItems(2)}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <div style={{ height: 48 }}>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
        onAtBottomChange={onAtBottomChange}
        shortSizeAlign="bottom"
      />,
    );

    const scroller = container.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    if (!scroller) throw new Error("Expected transcript scroller");

    defineScrollerMetrics(scroller, { clientHeight: 400, clientWidth: 900, scrollHeight: 400 });
    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
    });

    expect(onAtBottomChange).toHaveBeenCalledWith(true);
    onAtBottomChange.mockClear();

    defineScrollerMetrics(scroller, { clientHeight: 300, clientWidth: 900, scrollHeight: 400 });
    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
    });

    expect(onAtBottomChange).not.toHaveBeenCalledWith(false);
  });

  it("reopens a previously detached session at bottom", () => {
    const sessionId = "session-detached-revisit";
    const initial = render(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId={sessionId}
        isActive
        listItems={makeItems(20)}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    const scroller = initial.container.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    if (!scroller) throw new Error("Expected transcript scroller");
    defineScrollerMetrics(scroller, { clientHeight: 300, clientWidth: 900, scrollHeight: 1400 });

    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
      scroller.scrollTop = 920;
      fireEvent.scroll(scroller);
      fireEvent.wheel(scroller, { deltaY: -120 });
      scroller.scrollTop = 500;
      fireEvent.scroll(scroller);
    });

    initial.unmount();

    const reopened = render(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId={sessionId}
        isActive
        listItems={makeItems(20)}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    const reopenedScroller = reopened.container.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    if (!reopenedScroller) throw new Error("Expected reopened transcript scroller");
    defineScrollerMetrics(reopenedScroller, { clientHeight: 300, clientWidth: 900, scrollHeight: 1400 });

    act(() => {
      resizeObserverInstances[resizeObserverInstances.length - 1]?.callback([], {} as ResizeObserver);
    });

    expect(reopenedScroller.getAttribute("data-pretext-virtualizer-snapshot-last-index")).toBe("19");
    expect(screen.queryByRole("button", { name: "Jump to latest" })).not.toBeInTheDocument();
  });

  it("reopens at bottom after background warm priming updates the prepared transcript", () => {
    const sessionId = "session-detached-revisit-after-prime";
    const initial = render(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId={sessionId}
        isActive
        listItems={makeItems(20)}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    const scroller = initial.container.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    if (!scroller) throw new Error("Expected transcript scroller");
    defineScrollerMetrics(scroller, { clientHeight: 300, clientWidth: 900, scrollHeight: 1400 });

    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
      scroller.scrollTop = 920;
      fireEvent.scroll(scroller);
      fireEvent.wheel(scroller, { deltaY: -120 });
      scroller.scrollTop = 500;
      fireEvent.scroll(scroller);
    });
    initial.unmount();

    primeSessionPretextRuntime({
      sessionId,
      listItems: makeItems(24),
      uiState: createDefaultSessionTranscriptUiState(context.verbosity, context.turnToolsLoading),
      viewportWidth: 900,
      viewportHeight: 300,
    });

    const reopened = render(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId={sessionId}
        isActive
        listItems={makeItems(24)}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    const reopenedScroller = reopened.container.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    if (!reopenedScroller) throw new Error("Expected reopened transcript scroller");
    defineScrollerMetrics(reopenedScroller, { clientHeight: 300, clientWidth: 900, scrollHeight: 1800 });

    act(() => {
      resizeObserverInstances[resizeObserverInstances.length - 1]?.callback([], {} as ResizeObserver);
    });

    expect(reopenedScroller.getAttribute("data-pretext-virtualizer-snapshot-last-index")).toBe("23");
    expect(screen.queryByRole("button", { name: "Jump to latest" })).not.toBeInTheDocument();
  });

  it("reopens at bottom when the current transcript is newer than the prepared runtime", () => {
    const sessionId = "session-detached-revisit-current-items-newer";
    const initial = render(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId={sessionId}
        isActive
        listItems={makeItems(20)}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    const scroller = initial.container.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    if (!scroller) throw new Error("Expected transcript scroller");
    defineScrollerMetrics(scroller, { clientHeight: 300, clientWidth: 900, scrollHeight: 1400 });

    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
      scroller.scrollTop = 500;
      fireEvent.scroll(scroller);
    });
    initial.unmount();

    const reopened = render(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId={sessionId}
        isActive
        listItems={makeItems(24)}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    const reopenedScroller = reopened.container.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    if (!reopenedScroller) throw new Error("Expected reopened transcript scroller");
    defineScrollerMetrics(reopenedScroller, { clientHeight: 300, clientWidth: 900, scrollHeight: 1800 });

    act(() => {
      resizeObserverInstances[resizeObserverInstances.length - 1]?.callback([], {} as ResizeObserver);
    });

    expect(reopenedScroller.getAttribute("data-pretext-virtualizer-snapshot-last-index")).toBe("23");
    expect(screen.queryByRole("button", { name: "Jump to latest" })).not.toBeInTheDocument();
  });

  it("remeasures immediately when a warmed runtime reopens at a different visible width", () => {
    const sessionId = "session-reopen-warm-width-mismatch";
    const listItems = makeWrappingItems(24);
    primeSessionPretextRuntime({
      sessionId,
      listItems,
      uiState: createDefaultSessionTranscriptUiState(context.verbosity, context.turnToolsLoading),
      viewportWidth: 900,
      viewportHeight: 300,
    });

    const runtime = getOrCreateSessionPretextRuntime(sessionId);
    const patchItemsSpy = vi.spyOn(runtime.core, "patchItems");
    patchItemsSpy.mockClear();

    const originalClientWidth = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientWidth");
    const originalClientHeight = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientHeight");
    const originalScrollHeight = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "scrollHeight");

    Object.defineProperty(HTMLElement.prototype, "clientWidth", {
      configurable: true,
      get() {
        return this.getAttribute?.("data-pretext-virtualizer-list") === "1" ? 1280 : 0;
      },
    });
    Object.defineProperty(HTMLElement.prototype, "clientHeight", {
      configurable: true,
      get() {
        return this.getAttribute?.("data-pretext-virtualizer-list") === "1" ? 420 : 0;
      },
    });
    Object.defineProperty(HTMLElement.prototype, "scrollHeight", {
      configurable: true,
      get() {
        return this.getAttribute?.("data-pretext-virtualizer-list") === "1" ? 5200 : 0;
      },
    });

    try {
      render(
        <SessionThreadPretextVirtualizerList
          style={{ height: 420 }}
          sessionId={sessionId}
          isActive
          listItems={listItems}
          threadProjectionOp={noopProjectionOp}
          itemContent={(_, item) => <div>{"content" in item ? item.content : item.id}</div>}
          itemKey={(item) => item.id}
          context={context}
        />,
      );
    } finally {
      if (originalClientWidth) {
        Object.defineProperty(HTMLElement.prototype, "clientWidth", originalClientWidth);
      } else {
        Reflect.deleteProperty(HTMLElement.prototype, "clientWidth");
      }
      if (originalClientHeight) {
        Object.defineProperty(HTMLElement.prototype, "clientHeight", originalClientHeight);
      } else {
        Reflect.deleteProperty(HTMLElement.prototype, "clientHeight");
      }
      if (originalScrollHeight) {
        Object.defineProperty(HTMLElement.prototype, "scrollHeight", originalScrollHeight);
      } else {
        Reflect.deleteProperty(HTMLElement.prototype, "scrollHeight");
      }
    }

    expect(patchItemsSpy).toHaveBeenCalled();
  });

  it("preserves the detached anchor item when the viewport width changes", () => {
    const listItems = makeWrappingItems(24);
    const { container } = render(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId="session-detached-resize"
        isActive
        listItems={listItems}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <div>{"content" in item ? item.content : item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    const scroller = container.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    if (!scroller) throw new Error("Expected transcript scroller");
    defineScrollerMetrics(scroller, { clientHeight: 400, clientWidth: 900, scrollHeight: 5200 });

    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
      scroller.scrollTop = 2400;
      fireEvent.scroll(scroller);
      fireEvent.wheel(scroller, { deltaY: -120 });
      scroller.scrollTop = 1900;
      fireEvent.scroll(scroller);
    });

    const beforeIds = getRenderedItemIds(container);
    const anchorId = beforeIds[Math.floor(beforeIds.length / 2)] ?? null;
    expect(anchorId).not.toBeNull();

    defineScrollerMetrics(scroller, { clientHeight: 400, clientWidth: 420, scrollHeight: 7200 });
    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
    });

    const afterIds = getRenderedItemIds(container);
    expect(afterIds).toContain(String(anchorId));
  });

  it("remeasures items when the visible viewport width changes", () => {
    const sessionId = "session-width-remeasure";
    const listItems = makeWrappingItems(16);
    const { container } = render(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId={sessionId}
        isActive
        listItems={listItems}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <div>{"content" in item ? item.content : item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    const scroller = container.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    if (!scroller) throw new Error("Expected transcript scroller");
    defineScrollerMetrics(scroller, { clientHeight: 400, clientWidth: 420, scrollHeight: 7200 });

    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
    });

    const runtime = getOrCreateSessionPretextRuntime(sessionId);
    const syncItemsSpy = vi.spyOn(runtime.core, "syncItems");
    syncItemsSpy.mockClear();

    defineScrollerMetrics(scroller, { clientHeight: 400, clientWidth: 900, scrollHeight: 5200 });
    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
    });

    expect(syncItemsSpy).toHaveBeenCalled();
  });

  it("primes hidden-session transcript state for bottom reopen semantics", () => {
    const sessionId = "session-hidden-prime";
    const initialItems = makeItems(20);
    const updatedItems = makeItems(24);
    const runtime = getOrCreateSessionPretextRuntime(sessionId);

    primeSessionPretextRuntime({
      sessionId,
      listItems: initialItems,
      uiState: createDefaultSessionTranscriptUiState(context.verbosity, context.turnToolsLoading),
      viewportWidth: 900,
      viewportHeight: 300,
    });
    primeSessionPretextRuntime({
      sessionId,
      listItems: updatedItems,
      uiState: createDefaultSessionTranscriptUiState(context.verbosity, context.turnToolsLoading),
      viewportWidth: 900,
      viewportHeight: 300,
    });

    const preparedAfter = readSessionPretextRuntimePreparedState(runtime);

    expect(preparedAfter.listItems).toEqual(updatedItems);
    expect(preparedAfter.snapshot.anchor.kind).toBe("bottom");
  });

  it("resyncs row offsets when only layout context changes", () => {
    const expandableContent = Array.from(
      { length: 28 },
      (_, index) => `- expanded row ${index + 1} with enough text to wrap and change the measured height`,
    ).join("\n");
    const listItems: WorkbenchListItem[] = [
      {
        kind: "message",
        id: "message-expandable",
        role: "user",
        content: expandableContent,
        attachments: [],
        created_at: "2026-04-06T00:00:00Z",
      },
      {
        kind: "turn_status",
        id: "status-1",
        turn_id: "turn-1",
        created_at: "2026-04-06T00:01:00Z",
        started_at: "2026-04-06T00:01:00Z",
        updated_at: "2026-04-06T00:01:05Z",
        status: "completed",
        assistant_messages_content: "done",
      },
    ];

    const { container, rerender } = render(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId="session-1"
        isActive
        listItems={listItems}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={{ ...context, expandedMessageById: {} }}
      />,
    );

    const scroller = container.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    if (!scroller) throw new Error("Expected transcript scroller");
    defineScrollerMetrics(scroller, { clientHeight: 300, clientWidth: 900, scrollHeight: 1400 });

    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
    });

    const shellsBefore = container.querySelectorAll<HTMLElement>("[data-pretext-virtualizer-row-shell='1']");
    const secondTopBefore = Number.parseFloat(shellsBefore[1]?.style.top ?? "0");

    rerender(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId="session-1"
        isActive
        listItems={listItems}
        threadProjectionOp={{
          kind: "toggle_expansion",
          projectionRevision: 1,
          changedItemIds: ["message-expandable"],
          remeasureItemIds: ["message-expandable", "status-1"],
        }}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={{ ...context, expandedMessageById: { "message-expandable": true } }}
      />,
    );

    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
    });

    const shellsAfter = container.querySelectorAll<HTMLElement>("[data-pretext-virtualizer-row-shell='1']");
    const secondTopAfter = Number.parseFloat(shellsAfter[1]?.style.top ?? "0");

    expect(secondTopAfter).toBeGreaterThan(secondTopBefore);
  });

  it("does not reuse expanded prepared geometry when reopening the same session collapsed", () => {
    const sessionId = "session-reopen-collapsed-layout";
    const expandableContent = Array.from(
      { length: 28 },
      (_, index) => `- expanded row ${index + 1} with enough text to wrap and change the measured height`,
    ).join("\n");
    const listItems: WorkbenchListItem[] = [
      {
        kind: "message",
        id: "message-expandable",
        role: "user",
        content: expandableContent,
        attachments: [],
        created_at: "2026-04-10T00:00:00Z",
      },
      {
        kind: "turn_status",
        id: "status-1",
        turn_id: "turn-1",
        created_at: "2026-04-10T00:01:00Z",
        started_at: "2026-04-10T00:01:00Z",
        updated_at: "2026-04-10T00:01:05Z",
        status: "completed",
        assistant_messages_content: "done",
      },
    ];

    const expandedContext = { ...context, expandedMessageById: { "message-expandable": true } };
    const collapsedContext = { ...context, expandedMessageById: {} };
    const originalClientWidth = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientWidth");
    const originalClientHeight = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientHeight");
    const originalScrollHeight = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "scrollHeight");

    Object.defineProperty(HTMLElement.prototype, "clientWidth", {
      configurable: true,
      get() {
        return this.getAttribute?.("data-pretext-virtualizer-list") === "1" ? 900 : 0;
      },
    });
    Object.defineProperty(HTMLElement.prototype, "clientHeight", {
      configurable: true,
      get() {
        return this.getAttribute?.("data-pretext-virtualizer-list") === "1" ? 300 : 0;
      },
    });
    Object.defineProperty(HTMLElement.prototype, "scrollHeight", {
      configurable: true,
      get() {
        return this.getAttribute?.("data-pretext-virtualizer-list") === "1" ? 2200 : 0;
      },
    });

    try {
      const expandedRender = render(
        <SessionThreadPretextVirtualizerList
          style={{ height: 400 }}
          sessionId={sessionId}
          isActive
          listItems={listItems}
          threadProjectionOp={noopProjectionOp}
          itemContent={(_, item) => <div>{item.id}</div>}
          itemKey={(item) => item.id}
          context={expandedContext}
        />,
      );

      const expandedShells = expandedRender.container.querySelectorAll<HTMLElement>("[data-pretext-virtualizer-row-shell='1']");
      const expandedStatusTop = Number.parseFloat(expandedShells[1]?.style.top ?? "0");

      expandedRender.unmount();

      const runtime = getOrCreateSessionPretextRuntime(sessionId);
      const syncItemsSpy = vi.spyOn(runtime.core, "syncItems");
      syncItemsSpy.mockClear();

      const reopenedRender = render(
        <SessionThreadPretextVirtualizerList
          style={{ height: 400 }}
          sessionId={sessionId}
          isActive
          listItems={listItems}
          threadProjectionOp={noopProjectionOp}
          itemContent={(_, item) => <div>{item.id}</div>}
          itemKey={(item) => item.id}
          context={collapsedContext}
        />,
      );

      const reopenedShells = reopenedRender.container.querySelectorAll<HTMLElement>("[data-pretext-virtualizer-row-shell='1']");
      const reopenedStatusTop = Number.parseFloat(reopenedShells[1]?.style.top ?? "0");

      const freshCollapsedRender = render(
        <SessionThreadPretextVirtualizerList
          style={{ height: 400 }}
          sessionId="session-reopen-collapsed-layout-fresh"
          isActive
          listItems={listItems}
          threadProjectionOp={noopProjectionOp}
          itemContent={(_, item) => <div>{item.id}</div>}
          itemKey={(item) => item.id}
          context={collapsedContext}
        />,
      );

      const freshCollapsedShells = freshCollapsedRender.container.querySelectorAll<HTMLElement>("[data-pretext-virtualizer-row-shell='1']");
      const freshCollapsedStatusTop = Number.parseFloat(freshCollapsedShells[1]?.style.top ?? "0");

      expect(syncItemsSpy).toHaveBeenCalled();
      expect(expandedStatusTop).toBeGreaterThan(freshCollapsedStatusTop);
      expect(reopenedStatusTop).toBe(freshCollapsedStatusTop);
    } finally {
      if (originalClientWidth) {
        Object.defineProperty(HTMLElement.prototype, "clientWidth", originalClientWidth);
      } else {
        Reflect.deleteProperty(HTMLElement.prototype, "clientWidth");
      }
      if (originalClientHeight) {
        Object.defineProperty(HTMLElement.prototype, "clientHeight", originalClientHeight);
      } else {
        Reflect.deleteProperty(HTMLElement.prototype, "clientHeight");
      }
      if (originalScrollHeight) {
        Object.defineProperty(HTMLElement.prototype, "scrollHeight", originalScrollHeight);
      } else {
        Reflect.deleteProperty(HTMLElement.prototype, "scrollHeight");
      }
    }
  });

  it("treats prepared snapshots without layout metadata as stale on reopen", () => {
    const sessionId = "session-reopen-missing-layout-key";
    const listItems: WorkbenchListItem[] = [
      {
        kind: "message",
        id: "message-expandable",
        role: "user",
        content: Array.from(
          { length: 24 },
          (_, index) => `- expanded row ${index + 1} with enough text to wrap and change the measured height`,
        ).join("\n"),
        attachments: [],
        created_at: "2026-04-10T00:00:00Z",
      },
      {
        kind: "turn_status",
        id: "status-1",
        turn_id: "turn-1",
        created_at: "2026-04-10T00:01:00Z",
        started_at: "2026-04-10T00:01:00Z",
        updated_at: "2026-04-10T00:01:05Z",
        status: "completed",
        assistant_messages_content: "done",
      },
    ];

    const originalClientWidth = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientWidth");
    const originalClientHeight = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientHeight");
    const originalScrollHeight = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "scrollHeight");

    Object.defineProperty(HTMLElement.prototype, "clientWidth", {
      configurable: true,
      get() {
        return this.getAttribute?.("data-pretext-virtualizer-list") === "1" ? 900 : 0;
      },
    });
    Object.defineProperty(HTMLElement.prototype, "clientHeight", {
      configurable: true,
      get() {
        return this.getAttribute?.("data-pretext-virtualizer-list") === "1" ? 300 : 0;
      },
    });
    Object.defineProperty(HTMLElement.prototype, "scrollHeight", {
      configurable: true,
      get() {
        return this.getAttribute?.("data-pretext-virtualizer-list") === "1" ? 2200 : 0;
      },
    });

    try {
      const firstRender = render(
        <SessionThreadPretextVirtualizerList
          style={{ height: 400 }}
          sessionId={sessionId}
          isActive
          listItems={listItems}
          threadProjectionOp={noopProjectionOp}
          itemContent={(_, item) => <div>{item.id}</div>}
          itemKey={(item) => item.id}
          context={{ ...context, expandedMessageById: { "message-expandable": true } }}
        />,
      );
      firstRender.unmount();

      const runtime = getOrCreateSessionPretextRuntime(sessionId) as ReturnType<typeof getOrCreateSessionPretextRuntime> & {
        preparedLayoutKey: string | null;
      };
      runtime.preparedLayoutKey = null;
      const syncItemsSpy = vi.spyOn(runtime.core, "syncItems");
      syncItemsSpy.mockClear();

      render(
        <SessionThreadPretextVirtualizerList
          style={{ height: 400 }}
          sessionId={sessionId}
          isActive
          listItems={listItems}
          threadProjectionOp={noopProjectionOp}
          itemContent={(_, item) => <div>{item.id}</div>}
          itemKey={(item) => item.id}
          context={{ ...context, expandedMessageById: {} }}
        />,
      );

      expect(syncItemsSpy).toHaveBeenCalled();
    } finally {
      if (originalClientWidth) {
        Object.defineProperty(HTMLElement.prototype, "clientWidth", originalClientWidth);
      } else {
        Reflect.deleteProperty(HTMLElement.prototype, "clientWidth");
      }
      if (originalClientHeight) {
        Object.defineProperty(HTMLElement.prototype, "clientHeight", originalClientHeight);
      } else {
        Reflect.deleteProperty(HTMLElement.prototype, "clientHeight");
      }
      if (originalScrollHeight) {
        Object.defineProperty(HTMLElement.prototype, "scrollHeight", originalScrollHeight);
      } else {
        Reflect.deleteProperty(HTMLElement.prototype, "scrollHeight");
      }
    }
  });

  it("does not re-enter projection sync during ordinary scroll with unchanged items", () => {
    const sessionId = "session-scroll";
    const { container } = render(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId={sessionId}
        isActive
        listItems={makeItems(20)}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    const scroller = container.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    if (!scroller) throw new Error("Expected transcript scroller");
    defineScrollerMetrics(scroller, { clientHeight: 300, clientWidth: 900, scrollHeight: 1400 });

    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
    });

    const runtime = getOrCreateSessionPretextRuntime(sessionId);
    const syncViewportSpy = vi.spyOn(runtime.core, "syncViewport");
    const syncItemsSpy = vi.spyOn(runtime.core, "syncItems");
    syncViewportSpy.mockClear();
    syncItemsSpy.mockClear();

    act(() => {
      scroller.scrollTop = 500;
      fireEvent.scroll(scroller);
    });

    expect(syncViewportSpy).toHaveBeenCalledTimes(1);
    expect(syncItemsSpy).not.toHaveBeenCalled();
  });

  it("consumes a non-noop projection op only once across unrelated rerenders", () => {
    const sessionId = "session-projection-edge";
    const initialItems = makeItems(4);
    const nextItems = initialItems.map((item, index) =>
      index === 1
        ? {
            ...item,
            content: item.kind === "message" ? `${item.content} updated` : "updated",
          }
        : item,
    );

    const { container, rerender } = render(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId={sessionId}
        isActive
        listItems={initialItems}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    const scroller = container.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    if (!scroller) throw new Error("Expected transcript scroller");
    defineScrollerMetrics(scroller, { clientHeight: 300, clientWidth: 900, scrollHeight: 1400 });

    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
    });

    const runtime = getOrCreateSessionPretextRuntime(sessionId);
    const patchItemsSpy = vi.spyOn(runtime.core, "patchItems");
    const syncItemsSpy = vi.spyOn(runtime.core, "syncItems");
    patchItemsSpy.mockClear();
    syncItemsSpy.mockClear();

    const projectionOp = {
      kind: "hydrate_tools" as const,
      projectionRevision: 1,
      changedItemIds: [nextItems[1]!.id],
      remeasureItemIds: [nextItems[1]!.id],
    };

    rerender(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId={sessionId}
        isActive
        listItems={nextItems}
        threadProjectionOp={projectionOp}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    expect(patchItemsSpy).toHaveBeenCalledTimes(1);
    expect(syncItemsSpy).not.toHaveBeenCalled();

    rerender(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId={sessionId}
        isActive
        listItems={nextItems}
        threadProjectionOp={projectionOp}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    expect(patchItemsSpy).toHaveBeenCalledTimes(1);
    expect(syncItemsSpy).not.toHaveBeenCalled();
  });

  it("does not resync when the items array is recreated with the same row objects", () => {
    const sessionId = "session-stable-item-refs";
    const initialItems = makeItems(4);

    const { container, rerender } = render(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId={sessionId}
        isActive
        listItems={initialItems}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    const scroller = container.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    if (!scroller) throw new Error("Expected transcript scroller");
    defineScrollerMetrics(scroller, { clientHeight: 300, clientWidth: 900, scrollHeight: 1400 });

    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
    });

    const runtime = getOrCreateSessionPretextRuntime(sessionId);
    const syncItemsSpy = vi.spyOn(runtime.core, "syncItems");
    syncItemsSpy.mockClear();

    rerender(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId={sessionId}
        isActive
        listItems={[...initialItems]}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    expect(syncItemsSpy).not.toHaveBeenCalled();
  });

  it("remeasures a same-id streaming assistant row when partial markdown introduces hard breaks", () => {
    const sessionId = "session-streaming-hard-breaks";
    const initialItems = [makePendingAssistantItem("Short version:")];
    const streamedItems = [
      makePendingAssistantItem(
        "Short version:  \nthe next system should answer not just which title wins.  \nIt should answer which article shape wins.",
      ),
    ];

    const { container, rerender } = render(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId={sessionId}
        isActive
        listItems={initialItems}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    const scroller = container.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    if (!scroller) throw new Error("Expected transcript scroller");
    defineScrollerMetrics(scroller, { clientHeight: 300, clientWidth: 900, scrollHeight: 900 });

    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
    });

    const runtime = getOrCreateSessionPretextRuntime(sessionId);
    const syncItemsSpy = vi.spyOn(runtime.core, "syncItems");
    syncItemsSpy.mockClear();
    const beforeHeight = runtime.core.getHeightForIndex(0);

    rerender(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId={sessionId}
        isActive
        listItems={streamedItems}
        threadProjectionOp={{
          kind: "reconcile",
          projectionRevision: 1,
          changedItemIds: ["assistant-turn-1-pending"],
          remeasureItemIds: ["assistant-turn-1-pending"],
        }}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    const afterHeight = runtime.core.getHeightForIndex(0);

    expect(syncItemsSpy).toHaveBeenCalledTimes(1);
    expect(afterHeight).toBeGreaterThan(beforeHeight + 30);
  });

  it("syncs instead of appending when a pending assistant grows and a tail row arrives together", () => {
    const sessionId = "session-streaming-mixed-append";
    const initialItems = [makePendingAssistantItem("Short version:"), makeTurnStatusItem()];
    const streamedItems: WorkbenchListItem[] = [
      makePendingAssistantItem(
        "Short version:  \nthe next system should answer not just which title wins.  \nIt should answer which article shape wins.",
      ),
      initialItems[1]!,
      {
        kind: "message",
        id: "message-appended-1",
        role: "assistant",
        content: "appended tail row",
        attachments: [],
        created_at: "2026-04-08T00:00:02Z",
      },
    ];
    const projectionOp = classifyWorkbenchThreadProjectionOp({
      current: initialItems,
      next: streamedItems,
      projectionRevision: 1,
    });

    expect(projectionOp.kind).toBe("reconcile");

    const { container, rerender } = render(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId={sessionId}
        isActive
        listItems={initialItems}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    const scroller = container.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    if (!scroller) throw new Error("Expected transcript scroller");
    defineScrollerMetrics(scroller, { clientHeight: 300, clientWidth: 900, scrollHeight: 900 });

    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
    });

    const runtime = getOrCreateSessionPretextRuntime(sessionId);
    const syncItemsSpy = vi.spyOn(runtime.core, "syncItems");
    const appendItemsSpy = vi.spyOn(runtime.core, "appendItems");
    syncItemsSpy.mockClear();
    appendItemsSpy.mockClear();
    const beforeHeight = runtime.core.getHeightForIndex(0);

    rerender(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId={sessionId}
        isActive
        listItems={streamedItems}
        threadProjectionOp={projectionOp}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    const afterHeight = runtime.core.getHeightForIndex(0);

    expect(appendItemsSpy).not.toHaveBeenCalled();
    expect(syncItemsSpy).toHaveBeenCalledTimes(1);
    expect(afterHeight).toBeGreaterThan(beforeHeight + 30);
  });

  it("only replans the changed middle window for non-prefix projection updates", () => {
    const sessionId = "session-middle-insert";
    const initialItems = makeItems(4);
    const insertedItems: WorkbenchListItem[] = [
      initialItems[0]!,
      {
        kind: "message",
        id: "message-inserted-1",
        role: "assistant",
        content: "inserted one",
        attachments: [],
        created_at: "2026-03-19T00:00:00Z",
      },
      {
        kind: "message",
        id: "message-inserted-2",
        role: "assistant",
        content: "inserted two",
        attachments: [],
        created_at: "2026-03-19T00:00:01Z",
      },
      ...initialItems.slice(1),
    ];

    const rowLayoutSpy = vi.spyOn(defaultTranscriptLayoutPlanner, "planRow");

    const { container, rerender } = render(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId={sessionId}
        isActive
        listItems={initialItems}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    const scroller = container.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    if (!scroller) throw new Error("Expected transcript scroller");
    defineScrollerMetrics(scroller, { clientHeight: 300, clientWidth: 900, scrollHeight: 1400 });

    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
    });

    rowLayoutSpy.mockClear();

    rerender(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId={sessionId}
        isActive
        listItems={insertedItems}
        threadProjectionOp={{
          kind: "hydrate_tools",
          projectionRevision: 1,
          changedItemIds: ["message-inserted-1", "message-inserted-2", "message-2", "message-3", "message-4"],
          remeasureItemIds: ["message-inserted-1", "message-inserted-2"],
        }}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={context}
      />,
    );

    expect(rowLayoutSpy.mock.calls.map(([item]) => item.id)).toEqual([
      "message-inserted-1",
      "message-inserted-2",
    ]);
  });

  it("replans the affected row when context changes even if the projection op is noop", () => {
    const expandableContent = Array.from(
      { length: 28 },
      (_, index) => `- expanded row ${index + 1} with enough text to wrap and change the measured height`,
    ).join("\n");
    const listItems: WorkbenchListItem[] = [
      {
        kind: "message",
        id: "message-expandable-noop",
        role: "user",
        content: expandableContent,
        attachments: [],
        created_at: "2026-04-06T00:00:00Z",
      },
      {
        kind: "turn_status",
        id: "status-2",
        turn_id: "turn-2",
        created_at: "2026-04-06T00:01:00Z",
        started_at: "2026-04-06T00:01:00Z",
        updated_at: "2026-04-06T00:01:05Z",
        status: "completed",
        assistant_messages_content: "done",
      },
    ];

    const sessionId = "session-noop-layout";
    const rowLayoutSpy = vi.spyOn(defaultTranscriptLayoutPlanner, "planRow");
    const { container, rerender } = render(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId={sessionId}
        isActive
        listItems={listItems}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={{ ...context, expandedMessageById: {} }}
      />,
    );

    const scroller = container.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    if (!scroller) throw new Error("Expected transcript scroller");
    defineScrollerMetrics(scroller, { clientHeight: 300, clientWidth: 900, scrollHeight: 1400 });

    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
    });

    const runtime = getOrCreateSessionPretextRuntime(sessionId);
    const syncItemsSpy = vi.spyOn(runtime.core, "syncItems");
    syncItemsSpy.mockClear();
    rowLayoutSpy.mockClear();

    rerender(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId={sessionId}
        isActive
        listItems={listItems}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={{ ...context, expandedMessageById: { "message-expandable-noop": true } }}
      />,
    );

    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
    });

    expect(syncItemsSpy).toHaveBeenCalledTimes(1);
    expect(rowLayoutSpy.mock.calls.map(([item]) => item.id)).toEqual(["message-expandable-noop"]);
  });

  it("does not replan when context objects change by reference but keep the same layout semantics", () => {
    const sessionId = "session-noop-semantic";
    const initialItems = makeItems(3);

    const { container, rerender } = render(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId={sessionId}
        isActive
        listItems={initialItems}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={{ ...context, expandedMessageById: {} }}
      />,
    );

    const scroller = container.querySelector<HTMLElement>("[data-pretext-virtualizer-list='1']");
    if (!scroller) throw new Error("Expected transcript scroller");
    defineScrollerMetrics(scroller, { clientHeight: 300, clientWidth: 900, scrollHeight: 1400 });

    act(() => {
      resizeObserverInstances[0]?.callback([], {} as ResizeObserver);
    });

    const runtime = getOrCreateSessionPretextRuntime(sessionId);
    const syncItemsSpy = vi.spyOn(runtime.core, "syncItems");
    syncItemsSpy.mockClear();

    rerender(
      <SessionThreadPretextVirtualizerList
        style={{ height: 400 }}
        sessionId={sessionId}
        isActive
        listItems={initialItems}
        threadProjectionOp={noopProjectionOp}
        itemContent={(_, item) => <div>{item.id}</div>}
        itemKey={(item) => item.id}
        context={{ ...context, expandedMessageById: {} }}
      />,
    );

    expect(syncItemsSpy).not.toHaveBeenCalled();
  });
});
