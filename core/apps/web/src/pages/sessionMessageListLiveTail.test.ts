import { describe, expect, it } from "vitest";
import type { WorkbenchListItem } from "./SessionPage.types";
import { splitWorkbenchListItemsByGroup } from "./sessionMessageListLiveTail";

function mkItem(id: string): WorkbenchListItem {
  return {
    kind: "message",
    id,
    role: "user",
    content: id,
    attachments: [],
    created_at: "2025-01-01T00:00:00.000Z",
  };
}

describe("splitWorkbenchListItemsByGroup", () => {
  it("keeps all items in history when no live group is configured", () => {
    const listItems = [mkItem("a"), mkItem("b"), mkItem("c")];

    const result = splitWorkbenchListItemsByGroup({
      listItems,
      groupRanges: new Map(),
      liveGroupKey: null,
    });

    expect(result.historyListItems).toEqual(listItems);
    expect(result.liveTailItems).toEqual([]);
  });

  it("splits list at the configured group start", () => {
    const listItems = [mkItem("a"), mkItem("b"), mkItem("c"), mkItem("d")];
    const groupRanges = new Map([
      ["turn-1", { start: 1, end: 4 }],
      ["turn-2", { start: 4, end: 4 }],
    ]);

    const result = splitWorkbenchListItemsByGroup({
      listItems,
      groupRanges,
      liveGroupKey: "turn-1",
    });

    expect(result.historyListItems).toEqual([mkItem("a")]);
    expect(result.liveTailItems).toEqual([mkItem("b"), mkItem("c"), mkItem("d")]);
  });

  it("falls back to full history when the live group key is missing", () => {
    const listItems = [mkItem("a"), mkItem("b"), mkItem("c")];

    const result = splitWorkbenchListItemsByGroup({
      listItems,
      groupRanges: new Map([["turn-1", { start: 0, end: 1 }]]),
      liveGroupKey: "turn-2",
    });

    expect(result.historyListItems).toEqual(listItems);
    expect(result.liveTailItems).toEqual([]);
  });
});
