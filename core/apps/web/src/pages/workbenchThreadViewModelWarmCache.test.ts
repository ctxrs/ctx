import { beforeEach, describe, expect, it } from "vitest";
import type { WorkbenchThreadViewModelWarmSnapshot } from "./workbenchThreadViewModelWarmCache";
import {
  getWarmWorkbenchThreadViewModelCacheSize,
  persistWarmWorkbenchThreadViewModel,
  pruneWarmWorkbenchThreadViewModelCache,
  readWarmWorkbenchThreadViewModel,
  resetWarmWorkbenchThreadViewModelCache,
} from "./workbenchThreadViewModelWarmCache";

function makeSnapshot(warmKey: string): WorkbenchThreadViewModelWarmSnapshot {
  return {
    sourceKey: `source:${warmKey}`,
    layoutKey: "verbosity:default",
    warmKey,
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
  };
}

describe("workbenchThreadViewModelWarmCache", () => {
  beforeEach(() => {
    resetWarmWorkbenchThreadViewModelCache();
  });

  it("prunes non-retained warm snapshots", () => {
    persistWarmWorkbenchThreadViewModel("session-1", makeSnapshot("warm-1"));
    persistWarmWorkbenchThreadViewModel("session-2", makeSnapshot("warm-2"));
    persistWarmWorkbenchThreadViewModel("session-3", makeSnapshot("warm-3"));

    expect(getWarmWorkbenchThreadViewModelCacheSize()).toBe(3);

    pruneWarmWorkbenchThreadViewModelCache(["session-2"]);

    expect(getWarmWorkbenchThreadViewModelCacheSize()).toBe(1);
  });

  it("reads snapshots through the shared session transcript cache", () => {
    const snapshot = makeSnapshot("warm-1");

    persistWarmWorkbenchThreadViewModel("session-1", snapshot);

    expect(readWarmWorkbenchThreadViewModel("session-1", "warm-1")).toBe(snapshot);
    expect(readWarmWorkbenchThreadViewModel("session-1", "warm-miss")).toBeNull();
  });
});
