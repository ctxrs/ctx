import { createPretextVirtualizerCore } from "@pretext-virtualizer/core";
import { describe, expect, it } from "vitest";
import type { WorkbenchListItem } from "../SessionPage.types";
import type { WorkbenchThreadProjectionOp } from "../sessionThreadProjection";
import {
  buildVisibleProjectionUpdatePlan,
  type SessionThreadVisibleProjectionPreparedState,
} from "./pretextVirtualizerProjectionController";

function makeMessage(id: string): Extract<WorkbenchListItem, { kind: "message" }> {
  return {
    kind: "message",
    id,
    role: "user",
    content: id,
    attachments: [],
    created_at: "2026-04-19T00:00:00.000Z",
  };
}

function createPreparedState(
  items: readonly WorkbenchListItem[],
  heights: Record<string, number>,
): {
  core: ReturnType<typeof createPretextVirtualizerCore<WorkbenchListItem>>;
  preparedState: SessionThreadVisibleProjectionPreparedState;
} {
  const core = createPretextVirtualizerCore<WorkbenchListItem>({
    initialItems: items,
    getPlannedLayout: (item) => ({ height: heights[item.id] ?? 40 }),
    getId: (item) => item.id,
    getLayoutRevision: () => 0,
    viewportHeight: 120,
    viewportWidth: 320,
    overscanPx: 0,
  });
  const snapshot = core.syncViewport({
    height: 120,
    width: 320,
    scrollTop: 0,
  });
  return {
    core,
    preparedState: {
      snapshot,
      listItems: items,
    },
  };
}

const noopProjectionOp: WorkbenchThreadProjectionOp = {
  kind: "noop",
  projectionRevision: 0,
  changedItemIds: [],
  remeasureItemIds: [],
};

describe("buildVisibleProjectionUpdatePlan", () => {
  it("returns noop when visible inputs, projection, and ui state are unchanged", () => {
    const items = [makeMessage("message-1"), makeMessage("message-2")];
    const { core, preparedState } = createPreparedState(items, {
      "message-1": 40,
      "message-2": 40,
    });

    const plan = buildVisibleProjectionUpdatePlan({
      core,
      preparedState,
      listItems: items,
      threadProjectionOp: noopProjectionOp,
      runtimeUiStateLayoutRevision: "ui-a",
      lastAppliedUiStateLayoutRevision: "ui-a",
      lastAppliedProjectionOpKey: null,
      viewport: {
        width: 320,
        height: 120,
        scrollTop: 0,
      },
      followBottom: false,
      atBottom: false,
      activeChangedItemId: null,
      getLayoutRevision: () => 0,
      bottomThresholdPx: 12,
    });

    expect(plan).toEqual({
      kind: "noop",
      projectionOpKey: null,
      uiStateChanged: false,
      itemsChanged: false,
      projectionChanged: false,
    });
  });

  it("skips a first visible relayout when the prepared items already match a reconcile op", () => {
    const items = [makeMessage("message-1"), makeMessage("message-2")];
    const { core, preparedState } = createPreparedState(items, {
      "message-1": 40,
      "message-2": 40,
    });

    const plan = buildVisibleProjectionUpdatePlan({
      core,
      preparedState,
      listItems: items,
      threadProjectionOp: {
        kind: "reconcile",
        projectionRevision: 7,
        changedItemIds: ["message-1", "message-2"],
        remeasureItemIds: ["message-1", "message-2"],
      },
      runtimeUiStateLayoutRevision: "ui-a",
      lastAppliedUiStateLayoutRevision: "ui-a",
      lastAppliedProjectionOpKey: null,
      viewport: {
        width: 320,
        height: 120,
        scrollTop: 0,
      },
      followBottom: false,
      atBottom: false,
      activeChangedItemId: null,
      getLayoutRevision: () => 0,
      bottomThresholdPx: 12,
    });

    expect(plan).toEqual({
      kind: "noop",
      projectionOpKey: expect.stringContaining("7|reconcile|message-1,message-2|message-1,message-2|"),
      uiStateChanged: false,
      itemsChanged: false,
      projectionChanged: true,
    });
  });

  it("returns a localized patch plan for mid-list projection updates", () => {
    const initialItems = [
      makeMessage("message-1"),
      makeMessage("message-2"),
      makeMessage("message-3"),
      makeMessage("message-4"),
    ];
    const nextItems = [
      initialItems[0]!,
      makeMessage("message-inserted-1"),
      makeMessage("message-inserted-2"),
      ...initialItems.slice(1),
    ];
    const { core, preparedState } = createPreparedState(initialItems, {
      "message-1": 40,
      "message-2": 40,
      "message-3": 40,
      "message-4": 40,
      "message-inserted-1": 40,
      "message-inserted-2": 40,
    });

    const plan = buildVisibleProjectionUpdatePlan({
      core,
      preparedState,
      listItems: nextItems,
      threadProjectionOp: {
        kind: "hydrate_tools",
        projectionRevision: 1,
        changedItemIds: [
          "message-inserted-1",
          "message-inserted-2",
          "message-2",
          "message-3",
          "message-4",
        ],
        remeasureItemIds: ["message-inserted-1", "message-inserted-2"],
      },
      runtimeUiStateLayoutRevision: "ui-a",
      lastAppliedUiStateLayoutRevision: "ui-a",
      lastAppliedProjectionOpKey: null,
      viewport: {
        width: 320,
        height: 120,
        scrollTop: 0,
      },
      followBottom: false,
      atBottom: false,
      activeChangedItemId: null,
      getLayoutRevision: () => 0,
      bottomThresholdPx: 12,
    });

    expect(plan.kind).toBe("apply");
    if (plan.kind !== "apply") {
      throw new Error("Expected apply plan");
    }
    expect(plan.changeKind).toBe("localized-patch");
    expect(plan.reason).toBe("visible:hydrate_tools");
    expect(plan.shouldFollowBottom).toBe(false);
    expect(plan.projectionOpKey).toContain(
      "1|hydrate_tools|message-inserted-1,message-inserted-2,message-2,message-3,message-4|message-inserted-1,message-inserted-2|",
    );
  });

  it("treats same-row append_stream updates as localized patches", () => {
    const initialItems = [makeMessage("message-1"), makeMessage("message-2")];
    const nextItems = [
      initialItems[0]!,
      {
        ...makeMessage("message-2"),
        content: "streamed update",
      },
    ];
    const { core, preparedState } = createPreparedState(initialItems, {
      "message-1": 40,
      "message-2": 40,
    });

    const plan = buildVisibleProjectionUpdatePlan({
      core,
      preparedState,
      listItems: nextItems,
      threadProjectionOp: {
        kind: "append_stream",
        projectionRevision: 1,
        changedItemIds: ["message-2"],
        remeasureItemIds: ["message-1", "message-2"],
      },
      runtimeUiStateLayoutRevision: "ui-a",
      lastAppliedUiStateLayoutRevision: "ui-a",
      lastAppliedProjectionOpKey: null,
      viewport: {
        width: 320,
        height: 120,
        scrollTop: 0,
      },
      followBottom: false,
      atBottom: false,
      activeChangedItemId: "message-2",
      getLayoutRevision: (item) => (item.kind === "message" ? item.content : 0),
      bottomThresholdPx: 12,
    });

    expect(plan.kind).toBe("apply");
    if (plan.kind !== "apply") {
      throw new Error("Expected apply plan");
    }
    expect(plan.changeKind).toBe("localized-patch");
    expect(plan.reason).toBe("visible:append_stream");
  });

  it("keeps consecutive same-row append_stream updates localized", () => {
    const firstItems = [
      makeMessage("message-1"),
      {
        ...makeMessage("message-2"),
        content: "streamed update",
      },
    ];
    const secondItems = [
      firstItems[0]!,
      {
        ...makeMessage("message-2"),
        content: "streamed update plus more text",
      },
    ];
    const { core, preparedState } = createPreparedState(firstItems, {
      "message-1": 40,
      "message-2": 40,
    });
    const threadProjectionOp: WorkbenchThreadProjectionOp = {
      kind: "append_stream",
      projectionRevision: 1,
      changedItemIds: ["message-2"],
      remeasureItemIds: ["message-1", "message-2"],
    };
    const getLayoutRevision = (item: WorkbenchListItem) =>
      item.kind === "message" ? item.content : 0;
    const firstPlan = buildVisibleProjectionUpdatePlan({
      core,
      preparedState,
      listItems: firstItems,
      threadProjectionOp,
      runtimeUiStateLayoutRevision: "ui-a",
      lastAppliedUiStateLayoutRevision: "ui-a",
      lastAppliedProjectionOpKey: null,
      viewport: {
        width: 320,
        height: 120,
        scrollTop: 0,
      },
      followBottom: false,
      atBottom: false,
      activeChangedItemId: "message-2",
      getLayoutRevision,
      bottomThresholdPx: 12,
    });

    expect(firstPlan).toEqual({
      kind: "noop",
      projectionOpKey: expect.stringContaining("1|append_stream|message-2|message-1,message-2|"),
      uiStateChanged: false,
      itemsChanged: false,
      projectionChanged: true,
    });
    const secondPlan = buildVisibleProjectionUpdatePlan({
      core,
      preparedState,
      listItems: secondItems,
      threadProjectionOp,
      runtimeUiStateLayoutRevision: "ui-a",
      lastAppliedUiStateLayoutRevision: "ui-a",
      lastAppliedProjectionOpKey: firstPlan.projectionOpKey,
      viewport: {
        width: 320,
        height: 120,
        scrollTop: 0,
      },
      followBottom: false,
      atBottom: false,
      activeChangedItemId: "message-2",
      getLayoutRevision,
      bottomThresholdPx: 12,
    });

    expect(secondPlan.kind).toBe("apply");
    if (secondPlan.kind !== "apply") {
      throw new Error("Expected apply plan");
    }
    expect(secondPlan.projectionOpKey).not.toBe(firstPlan.projectionOpKey);
    expect(secondPlan.changeKind).toBe("localized-patch");
    expect(secondPlan.reason).toBe("visible:append_stream");
  });

  it("forces a full relayout when only the ui state revision changes", () => {
    const items = [makeMessage("message-1"), makeMessage("message-2")];
    const { core, preparedState } = createPreparedState(items, {
      "message-1": 40,
      "message-2": 40,
    });

    const plan = buildVisibleProjectionUpdatePlan({
      core,
      preparedState,
      listItems: items,
      threadProjectionOp: noopProjectionOp,
      runtimeUiStateLayoutRevision: "ui-b",
      lastAppliedUiStateLayoutRevision: "ui-a",
      lastAppliedProjectionOpKey: null,
      viewport: {
        width: 320,
        height: 120,
        scrollTop: 0,
      },
      followBottom: false,
      atBottom: false,
      activeChangedItemId: null,
      getLayoutRevision: () => 0,
      bottomThresholdPx: 12,
    });

    expect(plan.kind).toBe("apply");
    if (plan.kind !== "apply") {
      throw new Error("Expected apply plan");
    }
    expect(plan.changeKind).toBe("full-relayout");
    expect(plan.reason).toBe("visible:ui-state");
    expect(plan.uiStateChanged).toBe(true);
  });

  it("anchors prepend-history updates to the current top visible row when detached", () => {
    const currentItems = [
      makeMessage("message-3"),
      makeMessage("message-4"),
      makeMessage("message-5"),
    ];
    const nextItems = [
      makeMessage("message-1"),
      makeMessage("message-2"),
      ...currentItems,
    ];
    const { core, preparedState } = createPreparedState(currentItems, {
      "message-1": 40,
      "message-2": 40,
      "message-3": 60,
      "message-4": 80,
      "message-5": 40,
    });

    const plan = buildVisibleProjectionUpdatePlan({
      core,
      preparedState,
      listItems: nextItems,
      threadProjectionOp: {
        kind: "prepend_history",
        projectionRevision: 1,
        changedItemIds: ["message-1", "message-2"],
        remeasureItemIds: ["message-1", "message-2"],
      },
      runtimeUiStateLayoutRevision: "ui-a",
      lastAppliedUiStateLayoutRevision: "ui-a",
      lastAppliedProjectionOpKey: null,
      viewport: {
        width: 320,
        height: 120,
        scrollTop: 0,
      },
      followBottom: false,
      atBottom: false,
      activeChangedItemId: null,
      getLayoutRevision: () => 0,
      bottomThresholdPx: 12,
    });

    expect(plan.kind).toBe("apply");
    if (plan.kind !== "apply") {
      throw new Error("Expected apply plan");
    }
    expect(plan.anchorOverride).toEqual({
      kind: "item",
      id: "message-3",
      index: 0,
      offsetPx: 0,
      offsetRatio: 0,
    });
  });
});
