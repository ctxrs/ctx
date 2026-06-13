import { describe, expect, it, vi } from "vitest";
import type { ItemLocation, VirtuosoMessageListMethods } from "@virtuoso.dev/message-list";
import type { WorkbenchListItem } from "./SessionPage.types";
import type { WorkbenchMessageListContext } from "./SessionPage.thread";
import { applySessionMessageListInvalidation } from "./sessionMessageListInvalidation";

type MessageListMethods = VirtuosoMessageListMethods<WorkbenchListItem, WorkbenchMessageListContext>;

const INITIAL_LOCATION_BOTTOM: ItemLocation = { index: "LAST", align: "end" };

const buildItem = (id: string): WorkbenchListItem =>
  ({
    kind: "message",
    id,
    role: "user",
    content: id,
    attachments: [],
    created_at: "2026-03-16T00:00:00.000Z",
  }) as unknown as WorkbenchListItem;

function createFakeMethods() {
  return {
    cancelSmoothScroll: vi.fn(),
    data: {
      replace: vi.fn(),
      deleteRange: vi.fn(),
    },
  } as unknown as MessageListMethods;
}

describe("applySessionMessageListInvalidation", () => {
  it("defers shared size-cache key changes to stable row remounting instead of replacing the whole list", () => {
    const current = [buildItem("b"), buildItem("c")];
    const nextRaw = [buildItem("a"), buildItem("b"), buildItem("c"), buildItem("live-1")];
    const next = [buildItem("a"), buildItem("b"), buildItem("c")];
    const methods = createFakeMethods();

    const handled = applySessionMessageListInvalidation({
      sessionId: "session-1",
      layoutRevision: "layout-1",
      current,
      nextRaw,
      next,
      currentLen: current.length,
      effectiveNextLen: next.length,
      methods,
      initialLocation: INITIAL_LOCATION_BOTTOM,
      lastLayoutRevisionRef: { current: "layout-1" },
      historyExpectedRef: { current: false },
      historyRequestedAtTopRef: { current: false },
      historyRequestedAnchorIdRef: { current: null },
      stickToBottomRef: { current: false },
      renderedTopIdRef: { current: "b" },
      renderedAnchorIdRef: { current: "c" },
      suppressIdDiffLogsRef: { current: null },
      snapToBottom: vi.fn(),
      startFlashProbe: vi.fn(),
      recordDebugSnapshot: vi.fn(),
      logMessageListDebug: vi.fn(),
      showDebug: false,
    });

    expect(handled).toBe(false);
    expect(methods.data.replace).not.toHaveBeenCalled();
  });
});
