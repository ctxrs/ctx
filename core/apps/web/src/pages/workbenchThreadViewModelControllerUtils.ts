import type { Message, SessionEvent, SessionTurn, SessionTurnTool } from "../api/client";
import type { AssistantStreamingState } from "../state/assistantStreaming";
import type { SessionViewVerbosity } from "../state/uiStateStore";
import type { AskUserQuestionAnswerState, WorkbenchListItem, WorkbenchThreadView } from "./SessionPage.types";
import { filterThreadItemsForVerbosity } from "./SessionPage.workbenchViewModel";
import type { WorkbenchThreadProjectionOp } from "./sessionThreadProjection";

export type WorkbenchThreadControllerParams = {
  sessionId: string;
  projectionRev?: number;
  turnsStamp: string;
  assistantStreamingStamp: string;
  messagesStamp: string;
  eventsStamp: string;
  verbosity: SessionViewVerbosity;
  turns: SessionTurn[];
  assistantStreamingByTurnId?: Record<string, AssistantStreamingState>;
  messages: Message[];
  events: SessionEvent[];
  toolsByTurnId: Record<string, SessionTurnTool[]>;
  toolSummariesReady: boolean;
  askUserQuestionAnswers: Map<string, AskUserQuestionAnswerState>;
  // If enabled, we fall back to full rebuilds to keep debugEvents accurate.
  enableDebugEvents: boolean;
};

export type WorkbenchThreadInternalState = {
  view: WorkbenchThreadView;
  listItems: WorkbenchListItem[];
  groupRanges: Map<string, { start: number; end: number }>;
  projectionRevision: number;
  lastOp: WorkbenchThreadProjectionOp;
  changedItemIds: string[];
  remeasureItemIds: string[];
  // Snapshot markers for cheap "append-only" detection.
  turnsLen: number;
  messagesLen: number;
  eventsLen: number;
};

function haveSameIds(previous: readonly string[], next: readonly string[]): boolean {
  if (previous === next) return true;
  if (previous.length !== next.length) return false;
  for (let index = 0; index < previous.length; index += 1) {
    if (previous[index] !== next[index]) return false;
  }
  return true;
}

export function areInternalStatesEquivalent(
  previous: WorkbenchThreadInternalState,
  next: WorkbenchThreadInternalState,
): boolean {
  return (
    previous === next ||
    (previous.view === next.view &&
      previous.listItems === next.listItems &&
      previous.groupRanges === next.groupRanges &&
      previous.projectionRevision === next.projectionRevision &&
      previous.lastOp.kind === next.lastOp.kind &&
      previous.lastOp.projectionRevision === next.lastOp.projectionRevision &&
      haveSameIds(previous.lastOp.changedItemIds, next.lastOp.changedItemIds) &&
      haveSameIds(previous.lastOp.remeasureItemIds, next.lastOp.remeasureItemIds) &&
      haveSameIds(previous.changedItemIds, next.changedItemIds) &&
      haveSameIds(previous.remeasureItemIds, next.remeasureItemIds) &&
      previous.turnsLen === next.turnsLen &&
      previous.messagesLen === next.messagesLen &&
      previous.eventsLen === next.eventsLen)
  );
}

export function getTurnGroupKey(turnId: string): string {
  return `turn-${turnId}`;
}

export function areToolMapsShallowEqual(
  previous: Record<string, SessionTurnTool[]>,
  next: Record<string, SessionTurnTool[]>,
): boolean {
  if (previous === next) return true;
  const previousKeys = Object.keys(previous);
  const nextKeys = Object.keys(next);
  if (previousKeys.length !== nextKeys.length) return false;
  for (const key of previousKeys) {
    if (!(key in next)) return false;
    if (previous[key] !== next[key]) return false;
  }
  return true;
}

export function collectChangedToolTurnIds(
  previous: Record<string, SessionTurnTool[]>,
  next: Record<string, SessionTurnTool[]>,
): Set<string> {
  const changed = new Set<string>();
  const keys = new Set([...Object.keys(previous), ...Object.keys(next)]);
  for (const key of keys) {
    if (previous[key] !== next[key]) {
      changed.add(key);
    }
  }
  return changed;
}

export function collectChangedAssistantStreamingTurnIds(
  previous: Record<string, AssistantStreamingState>,
  next: Record<string, AssistantStreamingState>,
): Set<string> {
  const changed = new Set<string>();
  const keys = new Set([...Object.keys(previous), ...Object.keys(next)]);
  for (const key of keys) {
    const previousState = previous[key];
    const nextState = next[key];
    if (
      previousState?.content !== nextState?.content ||
      previousState?.providerMessageId !== nextState?.providerMessageId ||
      previousState?.orderSeq !== nextState?.orderSeq
    ) {
      changed.add(key);
    }
  }
  return changed;
}

export function buildGroupSegment(
  group: WorkbenchThreadView["groups"][number],
  verbosity: SessionViewVerbosity,
): WorkbenchListItem[] {
  const segment: WorkbenchListItem[] = [];
  if (group.header) {
    segment.push({ kind: "turn_header", id: `turn-header-${group.header.id}`, header: group.header });
  }
  segment.push(...filterThreadItemsForVerbosity(group.items, verbosity));
  return segment;
}

export function flattenWorkbenchGroups(
  groups: WorkbenchThreadView["groups"],
  verbosity: SessionViewVerbosity,
): {
  listItems: WorkbenchListItem[];
  groupRanges: Map<string, { start: number; end: number }>;
} {
  const listItems: WorkbenchListItem[] = [];
  const groupRanges = new Map<string, { start: number; end: number }>();
  for (const group of groups) {
    const start = listItems.length;
    listItems.push(...buildGroupSegment(group, verbosity));
    groupRanges.set(String(group.key ?? getTurnGroupKey("")), {
      start,
      end: listItems.length,
    });
  }
  return { listItems, groupRanges };
}

export function hasExactStableSuffixById<T>(
  previous: readonly T[],
  next: readonly T[],
  getId: (value: T) => string,
): boolean {
  if (next.length < previous.length) return false;
  const offset = next.length - previous.length;
  for (let index = 0; index < previous.length; index += 1) {
    if (
      previous[index] !== next[offset + index] ||
      getId(previous[index]!) !== getId(next[offset + index]!)
    ) {
      return false;
    }
  }
  return true;
}

export function buildSubsetToolMap(
  turnIds: ReadonlySet<string>,
  toolsByTurnId: Record<string, SessionTurnTool[]>,
): Record<string, SessionTurnTool[]> {
  const subset: Record<string, SessionTurnTool[]> = {};
  for (const turnId of turnIds) {
    subset[turnId] = toolsByTurnId[turnId] ?? [];
  }
  return subset;
}

export function buildSubsetAssistantStreamingMap(
  turnIds: ReadonlySet<string>,
  assistantStreamingByTurnId: Record<string, AssistantStreamingState>,
): Record<string, AssistantStreamingState> {
  const subset: Record<string, AssistantStreamingState> = {};
  for (const turnId of turnIds) {
    const state = assistantStreamingByTurnId[turnId];
    if (state) {
      subset[turnId] = state;
    }
  }
  return subset;
}
