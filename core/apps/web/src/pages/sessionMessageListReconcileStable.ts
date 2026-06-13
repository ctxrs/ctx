import type { MutableRefObject } from "react";
import type { WorkbenchListItem } from "./SessionPage.types";
import { applyStableListUpdate } from "./sessionMessageListStableUpdate";
import type {
  MessageListMethods,
  SessionMessageListReconcileParams as Params,
} from "./sessionMessageListReconcileTypes";

type PureAppendArgs = Pick<
  Params,
  | "sessionId"
  | "stickToBottomRef"
  | "renderedAnchorIdRef"
  | "appendBehavior"
  | "snapToBottom"
  | "recordDebugSnapshot"
  | "logMessageListDebug"
> & {
  methods: MessageListMethods;
  current: WorkbenchListItem[];
  next: WorkbenchListItem[];
  currentLen: number;
  effectiveNextLen: number;
};

export function tryApplyPureAppendUpdate({
  methods,
  current,
  next,
  currentLen,
  effectiveNextLen,
  stickToBottomRef,
  renderedAnchorIdRef,
  appendBehavior,
  snapToBottom,
  recordDebugSnapshot,
  logMessageListDebug,
}: PureAppendArgs): boolean {
  if (effectiveNextLen <= currentLen) return false;
  for (let i = 0; i < currentLen; i += 1) {
    if (next[i]?.id !== current[i]?.id) {
      return false;
    }
  }

  const suffix = next.slice(currentLen);
  const anchorId = renderedAnchorIdRef.current;
  const anchorIndex = anchorId ? next.findIndex((it) => it.id === anchorId) : -1;
  const updateResult = applyStableListUpdate({
    methods,
    current,
    next: next.slice(0, currentLen),
    suffix,
    stickToBottom: stickToBottomRef.current,
    anchorIndex,
    appendBehavior,
  });
  if (stickToBottomRef.current && updateResult.mode === "remeasure") {
    snapToBottom(methods);
  }
  recordDebugSnapshot("data:append", {
    suffixLen: suffix.length,
    nextLen: effectiveNextLen,
    currentLen,
    changedSpans: updateResult.changedSpans,
  });
  logMessageListDebug("data:append", {
    suffixLen: suffix.length,
    nextLen: effectiveNextLen,
    currentLen,
    stickToBottom: stickToBottomRef.current,
    anchorId,
    changedSpans: updateResult.changedSpans,
  });
  return true;
}

type SameLengthArgs = Pick<
  Params,
  | "sessionId"
  | "showDebug"
  | "stickToBottomRef"
  | "renderedAnchorIdRef"
  | "renderedTopIdRef"
  | "appendBehavior"
  | "snapToBottom"
  | "recordDebugSnapshot"
  | "startFlashProbe"
  | "logMessageListDebug"
> & {
  methods: MessageListMethods;
  current: WorkbenchListItem[];
  next: WorkbenchListItem[];
  currentLen: number;
  effectiveNextLen: number;
  hasLocalizedThreadOp: boolean;
  threadOp: Params["threadOp"];
};

export function tryApplySameLengthUpdate({
  sessionId,
  showDebug,
  methods,
  current,
  next,
  currentLen,
  effectiveNextLen,
  stickToBottomRef,
  renderedAnchorIdRef,
  renderedTopIdRef,
  appendBehavior,
  snapToBottom,
  recordDebugSnapshot,
  startFlashProbe,
  logMessageListDebug,
  hasLocalizedThreadOp,
  threadOp,
}: SameLengthArgs): boolean {
  if (effectiveNextLen !== currentLen) return false;
  for (let i = 0; i < currentLen; i += 1) {
    if (next[i]?.id !== current[i]?.id) {
      return false;
    }
  }

  const anchorId = renderedAnchorIdRef.current;
  const anchorIndex = anchorId ? next.findIndex((it) => it.id === anchorId) : -1;
  const updateResult = applyStableListUpdate({
    methods,
    current,
    next,
    stickToBottom: stickToBottomRef.current,
    anchorIndex,
    appendBehavior,
    forceRemeasureItemIds: hasLocalizedThreadOp ? (threadOp?.remeasureItemIds ?? []) : [],
  });
  if (stickToBottomRef.current && updateResult.mode === "remeasure") {
    snapToBottom(methods);
  }
  const updateLabel = updateResult.mode === "remeasure" ? "data:remeasure" : "data:map";
  if (updateResult.mode === "remeasure") {
    startFlashProbe("data:remeasure", {
      nextLen: effectiveNextLen,
      currentLen,
      anchorId,
      anchorIndex,
      stickToBottom: stickToBottomRef.current,
      changedSpans: updateResult.changedSpans,
    });
  }
  logSameLengthDevDebug({
    sessionId,
    showDebug,
    current,
    next,
    currentLen,
    effectiveNextLen,
    stickToBottomRef,
    renderedTopIdRef,
    anchorId,
    anchorIndex,
    updateLabel,
    changedSpans: updateResult.changedSpans,
    updateMode: updateResult.mode,
  });
  recordDebugSnapshot(updateLabel, {
    nextLen: effectiveNextLen,
    currentLen,
    anchorId,
    anchorIndex,
    stickToBottom: stickToBottomRef.current,
    changedSpans: updateResult.changedSpans,
  });
  logMessageListDebug(updateLabel, {
    nextLen: effectiveNextLen,
    currentLen,
    anchorId,
    anchorIndex,
    stickToBottom: stickToBottomRef.current,
    changedSpans: updateResult.changedSpans,
  });
  return true;
}

function logSameLengthDevDebug({
  sessionId,
  showDebug,
  current,
  next,
  currentLen,
  effectiveNextLen,
  stickToBottomRef,
  renderedTopIdRef,
  anchorId,
  anchorIndex,
  updateLabel,
  changedSpans,
  updateMode,
}: {
  sessionId: string;
  showDebug: boolean;
  current: WorkbenchListItem[];
  next: WorkbenchListItem[];
  currentLen: number;
  effectiveNextLen: number;
  stickToBottomRef: MutableRefObject<boolean>;
  renderedTopIdRef: MutableRefObject<string | null>;
  anchorId: string | null;
  anchorIndex: number;
  updateLabel: string;
  changedSpans: unknown;
  updateMode: "map" | "remeasure";
}) {
  if (!import.meta.env.DEV || !showDebug) return;
  let changedByRef = 0;
  const sampleChangedIds: string[] = [];
  for (let i = 0; i < currentLen; i += 1) {
    if (current[i] !== next[i]) {
      changedByRef += 1;
      if (sampleChangedIds.length < 8) {
        sampleChangedIds.push(String(next[i]?.id ?? current[i]?.id ?? ""));
      }
    }
  }
  const mapMode =
    updateMode === "remeasure"
      ? "batch:remeasure"
      : !stickToBottomRef.current && anchorIndex >= 0
        ? "mapWithAnchor"
        : stickToBottomRef.current
          ? "map:auto"
          : "map";
  // eslint-disable-next-line no-console
  console.debug(`[MessageList][${updateLabel}]`, {
    sessionId,
    nextLen: effectiveNextLen,
    currentLen,
    stickToBottom: stickToBottomRef.current,
    anchorId,
    anchorIndex,
    mapMode,
    changedByRef,
    sampleChangedIds,
    changedSpans,
    renderedTopId: renderedTopIdRef.current,
  });
}
