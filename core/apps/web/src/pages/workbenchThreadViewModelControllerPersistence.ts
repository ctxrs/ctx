import type { AssistantStreamingState } from "../state/assistantStreaming";
import {
  buildWorkbenchThreadViewModelLayoutKey,
  buildWorkbenchThreadViewModelSourceKey,
  buildWorkbenchThreadViewModelWarmKey,
  persistWarmWorkbenchThreadViewModel,
  type WorkbenchThreadViewModelPerTurnCaches,
} from "./workbenchThreadViewModelWarmCache";
import type {
  WorkbenchThreadControllerParams,
  WorkbenchThreadInternalState,
} from "./workbenchThreadViewModelControllerUtils";

type PersistWorkbenchThreadControllerSnapshotArgs = Omit<
  WorkbenchThreadControllerParams,
  "assistantStreamingByTurnId" | "projectionRev"
> & {
  projectionRev: number;
  assistantStreamingByTurnId: Record<string, AssistantStreamingState>;
  nextState: WorkbenchThreadInternalState;
  caches: WorkbenchThreadViewModelPerTurnCaches;
};

export function persistWorkbenchThreadControllerSnapshot({
  sessionId,
  projectionRev,
  turnsStamp,
  assistantStreamingStamp,
  messagesStamp,
  eventsStamp,
  verbosity,
  turns,
  assistantStreamingByTurnId,
  messages,
  events,
  toolsByTurnId,
  toolSummariesReady,
  askUserQuestionAnswers,
  enableDebugEvents,
  nextState,
  caches,
}: PersistWorkbenchThreadControllerSnapshotArgs) {
  const sourceKey = buildWorkbenchThreadViewModelSourceKey({
    sessionId,
    projectionRev,
    turnsStamp,
    assistantStreamingStamp,
    messagesStamp,
    eventsStamp,
    verbosity,
    turns,
    assistantStreamingByTurnId,
    messages,
    events,
    toolsByTurnId,
    toolSummariesReady,
    askUserQuestionAnswers,
    enableDebugEvents,
  });
  const layoutKey = buildWorkbenchThreadViewModelLayoutKey({ verbosity });
  persistWarmWorkbenchThreadViewModel(sessionId, {
    sourceKey,
    layoutKey,
    warmKey: buildWorkbenchThreadViewModelWarmKey({
      sessionId,
      projectionRev,
      turnsStamp,
      assistantStreamingStamp,
      messagesStamp,
      eventsStamp,
      verbosity,
      turns,
      assistantStreamingByTurnId,
      messages,
      events,
      toolsByTurnId,
      toolSummariesReady,
      askUserQuestionAnswers,
      enableDebugEvents,
    }),
    projectionRevision: nextState.projectionRevision,
    view: nextState.view,
    listItems: nextState.listItems,
    groupRanges: nextState.groupRanges,
    turnsLen: nextState.turnsLen,
    messagesLen: nextState.messagesLen,
    eventsLen: nextState.eventsLen,
    caches,
  });
}
