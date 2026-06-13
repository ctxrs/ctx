import { useCallback, useMemo, useRef, useSyncExternalStore } from "react";

type RelativeNowStore = {
  subscribe: (listener: () => void) => () => void;
  getSnapshot: () => number;
};

const stores = new Map<number, RelativeNowStore>();

function createStore(intervalMs: number): RelativeNowStore {
  let nowMs = Date.now();
  let timer: number | null = null;
  let visibilityHandler: (() => void) | null = null;
  const listeners = new Set<() => void>();

  const notify = () => {
    for (const listener of listeners) {
      listener();
    }
  };

  const tick = () => {
    nowMs = Date.now();
    notify();
  };

  const ensureRunning = () => {
    if (intervalMs > 0 && timer === null) {
      tick();
      timer = window.setInterval(tick, intervalMs);
    }
    if (!visibilityHandler) {
      visibilityHandler = () => {
        if (!document.hidden) tick();
      };
      document.addEventListener("visibilitychange", visibilityHandler);
    }
  };

  const stopIfIdle = () => {
    if (listeners.size > 0) return;
    if (timer !== null) {
      window.clearInterval(timer);
      timer = null;
    }
    if (visibilityHandler) {
      document.removeEventListener("visibilitychange", visibilityHandler);
      visibilityHandler = null;
    }
  };

  return {
    subscribe(listener) {
      listeners.add(listener);
      ensureRunning();
      return () => {
        listeners.delete(listener);
        stopIfIdle();
      };
    },
    getSnapshot() {
      return nowMs;
    },
  };
}

function getStore(intervalMs: number): RelativeNowStore {
  if (!stores.has(intervalMs)) {
    stores.set(intervalMs, createStore(intervalMs));
  }
  return stores.get(intervalMs)!;
}

export function useRelativeNowMs(intervalMs = 60_000, enabled = true): number {
  const store = useMemo(() => getStore(intervalMs), [intervalMs]);
  const frozenNowRef = useRef<number | null>(null);
  const subscribe = useCallback(
    (listener: () => void) => {
      if (!enabled) return () => {};
      return store.subscribe(listener);
    },
    [enabled, store],
  );
  const snapshot = useSyncExternalStore(subscribe, store.getSnapshot, store.getSnapshot);
  if (enabled) {
    frozenNowRef.current = snapshot;
    return snapshot;
  }
  if (frozenNowRef.current === null) {
    frozenNowRef.current = snapshot;
  }
  return frozenNowRef.current;
}
