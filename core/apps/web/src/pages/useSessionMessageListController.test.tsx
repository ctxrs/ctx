import { renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useSessionMessageListController } from "./useSessionMessageListController";
import type { WorkbenchListItem } from "./SessionPage.types";
import type { WorkbenchThreadProjectionOp } from "./sessionThreadProjection";
import type { ListScrollLocation } from "@virtuoso.dev/message-list";

let coalescedItems: WorkbenchListItem[] = [];

vi.mock("../components/hooks/useRafCoalesced", () => ({
  useRafCoalesced: () => coalescedItems,
}));

const makeSpacer = (id: string): WorkbenchListItem => ({ id, kind: "spacer", created_at: "2026-03-18T00:00:00.000Z" });
const makeAssistant = (content: string): WorkbenchListItem => ({
  id: "assistant-turn-1-pending",
  kind: "assistant",
  turn_id: "turn-1",
  created_at: "2026-03-18T00:00:00.000Z",
  content,
  thought: "",
  is_complete: false,
});
const makeTurnStatus = (): WorkbenchListItem => ({
  id: "turn-status-turn-1",
  kind: "turn_status",
  turn_id: "turn-1",
  created_at: "2026-03-18T00:00:01.000Z",
  started_at: "2026-03-18T00:00:00.000Z",
  updated_at: "2026-03-18T00:00:01.000Z",
  status: "running",
  custom_status: null,
  assistant_messages_content: "",
});

function createFakeMethods(initialItems: WorkbenchListItem[] = []) {
  let items = [...initialItems];
  const replace = vi.fn((next: WorkbenchListItem[]) => {
    items = [...next];
  });
  const deleteRange = vi.fn((start: number, count: number) => {
    items = [...items.slice(0, start), ...items.slice(start + count)];
  });
  const insert = vi.fn((inserted: WorkbenchListItem[], offset: number) => {
    items = [...items.slice(0, offset), ...inserted, ...items.slice(offset)];
  });
  const append = vi.fn((suffix: WorkbenchListItem[]) => {
    items = [...items, ...suffix];
  });
  const prepend = vi.fn((prefix: WorkbenchListItem[]) => {
    items = [...prefix, ...items];
  });
  const map = vi.fn((mapper: (item: WorkbenchListItem) => WorkbenchListItem) => {
    items = items.map((item) => mapper(item));
  });
  const mapWithAnchor = vi.fn((mapper: (item: WorkbenchListItem) => WorkbenchListItem) => {
    items = items.map((item) => mapper(item));
  });
  const batch = vi.fn((updater: () => void) => {
    updater();
  });
  const cancelSmoothScroll = vi.fn();
  const scrollToItem = vi.fn();
  const scrollerElement = vi.fn(() => null);

  return {
    methods: {
      data: {
        get: () => items,
        replace,
        deleteRange,
        insert,
        append,
        prepend,
        map,
        mapWithAnchor,
        batch,
      },
      cancelSmoothScroll,
      scrollToItem,
      scrollerElement,
    },
    spies: {
      replace,
      deleteRange,
      insert,
      append,
      prepend,
      map,
      mapWithAnchor,
      batch,
      cancelSmoothScroll,
      scrollToItem,
      scrollerElement,
    },
  };
}

function createFakeScroller({
  scrollHeight,
  clientHeight,
  scrollTop,
}: {
  scrollHeight: number;
  clientHeight: number;
  scrollTop: number;
}) {
  const scroller = document.createElement("div");
  Object.defineProperty(scroller, "scrollHeight", {
    configurable: true,
    value: scrollHeight,
  });
  Object.defineProperty(scroller, "clientHeight", {
    configurable: true,
    value: clientHeight,
  });
  scroller.scrollTop = scrollTop;
  return scroller;
}

function makeScrollLocation(overrides: Partial<ListScrollLocation> = {}): ListScrollLocation {
  return {
    bottomOffset: 0,
    isAtBottom: true,
    listOffset: -600,
    scrollHeight: 1200,
    visibleListHeight: 600,
    ...overrides,
  };
}

describe("useSessionMessageListController", () => {
  beforeEach(() => {
    coalescedItems = [];
    vi.unstubAllGlobals();
    vi.useRealTimers();
  });

  it("initializes the keyed message list from the visible coalesced items", () => {
    const rawItems = [makeSpacer("raw-1"), makeSpacer("raw-2"), makeSpacer("raw-3")];
    coalescedItems = [makeSpacer("visible-1"), makeSpacer("visible-2")];

    const { result } = renderHook(() =>
      useSessionMessageListController({
        sessionId: "session-1",
        isActive: false,
        loaded: true,
        listItems: rawItems,
        canLoadOlder: false,
        loadOlder: async () => {},
        layoutRevision: "layout-1",
        itemSizeCacheKey: () => null,
        showDebug: false,
      }),
    );

    expect(result.current.initialData).toEqual(coalescedItems);
  });

  it("bypasses coalescing during a session boundary", () => {
    const sessionOneRaw = [makeSpacer("session-1-raw")];
    const sessionTwoRaw = [makeSpacer("session-2-raw-1"), makeSpacer("session-2-raw-2")];
    coalescedItems = [makeSpacer("session-1-visible")];

    const { result, rerender } = renderHook(
      ({ sessionId, listItems }: { sessionId: string; listItems: WorkbenchListItem[] }) =>
        useSessionMessageListController({
          sessionId,
          isActive: false,
          loaded: true,
          listItems,
          canLoadOlder: false,
          loadOlder: async () => {},
          layoutRevision: "layout-1",
          itemSizeCacheKey: () => null,
          showDebug: false,
        }),
      {
        initialProps: {
          sessionId: "session-1",
          listItems: sessionOneRaw,
        },
      },
    );

    expect(result.current.initialData).toEqual(coalescedItems);

    coalescedItems = [makeSpacer("stale-visible-session-1")];
    rerender({
      sessionId: "session-2",
      listItems: sessionTwoRaw,
    });

    expect(result.current.initialData).toEqual(sessionTwoRaw);
  });

  it("replaces the list immediately when the session boundary changes", () => {
    const sessionOneItems = [makeSpacer("session-1-a"), makeSpacer("session-1-b")];
    const sessionTwoItems = [makeSpacer("session-2-a"), makeSpacer("session-2-b")];
    const fake = createFakeMethods(sessionOneItems);
    coalescedItems = sessionOneItems;

    const { result, rerender } = renderHook(
      ({ sessionId, listItems }: { sessionId: string; listItems: WorkbenchListItem[] }) =>
        useSessionMessageListController({
          sessionId,
          isActive: true,
          loaded: true,
          listItems,
          canLoadOlder: false,
          loadOlder: async () => {},
          layoutRevision: "layout-1",
          itemSizeCacheKey: () => null,
          showDebug: false,
        }),
      {
        initialProps: {
          sessionId: "session-1",
          listItems: sessionOneItems,
        },
      },
    );

    result.current.methodsRef.current = fake.methods as unknown as typeof result.current.methodsRef.current;
    rerender({
      sessionId: "session-1",
      listItems: sessionOneItems,
    });

    fake.spies.replace.mockClear();
    fake.spies.batch.mockClear();
    fake.spies.map.mockClear();
    fake.spies.mapWithAnchor.mockClear();
    coalescedItems = [makeSpacer("stale-session-1-visible")];

    rerender({
      sessionId: "session-2",
      listItems: sessionTwoItems,
    });

    expect(fake.spies.replace).toHaveBeenCalledWith(sessionTwoItems);
    expect(fake.spies.batch).not.toHaveBeenCalled();
    expect(fake.spies.map).not.toHaveBeenCalled();
    expect(fake.spies.mapWithAnchor).not.toHaveBeenCalled();
  });

  it("reconciles bottom-locked mixed structural updates without a full replace", () => {
    const initialItems = Array.from({ length: 10 }, (_, index) => makeSpacer(`current-${index}`));
    const mixedStructuralNext = [
      initialItems[0]!,
      ...Array.from({ length: 20 }, (_, index) => makeSpacer(`next-middle-${index}`)),
      initialItems.at(-1)!,
    ];
    const fake = createFakeMethods();
    coalescedItems = initialItems;

    const { result, rerender } = renderHook(
      ({ listItems, layoutRevision }: { listItems: WorkbenchListItem[]; layoutRevision: string }) =>
        useSessionMessageListController({
          sessionId: "session-1",
          isActive: true,
          loaded: true,
          listItems,
          canLoadOlder: false,
          loadOlder: async () => {},
          layoutRevision,
          itemSizeCacheKey: () => null,
          showDebug: false,
        }),
      {
        initialProps: {
          listItems: initialItems,
          layoutRevision: "layout-1",
        },
      },
    );

    result.current.methodsRef.current = fake.methods as unknown as typeof result.current.methodsRef.current;
    rerender({
      listItems: [...initialItems],
      layoutRevision: "layout-1",
    });

    fake.spies.replace.mockClear();
    fake.spies.deleteRange.mockClear();
    fake.spies.insert.mockClear();
    fake.spies.batch.mockClear();
    fake.spies.cancelSmoothScroll.mockClear();
    fake.spies.scrollToItem.mockClear();
    coalescedItems = mixedStructuralNext;

    rerender({
      listItems: mixedStructuralNext,
      layoutRevision: "layout-1",
    });

    expect(fake.spies.replace).not.toHaveBeenCalled();
    expect(fake.spies.batch).toHaveBeenCalled();
  });

  it("replaces same-length large middle churn while bottom-locked", () => {
    const initialItems = Array.from({ length: 231 }, (_, index) => makeSpacer(`current-${index}`));
    const mixedStructuralNext = [
      ...initialItems.slice(0, 92),
      ...Array.from({ length: 138 }, (_, index) => makeSpacer(`next-middle-${index}`)),
      initialItems.at(-1)!,
    ];
    const fake = createFakeMethods();
    coalescedItems = initialItems;

    const { result, rerender } = renderHook(
      ({ listItems, layoutRevision }: { listItems: WorkbenchListItem[]; layoutRevision: string }) =>
        useSessionMessageListController({
          sessionId: "session-1",
          isActive: true,
          loaded: true,
          listItems,
          canLoadOlder: false,
          loadOlder: async () => {},
          layoutRevision,
          itemSizeCacheKey: () => null,
          showDebug: false,
        }),
      {
        initialProps: {
          listItems: initialItems,
          layoutRevision: "layout-1",
        },
      },
    );

    result.current.methodsRef.current = fake.methods as unknown as typeof result.current.methodsRef.current;
    rerender({
      listItems: [...initialItems],
      layoutRevision: "layout-1",
    });

    fake.spies.replace.mockClear();
    fake.spies.deleteRange.mockClear();
    fake.spies.insert.mockClear();
    fake.spies.batch.mockClear();
    fake.spies.cancelSmoothScroll.mockClear();
    fake.spies.scrollToItem.mockClear();
    coalescedItems = mixedStructuralNext;

    rerender({
      listItems: mixedStructuralNext,
      layoutRevision: "layout-1",
    });

    expect(fake.spies.replace).toHaveBeenCalledTimes(1);
    expect(fake.spies.batch).not.toHaveBeenCalled();
    expect(fake.spies.cancelSmoothScroll).toHaveBeenCalledTimes(1);
  });

  it("replaces bottom-locked destructive shrink reconciles to purge stale row sizes", () => {
    const initialItems = Array.from({ length: 51 }, (_, index) => makeSpacer(`current-${index}`));
    const nextItems = [...Array.from({ length: 15 }, (_, index) => makeSpacer(`next-${index}`)), initialItems.at(-1)!];
    const threadOp: WorkbenchThreadProjectionOp = {
      kind: "reconcile",
      projectionRevision: 2,
      changedItemIds: nextItems.map((item) => item.id),
      remeasureItemIds: nextItems.map((item) => item.id),
    };
    const fake = createFakeMethods(initialItems);
    coalescedItems = initialItems;

    const { result, rerender } = renderHook(
      ({
        listItems,
        activeThreadOp,
      }: {
        listItems: WorkbenchListItem[];
        activeThreadOp: WorkbenchThreadProjectionOp | null;
      }) =>
        useSessionMessageListController({
          sessionId: "session-1",
          isActive: true,
          loaded: true,
          listItems,
          canLoadOlder: false,
          loadOlder: async () => {},
          layoutRevision: "layout-1",
          itemSizeCacheKey: () => null,
          threadOp: activeThreadOp,
          showDebug: false,
        }),
      {
        initialProps: {
          listItems: initialItems,
          activeThreadOp: null as WorkbenchThreadProjectionOp | null,
        },
      },
    );

    result.current.methodsRef.current = fake.methods as unknown as typeof result.current.methodsRef.current;
    rerender({
      listItems: [...initialItems],
      activeThreadOp: null,
    });

    fake.spies.replace.mockClear();
    fake.spies.batch.mockClear();
    fake.spies.cancelSmoothScroll.mockClear();
    fake.spies.scrollToItem.mockClear();
    coalescedItems = nextItems;

    rerender({
      listItems: nextItems,
      activeThreadOp: threadOp,
    });

    expect(fake.spies.replace).toHaveBeenCalledTimes(1);
    expect(fake.spies.batch).not.toHaveBeenCalled();
    expect(fake.spies.cancelSmoothScroll).toHaveBeenCalledTimes(1);
    expect(fake.spies.scrollToItem).toHaveBeenCalledWith({ index: "LAST", align: "end", behavior: "auto" });
  });

  it("settles bottom lock immediately for localized remeasure updates", () => {
    const initialItems = [makeAssistant("short reply"), makeTurnStatus()];
    const nextItems = [makeAssistant("short reply\nwith another line"), makeTurnStatus()];
    const threadOp: WorkbenchThreadProjectionOp = {
      kind: "reconcile",
      projectionRevision: 2,
      changedItemIds: ["assistant-turn-1-pending"],
      remeasureItemIds: ["assistant-turn-1-pending", "turn-status-turn-1"],
    };
    const fake = createFakeMethods(initialItems);
    coalescedItems = initialItems;

    const { result, rerender } = renderHook(
      ({ listItems, activeThreadOp }: { listItems: WorkbenchListItem[]; activeThreadOp: WorkbenchThreadProjectionOp | null }) =>
        useSessionMessageListController({
          sessionId: "session-1",
          isActive: true,
          loaded: true,
          listItems,
          canLoadOlder: false,
          loadOlder: async () => {},
          layoutRevision: "layout-1",
          itemSizeCacheKey: () => null,
          threadOp: activeThreadOp,
          showDebug: false,
        }),
      {
        initialProps: {
          listItems: initialItems,
          activeThreadOp: null as WorkbenchThreadProjectionOp | null,
        },
      },
    );

    result.current.methodsRef.current = fake.methods as unknown as typeof result.current.methodsRef.current;
    rerender({
      listItems: initialItems,
      activeThreadOp: null,
    });

    fake.spies.scrollToItem.mockClear();
    fake.spies.map.mockClear();
    fake.spies.replace.mockClear();
    coalescedItems = nextItems;

    rerender({
      listItems: nextItems,
      activeThreadOp: threadOp,
    });

    expect(fake.spies.replace).not.toHaveBeenCalled();
    expect(fake.spies.batch).toHaveBeenCalled();
    expect(fake.spies.map).toHaveBeenCalled();
    expect(fake.spies.scrollToItem).toHaveBeenCalledWith({ index: "LAST", align: "end", behavior: "auto" });
  });

  it("releases bottom lock on upward wheel intent before a streamed remeasure update", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-03-25T11:00:00.000Z"));
    const initialItems = [makeAssistant("short reply"), makeTurnStatus()];
    const nextItems = [makeAssistant("short reply\nwith another line"), makeTurnStatus()];
    const threadOp: WorkbenchThreadProjectionOp = {
      kind: "reconcile",
      projectionRevision: 2,
      changedItemIds: ["assistant-turn-1-pending"],
      remeasureItemIds: ["assistant-turn-1-pending", "turn-status-turn-1"],
    };
    const fake = createFakeMethods(initialItems);
    const scroller = createFakeScroller({
      scrollHeight: 1200,
      clientHeight: 600,
      scrollTop: 600,
    });
    const onAtBottomChange = vi.fn();
    fake.spies.scrollerElement.mockReturnValue(scroller as unknown as ReturnType<typeof fake.methods.scrollerElement>);
    coalescedItems = initialItems;

    const { result, rerender } = renderHook(
      ({ listItems, activeThreadOp }: { listItems: WorkbenchListItem[]; activeThreadOp: WorkbenchThreadProjectionOp | null }) =>
        useSessionMessageListController({
          sessionId: "session-1",
          isActive: true,
          loaded: true,
          listItems,
          canLoadOlder: false,
          loadOlder: async () => {},
          layoutRevision: "layout-1",
          itemSizeCacheKey: () => null,
          threadOp: activeThreadOp,
          showDebug: false,
          onAtBottomChange,
        }),
      {
        initialProps: {
          listItems: initialItems,
          activeThreadOp: null as WorkbenchThreadProjectionOp | null,
        },
      },
    );

    result.current.methodsRef.current = fake.methods as unknown as typeof result.current.methodsRef.current;
    rerender({
      listItems: initialItems,
      activeThreadOp: null,
    });

    scroller.dispatchEvent(new WheelEvent("wheel", { deltaY: -40 }));
    result.current.onScroll(makeScrollLocation());

    fake.spies.scrollToItem.mockClear();
    fake.spies.map.mockClear();
    fake.spies.replace.mockClear();
    fake.spies.batch.mockClear();
    coalescedItems = nextItems;

    rerender({
      listItems: nextItems,
      activeThreadOp: threadOp,
    });

    expect(onAtBottomChange).toHaveBeenCalledWith(false);
    expect(fake.spies.replace).not.toHaveBeenCalled();
    expect(fake.spies.batch).toHaveBeenCalled();
    expect(fake.spies.map).toHaveBeenCalled();
    expect(fake.spies.scrollToItem).not.toHaveBeenCalled();
  });

  it("allows bottom lock to re-enter after the user-scroll hold window expires", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-03-25T11:00:00.000Z"));
    const fake = createFakeMethods([makeAssistant("short reply"), makeTurnStatus()]);
    const scroller = createFakeScroller({
      scrollHeight: 1200,
      clientHeight: 600,
      scrollTop: 600,
    });
    const onAtBottomChange = vi.fn();
    fake.spies.scrollerElement.mockReturnValue(scroller as unknown as ReturnType<typeof fake.methods.scrollerElement>);

    const { result, rerender } = renderHook(() =>
      useSessionMessageListController({
        sessionId: "session-1",
        isActive: true,
        loaded: true,
        listItems: [makeAssistant("short reply"), makeTurnStatus()],
        canLoadOlder: false,
        loadOlder: async () => {},
        layoutRevision: "layout-1",
        itemSizeCacheKey: () => null,
        showDebug: false,
        onAtBottomChange,
      }),
    );

    result.current.methodsRef.current = fake.methods as unknown as typeof result.current.methodsRef.current;
    rerender();

    scroller.dispatchEvent(new WheelEvent("wheel", { deltaY: -40 }));
    result.current.onScroll(makeScrollLocation());

    vi.advanceTimersByTime(700);
    result.current.onScroll(makeScrollLocation());

    expect(onAtBottomChange).toHaveBeenCalledWith(false);
    expect(onAtBottomChange).toHaveBeenLastCalledWith(true);
  });

});
