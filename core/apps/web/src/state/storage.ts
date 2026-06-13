import { desktopStorageBatch, desktopStorageGet, isDesktopApp } from "../utils/desktop";

export interface WebappStorage {
  getKv: <T>(key: string) => Promise<T | null>;
  setKv: <T>(key: string, value: T) => Promise<void>;
  deleteKv: (key: string) => Promise<void>;
  getSnapshot: <T>(key: string) => Promise<T | null>;
  setSnapshot: <T>(key: string, value: T) => Promise<void>;
  deleteSnapshot: (key: string) => Promise<void>;
  getHistoryPage: <T>(key: string) => Promise<T | null>;
  setHistoryPage: <T>(key: string, value: T) => Promise<void>;
  deleteHistoryPage: (key: string) => Promise<void>;
  flush: () => Promise<void>;
}

type StorageBatchOp =
  | { kind: "set"; key: string; value: unknown }
  | { kind: "delete"; key: string };

type StorageBackend = {
  get: (key: string) => Promise<unknown | null>;
  batch: (ops: StorageBatchOp[]) => Promise<void>;
};

type StoredRecord = {
  key: string;
  value: unknown;
  updatedAtMs: number;
};

const DB_NAME = "ctx-ui";
const DB_VERSION = 1;
const STORE_NAME = "kv";

const READ_CACHE_LIMIT = 256;
const WRITE_BEHIND_DELAY_MS = 200;
const WRITE_BEHIND_RETRY_MS = 2000;
const WRITE_BEHIND_MAX_PENDING = 200;

let dbPromise: Promise<IDBDatabase> | null = null;
let backendPromise: Promise<StorageBackend> | null = null;
let storageInstance: WebappStorage | null = null;

function requestToPromise<T>(req: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    req.addEventListener("success", () => resolve(req.result));
    req.addEventListener("error", () => reject(req.error ?? new Error("IndexedDB request failed")));
  });
}

function txDone(tx: IDBTransaction): Promise<void> {
  return new Promise((resolve, reject) => {
    tx.addEventListener("complete", () => resolve());
    tx.addEventListener("abort", () => reject(tx.error ?? new Error("IndexedDB transaction aborted")));
    tx.addEventListener("error", () => reject(tx.error ?? new Error("IndexedDB transaction failed")));
  });
}

function attachDbHandlers(db: IDBDatabase) {
  const reset = () => {
    try {
      db.close();
    } catch {
      // ignore
    }
    dbPromise = null;
  };
  db.addEventListener("versionchange", reset);
  db.addEventListener("close", () => {
    dbPromise = null;
  });
}

async function openDb(): Promise<IDBDatabase> {
  if (dbPromise) return dbPromise;
  if (typeof indexedDB === "undefined") {
    throw new Error("IndexedDB is unavailable in this environment.");
  }

  dbPromise = new Promise((resolve, reject) => {
    const req = indexedDB.open(DB_NAME, DB_VERSION);
    req.addEventListener("upgradeneeded", () => {
      const db = req.result;
      if (!db.objectStoreNames.contains(STORE_NAME)) {
        db.createObjectStore(STORE_NAME, { keyPath: "key" });
      }
    });
    req.addEventListener("success", () => {
      const db = req.result;
      attachDbHandlers(db);
      resolve(db);
    });
    req.addEventListener("error", () => reject(req.error ?? new Error("Failed to open IndexedDB")));
  });

  dbPromise.catch(() => {
    dbPromise = null;
  });

  return dbPromise;
}

function createIdbBackend(): StorageBackend {
  return {
    get: async (key) => {
      const db = await openDb();
      const tx = db.transaction(STORE_NAME, "readonly");
      const store = tx.objectStore(STORE_NAME);
      const rec = (await requestToPromise(store.get(key))) as StoredRecord | undefined;
      await txDone(tx);
      return rec?.value ?? null;
    },
    batch: async (ops) => {
      if (ops.length === 0) return;
      const db = await openDb();
      const tx = db.transaction(STORE_NAME, "readwrite");
      const store = tx.objectStore(STORE_NAME);
      const now = Date.now();
      for (const op of ops) {
        if (op.kind === "set") {
          store.put({ key: op.key, value: op.value, updatedAtMs: now } satisfies StoredRecord);
        } else {
          store.delete(op.key);
        }
      }
      await txDone(tx);
    },
  };
}

async function getBackend(): Promise<StorageBackend> {
  if (backendPromise) return backendPromise;
  backendPromise = (async () => {
    if (isDesktopApp()) {
      try {
        await desktopStorageGet("__ctx_ui_storage_probe__");
        return {
          get: desktopStorageGet,
          batch: desktopStorageBatch,
        };
      } catch (err) {
        console.warn("Desktop storage unavailable, falling back to IndexedDB.", err);
      }
    }
    return createIdbBackend();
  })();
  return backendPromise;
}

class WriteBehindStorage {
  private pending = new Map<string, StorageBatchOp>();
  private readCache = new Map<string, unknown>();
  private flushTimer: ReturnType<typeof globalThis.setTimeout> | null = null;
  private flushing = false;
  private flushPromise: Promise<void> | null = null;

  async get<T>(key: string): Promise<T | null> {
    const pending = this.pending.get(key);
    if (pending?.kind === "delete") return null;
    if (pending?.kind === "set") {
      this.touchCache(key, pending.value);
      return pending.value as T;
    }
    if (this.readCache.has(key)) {
      const cached = this.readCache.get(key);
      this.touchCache(key, cached);
      return (cached ?? null) as T | null;
    }
    const backend = await getBackend();
    const value = await backend.get(key);
    if (value !== null && value !== undefined) {
      this.touchCache(key, value);
    }
    return (value ?? null) as T | null;
  }

  async set<T>(key: string, value: T): Promise<void> {
    this.pending.set(key, { kind: "set", key, value });
    this.touchCache(key, value);
    if (this.pending.size >= WRITE_BEHIND_MAX_PENDING) {
      this.scheduleFlush(0);
    } else {
      this.scheduleFlush();
    }
  }

  async delete(key: string): Promise<void> {
    this.pending.set(key, { kind: "delete", key });
    this.readCache.delete(key);
    if (this.pending.size >= WRITE_BEHIND_MAX_PENDING) {
      this.scheduleFlush(0);
    } else {
      this.scheduleFlush();
    }
  }

  async flush(): Promise<void> {
    if (this.flushing) {
      return this.flushPromise ?? Promise.resolve();
    }
    if (this.pending.size === 0) return;
    this.clearFlushTimer();
    this.flushing = true;
    const batchEntries = Array.from(this.pending.entries());
    this.pending.clear();
    const flushPromise = (async () => {
      try {
        const backend = await getBackend();
        await backend.batch(batchEntries.map(([, op]) => op));
      } catch (err) {
        for (const [key, op] of batchEntries) {
          if (!this.pending.has(key)) {
            this.pending.set(key, op);
          }
        }
        this.scheduleFlush(WRITE_BEHIND_RETRY_MS);
        throw err;
      } finally {
        this.flushing = false;
        if (this.pending.size > 0) {
          this.scheduleFlush();
        }
      }
    })();
    this.flushPromise = flushPromise;
    try {
      await flushPromise;
    } finally {
      if (this.flushPromise === flushPromise) {
        this.flushPromise = null;
      }
    }
  }

  private scheduleFlush(delayMs: number = WRITE_BEHIND_DELAY_MS) {
    if (this.flushTimer !== null) return;
    this.flushTimer = globalThis.setTimeout(() => {
      this.flushTimer = null;
      void this.flush().catch(() => {});
    }, delayMs);
  }

  private clearFlushTimer() {
    if (this.flushTimer === null) return;
    globalThis.clearTimeout(this.flushTimer);
    this.flushTimer = null;
  }

  private touchCache(key: string, value: unknown) {
    if (this.readCache.has(key)) {
      this.readCache.delete(key);
    }
    this.readCache.set(key, value);
    if (this.readCache.size > READ_CACHE_LIMIT) {
      const oldest = this.readCache.keys().next().value;
      if (oldest) {
        this.readCache.delete(oldest);
      }
    }
  }
}

function createWebappStorage(): WebappStorage {
  const storage = new WriteBehindStorage();
  return {
    getKv: (key) => storage.get(key),
    setKv: (key, value) => storage.set(key, value),
    deleteKv: (key) => storage.delete(key),
    getSnapshot: (key) => storage.get(key),
    setSnapshot: (key, value) => storage.set(key, value),
    deleteSnapshot: (key) => storage.delete(key),
    getHistoryPage: (key) => storage.get(key),
    setHistoryPage: (key, value) => storage.set(key, value),
    deleteHistoryPage: (key) => storage.delete(key),
    flush: () => storage.flush(),
  };
}

export function getWebappStorage(): WebappStorage {
  if (!storageInstance) {
    storageInstance = createWebappStorage();
  }
  return storageInstance;
}
