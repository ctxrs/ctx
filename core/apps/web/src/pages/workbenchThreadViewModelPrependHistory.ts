import type { Message, SessionEvent, SessionTurn, SessionTurnTool } from "../api/client";
import { idToString } from "../api/client";
import type { AssistantStreamingState } from "../state/assistantStreaming";
import type { SessionViewVerbosity } from "../state/uiStateStore";
import type { AskUserQuestionAnswerState } from "./SessionPage.types";
import { buildWorkbenchThreadViewModelFromTurns } from "./SessionPage.workbenchViewModel";
import { classifyWorkbenchThreadProjectionOp } from "./sessionThreadProjection";
import type { WorkbenchThreadViewModelPerTurnCaches } from "./workbenchThreadViewModelWarmCache";
import {
  buildSubsetAssistantStreamingMap,
  buildSubsetToolMap,
  flattenWorkbenchGroups,
  getTurnGroupKey,
  hasExactStableSuffixById,
  type WorkbenchThreadInternalState,
} from "./workbenchThreadViewModelControllerUtils";

type BuildPrependedHistoryUpdateArgs = {
  eventsStampChanged: boolean;
  previousTurns: SessionTurn[];
  previousMessages: Message[];
  turns: SessionTurn[];
  messages: Message[];
  events: SessionEvent[];
  state: WorkbenchThreadInternalState;
  toolSummariesReady: boolean;
  toolsByTurnId: Record<string, SessionTurnTool[]>;
  assistantStreamingByTurnId: Record<string, AssistantStreamingState>;
  askUserQuestionAnswers: Map<string, AskUserQuestionAnswerState>;
  verbosity: SessionViewVerbosity;
  projectionRev: number;
  perTurnCaches: WorkbenchThreadViewModelPerTurnCaches;
};

type PrependedHistoryUpdate = {
  state: WorkbenchThreadInternalState;
  caches: WorkbenchThreadViewModelPerTurnCaches;
};

export function buildPrependedHistoryUpdate({
  eventsStampChanged,
  previousTurns,
  previousMessages,
  turns,
  messages,
  events,
  state,
  toolSummariesReady,
  toolsByTurnId,
  assistantStreamingByTurnId,
  askUserQuestionAnswers,
  verbosity,
  projectionRev,
  perTurnCaches,
}: BuildPrependedHistoryUpdateArgs): PrependedHistoryUpdate | null {
  if (eventsStampChanged || events.length !== state.eventsLen) {
    return null;
  }
  if (turns.length <= previousTurns.length || messages.length < previousMessages.length) {
    return null;
  }
  if (
    !hasExactStableSuffixById(previousTurns, turns, (turn) => idToString(turn.turn_id)) ||
    !hasExactStableSuffixById(previousMessages, messages, (message) => idToString(message.id))
  ) {
    return null;
  }

  const prependedTurns = turns.slice(0, turns.length - previousTurns.length);
  if (prependedTurns.length === 0) {
    return null;
  }
  const prependedMessages = messages.slice(0, messages.length - previousMessages.length);
  const prependedTurnIds = prependedTurns
    .map((turn) => idToString(turn.turn_id))
    .filter((turnId) => turnId.length > 0);
  if (prependedTurnIds.length !== prependedTurns.length) {
    return null;
  }
  const prependedTurnIdSet = new Set(prependedTurnIds);
  for (const message of prependedMessages) {
    const turnId = idToString(message.turn_id);
    if (!turnId || !prependedTurnIdSet.has(turnId)) {
      return null;
    }
  }

  const currentTurnGroupKeys = state.view.groups
    .map((group) => String(group.key ?? ""))
    .filter((groupKey) => groupKey.startsWith("turn-"));
  if (currentTurnGroupKeys.length !== previousTurns.length) {
    return null;
  }
  for (let index = 0; index < previousTurns.length; index += 1) {
    const previousTurnId = idToString(previousTurns[index]?.turn_id);
    if (!previousTurnId || currentTurnGroupKeys[index] !== getTurnGroupKey(previousTurnId)) {
      return null;
    }
  }

  const prependedEvents = events.filter((event) => {
    const turnId = idToString(event.turn_id ?? "");
    return turnId.length > 0 && prependedTurnIdSet.has(turnId);
  });
  const prependedView = buildWorkbenchThreadViewModelFromTurns(
    prependedTurns,
    prependedMessages,
    toolSummariesReady ? buildSubsetToolMap(prependedTurnIdSet, toolsByTurnId) : {},
    prependedEvents,
    buildSubsetAssistantStreamingMap(prependedTurnIdSet, assistantStreamingByTurnId),
    askUserQuestionAnswers,
  );
  const prependedFlattened = flattenWorkbenchGroups(prependedView.groups, verbosity);
  const prependedItemCount = prependedFlattened.listItems.length;
  if (prependedItemCount === 0) {
    return null;
  }

  const nextList = [...prependedFlattened.listItems, ...state.listItems];
  const nextRanges = buildPrependedRanges(prependedFlattened.groupRanges, state.groupRanges, prependedItemCount);
  const nextOp = classifyWorkbenchThreadProjectionOp({
    current: state.listItems,
    next: nextList,
    projectionRevision: projectionRev,
    fallbackKind: "prepend_history",
  });
  if (nextOp.kind !== "prepend_history") {
    return null;
  }

  const nextMessagesByTurnId = new Map(perTurnCaches.messagesByTurnId);
  const nextEventsByTurnId = new Map(perTurnCaches.eventsByTurnId);
  for (const turnId of prependedTurnIdSet) {
    nextMessagesByTurnId.set(
      turnId,
      prependedMessages.filter((message) => idToString(message.turn_id) === turnId),
    );
    if (!nextEventsByTurnId.has(turnId)) {
      nextEventsByTurnId.set(turnId, []);
    }
  }

  return {
    caches: {
      messagesByTurnId: nextMessagesByTurnId,
      eventsByTurnId: nextEventsByTurnId,
    },
    state: {
      view: {
        groups: [...prependedView.groups, ...state.view.groups],
        debugEvents: state.view.debugEvents,
      },
      listItems: nextList,
      groupRanges: nextRanges,
      projectionRevision: projectionRev,
      lastOp: nextOp,
      changedItemIds: nextOp.changedItemIds,
      remeasureItemIds: nextOp.remeasureItemIds,
      turnsLen: turns.length,
      messagesLen: messages.length,
      eventsLen: state.eventsLen,
    },
  };
}

function buildPrependedRanges(
  prependedRanges: Map<string, { start: number; end: number }>,
  currentRanges: Map<string, { start: number; end: number }>,
  prependedItemCount: number,
): Map<string, { start: number; end: number }> {
  const nextRanges = new Map<string, { start: number; end: number }>(prependedRanges);
  for (const [groupKey, range] of currentRanges.entries()) {
    nextRanges.set(groupKey, {
      start: range.start + prependedItemCount,
      end: range.end + prependedItemCount,
    });
  }
  return nextRanges;
}
