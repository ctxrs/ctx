type Listener = () => void;

type DaemonResourceEntry<TData> = {
  data?: TData;
  inFlight?: Promise<TData>;
  stale?: boolean;
  listeners: Set<Listener>;
};

type DaemonResourceLoadFn<TData> = (
  current: TData | undefined,
) => Promise<TData>;

type DaemonResourceNormalizeFn<TKey, TData> = (args: {
  key: TKey;
  next: TData;
  current: TData | undefined;
}) => TData;

export type DaemonResourceStore<TKey, TData> = {
  getCached: (key: TKey) => TData | undefined;
  getSnapshot: (key: TKey) => TData;
  subscribe: (key: TKey, listener: Listener) => () => void;
  update: (key: TKey, updater: (current: TData) => TData) => TData;
  hasCached: (key: TKey) => boolean;
  load: (key: TKey, load: DaemonResourceLoadFn<TData>) => Promise<TData>;
  refresh: (key: TKey, load: DaemonResourceLoadFn<TData>) => Promise<TData>;
  invalidate: (key: TKey) => void;
};

type CreateDaemonResourceStoreOptions<TKey, TData> = {
  defaultData: TData;
  keyToString: (key: TKey) => string;
  normalize?: DaemonResourceNormalizeFn<TKey, TData>;
};

export function createDaemonResourceStore<TKey, TData>(
  options: CreateDaemonResourceStoreOptions<TKey, TData>,
): DaemonResourceStore<TKey, TData> {
  const entries = new Map<string, DaemonResourceEntry<TData>>();

  const getOrCreateEntry = (keyString: string): DaemonResourceEntry<TData> => {
    let entry = entries.get(keyString);
    if (!entry) {
      entry = { listeners: new Set() };
      entries.set(keyString, entry);
    }
    return entry;
  };

  const emit = (entry: DaemonResourceEntry<TData>): void => {
    for (const listener of entry.listeners) {
      listener();
    }
  };

  const setEntryData = (
    key: TKey,
    entry: DaemonResourceEntry<TData>,
    next: TData,
  ): TData => {
    const keyString = options.keyToString(key);
    const normalized = options.normalize
      ? options.normalize({ key, next, current: entry.data })
      : next;
    entry.data = normalized;
    entry.stale = false;
    entries.set(keyString, entry);
    emit(entry);
    return normalized;
  };

  const loadFresh = (
    key: TKey,
    entry: DaemonResourceEntry<TData>,
    load: DaemonResourceLoadFn<TData>,
  ): Promise<TData> => {
    const request = load(entry.data)
      .then((next) => setEntryData(key, entry, next))
      .finally(() => {
        if (entry.inFlight === request) {
          entry.inFlight = undefined;
        }
      });
    entry.inFlight = request;
    entry.stale = false;
    return request;
  };

  return {
    getCached(key) {
      return entries.get(options.keyToString(key))?.data;
    },

    getSnapshot(key) {
      return entries.get(options.keyToString(key))?.data ?? options.defaultData;
    },

    subscribe(key, listener) {
      const keyString = options.keyToString(key);
      const entry = getOrCreateEntry(keyString);
      entry.listeners.add(listener);
      return () => {
        entry.listeners.delete(listener);
        if (entry.listeners.size === 0 && !entry.inFlight && entry.data === undefined) {
          entries.delete(keyString);
        }
      };
    },

    update(key, updater) {
      const entry = getOrCreateEntry(options.keyToString(key));
      const next = updater(entry.data ?? options.defaultData);
      return setEntryData(key, entry, next);
    },

    hasCached(key) {
      return entries.get(options.keyToString(key))?.data !== undefined;
    },

    load(key, load) {
      const keyString = options.keyToString(key);
      const entry = getOrCreateEntry(keyString);
      if (entry.data !== undefined && !entry.stale) {
        return Promise.resolve(entry.data);
      }
      if (entry.inFlight) {
        return entry.inFlight;
      }
      return loadFresh(key, entry, load);
    },

    refresh(key, load) {
      const entry = getOrCreateEntry(options.keyToString(key));
      if (entry.inFlight) {
        return entry.inFlight;
      }
      return loadFresh(key, entry, load);
    },

    invalidate(key) {
      const entry = entries.get(options.keyToString(key));
      if (!entry) return;
      entry.stale = true;
    },
  };
}
