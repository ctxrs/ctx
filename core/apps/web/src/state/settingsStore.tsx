import { createContext, useContext, useEffect, useMemo, useSyncExternalStore } from "react";
import type { PublicSettings, UpdateSettingsRequest } from "../api/client";
import { getSettings, updateSettings } from "../api/client";
import { errorMessage } from "../utils/errorMessage";
import { loadSettingsV2, saveSettingsV2 } from "./uiStateStore";

type SettingsSnapshot = {
  settings: PublicSettings | null;
  loaded: boolean;
  loading: boolean;
  error: string | null;
};

class SettingsStore {
  private listeners = new Set<() => void>();
  private snapshot: SettingsSnapshot = {
    settings: null,
    loaded: false,
    loading: false,
    error: null,
  };
  private inflight: Promise<void> | null = null;

  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  getSnapshot = () => this.snapshot;

  private publish() {
    for (const l of this.listeners) l();
  }

  ensureLoaded() {
    if (this.inflight) return this.inflight;
    if (this.snapshot.loaded) return Promise.resolve();
    this.inflight = this.loadOnce().finally(() => {
      this.inflight = null;
    });
    return this.inflight;
  }

  async refresh() {
    return this.loadOnce({ force: true });
  }

  async update(patch: UpdateSettingsRequest) {
    const next = await updateSettings(patch);
    this.snapshot = {
      ...this.snapshot,
      settings: next,
      loaded: true,
      loading: false,
      error: null,
    };
    this.publish();
    await saveSettingsV2(next);
  }

  private async loadOnce(opts?: { force?: boolean }) {
    if (this.snapshot.loading) return;
    this.snapshot = { ...this.snapshot, loading: true };
    this.publish();

    const cached = await loadSettingsV2();
    if (cached && !this.snapshot.loaded) {
      this.snapshot = {
        settings: cached.settings,
        loaded: true,
        loading: true,
        error: null,
      };
      this.publish();
    }

    if (!opts?.force && cached) {
      this.snapshot = { ...this.snapshot, loading: false };
      this.publish();
      return;
    }

    try {
      const settings = await getSettings();
      this.snapshot = {
        settings,
        loaded: true,
        loading: false,
        error: null,
      };
      this.publish();
      await saveSettingsV2(settings);
    } catch (e: unknown) {
      this.snapshot = {
        ...this.snapshot,
        loading: false,
        error: errorMessage(e),
      };
      this.publish();
    }
  }
}

const SettingsStoreContext = createContext<SettingsStore | null>(null);

export function SettingsStoreProvider({ children }: { children: React.ReactNode }) {
  const store = useMemo(() => new SettingsStore(), []);
  useEffect(() => {
    store.ensureLoaded();
  }, [store]);
  return <SettingsStoreContext.Provider value={store}>{children}</SettingsStoreContext.Provider>;
}

export function useSettingsStore() {
  const store = useContext(SettingsStoreContext);
  if (!store) throw new Error("SettingsStoreProvider missing");
  return store;
}

export function useSettingsSnapshot() {
  const store = useSettingsStore();
  return useSyncExternalStore(store.subscribe, store.getSnapshot, store.getSnapshot);
}
