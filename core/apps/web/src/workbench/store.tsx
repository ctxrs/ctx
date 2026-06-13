import React, { createContext, useCallback, useContext, useEffect, useMemo, useSyncExternalStore } from "react";
import type { MessageAttachment } from "../api/client";
import { randomUuid } from "../utils/randomUuid";
import { errorMessage } from "../utils/errorMessage";
import {
  defaultWindowState,
  ensureLeafActiveTab,
  findLeaf,
  getActiveTabFromLeaf,
  getOrCreateWindowId,
  readSessionWindowV1,
  updateLeaf,
  writeSessionWindowV1,
} from "../utils/workbenchStoreLayout";
import type { WorkbenchModeId } from "../components/WorkbenchComposer";
import type { PersistedWorkbenchWindowV1, WorkbenchDraft, WorkbenchTab } from "./types";
import {
  loadWorkbenchDraftV1,
  loadWorkbenchWindowV1,
  saveWorkbenchDraftV1,
  saveWorkbenchWindowV1Immediate,
  workbenchDaemonKey,
} from "./persistence";

type WorkbenchDraftValue = {
  text: string;
  modeId: WorkbenchModeId;
  attachments: MessageAttachment[];
};
type WorkbenchDraftUpdate =
  | {
      text: string;
      modeId: WorkbenchModeId;
      attachments?: MessageAttachment[];
    }
  | ((prev: WorkbenchDraftValue) => {
      text: string;
      modeId: WorkbenchModeId;
      attachments?: MessageAttachment[];
    });

export const NEW_TASK_DRAFT_KEY = "new_task";

export function sessionDraftKey(sessionId: string): string {
  return `session:${sessionId}`;
}

type DraftSnapshot = {
  byKey: Record<string, WorkbenchDraft | undefined>;
  loadedKeys: Record<string, boolean | undefined>;
};

export type WorkbenchNavToken = number;
export type WorkbenchNavSource = "system" | "user";
export type WorkbenchNavOpts = {
  navToken?: WorkbenchNavToken;
  source?: WorkbenchNavSource;
};

export type WorkbenchStoreSnapshot = {
  workspaceId: string;
  windowId: string;
  hydrated: boolean;
  persistEnabled: boolean;
  warnings: string[];
  window: PersistedWorkbenchWindowV1;
  drafts: DraftSnapshot;
};

export type WorkbenchShellSnapshot = {
  workspaceId: string;
  windowId: string;
  hydrated: boolean;
  warnings: string[];
  window: PersistedWorkbenchWindowV1;
};

type WorkbenchStoreListener = () => void;

type DraftBroadcastMsg =
  | {
      type: "draft";
      workspaceId: string;
      windowId: string;
      draftKey: string;
      draft: WorkbenchDraft;
    }
  | {
      type: "draft_delete";
      workspaceId: string;
      windowId: string;
      draftKey: string;
      updatedAtMs: number;
    };

export class WorkbenchStore {
  private listeners = new Set<WorkbenchStoreListener>();
  private snapshot: WorkbenchStoreSnapshot;
  private persistEnabled = true;
  private persistTimer: number | null = null;
  private draftTimers = new Map<string, number>();
  private draftLoadsInFlight = new Map<string, Promise<void>>();
  private channel: BroadcastChannel | null = null;
  private layoutDirtyBeforeHydrate = false;
  private seededFromSessionStorage = false;
  private shellSnapshotCache: {
    window: PersistedWorkbenchWindowV1;
    warnings: string[];
    hydrated: boolean;
    value: WorkbenchShellSnapshot;
  } | null = null;
  private navEpoch = 0;

  constructor(workspaceId: string) {
    const windowId = getOrCreateWindowId();
    this.snapshot = {
      workspaceId,
      windowId,
      hydrated: false,
      persistEnabled: true,
      warnings: [],
      window: defaultWindowState(),
      drafts: { byKey: {}, loadedKeys: {} },
    };

    const sessionWindow = readSessionWindowV1(workspaceId, windowId);
    if (sessionWindow) {
      this.snapshot = { ...this.snapshot, window: sessionWindow };
      this.seededFromSessionStorage = true;
    }
  }

  subscribe = (listener: WorkbenchStoreListener): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  getSnapshot = (): WorkbenchStoreSnapshot => this.snapshot;

  getShellSnapshot = (): WorkbenchShellSnapshot => {
    const { workspaceId, windowId, hydrated, warnings, window } = this.snapshot;
    const cache = this.shellSnapshotCache;
    if (cache && cache.window === window && cache.warnings === warnings && cache.hydrated === hydrated) {
      return cache.value;
    }
    const value: WorkbenchShellSnapshot = { workspaceId, windowId, hydrated, warnings, window };
    this.shellSnapshotCache = { window, warnings, hydrated, value };
    return value;
  };

  init = () => {
    this.hydrate().catch(() => {});
    this.initBroadcast();
    this.ensureDraftLoaded(NEW_TASK_DRAFT_KEY).catch(() => {});
  };

  private publish() {
    for (const l of this.listeners) l();
  }

  private markLayoutDirty() {
    if (!this.snapshot.hydrated) {
      this.layoutDirtyBeforeHydrate = true;
    }
  }

  private addWarning(msg: string) {
    if (this.snapshot.warnings.includes(msg)) return;
    this.snapshot = { ...this.snapshot, warnings: [...this.snapshot.warnings, msg] };
    this.publish();
  }

  private setWindow(
    next: PersistedWorkbenchWindowV1,
    opts?: { skipPersist?: boolean; persistDelayMs?: number },
  ) {
    this.snapshot = { ...this.snapshot, window: next };
    writeSessionWindowV1(this.snapshot.workspaceId, this.snapshot.windowId, next);
    this.publish();
    if (!opts?.skipPersist) this.schedulePersistWindow(opts?.persistDelayMs);
  }

  private schedulePersistWindow(delayMs?: number) {
    if (!this.persistEnabled) return;
    if (this.persistTimer) window.clearTimeout(this.persistTimer);
    const waitMs = Math.max(0, Math.min(5_000, typeof delayMs === "number" ? delayMs : 250));
    if (waitMs === 0) {
      const { workspaceId, windowId, window } = this.snapshot;
      saveWorkbenchWindowV1Immediate(workspaceId, windowId, window).catch((e: unknown) => {
        this.persistEnabled = false;
        this.snapshot = { ...this.snapshot, persistEnabled: false };
        this.addWarning(`Workbench persistence disabled: ${errorMessage(e)}`);
      });
      return;
    }
    this.persistTimer = window.setTimeout(() => {
      this.persistTimer = null;
      const { workspaceId, windowId, window } = this.snapshot;
      saveWorkbenchWindowV1Immediate(workspaceId, windowId, window).catch((e: unknown) => {
        this.persistEnabled = false;
        this.snapshot = { ...this.snapshot, persistEnabled: false };
        this.addWarning(`Workbench persistence disabled: ${errorMessage(e)}`);
      });
    }, waitMs);
  }

  private async hydrate() {
    const { workspaceId, windowId } = this.snapshot;
    try {
      const loaded = await loadWorkbenchWindowV1(workspaceId, windowId);
      if (loaded && !this.layoutDirtyBeforeHydrate && !this.seededFromSessionStorage) {
        this.snapshot = { ...this.snapshot, window: loaded };
      }
    } catch (e: unknown) {
      this.persistEnabled = false;
      this.snapshot = { ...this.snapshot, persistEnabled: false };
      this.addWarning(`IndexedDB unavailable: ${errorMessage(e)}`);
    } finally {
      this.snapshot = { ...this.snapshot, hydrated: true };
      this.publish();
    }
  }

  private initBroadcast() {
    if (typeof BroadcastChannel === "undefined") {
      this.addWarning("BroadcastChannel unavailable: drafts will not sync across windows.");
      return;
    }
    const { workspaceId } = this.snapshot;
    const name = `ctx-wb-drafts-${encodeURIComponent(workbenchDaemonKey())}-${encodeURIComponent(workspaceId)}`;
    const ch = new BroadcastChannel(name);
    this.channel = ch;
    ch.addEventListener("message", (ev) => {
      const data = ev.data as DraftBroadcastMsg | undefined;
      if (!data || typeof data !== "object") return;
      if (data.workspaceId !== workspaceId) return;
      if (data.windowId === this.snapshot.windowId) return;
      if (data.type === "draft") {
        this.applyRemoteDraft(data.draftKey, data.draft);
      } else if (data.type === "draft_delete") {
        this.applyRemoteDraftDelete(data.draftKey, data.updatedAtMs);
      }
    });
  }

  private applyRemoteDraft(draftKey: string, draft: WorkbenchDraft) {
    const existing = this.snapshot.drafts.byKey[draftKey];
    if (existing && existing.updatedAtMs >= draft.updatedAtMs) return;
    const nextDrafts: DraftSnapshot = {
      byKey: { ...this.snapshot.drafts.byKey, [draftKey]: draft },
      loadedKeys: { ...this.snapshot.drafts.loadedKeys, [draftKey]: true },
    };
    this.snapshot = { ...this.snapshot, drafts: nextDrafts };
    this.publish();
  }

  private applyRemoteDraftDelete(draftKey: string, updatedAtMs: number) {
    const existing = this.snapshot.drafts.byKey[draftKey];
    if (existing && existing.updatedAtMs > updatedAtMs) return;
    const nextByKey = { ...this.snapshot.drafts.byKey };
    delete nextByKey[draftKey];
    const nextLoaded = { ...this.snapshot.drafts.loadedKeys, [draftKey]: true };
    this.snapshot = { ...this.snapshot, drafts: { byKey: nextByKey, loadedKeys: nextLoaded } };
    this.publish();
  }

  getFocusedLeaf(): Extract<PersistedWorkbenchWindowV1["layout"], { kind: "leaf" }> | null {
    return findLeaf(this.snapshot.window.layout, this.snapshot.window.focusedLeafId);
  }

  getActiveTab(): WorkbenchTab | null {
    const leaf = this.getFocusedLeaf();
    if (!leaf) return null;
    return getActiveTabFromLeaf(leaf);
  }

  getNavToken = (): WorkbenchNavToken => this.navEpoch;

  bumpNavToken = (): WorkbenchNavToken => {
    this.navEpoch += 1;
    return this.navEpoch;
  };

  private shouldApplyNavToken(navToken?: WorkbenchNavToken): boolean {
    return navToken === undefined || navToken === this.navEpoch;
  }

  private applyNavSource(source?: WorkbenchNavSource) {
    if (source !== "system") this.bumpNavToken();
  }

  focusNewTask = (opts?: WorkbenchNavOpts): boolean => {
    if (!this.shouldApplyNavToken(opts?.navToken)) return false;
    this.applyNavSource(opts?.source);
    this.markLayoutDirty();
    const win = this.snapshot.window;
    const leafId = win.focusedLeafId;
    const leaf = findLeaf(win.layout, leafId);
    if (!leaf) return false;
    const existing = leaf.tabs.find((t) => t.kind === "new_task");
    const tabId = existing?.id ?? randomUuid();
    const newTab: WorkbenchTab = { id: tabId, kind: "new_task" };
    const nextTabs = existing ? leaf.tabs : [newTab, ...leaf.tabs];
    const nextLeaf = ensureLeafActiveTab({ ...leaf, tabs: nextTabs, activeTabId: tabId });
    this.setWindow({ ...win, layout: updateLeaf(win.layout, leafId, () => nextLeaf) }, { persistDelayMs: 0 });
    return true;
  };

  focusTask = (taskId: string, sessionId?: string | null, opts?: WorkbenchNavOpts): boolean => {
    const tid = String(taskId).trim();
    if (!tid) return false;
    if (!this.shouldApplyNavToken(opts?.navToken)) return false;
    this.applyNavSource(opts?.source);
    this.markLayoutDirty();
    const win = this.snapshot.window;
    const leafId = win.focusedLeafId;
    const leaf = findLeaf(win.layout, leafId);
    if (!leaf) return false;

    const existing = leaf.tabs.find((t) => t.kind === "task" && t.ref.taskId === tid);
    const tabId = existing?.id ?? randomUuid();
    const newTab: WorkbenchTab = {
      id: tabId,
      kind: "task",
      ref: { taskId: tid, sessionId: sessionId ?? null },
    };
    const nextTabs = existing
      ? leaf.tabs.map((t) => {
        if (t.id !== tabId) return t;
        if (t.kind !== "task") return t;
        const nextSessionId = sessionId === undefined ? (t.ref.sessionId ?? null) : sessionId;
        return { ...t, ref: { ...t.ref, sessionId: nextSessionId } };
      })
      : [newTab, ...leaf.tabs];

    const nextLeaf = ensureLeafActiveTab({ ...leaf, tabs: nextTabs, activeTabId: tabId });
    this.setWindow({ ...win, layout: updateLeaf(win.layout, leafId, () => nextLeaf) }, { persistDelayMs: 0 });
    return true;
  };

  setActiveSessionForActiveTask = (sessionId: string | null, opts?: WorkbenchNavOpts): boolean => {
    if (!this.shouldApplyNavToken(opts?.navToken)) return false;
    this.applyNavSource(opts?.source);
    this.markLayoutDirty();
    const win = this.snapshot.window;
    const leafId = win.focusedLeafId;
    const leaf = findLeaf(win.layout, leafId);
    if (!leaf) return false;
    const tab = getActiveTabFromLeaf(leaf);
    if (!tab || tab.kind !== "task") return false;
    const nextLeaf = ensureLeafActiveTab({
      ...leaf,
      tabs: leaf.tabs.map((t) =>
        t.id === tab.id && t.kind === "task"
          ? { ...t, ref: { ...t.ref, sessionId } }
          : t,
      ),
    });
    this.setWindow({ ...win, layout: updateLeaf(win.layout, leafId, () => nextLeaf) }, { persistDelayMs: 0 });
    return true;
  };

  getDraft = (draftKey: string): WorkbenchDraft | null => {
    return this.snapshot.drafts.byKey[draftKey] ?? null;
  };

  ensureDraftLoaded = async (draftKey: string) => {
    const key = String(draftKey || "").trim();
    if (!key) return;
    if (this.snapshot.drafts.loadedKeys[key]) return;
    if (this.draftLoadsInFlight.has(key)) return this.draftLoadsInFlight.get(key)!;
    const p = (async () => {
      try {
        const loaded = await loadWorkbenchDraftV1(this.snapshot.workspaceId, key);
        if (loaded) {
          const existing = this.snapshot.drafts.byKey[key];
          if (!existing || existing.updatedAtMs < loaded.updatedAtMs) {
            const nextDrafts: DraftSnapshot = {
              byKey: { ...this.snapshot.drafts.byKey, [key]: loaded },
              loadedKeys: { ...this.snapshot.drafts.loadedKeys, [key]: true },
            };
            this.snapshot = { ...this.snapshot, drafts: nextDrafts };
            this.publish();
          }
        } else {
          const nextLoaded = { ...this.snapshot.drafts.loadedKeys, [key]: true };
          this.snapshot = { ...this.snapshot, drafts: { ...this.snapshot.drafts, loadedKeys: nextLoaded } };
          this.publish();
        }
      } catch (e: unknown) {
        this.addWarning(`Draft persistence error: ${errorMessage(e)}`);
      } finally {
        this.draftLoadsInFlight.delete(key);
      }
    })();
    this.draftLoadsInFlight.set(key, p);
    return p;
  };

  setDraft = (draftKey: string, next: WorkbenchDraftUpdate) => {
    const key = String(draftKey || "").trim();
    if (!key) return;
    const now = Date.now();
    const current = this.snapshot.drafts.byKey[key];
    const currentValue: WorkbenchDraftValue = {
      text: current?.text ?? "",
      modeId: current?.modeId ?? "default",
      attachments: current?.attachments ?? [],
    };
    const resolved = typeof next === "function" ? next(currentValue) : next;
    const attachments = resolved.attachments ?? currentValue.attachments;
    const draft: WorkbenchDraft = {
      text: resolved.text,
      modeId: resolved.modeId,
      attachments,
      updatedAtMs: now,
    };
    if (
      current &&
      current.text === draft.text &&
      current.modeId === draft.modeId &&
      JSON.stringify(current.attachments) === JSON.stringify(draft.attachments)
    ) {
      return;
    }
    const nextDrafts: DraftSnapshot = {
      byKey: { ...this.snapshot.drafts.byKey, [key]: draft },
      loadedKeys: { ...this.snapshot.drafts.loadedKeys, [key]: true },
    };
    this.snapshot = { ...this.snapshot, drafts: nextDrafts };
    this.publish();

    this.schedulePersistDraft(key, draft);

    try {
      this.channel?.postMessage({
        type: "draft",
        workspaceId: this.snapshot.workspaceId,
        windowId: this.snapshot.windowId,
        draftKey: key,
        draft,
      } satisfies DraftBroadcastMsg);
    } catch {
      // ignore
    }
  };

  flushDraft = async (draftKey: string): Promise<void> => {
    const key = String(draftKey || "").trim();
    if (!key) return;
    if (!this.persistEnabled) return;

    const draft = this.snapshot.drafts.byKey[key];
    if (!draft) return;

    const existingTimer = this.draftTimers.get(key);
    if (existingTimer) window.clearTimeout(existingTimer);
    this.draftTimers.delete(key);

    try {
      await saveWorkbenchDraftV1(this.snapshot.workspaceId, key, draft);
    } catch (e: unknown) {
      this.addWarning(`Draft persistence failed: ${errorMessage(e)}`);
    }
  };

  private schedulePersistDraft(key: string, draft: WorkbenchDraft) {
    if (!this.persistEnabled) return;
    const existingTimer = this.draftTimers.get(key);
    if (existingTimer) window.clearTimeout(existingTimer);
    const timer = window.setTimeout(() => {
      this.draftTimers.delete(key);
      saveWorkbenchDraftV1(this.snapshot.workspaceId, key, draft).catch((e: unknown) => {
        this.addWarning(`Draft persistence failed: ${errorMessage(e)}`);
      });
    }, 200);
    this.draftTimers.set(key, timer);
  }
}

const WorkbenchStoreContext = createContext<WorkbenchStore | null>(null);

export function WorkbenchStoreProvider({ workspaceId, children }: { workspaceId: string; children: React.ReactNode }) {
  const store = useMemo(() => new WorkbenchStore(workspaceId), [workspaceId]);
  useEffect(() => {
    store.init();
    return () => {
      // best-effort cleanup
    };
  }, [store]);
  return <WorkbenchStoreContext.Provider value={store}>{children}</WorkbenchStoreContext.Provider>;
}

export function useWorkbenchStore(): WorkbenchStore {
  const s = useContext(WorkbenchStoreContext);
  if (!s) throw new Error("WorkbenchStoreProvider missing");
  return s;
}

export function useWorkbenchSnapshot(): WorkbenchStoreSnapshot {
  const store = useWorkbenchStore();
  return useSyncExternalStore(store.subscribe, store.getSnapshot, store.getSnapshot);
}

export function useWorkbenchShellSnapshot(): WorkbenchShellSnapshot {
  const store = useWorkbenchStore();
  return useSyncExternalStore(store.subscribe, store.getShellSnapshot, store.getShellSnapshot);
}

export function useActiveWorkbenchTab(): WorkbenchTab | null {
  const snap = useWorkbenchSnapshot();
  const leaf = findLeaf(snap.window.layout, snap.window.focusedLeafId);
  if (!leaf) return null;
  return getActiveTabFromLeaf(leaf);
}

export function useActiveWorkbenchIds(): { taskId: string | null; sessionId: string | null } {
  const tab = useActiveWorkbenchTab();
  if (!tab) return { taskId: null, sessionId: null };
  if (tab.kind === "task") return { taskId: tab.ref.taskId, sessionId: tab.ref.sessionId ?? null };
  return { taskId: null, sessionId: null };
}

export function useWorkbenchDraft(
  draftKey: string,
  fallback?: { text: string; modeId: WorkbenchModeId; attachments?: MessageAttachment[] },
): {
  value: WorkbenchDraftValue;
  setValue: (next: WorkbenchDraftUpdate) => void;
  updatedAtMs: number;
} {
  const store = useWorkbenchStore();
  const snap = useWorkbenchSnapshot();
  const key = String(draftKey || "").trim();
  const draft = (key && snap.drafts.byKey[key]) || null;
  const loaded = !!(key && snap.drafts.loadedKeys[key]);

  useEffect(() => {
    if (!key) return;
    if (loaded) return;
    store.ensureDraftLoaded(key).catch(() => {});
  }, [store, key, loaded]);

  const value = useMemo<WorkbenchDraftValue>(() => {
    if (draft) return { text: draft.text, modeId: draft.modeId, attachments: draft.attachments ?? [] };
    return { text: fallback?.text ?? "", modeId: fallback?.modeId ?? "default", attachments: fallback?.attachments ?? [] };
  }, [draft, fallback]);

  const setValue = useCallback(
    (next: WorkbenchDraftUpdate) => {
      store.setDraft(key, next);
    },
    [store, key],
  );

  return { value, setValue, updatedAtMs: draft?.updatedAtMs ?? 0 };
}

export function useNewTaskDraft() {
  return useWorkbenchDraft(NEW_TASK_DRAFT_KEY, { text: "", modeId: "default", attachments: [] });
}
