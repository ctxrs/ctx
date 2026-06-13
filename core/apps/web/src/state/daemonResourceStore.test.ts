import { describe, expect, it, vi } from "vitest";
import { createDaemonResourceStore } from "./daemonResourceStore";

describe("daemonResourceStore", () => {
  it("returns the default snapshot before any load", () => {
    const store = createDaemonResourceStore<string, { value: number }>({
      defaultData: { value: 0 },
      keyToString: (key) => key,
    });

    expect(store.getCached("alpha")).toBeUndefined();
    expect(store.getSnapshot("alpha")).toEqual({ value: 0 });
    expect(store.hasCached("alpha")).toBe(false);
  });

  it("dedupes concurrent loads and caches the resolved value", async () => {
    let resolveLoad: ((value: { value: number }) => void) | undefined;
    const load = vi.fn(
      (_current: { value: number } | undefined) => new Promise<{ value: number }>((resolve) => {
        resolveLoad = resolve;
      }),
    );
    const store = createDaemonResourceStore<string, { value: number }>({
      defaultData: { value: 0 },
      keyToString: (key) => key,
    });

    const first = store.load("alpha", load);
    const second = store.load("alpha", load);

    expect(load).toHaveBeenCalledTimes(1);
    expect(first).toBe(second);

    if (!resolveLoad) {
      throw new Error("expected load promise resolver");
    }
    resolveLoad({ value: 7 });

    await expect(first).resolves.toEqual({ value: 7 });
    expect(store.getSnapshot("alpha")).toEqual({ value: 7 });
    expect(store.hasCached("alpha")).toBe(true);
  });

  it("marks cached data stale and reloads on the next load", async () => {
    const load = vi.fn(async (_current: { value: number } | undefined) => ({ value: 0 }))
      .mockResolvedValueOnce({ value: 1 })
      .mockResolvedValueOnce({ value: 2 });
    const store = createDaemonResourceStore<string, { value: number }>({
      defaultData: { value: 0 },
      keyToString: (key) => key,
    });

    await expect(store.load("alpha", load)).resolves.toEqual({ value: 1 });
    store.invalidate("alpha");
    await expect(store.load("alpha", load)).resolves.toEqual({ value: 2 });

    expect(load).toHaveBeenCalledTimes(2);
    expect(store.getSnapshot("alpha")).toEqual({ value: 2 });
  });

  it("supports local updates and subscriber notifications", () => {
    const store = createDaemonResourceStore<string, { value: number }>({
      defaultData: { value: 0 },
      keyToString: (key) => key,
    });
    const listener = vi.fn();
    const unsubscribe = store.subscribe("alpha", listener);

    expect(store.update("alpha", (current) => ({ value: current.value + 1 }))).toEqual({ value: 1 });
    expect(listener).toHaveBeenCalledTimes(1);

    unsubscribe();
    store.update("alpha", (current) => ({ value: current.value + 1 }));
    expect(listener).toHaveBeenCalledTimes(1);
  });
});
