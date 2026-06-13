import {
  createPretextVirtualizerCore,
  type PretextVirtualizerDiagnosticEvent,
  type PretextVirtualizerSnapshot,
} from "@pretext-virtualizer/core";
import type { WorkbenchListItem } from "../sessionView/SessionPage.types";
import {
  type WorkbenchMessageListUiState,
} from "../sessionMessageListItemIdentity";
import {
  addPretextPerfBucket,
  incrementPretextPerfCounter,
  recordPretextPerfEvent,
} from "../../utils/pretextPerfDiagnostics";
import {
  bindSessionPretextRuntime,
  type SessionPretextRuntimeBindings,
} from "./pretextSessionRuntimeBindings";
import { clearPretextRowMeasurementOverrides } from "./pretextRowMeasurementOverrides";
import {
  buildSessionPretextRuntimeLayoutKey,
  buildSessionPretextRuntimeSourceKey,
  createDefaultSessionTranscriptUiState,
  getSessionTranscriptUiStateRevision,
  normalizeViewportDimension,
} from "./pretextSessionRuntimeInputs";

// Exact row heights let the session keep a tight render budget.
export const SESSION_PRETEXT_OVERSCAN_PX = 640;
export const SESSION_PRETEXT_BOTTOM_THRESHOLD_PX = 16;

type PlannedLayoutGetter = (
  item: WorkbenchListItem,
  viewport: {
    width: number;
    widthBucket: `w${number}`;
  },
) => {
  height: number;
};

export type SessionPretextRuntimeRecord = {
  sessionId: string;
  core: ReturnType<typeof createPretextVirtualizerCore<WorkbenchListItem>>;
  callbacks: {
    getLayoutRevision: (item: WorkbenchListItem) => string | number;
    getPlannedLayout: PlannedLayoutGetter;
    onDiagnosticEvent?: ((event: PretextVirtualizerDiagnosticEvent<WorkbenchListItem>) => void) | null;
  };
  uiState: WorkbenchMessageListUiState;
  uiStateRevision: string;
  preparedSourceKey: string | null;
  preparedLayoutKey: string | null;
  preparedSnapshot: PretextVirtualizerSnapshot<WorkbenchListItem>;
  preparedItems: readonly WorkbenchListItem[];
};

export type SessionTranscriptWarmCacheEntry = {
  sourceKey: string;
  layoutKey: string;
  warmKey: string;
  snapshot: unknown;
  updatedAtMs: number;
};

type SessionTranscriptCacheRecord = {
  sessionId: string;
  warmEntry: SessionTranscriptWarmCacheEntry | null;
  runtime: SessionPretextRuntimeRecord | null;
};

type PrimeSessionPretextRuntimeParams = {
  sessionId: string;
  listItems: readonly WorkbenchListItem[];
  uiState: WorkbenchMessageListUiState;
  viewportWidth: number;
  viewportHeight?: number;
  sourceKey?: string;
  layoutKey?: string;
};

const sessionTranscriptCache = new Map<string, SessionTranscriptCacheRecord>();

function createSessionTranscriptCacheRecord(sessionId: string): SessionTranscriptCacheRecord {
  return {
    sessionId,
    warmEntry: null,
    runtime: null,
  };
}

function getOrCreateSessionTranscriptCacheRecord(sessionId: string): SessionTranscriptCacheRecord {
  let record = sessionTranscriptCache.get(sessionId);
  if (!record) {
    record = createSessionTranscriptCacheRecord(sessionId);
    sessionTranscriptCache.set(sessionId, record);
  }
  return record;
}

function deleteSessionTranscriptCacheRecordIfEmpty(record: SessionTranscriptCacheRecord): void {
  if (record.warmEntry != null || record.runtime != null) {
    return;
  }
  sessionTranscriptCache.delete(record.sessionId);
}

export function readSessionTranscriptWarmEntry(sessionId: string): SessionTranscriptWarmCacheEntry | null {
  return sessionTranscriptCache.get(sessionId)?.warmEntry ?? null;
}

export function persistSessionTranscriptWarmEntry(
  sessionId: string,
  warmEntry: SessionTranscriptWarmCacheEntry,
): void {
  const record = getOrCreateSessionTranscriptCacheRecord(sessionId);
  record.warmEntry = warmEntry;
}

export function pruneSessionTranscriptWarmEntries(retainedSessionIds: readonly string[]): void {
  const retained = new Set(retainedSessionIds);
  for (const record of sessionTranscriptCache.values()) {
    if (record.warmEntry == null || retained.has(record.sessionId)) continue;
    record.warmEntry = null;
    deleteSessionTranscriptCacheRecordIfEmpty(record);
  }
}

export function countSessionTranscriptWarmEntries(): number {
  let count = 0;
  for (const record of sessionTranscriptCache.values()) {
    if (record.warmEntry != null) {
      count += 1;
    }
  }
  return count;
}

export function resetSessionTranscriptWarmEntries(): void {
  for (const record of sessionTranscriptCache.values()) {
    record.warmEntry = null;
    deleteSessionTranscriptCacheRecordIfEmpty(record);
  }
}

export {
  buildSessionPretextRuntimeLayoutKey,
  buildSessionPretextRuntimeSourceKey,
  createDefaultSessionTranscriptUiState,
};

function createSessionPretextRuntime(sessionId: string): SessionPretextRuntimeRecord {
  const uiState = createDefaultSessionTranscriptUiState();
  const callbacks: SessionPretextRuntimeRecord["callbacks"] = {
    getLayoutRevision: () => 0,
    getPlannedLayout: () => ({ height: 1 }),
    onDiagnosticEvent: null,
  };
  const core = createPretextVirtualizerCore<WorkbenchListItem>({
    initialItems: [],
    getId: (item) => item.id,
    getLayoutRevision: (item) => callbacks.getLayoutRevision(item),
    getPlannedLayout: (item, viewport) => callbacks.getPlannedLayout(item, viewport),
    overscanPx: SESSION_PRETEXT_OVERSCAN_PX,
    bottomThresholdPx: SESSION_PRETEXT_BOTTOM_THRESHOLD_PX,
    onDiagnosticEvent: (event) => {
      callbacks.onDiagnosticEvent?.(event);
    },
  });

  const record: SessionPretextRuntimeRecord = {
    sessionId,
    core,
    callbacks,
    uiState,
    uiStateRevision: getSessionTranscriptUiStateRevision(uiState),
    preparedSourceKey: null,
    preparedLayoutKey: null,
    preparedSnapshot: core.getSnapshot(),
    preparedItems: [],
  };
  bindSessionPretextRuntime(record, {
    uiState: record.uiState,
    listItems: record.preparedItems,
    uiStateRevision: record.uiStateRevision,
  });
  return record;
}

export function getOrCreateSessionPretextRuntime(
  sessionId: string,
  bindings?: SessionPretextRuntimeBindings,
): SessionPretextRuntimeRecord {
  const cacheRecord = getOrCreateSessionTranscriptCacheRecord(sessionId);
  let record = cacheRecord.runtime;
  if (!record) {
    record = createSessionPretextRuntime(sessionId);
    cacheRecord.runtime = record;
  }
  if (bindings) {
    bindSessionPretextRuntime(record, bindings);
  }
  return record;
}

export function readSessionPretextRuntime(sessionId: string): SessionPretextRuntimeRecord | null {
  return sessionTranscriptCache.get(sessionId)?.runtime ?? null;
}

export function isSessionPretextRuntimePreparedFor({
  sessionId,
  sourceKey,
  layoutKey,
  viewportWidth,
  viewportHeight,
}: {
  sessionId: string;
  sourceKey: string;
  layoutKey: string;
  viewportWidth: number;
  viewportHeight?: number;
}): boolean {
  const record = readSessionPretextRuntime(sessionId);
  if (!record) return false;
  if (record.preparedSourceKey !== sourceKey || record.preparedLayoutKey !== layoutKey) return false;
  const nextWidth = normalizeViewportDimension(viewportWidth);
  const nextHeight = normalizeViewportDimension(viewportHeight);
  if (nextWidth > 0 && record.preparedSnapshot.viewportWidth !== nextWidth) return false;
  if (nextHeight > 0 && record.preparedSnapshot.viewportHeight !== nextHeight) return false;
  return true;
}

export function noteSessionPretextRuntimeSnapshot(
  record: SessionPretextRuntimeRecord,
  snapshot: PretextVirtualizerSnapshot<WorkbenchListItem>,
  listItems: readonly WorkbenchListItem[],
  preparedKeys?: {
    sourceKey?: string | null;
    layoutKey?: string | null;
  },
): void {
  record.preparedSnapshot = snapshot;
  record.preparedItems = listItems;
  if (preparedKeys && "sourceKey" in preparedKeys) {
    record.preparedSourceKey = preparedKeys.sourceKey ?? null;
  }
  if (preparedKeys && "layoutKey" in preparedKeys) {
    record.preparedLayoutKey = preparedKeys.layoutKey ?? null;
  }
}

export function primeSessionPretextRuntime(
  params: PrimeSessionPretextRuntimeParams,
): SessionPretextRuntimeRecord {
  const record = getOrCreateSessionPretextRuntime(params.sessionId);
  incrementPretextPerfCounter("pretext_runtime_prime_calls");
  const nextUiStateRevision = getSessionTranscriptUiStateRevision(params.uiState, params.listItems);
  const nextSourceKey = params.sourceKey ?? buildSessionPretextRuntimeSourceKey(params.listItems, params.uiState);
  const nextLayoutKey =
    params.layoutKey ??
    buildSessionPretextRuntimeLayoutKey({
      uiState: params.uiState,
      listItems: params.listItems,
    });
  const uiStateChanged = record.uiStateRevision !== nextUiStateRevision;
  const itemsChanged = record.preparedSourceKey !== nextSourceKey;
  const layoutChanged = record.preparedLayoutKey !== nextLayoutKey;
  if (uiStateChanged) {
    bindSessionPretextRuntime(record, {
      uiState: params.uiState,
      listItems: params.listItems,
      uiStateRevision: nextUiStateRevision,
    });
  }
  const nextWidth = normalizeViewportDimension(params.viewportWidth);
  const nextHeight = normalizeViewportDimension(params.viewportHeight);
  const viewportChanged =
    (nextWidth > 0 && record.preparedSnapshot.viewportWidth !== nextWidth) ||
    (nextHeight > 0 && record.preparedSnapshot.viewportHeight !== nextHeight);
  const requiresItemSync = itemsChanged || uiStateChanged || layoutChanged;
  if (!requiresItemSync && !viewportChanged) {
    incrementPretextPerfCounter("pretext_runtime_prime_noop");
    return record;
  }
  if (viewportChanged) {
    incrementPretextPerfCounter("pretext_runtime_prime_viewport_sync");
    record.preparedSnapshot = record.core.syncViewport({
      width: nextWidth,
      height: nextHeight,
      scrollTop: record.preparedSnapshot.scrollTop,
    });
  }
  if (requiresItemSync) {
    incrementPretextPerfCounter("pretext_runtime_prime_replace_items");
    incrementPretextPerfCounter("pretext_runtime_prime_replace_item_count", params.listItems.length);
    addPretextPerfBucket(
      "pretext_runtime_prime_replace_reason",
      itemsChanged && uiStateChanged
        ? "items+ui"
        : itemsChanged
          ? "items"
          : layoutChanged
            ? "layout"
            : "ui",
    );
    recordPretextPerfEvent("runtime-prime:replace-items", {
      sessionId: params.sessionId,
      itemCount: params.listItems.length,
      itemsChanged,
      uiStateChanged,
      layoutChanged,
      viewportChanged,
    });
    const anchor = { kind: "bottom" as const };
    record.preparedSnapshot = record.core.syncItems(params.listItems, anchor);
    record.preparedItems = params.listItems;
  }
  record.preparedSourceKey = nextSourceKey;
  record.preparedLayoutKey = nextLayoutKey;
  return record;
}

export function readSessionPretextRuntimePreparedState(record: SessionPretextRuntimeRecord): {
  snapshot: PretextVirtualizerSnapshot<WorkbenchListItem>;
  listItems: readonly WorkbenchListItem[];
  sourceKey: string | null;
  layoutKey: string | null;
} {
  return {
    snapshot: record.preparedSnapshot,
    listItems: record.preparedItems,
    sourceKey: record.preparedSourceKey,
    layoutKey: record.preparedLayoutKey,
  };
}

export function pruneSessionPretextRuntimeCache(retainedPreparedSessionIds: readonly string[]): void {
  const retained = new Set(retainedPreparedSessionIds);
  let deletedCount = 0;
  for (const cacheRecord of sessionTranscriptCache.values()) {
    const record = cacheRecord.runtime;
    if (!record) continue;
    if (retained.has(cacheRecord.sessionId)) continue;
    cacheRecord.runtime = null;
    deleteSessionTranscriptCacheRecordIfEmpty(cacheRecord);
    deletedCount += 1;
  }
  if (deletedCount > 0) {
    incrementPretextPerfCounter("pretext_runtime_cache_pruned_entries", deletedCount);
  }
}

export function getSessionPretextRuntimeCacheSize(): number {
  let count = 0;
  for (const record of sessionTranscriptCache.values()) {
    if (record.runtime != null) {
      count += 1;
    }
  }
  return count;
}

export function resetSessionPretextRuntimeCache(): void {
  clearPretextRowMeasurementOverrides();
  for (const record of sessionTranscriptCache.values()) {
    record.runtime = null;
    deleteSessionTranscriptCacheRecordIfEmpty(record);
  }
}
