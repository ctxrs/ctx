import { render, waitFor } from "@testing-library/react";
import {
  VirtuosoMessageListTestingContext,
  type DataWithScrollModifier,
  type VirtuosoMessageListMethods,
} from "@virtuoso.dev/message-list";
import { beforeEach, describe, expect, it } from "vitest";
import type { MutableRefObject } from "react";
import { WorkbenchMessageListStack, type WorkbenchMessageListContext } from "./SessionPage.thread";
import type { WorkbenchListItem } from "../sessionView/SessionPage.types";
import { getWorkbenchListItemRenderKey } from "../sessionMessageListStableUpdate";

const baseContext: WorkbenchMessageListContext = {
  loaded: true,
  loadingOlder: false,
};

const buildAssistantItem = (
  overrides: Partial<Extract<WorkbenchListItem, { kind: "assistant" }>> = {},
): WorkbenchListItem => ({
  kind: "assistant",
  id: "assistant-turn-1-pending",
  turn_id: "turn-1",
  created_at: "2026-03-15T00:00:02.500Z",
  content: "short reply",
  thought: "",
  is_complete: false,
  ...overrides,
});

const buildTurnStatusItem = (
  overrides: Partial<Extract<WorkbenchListItem, { kind: "turn_status" }>> = {},
): WorkbenchListItem => ({
  kind: "turn_status",
  id: "turn-status-turn-1",
  turn_id: "turn-1",
  created_at: "2026-03-15T00:00:02.000Z",
  started_at: "2026-03-15T00:00:00.000Z",
  updated_at: "2026-03-15T00:00:02.000Z",
  status: "running",
  custom_status: null,
  assistant_messages_content: "",
  ...overrides,
});

const buildToolItem = (
  overrides: Partial<Extract<WorkbenchListItem, { kind: "tool" }>> = {},
): WorkbenchListItem => ({
  kind: "tool",
  id: "tool-turn-1-tool-1",
  created_at: "2026-03-15T00:00:00.000Z",
  updated_at: "2026-03-15T00:00:01.000Z",
  tool_call_id: "tool-1",
  tool_kind: "execute",
  title: "Run",
  subtitle: "echo ok",
  status: "completed",
  locations: [],
  input: { command: "echo ok" },
  output_text: "ok",
  raw: null,
  updates_seen: 1,
  ...overrides,
});

function renderMessageList({
  data,
  context = baseContext,
  itemKey = (item: WorkbenchListItem) => getWorkbenchListItemRenderKey(item, context),
}: {
  data: WorkbenchListItem[];
  context?: WorkbenchMessageListContext;
  itemKey?: (item: WorkbenchListItem) => string;
}) {
  const listRef =
    {
      current: null,
    } as MutableRefObject<VirtuosoMessageListMethods<WorkbenchListItem, WorkbenchMessageListContext> | null>;
  const dataState: DataWithScrollModifier<WorkbenchListItem> = { data };

  return render(
    <VirtuosoMessageListTestingContext.Provider value={{ viewportHeight: 600, itemHeight: 120 }}>
      <WorkbenchMessageListStack
        virtuosoStyle={{ height: 400 }}
        initialData={data}
        itemContent={(_index, item) => <div>{item.kind === "assistant" ? item.content : item.kind}</div>}
        itemIdentity={(item) => item.id}
        itemKey={itemKey}
        increaseViewportBy={0}
        initialLocation={{ index: 0, align: "start" }}
        dataState={dataState}
        context={context}
        onScroll={() => {}}
        onRenderedDataChange={() => {}}
        listRef={listRef}
        licenseKey=""
        shortSizeAlign="top"
      />
    </VirtuosoMessageListTestingContext.Provider>,
  );
}

async function findMeasuredWrapper(container: HTMLElement, itemId: string): Promise<HTMLElement> {
  await waitFor(() => {
    const row = container.querySelector(`[data-thread-item-id="${itemId}"]`);
    expect(row).not.toBeNull();
  });
  const row = container.querySelector(`[data-thread-item-id="${itemId}"]`);
  if (!(row instanceof HTMLElement)) {
    throw new Error(`Expected row ${itemId} to be rendered`);
  }
  const wrapper = row.parentElement;
  if (!(wrapper instanceof HTMLElement)) {
    throw new Error(`Expected measured wrapper for ${itemId}`);
  }
  return wrapper;
}

describe("WorkbenchMessageListStack", () => {
  beforeEach(() => {
    class ResizeObserverStub {
      observe() {}
      unobserve() {}
      disconnect() {}
    }
    Object.defineProperty(globalThis, "ResizeObserver", {
      configurable: true,
      writable: true,
      value: ResizeObserverStub,
    });
  });

  it("remounts the measured wrapper when a row's layout key changes", async () => {
    const current = [buildAssistantItem()];
    const next = [
      buildAssistantItem({
        content: "this reply is now much longer and should force the measured wrapper to remount",
      }),
    ];
    const nextContext: WorkbenchMessageListContext = {
      ...baseContext,
      renderRevisionByItemId: { [current[0].id]: 1 },
    };
    const view = renderMessageList({ data: current });
    const firstWrapper = await findMeasuredWrapper(view.container, current[0].id);

    view.rerender(
      <VirtuosoMessageListTestingContext.Provider value={{ viewportHeight: 600, itemHeight: 120 }}>
        <WorkbenchMessageListStack
          virtuosoStyle={{ height: 400 }}
          initialData={current}
          itemContent={(_index, item) => <div>{item.kind === "assistant" ? item.content : item.kind}</div>}
          itemIdentity={(item) => item.id}
          itemKey={(item) => getWorkbenchListItemRenderKey(item, nextContext)}
          increaseViewportBy={0}
          initialLocation={{ index: 0, align: "start" }}
          dataState={{ data: next }}
          context={nextContext}
          onScroll={() => {}}
          onRenderedDataChange={() => {}}
          listRef={
            {
              current: null,
            } as MutableRefObject<VirtuosoMessageListMethods<WorkbenchListItem, WorkbenchMessageListContext> | null>
          }
          licenseKey=""
          shortSizeAlign="top"
        />
      </VirtuosoMessageListTestingContext.Provider>,
    );

    const secondWrapper = await findMeasuredWrapper(view.container, next[0].id);
    expect(secondWrapper).not.toBe(firstWrapper);
  });

  it("keeps the measured wrapper when only non-layout status fields change", async () => {
    const current = [buildTurnStatusItem({ updated_at: "2026-03-15T00:00:05.000Z" })];
    const next = [buildTurnStatusItem({ updated_at: "2026-03-15T00:00:12.000Z" })];
    const view = renderMessageList({ data: current });
    const firstWrapper = await findMeasuredWrapper(view.container, current[0].id);

    view.rerender(
      <VirtuosoMessageListTestingContext.Provider value={{ viewportHeight: 600, itemHeight: 120 }}>
        <WorkbenchMessageListStack
          virtuosoStyle={{ height: 400 }}
          initialData={current}
          itemContent={(_index, item) => <div>{item.kind === "assistant" ? item.content : item.kind}</div>}
          itemIdentity={(item) => item.id}
          itemKey={(item) => getWorkbenchListItemRenderKey(item, baseContext)}
          increaseViewportBy={0}
          initialLocation={{ index: 0, align: "start" }}
          dataState={{ data: next }}
          context={baseContext}
          onScroll={() => {}}
          onRenderedDataChange={() => {}}
          listRef={
            {
              current: null,
            } as MutableRefObject<VirtuosoMessageListMethods<WorkbenchListItem, WorkbenchMessageListContext> | null>
          }
          licenseKey=""
          shortSizeAlign="top"
        />
      </VirtuosoMessageListTestingContext.Provider>,
    );

    const secondWrapper = await findMeasuredWrapper(view.container, next[0].id);
    expect(secondWrapper).toBe(firstWrapper);
  });

  it("remounts only the toggled tool row wrapper when expansion state changes", async () => {
    const firstTool = buildToolItem();
    const secondTool = buildToolItem({
      id: "tool-turn-1-tool-2",
      tool_call_id: "tool-2",
      title: "Run second",
      subtitle: "echo second",
    });
    const current = [firstTool, secondTool];
    const collapsedContext: WorkbenchMessageListContext = {
      loaded: true,
      loadingOlder: false,
      expandedToolById: {},
    };
    const expandedContext: WorkbenchMessageListContext = {
      loaded: true,
      loadingOlder: false,
      expandedToolById: { [secondTool.id]: true },
      renderRevisionByItemId: { [secondTool.id]: 1 },
    };
    const view = renderMessageList({
      data: current,
      context: collapsedContext,
      itemKey: (item) => getWorkbenchListItemRenderKey(item, collapsedContext),
    });
    const firstToolWrapper = await findMeasuredWrapper(view.container, firstTool.id);
    const secondToolWrapper = await findMeasuredWrapper(view.container, secondTool.id);

    view.rerender(
      <VirtuosoMessageListTestingContext.Provider value={{ viewportHeight: 600, itemHeight: 120 }}>
        <WorkbenchMessageListStack
          virtuosoStyle={{ height: 400 }}
          initialData={current}
          itemContent={(_index, item) => <div>{item.kind === "assistant" ? item.content : item.kind}</div>}
          itemIdentity={(item) => item.id}
          itemKey={(item) => getWorkbenchListItemRenderKey(item, expandedContext)}
          increaseViewportBy={0}
          initialLocation={{ index: 0, align: "start" }}
          dataState={{ data: current }}
          context={expandedContext}
          onScroll={() => {}}
          onRenderedDataChange={() => {}}
          listRef={
            {
              current: null,
            } as MutableRefObject<VirtuosoMessageListMethods<WorkbenchListItem, WorkbenchMessageListContext> | null>
          }
          licenseKey=""
          shortSizeAlign="top"
        />
      </VirtuosoMessageListTestingContext.Provider>,
    );

    expect(await findMeasuredWrapper(view.container, firstTool.id)).toBe(firstToolWrapper);
    expect(await findMeasuredWrapper(view.container, secondTool.id)).not.toBe(secondToolWrapper);
  });

  it("remounts a measured wrapper when the same row shifts to a new list index", async () => {
    const shiftedAssistant = buildAssistantItem();
    const current = [shiftedAssistant, buildTurnStatusItem()];
    const next = [buildToolItem(), shiftedAssistant, buildTurnStatusItem()];
    const nextContext: WorkbenchMessageListContext = {
      ...baseContext,
      renderRevisionByItemId: { [shiftedAssistant.id]: 1 },
    };
    const view = renderMessageList({ data: current });
    const firstWrapper = await findMeasuredWrapper(view.container, shiftedAssistant.id);

    view.rerender(
      <VirtuosoMessageListTestingContext.Provider value={{ viewportHeight: 600, itemHeight: 120 }}>
        <WorkbenchMessageListStack
          virtuosoStyle={{ height: 400 }}
          initialData={current}
          itemContent={(_index, item) => <div>{item.kind === "assistant" ? item.content : item.kind}</div>}
          itemIdentity={(item) => item.id}
          itemKey={(item) => getWorkbenchListItemRenderKey(item, nextContext)}
          increaseViewportBy={0}
          initialLocation={{ index: 0, align: "start" }}
          dataState={{ data: next }}
          context={nextContext}
          onScroll={() => {}}
          onRenderedDataChange={() => {}}
          listRef={
            {
              current: null,
            } as MutableRefObject<VirtuosoMessageListMethods<WorkbenchListItem, WorkbenchMessageListContext> | null>
          }
          licenseKey=""
          shortSizeAlign="top"
        />
      </VirtuosoMessageListTestingContext.Provider>,
    );

    const secondWrapper = await findMeasuredWrapper(view.container, shiftedAssistant.id);
    expect(secondWrapper).not.toBe(firstWrapper);
  });

  it("remounts a turn-status row when a running turn terminalizes", async () => {
    const current = [buildTurnStatusItem({ status: "running", assistant_messages_content: "" })];
    const next = [buildTurnStatusItem({ status: "completed", assistant_messages_content: "done" })];
    const nextContext: WorkbenchMessageListContext = {
      ...baseContext,
      renderRevisionByItemId: { [current[0].id]: 1 },
    };
    const view = renderMessageList({ data: current });
    const firstWrapper = await findMeasuredWrapper(view.container, current[0].id);

    view.rerender(
      <VirtuosoMessageListTestingContext.Provider value={{ viewportHeight: 600, itemHeight: 120 }}>
        <WorkbenchMessageListStack
          virtuosoStyle={{ height: 400 }}
          initialData={current}
          itemContent={(_index, item) => <div>{item.kind === "turn_status" ? item.status : item.kind}</div>}
          itemIdentity={(item) => item.id}
          itemKey={(item) => getWorkbenchListItemRenderKey(item, nextContext)}
          increaseViewportBy={0}
          initialLocation={{ index: 0, align: "start" }}
          dataState={{ data: next }}
          context={nextContext}
          onScroll={() => {}}
          onRenderedDataChange={() => {}}
          listRef={
            {
              current: null,
            } as MutableRefObject<VirtuosoMessageListMethods<WorkbenchListItem, WorkbenchMessageListContext> | null>
          }
          licenseKey=""
          shortSizeAlign="top"
        />
      </VirtuosoMessageListTestingContext.Provider>,
    );

    expect(await findMeasuredWrapper(view.container, next[0].id)).not.toBe(firstWrapper);
    expect(view.container).toHaveTextContent("completed");
  });
});
