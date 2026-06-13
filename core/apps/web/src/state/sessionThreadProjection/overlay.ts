import { idToString, type Message } from "../../api/client";
import {
  buildPendingTurns,
  filterQueuedMessagesForPanel,
  filterTurnsForQueuedMessages,
  mergeMessagesForView,
  mergeQueuedMessagesForPanel,
} from "../../pages/workbenchViewModel/messageMerge";
import type { SessionCacheEntry } from "../sessionSupervisor/entryState";
import {
  buildAssistantStreamingStamp,
  buildMessagesStamp,
  buildTurnsStamp,
} from "./stamps";
import type { SessionThreadProjection } from "./types";

type SessionThreadProjectionOverlaySource = Pick<
  SessionCacheEntry,
  | "optimisticThreadMessages"
  | "optimisticQueuedMessages"
  | "optimisticQueueRemovalIds"
  | "overlayRev"
  | "queue"
>;

function deriveQueuedMessageIdsToShow(baseProjection: SessionThreadProjection): Set<string> {
  const turnStatusByUserMessageId = new Map<string, string>();
  for (const turn of baseProjection.turns) {
    const messageId = turn.user_message_id ? idToString(turn.user_message_id) : "";
    if (!messageId) continue;
    turnStatusByUserMessageId.set(messageId, String(turn.status));
  }

  const ids = new Set<string>();
  for (const message of baseProjection.messages) {
    if (message.delivery !== "queued") continue;
    const messageId = idToString(message.id);
    if (!messageId) continue;
    const status = turnStatusByUserMessageId.get(messageId);
    if (status && status !== "queued") {
      ids.add(messageId);
    }
  }
  return ids;
}

export function applySessionThreadProjectionOverlay(
  baseProjection: SessionThreadProjection,
  source: SessionThreadProjectionOverlaySource,
): SessionThreadProjection {
  const optimisticThreadMessages = source.optimisticThreadMessages ?? [];
  const optimisticQueueMessages = source.optimisticQueuedMessages ?? [];
  const optimisticQueueRemovalIds = source.optimisticQueueRemovalIds ?? [];
  const overlayRev = source.overlayRev ?? 0;

  if (overlayRev === 0) return baseProjection;

  const queuedMessageIdsToShow = deriveQueuedMessageIdsToShow(baseProjection);
  const mergedMessages = mergeMessagesForView(
    baseProjection.messages,
    optimisticThreadMessages,
    queuedMessageIdsToShow,
  );
  const pendingTurns = buildPendingTurns(baseProjection.turns, mergedMessages);
  const turnsWithPending =
    pendingTurns.length > 0 ? [...baseProjection.turns, ...pendingTurns] : baseProjection.turns;

  const mergedQueue = mergeQueuedMessagesForPanel(source.queue ?? [], optimisticQueueMessages);
  const filteredQueue = filterQueuedMessagesForPanel(mergedQueue, baseProjection.turns);
  const hiddenQueuedIds = new Set(optimisticQueueRemovalIds);
  const queueForThread =
    hiddenQueuedIds.size === 0
      ? filteredQueue
      : filteredQueue.filter((message) => {
          const messageId = idToString(message.id);
          return !messageId || !hiddenQueuedIds.has(messageId);
        });

  const queuedMessageIdsForThread = new Set<string>();
  for (const message of queueForThread) {
    const messageId = idToString(message.id);
    if (messageId) queuedMessageIdsForThread.add(messageId);
  }
  for (const messageId of optimisticQueueRemovalIds) {
    if (messageId) queuedMessageIdsForThread.add(messageId);
  }

  const turnsForThread = filterTurnsForQueuedMessages(turnsWithPending, queuedMessageIdsForThread);
  const assistantStreamingStamp =
    baseProjection.assistantStreamingStamp ||
    buildAssistantStreamingStamp(baseProjection.assistantStreamingByTurnId, 0);
  return {
    ...baseProjection,
    turns: turnsForThread,
    turnsStamp: buildTurnsStamp(turnsForThread, baseProjection.projectionRev + overlayRev),
    assistantStreamingByTurnId: baseProjection.assistantStreamingByTurnId,
    assistantStreamingStamp,
    messages: mergedMessages,
    messagesStamp: buildMessagesStamp(mergedMessages, baseProjection.projectionRev + overlayRev),
    projectionRev: baseProjection.projectionRev + overlayRev,
  };
}

export function selectSessionQueuePanelMessages(
  entry: SessionThreadProjectionOverlaySource | null | undefined,
  baseTurns: SessionThreadProjection["turns"],
): Message[] {
  if (!entry) return [];
  const mergedQueue = mergeQueuedMessagesForPanel(entry.queue ?? [], entry.optimisticQueuedMessages ?? []);
  const filteredQueue = filterQueuedMessagesForPanel(mergedQueue, baseTurns);
  const hiddenQueuedIds = new Set(entry.optimisticQueueRemovalIds ?? []);
  if (hiddenQueuedIds.size === 0) return filteredQueue;
  return filteredQueue.filter((message) => {
    const messageId = idToString(message.id);
    return !messageId || !hiddenQueuedIds.has(messageId);
  });
}
