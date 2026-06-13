import { describe, expect, it, vi } from "vitest";
import type { VirtuosoMessageListMethods } from "@virtuoso.dev/message-list";
import type { WorkbenchListItem } from "./SessionPage.types";
import type { WorkbenchMessageListContext } from "./SessionPage.thread";
import {
  applyStableListUpdate,
  applyStructuralStableListUpdate,
  getWorkbenchListItemRenderKey,
} from "./sessionMessageListStableUpdate";

type MessageListMethods = VirtuosoMessageListMethods<WorkbenchListItem, WorkbenchMessageListContext>;

const buildToolItem = (overrides: Partial<Extract<WorkbenchListItem, { kind: "tool" }>> = {}): WorkbenchListItem => ({
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

const buildTurnStatusItem = (
  overrides: Partial<Extract<WorkbenchListItem, { kind: "turn_status" }>> = {},
): WorkbenchListItem => ({
  kind: "turn_status",
  id: "turn-status-turn-1",
  turn_id: "turn-1",
  created_at: "2026-03-15T00:00:02.000Z",
  started_at: "2026-03-15T00:00:00.000Z",
  updated_at: "2026-03-15T00:00:02.000Z",
  status: "completed",
  custom_status: null,
  assistant_messages_content: "done",
  ...overrides,
});

const buildThoughtItem = (
  overrides: Partial<Extract<WorkbenchListItem, { kind: "thought" }>> = {},
): WorkbenchListItem => ({
  kind: "thought",
  id: "thought-turn-1-stream-1",
  turn_id: "turn-1",
  created_at: "2026-03-15T00:00:01.500Z",
  content: "thinking...",
  ...overrides,
});

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

const buildMessageItem = (
  overrides: Partial<Extract<WorkbenchListItem, { kind: "message" }>> = {},
): WorkbenchListItem => ({
  kind: "message",
  id: "message-user-1",
  role: "user",
  content: "hello",
  attachments: [],
  created_at: "2026-03-15T00:00:00.000Z",
  ...overrides,
});

function createFakeMethods(initial: WorkbenchListItem[]) {
  let store = [...initial];
  const calls: Array<{ method: string; args: unknown[] }> = [];

  const methods = {
    data: {
      get: () => store,
      replace: vi.fn((items: WorkbenchListItem[]) => {
        calls.push({ method: "replace", args: [items.map((item) => item.id)] });
        store = [...items];
      }),
      append: vi.fn((items: WorkbenchListItem[]) => {
        calls.push({ method: "append", args: [items.map((item) => item.id)] });
        store = [...store, ...items];
      }),
      prepend: vi.fn((items: WorkbenchListItem[]) => {
        calls.push({ method: "prepend", args: [items.map((item) => item.id)] });
        store = [...items, ...store];
      }),
      map: vi.fn((mapper: (item: WorkbenchListItem) => WorkbenchListItem, behavior?: unknown) => {
        calls.push({ method: "map", args: [behavior] });
        store = store.map((item) => mapper(item));
      }),
      mapWithAnchor: vi.fn((mapper: (item: WorkbenchListItem) => WorkbenchListItem, anchorIndex: number) => {
        calls.push({ method: "mapWithAnchor", args: [anchorIndex] });
        store = store.map((item) => mapper(item));
      }),
      deleteRange: vi.fn((offset: number, count: number) => {
        calls.push({ method: "deleteRange", args: [offset, count] });
        store.splice(offset, count);
      }),
      insert: vi.fn((items: WorkbenchListItem[], offset: number) => {
        calls.push({ method: "insert", args: [offset, items.map((item) => item.id)] });
        store.splice(offset, 0, ...items);
      }),
      batch: vi.fn((run: () => void) => {
        calls.push({ method: "batch", args: [] });
        run();
      }),
    },
  } as unknown as MessageListMethods;

  return {
    calls,
    methods,
    readStore: () => store,
  };
}

describe("applyStableListUpdate", () => {
  it("maps height-changing tool rows with stable ids so render-key remounting can remeasure them", () => {
    const current = [buildToolItem(), buildTurnStatusItem()];
    const next = [
      buildToolItem({
        subtitle:
          "this is a much longer tool summary that now wraps and should force the row to be remeasured before completed renders",
      }),
      buildTurnStatusItem(),
    ];
    const { methods, calls, readStore } = createFakeMethods(current);

    const result = applyStableListUpdate({
      methods,
      current,
      next,
      stickToBottom: false,
      anchorIndex: 2,
      appendBehavior: () => false,
    });

    expect(result).toEqual({
      mode: "remeasure",
      changedSpans: [{ start: 0, count: 1 }],
    });
    expect(calls.map((call) => call.method)).toEqual(["batch", "mapWithAnchor"]);
    expect(calls[1]?.args).toEqual([2]);
    expect(readStore()).toEqual(next);
  });

  it("keeps plain mapping when only non-layout turn-status fields change", () => {
    const current = [
      buildToolItem(),
      buildTurnStatusItem({
        status: "running",
        assistant_messages_content: "",
        updated_at: "2026-03-15T00:00:05.000Z",
      }),
    ];
    const next = [
      buildToolItem(),
      buildTurnStatusItem({
        status: "running",
        assistant_messages_content: "",
        updated_at: "2026-03-15T00:00:12.000Z",
      }),
    ];
    const { methods, calls, readStore } = createFakeMethods(current);

    const result = applyStableListUpdate({
      methods,
      current,
      next,
      stickToBottom: true,
      anchorIndex: -1,
      appendBehavior: () => false,
    });

    expect(result).toEqual({
      mode: "map",
      changedSpans: [],
    });
    expect(calls.map((call) => call.method)).toEqual(["map"]);
    expect(calls[0]?.args).toEqual(["auto"]);
    expect(readStore()).toEqual(next);
  });

  it("appends new rows while mapping earlier rows that changed layout", () => {
    const current = [buildToolItem(), buildTurnStatusItem({ status: "running", assistant_messages_content: "" })];
    const appendedStatus = buildTurnStatusItem({
      id: "turn-status-turn-2",
      turn_id: "turn-2",
      status: "completed",
    });
    const next = [
      buildToolItem({
        subtitle:
          "this row grew before a new trailing row was appended, so append-only updates still need remeasurement",
      }),
      buildTurnStatusItem({ status: "running", assistant_messages_content: "" }),
    ];
    const { methods, calls, readStore } = createFakeMethods(current);

    const result = applyStableListUpdate({
      methods,
      current,
      next,
      suffix: [appendedStatus],
      stickToBottom: false,
      anchorIndex: 1,
      appendBehavior: () => false,
    });

    expect(result).toEqual({
      mode: "remeasure",
      changedSpans: [{ start: 0, count: 1 }],
    });
    expect(calls.map((call) => call.method)).toEqual(["batch", "append", "mapWithAnchor"]);
    expect(calls[1]?.args).toEqual([["turn-status-turn-2"]]);
    expect(calls[2]?.args).toEqual([1]);
    expect(readStore()).toEqual([...next, appendedStatus]);
  });

  it("maps retained rows after a mixed middle insert changes the structural reconcile path", () => {
    const current = [buildToolItem(), buildTurnStatusItem({ status: "running", assistant_messages_content: "" })];
    const next = [
      buildToolItem({
        subtitle:
          "this row grew while a new thought row was inserted before the trailing status row, so structural reconcile still needs remeasurement",
      }),
      buildThoughtItem(),
      buildTurnStatusItem({ status: "running", assistant_messages_content: "" }),
    ];
    const { methods, calls, readStore } = createFakeMethods(current);

    const result = applyStructuralStableListUpdate({
      methods,
      current,
      next,
      prefixLen: 1,
      suffixLen: 1,
      stickToBottom: false,
      anchorIndex: 1,
      appendBehavior: () => false,
    });

    expect(result).toEqual({
      mode: "remeasure",
      changedSpans: [{ start: 0, count: 1 }],
    });
    expect(calls.map((call) => call.method)).toEqual(["batch", "insert", "mapWithAnchor"]);
    expect(calls[1]?.args).toEqual([1, ["thought-turn-1-stream-1"]]);
    expect(calls[2]?.args).toEqual([1]);
    expect(readStore()).toEqual(next);
  });

  it("forces remeasurement for structural tail replacements when localized neighbors keep stable ids", () => {
    const currentThought = buildThoughtItem();
    const currentStatus = buildTurnStatusItem({
      status: "running",
      assistant_messages_content: "",
      updated_at: "2026-03-15T00:00:05.000Z",
    });
    const nextThought = buildThoughtItem({
      id: "thought-turn-1-stream-2",
      content: "thinking...",
    });
    const nextStatus = buildTurnStatusItem({
      status: "running",
      assistant_messages_content: "",
      updated_at: "2026-03-15T00:00:06.000Z",
    });
    const current = [buildMessageItem(), currentThought, currentStatus];
    const next = [current[0]!, nextThought, nextStatus];
    const { methods, calls, readStore } = createFakeMethods(current);

    const result = applyStructuralStableListUpdate({
      methods,
      current,
      next,
      prefixLen: 1,
      suffixLen: 1,
      stickToBottom: true,
      anchorIndex: -1,
      appendBehavior: () => false,
      forceRemeasureItemIds: [nextThought.id, nextStatus.id],
    });

    expect(result).toEqual({
      mode: "remeasure",
      changedSpans: [{ start: 1, count: 2 }],
    });
    expect(calls.map((call) => call.method)).toEqual(["batch", "deleteRange", "insert", "map"]);
    expect(calls[1]?.args).toEqual([1, 1]);
    expect(calls[2]?.args).toEqual([1, [nextThought.id]]);
    expect(calls[3]?.args).toEqual(["auto"]);
    const store = readStore();
    expect(store).toEqual(next);
    expect(store[1]).not.toBe(nextThought);
    expect(store[2]).not.toBe(nextStatus);
  });
});

describe("getWorkbenchListItemRenderKey", () => {
  it("stays stable when a message row's layout-affecting content changes", () => {
    const current = buildAssistantItem();
    const next = buildAssistantItem({
      content: "this reply is now much longer and wraps across multiple lines",
    });

    expect(getWorkbenchListItemRenderKey(next)).toBe(getWorkbenchListItemRenderKey(current));
  });

  it("stays stable when turn-status updates do not affect layout", () => {
    const current = buildTurnStatusItem({
      status: "running",
      assistant_messages_content: "",
      updated_at: "2026-03-15T00:00:05.000Z",
    });
    const next = buildTurnStatusItem({
      status: "running",
      assistant_messages_content: "",
      updated_at: "2026-03-15T00:00:12.000Z",
    });

    expect(getWorkbenchListItemRenderKey(next)).toBe(getWorkbenchListItemRenderKey(current));
  });

  it("stays stable for tool rows when the external render revision changes", () => {
    const item = buildToolItem();
    const collapsedContext: WorkbenchMessageListContext = {
      loaded: true,
      loadingOlder: false,
      expandedToolById: {},
    };
    const expandedContext: WorkbenchMessageListContext = {
      loaded: true,
      loadingOlder: false,
      expandedToolById: { [item.id]: true },
    };

    expect(getWorkbenchListItemRenderKey(item, expandedContext)).toBe(
      getWorkbenchListItemRenderKey(item, collapsedContext),
    );
  });

  it("keeps unrelated tool rows stable when another tool toggles", () => {
    const item = buildToolItem();
    const otherToolId = "tool-turn-1-tool-2";
    const collapsedContext: WorkbenchMessageListContext = {
      loaded: true,
      loadingOlder: false,
      expandedToolById: {},
    };
    const expandedOtherContext: WorkbenchMessageListContext = {
      loaded: true,
      loadingOlder: false,
      expandedToolById: { [otherToolId]: true },
    };

    expect(getWorkbenchListItemRenderKey(item, expandedOtherContext)).toBe(
      getWorkbenchListItemRenderKey(item, collapsedContext),
    );
  });

  it("keeps message rows stable across content-only updates", () => {
    const current = buildMessageItem();
    const next = buildMessageItem({
      content: "hello with a longer body that still should not remount a stable user row",
    });

    expect(getWorkbenchListItemRenderKey(next)).toBe(getWorkbenchListItemRenderKey(current));
  });

  it("stays stable for assistant-role message rows when content grows", () => {
    const current = buildMessageItem({
      id: "assistant-msg-1",
      role: "assistant",
      content: "short reply",
    });
    const next = buildMessageItem({
      id: "assistant-msg-1",
      role: "assistant",
      content: "this completed assistant message is now much longer and should remeasure",
    });

    expect(getWorkbenchListItemRenderKey(next)).toBe(getWorkbenchListItemRenderKey(current));
  });
});
