type SessionMessageListDebugDetail = Record<string, unknown> | null;

export type SessionMessageListFlashSample = {
  atMs: number;
  scrollTop: number | null;
  clientHeight: number | null;
  scrollHeight: number | null;
  maxScrollTop: number | null;
  distanceFromMaxScrollPx: number | null;
  blankTailPx: number | null;
  firstItemId: string | null;
  firstItemTopPx: number | null;
  firstItemBottomPx: number | null;
  lastItemId: string | null;
  lastItemTopPx: number | null;
  lastItemBottomPx: number | null;
  renderedTopId: string | null;
  renderedAnchorId: string | null;
};

export type SessionMessageListFlashTrace = {
  seq: number;
  sessionId: string;
  cause: string;
  startedAtMs: number;
  finishedAtMs: number;
  sampleCount: number;
  maxAbsScrollTopDeltaPx: number;
  maxAbsScrollHeightDeltaPx: number;
  maxAbsClientHeightDeltaPx: number;
  maxAbsDistanceFromMaxDeltaPx: number;
  maxAbsFirstItemTopDeltaPx: number;
  maxAbsLastItemTopDeltaPx: number;
  scrollDirectionChanges: number;
  firstItemChanged: boolean;
  firstItemReverted: boolean;
  renderedTopChanged: boolean;
  renderedTopReverted: boolean;
  renderedAnchorChanged: boolean;
  renderedAnchorReverted: boolean;
  layoutShiftCount: number;
  layoutShiftValue: number;
  snapbackDetected: boolean;
  detail: SessionMessageListDebugDetail;
  samples: SessionMessageListFlashSample[];
};

export type SessionMessageListRowSizeMismatch = {
  seq: number;
  atMs: number;
  id: string;
  itemKind: string;
  itemKey: string;
  reason: string;
  dataIndex: number | null;
  knownSize: number;
  actualHeight: number;
  parentHeight: number;
  knownVsActualDeltaPx: number;
  knownVsParentDeltaPx: number;
  parentVsActualDeltaPx: number;
};

export type SessionMessageListDebugEntry = {
  seq: number;
  atMs: number;
  sessionId: string;
  cause: string;
  isActive: boolean;
  loaded: boolean;
  listCount: number;
  stickToBottom: boolean | null;
  renderedAnchorId: string | null;
  renderedTopId: string | null;
  firstItemId: string | null;
  firstItemTopPx: number | null;
  firstItemBottomPx: number | null;
  scrollTop: number | null;
  clientHeight: number | null;
  scrollHeight: number | null;
  maxScrollTop: number | null;
  distanceFromMaxScrollPx: number | null;
  blankTailPx: number | null;
  lastItemId: string | null;
  lastItemTopPx: number | null;
  lastItemBottomPx: number | null;
  lastItemOffscreenAbove: boolean | null;
  renderedItemCount: number | null;
  sessionViewHeight: number | null;
  sessionBottomHeight: number | null;
  composerHeight: number | null;
  queueHeight: number | null;
  impossibleTail: boolean;
  detail: SessionMessageListDebugDetail;
};

type SessionMessageListDebugStore = {
  seq: number;
  entries: SessionMessageListDebugEntry[];
  flashSeq: number;
  flashTraces: SessionMessageListFlashTrace[];
  rowSizeMismatchSeq: number;
  rowSizeMismatches: SessionMessageListRowSizeMismatch[];
};

type RecordSessionMessageListRowSizeMismatchParams = Omit<SessionMessageListRowSizeMismatch, "seq" | "atMs">;

type RecordSessionMessageListDebugSnapshotParams = {
  sessionId: string;
  cause: string;
  scroller: HTMLElement | null;
  isActive: boolean;
  loaded: boolean;
  listCount: number;
  stickToBottom: boolean | null;
  renderedAnchorId: string | null;
  renderedTopId: string | null;
  detail?: SessionMessageListDebugDetail;
};

declare global {
  interface Window {
    __wbSessionMessageListDebug?: SessionMessageListDebugStore;
  }
}

const MAX_SESSION_MESSAGE_LIST_DEBUG_ENTRIES = 400;
const MAX_SESSION_MESSAGE_LIST_FLASH_TRACES = 80;
const MAX_SESSION_MESSAGE_LIST_FLASH_SAMPLES = 24;
const MAX_SESSION_MESSAGE_LIST_ROW_SIZE_MISMATCHES = 400;
const IMPOSSIBLE_TAIL_THRESHOLD_PX = 96;
const FLASH_SNAPBACK_THRESHOLD_PX = 12;
const FLASH_SETTLE_THRESHOLD_PX = 3;

function roundPx(value: number | null): number | null {
  if (value == null || !Number.isFinite(value)) return null;
  return Math.round(value * 100) / 100;
}

function getStore(): SessionMessageListDebugStore {
  const existing = window.__wbSessionMessageListDebug;
  if (existing) {
    existing.seq = Number.isFinite(existing.seq) ? existing.seq : 0;
    existing.entries = Array.isArray(existing.entries) ? existing.entries : [];
    existing.flashSeq = Number.isFinite(existing.flashSeq) ? existing.flashSeq : 0;
    existing.flashTraces = Array.isArray(existing.flashTraces) ? existing.flashTraces : [];
    existing.rowSizeMismatchSeq = Number.isFinite(existing.rowSizeMismatchSeq) ? existing.rowSizeMismatchSeq : 0;
    existing.rowSizeMismatches = Array.isArray(existing.rowSizeMismatches) ? existing.rowSizeMismatches : [];
    return existing;
  }
  const created: SessionMessageListDebugStore = {
    seq: 0,
    entries: [],
    flashSeq: 0,
    flashTraces: [],
    rowSizeMismatchSeq: 0,
    rowSizeMismatches: [],
  };
  window.__wbSessionMessageListDebug = created;
  return created;
}

function renderedItem(scroller: HTMLElement, index: number): HTMLElement | null {
  const rendered = scroller.querySelectorAll<HTMLElement>("[role=\"listitem\"]");
  return rendered.length > index ? rendered.item(index) : null;
}

type SessionMessageListMeasuredState = {
  scrollTop: number | null;
  clientHeight: number | null;
  scrollHeight: number | null;
  maxScrollTop: number | null;
  distanceFromMaxScrollPx: number | null;
  blankTailPx: number | null;
  firstItemId: string | null;
  firstItemTopPx: number | null;
  firstItemBottomPx: number | null;
  lastItemId: string | null;
  lastItemTopPx: number | null;
  lastItemBottomPx: number | null;
  lastItemOffscreenAbove: boolean | null;
  renderedItemCount: number | null;
  sessionViewHeight: number | null;
  sessionBottomHeight: number | null;
  composerHeight: number | null;
  queueHeight: number | null;
  impossibleTail: boolean;
};

export function measureSessionMessageListState(scroller: HTMLElement | null): SessionMessageListMeasuredState {
  const scrollerRect = scroller?.getBoundingClientRect() ?? null;
  const sessionView = scroller?.closest(".wb-session-view") as HTMLElement | null;
  const sessionBottom = sessionView?.querySelector(".wb-session-bottom") as HTMLElement | null;
  const composer = sessionView?.querySelector(".wb-active-composer") as HTMLElement | null;
  const queuePanel = sessionView?.querySelector(".queue-panel") as HTMLElement | null;
  const firstItem = scroller ? renderedItem(scroller, 0) : null;
  const lastItem = scroller ? renderedItem(scroller, Math.max(0, scroller.querySelectorAll("[role=\"listitem\"]").length - 1)) : null;
  const firstItemRect = firstItem?.getBoundingClientRect() ?? null;
  const lastItemRect = lastItem?.getBoundingClientRect() ?? null;

  const scrollTop = scroller ? roundPx(scroller.scrollTop) : null;
  const clientHeight = scroller ? roundPx(scroller.clientHeight) : null;
  const scrollHeight = scroller ? roundPx(scroller.scrollHeight) : null;
  const maxScrollTop =
    scroller && clientHeight != null && scrollHeight != null ? roundPx(Math.max(0, scrollHeight - clientHeight)) : null;
  const distanceFromMaxScrollPx =
    scrollTop != null && maxScrollTop != null ? roundPx(Math.max(0, maxScrollTop - scrollTop)) : null;
  const blankTailPx =
    scrollerRect && lastItemRect ? roundPx(Math.max(0, scrollerRect.bottom - lastItemRect.bottom)) : null;
  const firstItemTopPx = scrollerRect && firstItemRect ? roundPx(firstItemRect.top - scrollerRect.top) : null;
  const firstItemBottomPx = scrollerRect && firstItemRect ? roundPx(firstItemRect.bottom - scrollerRect.top) : null;
  const lastItemTopPx = scrollerRect && lastItemRect ? roundPx(lastItemRect.top - scrollerRect.top) : null;
  const lastItemBottomPx = scrollerRect && lastItemRect ? roundPx(lastItemRect.bottom - scrollerRect.top) : null;
  const lastItemOffscreenAbove =
    scrollerRect && lastItemRect ? lastItemRect.bottom < scrollerRect.top - 1 : null;
  const impossibleTail =
    Boolean(distanceFromMaxScrollPx != null && distanceFromMaxScrollPx <= 2) &&
    Boolean((blankTailPx != null && blankTailPx > IMPOSSIBLE_TAIL_THRESHOLD_PX) || lastItemOffscreenAbove);

  return {
    scrollTop,
    clientHeight,
    scrollHeight,
    maxScrollTop,
    distanceFromMaxScrollPx,
    blankTailPx,
    firstItemId: firstItem?.getAttribute("data-thread-item-id") ?? null,
    firstItemTopPx,
    firstItemBottomPx,
    lastItemId: lastItem?.getAttribute("data-thread-item-id") ?? null,
    lastItemTopPx,
    lastItemBottomPx,
    lastItemOffscreenAbove,
    renderedItemCount: scroller ? scroller.querySelectorAll("[role=\"listitem\"]").length : null,
    sessionViewHeight: sessionView ? roundPx(sessionView.getBoundingClientRect().height) : null,
    sessionBottomHeight: sessionBottom ? roundPx(sessionBottom.getBoundingClientRect().height) : null,
    composerHeight: composer ? roundPx(composer.getBoundingClientRect().height) : null,
    queueHeight: queuePanel ? roundPx(queuePanel.getBoundingClientRect().height) : null,
    impossibleTail,
  };
}

function deltaFromBaseline(value: number | null, baseline: number | null): number {
  if (value == null || baseline == null) return 0;
  return value - baseline;
}

function maxAbs(values: number[]): number {
  return values.reduce((max, value) => Math.max(max, Math.abs(value)), 0);
}

function countDirectionChanges(values: number[]): number {
  let changes = 0;
  let lastSign = 0;
  for (const value of values) {
    if (Math.abs(value) < 1) continue;
    const sign = Math.sign(value);
    if (sign === 0) continue;
    if (lastSign !== 0 && sign !== lastSign) changes += 1;
    lastSign = sign;
  }
  return changes;
}

function didIdentifierRevert(values: Array<string | null>, baseline: string | null): boolean {
  if (baseline == null) return false;
  let changed = false;
  for (const value of values.slice(1)) {
    if (value !== baseline) {
      changed = true;
      continue;
    }
    if (changed) return true;
  }
  return false;
}

function detectSnapback(values: number[]): boolean {
  if (values.length < 3) return false;
  let furthestValue = 0;
  let furthestIndex = -1;
  for (let index = 1; index < values.length; index += 1) {
    const value = values[index] ?? 0;
    if (Math.abs(value) > Math.abs(furthestValue)) {
      furthestValue = value;
      furthestIndex = index;
    }
  }
  if (furthestIndex < 0 || Math.abs(furthestValue) < FLASH_SNAPBACK_THRESHOLD_PX) return false;
  for (let index = furthestIndex + 1; index < values.length; index += 1) {
    const value = values[index] ?? 0;
    if (Math.abs(value) <= FLASH_SETTLE_THRESHOLD_PX) return true;
    if (Math.sign(value) !== 0 && Math.sign(value) !== Math.sign(furthestValue)) return true;
  }
  return false;
}

type RecordSessionMessageListFlashTraceParams = {
  sessionId: string;
  cause: string;
  startedAtMs: number;
  finishedAtMs: number;
  samples: SessionMessageListFlashSample[];
  layoutShiftCount?: number;
  layoutShiftValue?: number;
  detail?: SessionMessageListDebugDetail;
};

export function recordSessionMessageListFlashTrace({
  sessionId,
  cause,
  startedAtMs,
  finishedAtMs,
  samples,
  layoutShiftCount = 0,
  layoutShiftValue = 0,
  detail = null,
}: RecordSessionMessageListFlashTraceParams): SessionMessageListFlashTrace | null {
  if (typeof window === "undefined") return null;
  const normalizedSamples = samples.slice(0, MAX_SESSION_MESSAGE_LIST_FLASH_SAMPLES);
  if (normalizedSamples.length === 0) return null;

  const baseline = normalizedSamples[0];
  const scrollTopDeltas = normalizedSamples.map((sample) => deltaFromBaseline(sample.scrollTop, baseline.scrollTop));
  const scrollHeightDeltas = normalizedSamples.map((sample) => deltaFromBaseline(sample.scrollHeight, baseline.scrollHeight));
  const clientHeightDeltas = normalizedSamples.map((sample) => deltaFromBaseline(sample.clientHeight, baseline.clientHeight));
  const distanceFromMaxDeltas = normalizedSamples.map((sample) =>
    deltaFromBaseline(sample.distanceFromMaxScrollPx, baseline.distanceFromMaxScrollPx),
  );
  const firstItemTopDeltas = normalizedSamples.map((sample) =>
    deltaFromBaseline(sample.firstItemTopPx, baseline.firstItemTopPx),
  );
  const lastItemTopDeltas = normalizedSamples.map((sample) =>
    deltaFromBaseline(sample.lastItemTopPx, baseline.lastItemTopPx),
  );
  const firstItemIds = normalizedSamples.map((sample) => sample.firstItemId);
  const renderedTopIds = normalizedSamples.map((sample) => sample.renderedTopId);
  const renderedAnchorIds = normalizedSamples.map((sample) => sample.renderedAnchorId);
  const stayedBottomLocked = normalizedSamples.every((sample) => {
    const distanceFromMax = sample.distanceFromMaxScrollPx;
    const blankTail = sample.blankTailPx;
    return (
      (distanceFromMax == null || Math.abs(distanceFromMax) <= FLASH_SETTLE_THRESHOLD_PX) &&
      (blankTail == null || Math.abs(blankTail) <= FLASH_SETTLE_THRESHOLD_PX)
    );
  });
  const scrollTopSnapbackDetected = detectSnapback(scrollTopDeltas);
  const firstItemSnapbackDetected = detectSnapback(firstItemTopDeltas);
  const distanceFromMaxSnapbackDetected = detectSnapback(distanceFromMaxDeltas);

  const trace: SessionMessageListFlashTrace = {
    seq: 0,
    sessionId,
    cause,
    startedAtMs,
    finishedAtMs,
    sampleCount: normalizedSamples.length,
    maxAbsScrollTopDeltaPx: maxAbs(scrollTopDeltas),
    maxAbsScrollHeightDeltaPx: maxAbs(scrollHeightDeltas),
    maxAbsClientHeightDeltaPx: maxAbs(clientHeightDeltas),
    maxAbsDistanceFromMaxDeltaPx: maxAbs(distanceFromMaxDeltas),
    maxAbsFirstItemTopDeltaPx: maxAbs(firstItemTopDeltas),
    maxAbsLastItemTopDeltaPx: maxAbs(lastItemTopDeltas),
    scrollDirectionChanges: countDirectionChanges(scrollTopDeltas),
    firstItemChanged: firstItemIds.some((value) => value !== baseline.firstItemId),
    firstItemReverted: didIdentifierRevert(firstItemIds, baseline.firstItemId),
    renderedTopChanged: renderedTopIds.some((value) => value !== baseline.renderedTopId),
    renderedTopReverted: didIdentifierRevert(renderedTopIds, baseline.renderedTopId),
    renderedAnchorChanged: renderedAnchorIds.some((value) => value !== baseline.renderedAnchorId),
    renderedAnchorReverted: didIdentifierRevert(renderedAnchorIds, baseline.renderedAnchorId),
    layoutShiftCount,
    layoutShiftValue: Math.round(layoutShiftValue * 1000) / 1000,
    snapbackDetected:
      distanceFromMaxSnapbackDetected ||
      (!(stayedBottomLocked && layoutShiftCount === 0) &&
        (scrollTopSnapbackDetected || firstItemSnapbackDetected)),
    detail,
    samples: normalizedSamples,
  };

  try {
    const store = getStore();
    trace.seq = store.flashSeq + 1;
    store.flashSeq = trace.seq;
    store.flashTraces.push(trace);
    if (store.flashTraces.length > MAX_SESSION_MESSAGE_LIST_FLASH_TRACES) {
      store.flashTraces.splice(0, store.flashTraces.length - MAX_SESSION_MESSAGE_LIST_FLASH_TRACES);
    }
  } catch {
    // Debug-only helper; never break the workbench for diagnostics.
  }

  return trace;
}

export function recordSessionMessageListRowSizeMismatch({
  id,
  itemKind,
  itemKey,
  reason,
  dataIndex,
  knownSize,
  actualHeight,
  parentHeight,
  knownVsActualDeltaPx,
  knownVsParentDeltaPx,
  parentVsActualDeltaPx,
}: RecordSessionMessageListRowSizeMismatchParams): SessionMessageListRowSizeMismatch | null {
  if (typeof window === "undefined") return null;

  const mismatch: SessionMessageListRowSizeMismatch = {
    seq: 0,
    atMs: Date.now(),
    id,
    itemKind,
    itemKey,
    reason,
    dataIndex,
    knownSize: roundPx(knownSize) ?? knownSize,
    actualHeight: roundPx(actualHeight) ?? actualHeight,
    parentHeight: roundPx(parentHeight) ?? parentHeight,
    knownVsActualDeltaPx: roundPx(knownVsActualDeltaPx) ?? knownVsActualDeltaPx,
    knownVsParentDeltaPx: roundPx(knownVsParentDeltaPx) ?? knownVsParentDeltaPx,
    parentVsActualDeltaPx: roundPx(parentVsActualDeltaPx) ?? parentVsActualDeltaPx,
  };

  try {
    const store = getStore();
    mismatch.seq = store.rowSizeMismatchSeq + 1;
    store.rowSizeMismatchSeq = mismatch.seq;
    store.rowSizeMismatches.push(mismatch);
    if (store.rowSizeMismatches.length > MAX_SESSION_MESSAGE_LIST_ROW_SIZE_MISMATCHES) {
      store.rowSizeMismatches.splice(0, store.rowSizeMismatches.length - MAX_SESSION_MESSAGE_LIST_ROW_SIZE_MISMATCHES);
    }
  } catch {
    // Debug-only helper; never break the workbench for diagnostics.
  }

  return mismatch;
}

export function recordSessionMessageListDebugSnapshot({
  sessionId,
  cause,
  scroller,
  isActive,
  loaded,
  listCount,
  stickToBottom,
  renderedAnchorId,
  renderedTopId,
  detail = null,
}: RecordSessionMessageListDebugSnapshotParams): void {
  if (typeof window === "undefined") return;

  const metrics = measureSessionMessageListState(scroller);

  const entry: SessionMessageListDebugEntry = {
    seq: 0,
    atMs: Date.now(),
    sessionId,
    cause,
    isActive,
    loaded,
    listCount,
    stickToBottom,
    renderedAnchorId,
    renderedTopId,
    firstItemId: metrics.firstItemId,
    firstItemTopPx: metrics.firstItemTopPx,
    firstItemBottomPx: metrics.firstItemBottomPx,
    scrollTop: metrics.scrollTop,
    clientHeight: metrics.clientHeight,
    scrollHeight: metrics.scrollHeight,
    maxScrollTop: metrics.maxScrollTop,
    distanceFromMaxScrollPx: metrics.distanceFromMaxScrollPx,
    blankTailPx: metrics.blankTailPx,
    lastItemId: metrics.lastItemId,
    lastItemTopPx: metrics.lastItemTopPx,
    lastItemBottomPx: metrics.lastItemBottomPx,
    lastItemOffscreenAbove: metrics.lastItemOffscreenAbove,
    renderedItemCount: metrics.renderedItemCount,
    sessionViewHeight: metrics.sessionViewHeight,
    sessionBottomHeight: metrics.sessionBottomHeight,
    composerHeight: metrics.composerHeight,
    queueHeight: metrics.queueHeight,
    impossibleTail: metrics.impossibleTail,
    detail,
  };

  try {
    const store = getStore();
    entry.seq = store.seq + 1;
    store.seq = entry.seq;
    store.entries.push(entry);
    if (store.entries.length > MAX_SESSION_MESSAGE_LIST_DEBUG_ENTRIES) {
      store.entries.splice(0, store.entries.length - MAX_SESSION_MESSAGE_LIST_DEBUG_ENTRIES);
    }
  } catch {
    // Debug-only helper; never break the workbench for diagnostics.
  }
}
