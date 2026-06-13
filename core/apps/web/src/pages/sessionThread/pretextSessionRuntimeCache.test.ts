import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { WorkbenchListItem } from "../sessionView/SessionPage.types";
import {
  buildSessionPretextRuntimeLayoutKey,
  buildSessionPretextRuntimeSourceKey,
  createDefaultSessionTranscriptUiState,
  getOrCreateSessionPretextRuntime,
  getSessionPretextRuntimeCacheSize,
  noteSessionPretextRuntimeSnapshot,
  persistSessionTranscriptWarmEntry,
  primeSessionPretextRuntime,
  pruneSessionPretextRuntimeCache,
  readSessionTranscriptWarmEntry,
  readSessionPretextRuntimePreparedState,
  resetSessionTranscriptWarmEntries,
  resetSessionPretextRuntimeCache,
} from "./pretextSessionRuntimeCache";
import { writePretextAssistantHeightOverride } from "./pretextRowMeasurementOverrides";

const makeItems = (count = 2): WorkbenchListItem[] =>
  Array.from({ length: count }, (_, index) => ({
    kind: "message" as const,
    id: `message-${index + 1}`,
    role: index % 2 === 0 ? ("user" as const) : ("assistant" as const),
    content: `message ${index + 1}`,
    attachments: [],
    created_at: `2026-03-17T00:${String(index).padStart(2, "0")}:00Z`,
  }));

describe("pretextSessionRuntimeCache", () => {
  beforeEach(() => {
    resetSessionPretextRuntimeCache();
    resetSessionTranscriptWarmEntries();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("re-primes detached prepared state for bottom reopen semantics", () => {
    const sessionId = "session-detached";
    const initialItems = makeItems(10);
    const updatedItems = makeItems(12);
    const runtime = getOrCreateSessionPretextRuntime(sessionId, {
      uiState: createDefaultSessionTranscriptUiState(),
    });

    runtime.core.replaceItems(initialItems, { kind: "bottom" });
    const detachedSnapshot = runtime.core.syncViewport({
      width: 900,
      height: 300,
      scrollTop: 420,
    });
    noteSessionPretextRuntimeSnapshot(runtime, detachedSnapshot, initialItems);

    primeSessionPretextRuntime({
      sessionId,
      listItems: updatedItems,
      uiState: createDefaultSessionTranscriptUiState(),
      viewportWidth: 900,
      viewportHeight: 300,
    });
    const preparedAfter = readSessionPretextRuntimePreparedState(runtime);

    expect(preparedAfter.listItems).toEqual(updatedItems);
    expect(preparedAfter.snapshot.anchor.kind).toBe("bottom");
  });

  it("skips item replacement when the ui state is semantically unchanged", () => {
    const listItems = makeItems(8);
    const runtime = primeSessionPretextRuntime({
      sessionId: "session-stable-ui",
      listItems,
      uiState: createDefaultSessionTranscriptUiState("default", ["turn-1"]),
      viewportWidth: 900,
      viewportHeight: 300,
    });
    const syncItemsSpy = vi.spyOn(runtime.core, "syncItems");

    primeSessionPretextRuntime({
      sessionId: "session-stable-ui",
      listItems,
      uiState: createDefaultSessionTranscriptUiState("default", ["turn-1"]),
      viewportWidth: 900,
      viewportHeight: 300,
    });

    expect(syncItemsSpy).not.toHaveBeenCalled();
  });

  it("skips keyed item replacement when equal content arrives as a new array", () => {
    const listItems = makeItems(8);
    const nextListItems = listItems.map((item) => ({ ...item }));
    const uiState = createDefaultSessionTranscriptUiState("default", ["turn-1"]);
    const sourceKey = buildSessionPretextRuntimeSourceKey(listItems, uiState);
    const layoutKey = buildSessionPretextRuntimeLayoutKey({ uiState, listItems });
    const runtime = primeSessionPretextRuntime({
      sessionId: "session-stable-key",
      listItems,
      uiState,
      viewportWidth: 900,
      viewportHeight: 300,
      sourceKey,
      layoutKey,
    });
    const syncItemsSpy = vi.spyOn(runtime.core, "syncItems");

    primeSessionPretextRuntime({
      sessionId: "session-stable-key",
      listItems: nextListItems,
      uiState,
      viewportWidth: 900,
      viewportHeight: 300,
      sourceKey,
      layoutKey,
    });

    expect(syncItemsSpy).not.toHaveBeenCalled();
    expect(readSessionPretextRuntimePreparedState(runtime).listItems).toBe(listItems);
  });

  it("records explicit source and layout keys for prepared runtimes", () => {
    const listItems = makeItems(3);
    const uiState = createDefaultSessionTranscriptUiState("default", ["turn-1"]);
    const runtime = primeSessionPretextRuntime({
      sessionId: "session-keyed",
      listItems,
      uiState,
      viewportWidth: 900,
      viewportHeight: 300,
      sourceKey: "warm-key-1",
      layoutKey: buildSessionPretextRuntimeLayoutKey({ uiState, listItems }),
    });

    expect(readSessionPretextRuntimePreparedState(runtime)).toMatchObject({
      sourceKey: "warm-key-1",
      layoutKey: buildSessionPretextRuntimeLayoutKey({ uiState, listItems }),
    });

    const updatedItems = [...listItems, makeItems(4)[3]!];
    primeSessionPretextRuntime({
      sessionId: "session-keyed",
      listItems: updatedItems,
      uiState,
      viewportWidth: 900,
      viewportHeight: 300,
      sourceKey: buildSessionPretextRuntimeSourceKey(updatedItems, uiState),
      layoutKey: buildSessionPretextRuntimeLayoutKey({ uiState, listItems: updatedItems }),
    });

    expect(readSessionPretextRuntimePreparedState(runtime)).toMatchObject({
      sourceKey: buildSessionPretextRuntimeSourceKey(updatedItems, uiState),
      layoutKey: buildSessionPretextRuntimeLayoutKey({ uiState, listItems: updatedItems }),
    });
  });

  it("refreshes the prepared source and layout keys when recording a snapshot under a new ui state", () => {
    const sessionId = "session-layout-key-refresh";
    const listItems = makeItems(2);
    const updatedItems = makeItems(3);
    const expandedUiState = createDefaultSessionTranscriptUiState();
    expandedUiState.expandedMessageById = { "message-1": true };
    const collapsedUiState = createDefaultSessionTranscriptUiState();

    const runtime = getOrCreateSessionPretextRuntime(sessionId, {
      uiState: expandedUiState,
    });
    const expandedSnapshot = runtime.core.replaceItems(listItems, { kind: "bottom" });
    noteSessionPretextRuntimeSnapshot(runtime, expandedSnapshot, listItems, {
      sourceKey: buildSessionPretextRuntimeSourceKey(listItems, expandedUiState),
      layoutKey: buildSessionPretextRuntimeLayoutKey({ uiState: expandedUiState, listItems }),
    });

    expect(readSessionPretextRuntimePreparedState(runtime)).toMatchObject({
      sourceKey: buildSessionPretextRuntimeSourceKey(listItems, expandedUiState),
      layoutKey: buildSessionPretextRuntimeLayoutKey({ uiState: expandedUiState, listItems }),
    });

    getOrCreateSessionPretextRuntime(sessionId, {
      uiState: collapsedUiState,
    });
    const collapsedSnapshot = runtime.core.syncItems(updatedItems, { kind: "bottom" });
    noteSessionPretextRuntimeSnapshot(runtime, collapsedSnapshot, updatedItems, {
      sourceKey: buildSessionPretextRuntimeSourceKey(updatedItems, collapsedUiState),
      layoutKey: buildSessionPretextRuntimeLayoutKey({ uiState: collapsedUiState, listItems: updatedItems }),
    });

    expect(readSessionPretextRuntimePreparedState(runtime)).toMatchObject({
      sourceKey: buildSessionPretextRuntimeSourceKey(updatedItems, collapsedUiState),
      layoutKey: buildSessionPretextRuntimeLayoutKey({ uiState: collapsedUiState, listItems: updatedItems }),
    });
  });

  it("preserves prepared keys when recording a scroll snapshot without content changes", () => {
    const sessionId = "session-scroll-snapshot";
    const listItems = makeItems(4);
    const uiState = createDefaultSessionTranscriptUiState();
    const runtime = primeSessionPretextRuntime({
      sessionId,
      listItems,
      uiState,
      viewportWidth: 900,
      viewportHeight: 300,
    });
    const preparedBefore = readSessionPretextRuntimePreparedState(runtime);

    const scrolledSnapshot = runtime.core.syncViewport({
      width: 900,
      height: 300,
      scrollTop: 120,
    });
    noteSessionPretextRuntimeSnapshot(runtime, scrolledSnapshot, listItems);

    expect(readSessionPretextRuntimePreparedState(runtime)).toMatchObject({
      sourceKey: preparedBefore.sourceKey,
      layoutKey: preparedBefore.layoutKey,
    });
  });

  it("changes the prepared source key when same-id item content changes", () => {
    const uiState = createDefaultSessionTranscriptUiState();
    const initialItems = makeItems(2);
    const updatedItems: WorkbenchListItem[] = initialItems.map((item) =>
      item.kind === "message" && item.id === "message-2"
        ? { ...item, content: `${item.content} marker` }
        : item,
    );

    expect(buildSessionPretextRuntimeSourceKey(updatedItems, uiState)).not.toBe(
      buildSessionPretextRuntimeSourceKey(initialItems, uiState),
    );
  });

  it("passes the runtime session id through planned-layout measurement hooks", () => {
    const sessionId = "session-override-hit";
    const item: WorkbenchListItem = {
      kind: "assistant",
      id: "assistant-1",
      turn_id: "turn-1",
      content: "Assistant correction candidate",
      thought: "",
      is_complete: true,
      created_at: "2026-04-25T00:00:00Z",
    };
    const runtime = getOrCreateSessionPretextRuntime(sessionId, {
      uiState: createDefaultSessionTranscriptUiState(),
    });

    expect(
      writePretextAssistantHeightOverride({
        sessionId,
        item,
        viewportWidth: 640,
        height: 123,
      }),
    ).toBe(true);

    expect(runtime.callbacks.getPlannedLayout(item, { width: 640, widthBucket: "w10" }).height).toBe(123);
  });

  it("keeps warm snapshots when pruning only the runtime slice", () => {
    const sessionId = "session-shared";
    persistSessionTranscriptWarmEntry(sessionId, {
      sourceKey: "source-1",
      layoutKey: "verbosity:default",
      warmKey: "warm-1",
      snapshot: {
        projectionRevision: 1,
        view: {
          groups: [],
          debugEvents: [],
        },
        listItems: [],
        groupRanges: new Map(),
        turnsLen: 0,
        messagesLen: 0,
        eventsLen: 0,
        caches: {
          messagesByTurnId: new Map(),
          eventsByTurnId: new Map(),
        },
      },
      updatedAtMs: Date.now(),
    });

    primeSessionPretextRuntime({
      sessionId,
      listItems: makeItems(2),
      uiState: createDefaultSessionTranscriptUiState(),
      viewportWidth: 900,
      viewportHeight: 300,
    });

    pruneSessionPretextRuntimeCache([]);

    expect(getSessionPretextRuntimeCacheSize()).toBe(0);
    expect(readSessionTranscriptWarmEntry(sessionId)).not.toBeNull();
  });

  it("evicts unretained prepared runtimes because reopen no longer depends on cached scroll state", () => {
    primeSessionPretextRuntime({
      sessionId: "session-retained",
      listItems: makeItems(4),
      uiState: createDefaultSessionTranscriptUiState(),
      viewportWidth: 900,
      viewportHeight: 300,
    });
    primeSessionPretextRuntime({
      sessionId: "session-evicted",
      listItems: makeItems(5),
      uiState: createDefaultSessionTranscriptUiState(),
      viewportWidth: 900,
      viewportHeight: 300,
    });
    primeSessionPretextRuntime({
      sessionId: "session-evicted-2",
      listItems: makeItems(6),
      uiState: createDefaultSessionTranscriptUiState(),
      viewportWidth: 900,
      viewportHeight: 300,
    });

    expect(getSessionPretextRuntimeCacheSize()).toBe(3);

    pruneSessionPretextRuntimeCache(["session-retained"]);

    expect(getSessionPretextRuntimeCacheSize()).toBe(1);
  });

  it("clears prepared runtimes when no sessions are retained", () => {
    primeSessionPretextRuntime({
      sessionId: "session-a",
      listItems: makeItems(3),
      uiState: createDefaultSessionTranscriptUiState(),
      viewportWidth: 900,
      viewportHeight: 300,
    });
    primeSessionPretextRuntime({
      sessionId: "session-b",
      listItems: makeItems(4),
      uiState: createDefaultSessionTranscriptUiState(),
      viewportWidth: 900,
      viewportHeight: 300,
    });

    pruneSessionPretextRuntimeCache([]);

    expect(getSessionPretextRuntimeCacheSize()).toBe(0);
  });
});
