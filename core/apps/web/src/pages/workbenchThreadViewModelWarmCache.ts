import type { Message, SessionEvent, SessionTurn, SessionTurnTool } from "../api/client";
import { idToString } from "../api/client";
import type { AssistantStreamingState } from "../state/assistantStreaming";
import type { SessionViewVerbosity } from "../state/uiStateStore";
import {
  addPretextPerfBucket,
  incrementPretextPerfCounter,
} from "../utils/pretextPerfDiagnostics";
import {
  buildWorkbenchThreadViewModelFromTurns,
  filterThreadItemsForVerbosity,
} from "./SessionPage.workbenchViewModel";
import type { AskUserQuestionAnswerState, WorkbenchListItem, WorkbenchThreadView } from "./SessionPage.types";
import {
  countSessionTranscriptWarmEntries,
  persistSessionTranscriptWarmEntry,
  pruneSessionTranscriptWarmEntries,
  readSessionTranscriptWarmEntry,
  resetSessionTranscriptWarmEntries,
} from "./sessionThread/pretextSessionRuntimeCache";

export type WorkbenchThreadViewModelWarmParams = {
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
  enableDebugEvents: boolean;
};

export type WorkbenchThreadViewModelPerTurnCaches = {
  messagesByTurnId: Map<string, Message[]>;
  eventsByTurnId: Map<string, SessionEvent[]>;
};

export type WorkbenchThreadViewModelWarmSnapshot = {
  sourceKey: string;
  layoutKey: string;
  warmKey: string;
  projectionRevision: number;
  view: WorkbenchThreadView;
  listItems: WorkbenchListItem[];
  groupRanges: Map<string, { start: number; end: number }>;
  turnsLen: number;
  messagesLen: number;
  eventsLen: number;
  caches: WorkbenchThreadViewModelPerTurnCaches;
};

function getTurnGroupKey(turnId: string): string {
  return `turn-${turnId}`;
}

function buildMessagesByTurnId(messages: Message[]): Map<string, Message[]> {
  const byTurnMsg = new Map<string, Message[]>();
  for (const message of messages) {
    const turnId = idToString(message.turn_id);
    if (!turnId) continue;
    const list = byTurnMsg.get(turnId) ?? [];
    list.push(message);
    byTurnMsg.set(turnId, list);
  }
  return byTurnMsg;
}

function buildEventsByTurnId(events: SessionEvent[]): Map<string, SessionEvent[]> {
  const byTurnEv = new Map<string, SessionEvent[]>();
  for (const event of events) {
    const turnId = idToString(event.turn_id ?? "");
    if (!turnId) continue;
    const list = byTurnEv.get(turnId) ?? [];
    list.push(event);
    byTurnEv.set(turnId, list);
  }
  return byTurnEv;
}

export function buildWorkbenchThreadViewModelPerTurnCaches(
  messages: Message[],
  events: SessionEvent[],
): WorkbenchThreadViewModelPerTurnCaches {
  return {
    messagesByTurnId: buildMessagesByTurnId(messages),
    eventsByTurnId: buildEventsByTurnId(events),
  };
}

function fingerprintAskUserQuestionAnswers(
  answers: Map<string, AskUserQuestionAnswerState>,
): string {
  return Array.from(answers.entries())
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([toolCallId, state]) =>
      JSON.stringify({
        toolCallId,
        outcome: state.outcome,
        answers: Object.keys(state.answers)
          .sort()
          .map((key) => [key, state.answers[key] ?? ""] as const),
      }))
    .join("|");
}

export function buildWorkbenchThreadViewModelSourceKey(
  params: WorkbenchThreadViewModelWarmParams,
): string {
  const projectionRev = params.projectionRev ?? 0;
  const askAnswers = fingerprintAskUserQuestionAnswers(params.askUserQuestionAnswers);
  return [
    projectionRev,
    params.turnsStamp,
    params.assistantStreamingStamp,
    params.messagesStamp,
    params.eventsStamp,
    params.toolSummariesReady ? "tools:1" : "tools:0",
    params.enableDebugEvents ? "debug:1" : "debug:0",
    `ask:${askAnswers}`,
  ].join("|");
}

export function buildWorkbenchThreadViewModelLayoutKey(
  params: Pick<WorkbenchThreadViewModelWarmParams, "verbosity">,
): string {
  return `verbosity:${params.verbosity}`;
}

export function buildWorkbenchThreadViewModelWarmKey(
  params: WorkbenchThreadViewModelWarmParams,
): string {
  return [
    buildWorkbenchThreadViewModelSourceKey(params),
    buildWorkbenchThreadViewModelLayoutKey(params),
  ].join("|");
}

export function buildWorkbenchThreadViewModelWarmSnapshot(
  params: WorkbenchThreadViewModelWarmParams,
): WorkbenchThreadViewModelWarmSnapshot {
  const {
    projectionRev,
    turns,
    assistantStreamingByTurnId = {},
    messages,
    toolsByTurnId,
    toolSummariesReady,
    events,
    askUserQuestionAnswers,
    verbosity,
  } = params;

  const view = buildWorkbenchThreadViewModelFromTurns(
    turns,
    messages,
    toolSummariesReady ? toolsByTurnId : {},
    events,
    assistantStreamingByTurnId,
    askUserQuestionAnswers,
  );

  const listItems: WorkbenchListItem[] = [];
  const groupRanges = new Map<string, { start: number; end: number }>();
  for (const group of view.groups) {
    const start = listItems.length;
    if (group.header) {
      listItems.push({ kind: "turn_header", id: `turn-header-${group.header.id}`, header: group.header });
    }
    listItems.push(...filterThreadItemsForVerbosity(group.items, verbosity));
    groupRanges.set(String(group.key ?? getTurnGroupKey("")), {
      start,
      end: listItems.length,
    });
  }

  return {
    sourceKey: buildWorkbenchThreadViewModelSourceKey(params),
    layoutKey: buildWorkbenchThreadViewModelLayoutKey(params),
    warmKey: buildWorkbenchThreadViewModelWarmKey(params),
    projectionRevision: projectionRev ?? 0,
    view,
    listItems,
    groupRanges,
    turnsLen: turns.length,
    messagesLen: messages.length,
    eventsLen: events.length,
    caches: buildWorkbenchThreadViewModelPerTurnCaches(messages, events),
  };
}

export function readWarmWorkbenchThreadViewModel(
  sessionId: string,
  warmKey: string,
): WorkbenchThreadViewModelWarmSnapshot | null {
  const entry = readSessionTranscriptWarmEntry(sessionId);
  if (!entry || entry.warmKey !== warmKey) {
    incrementPretextPerfCounter("pretext_warm_viewmodel_cache_miss");
    return null;
  }
  incrementPretextPerfCounter("pretext_warm_viewmodel_cache_hit");
  addPretextPerfBucket("pretext_warm_viewmodel_session", sessionId);
  return entry.snapshot as WorkbenchThreadViewModelWarmSnapshot;
}

export function persistWarmWorkbenchThreadViewModel(
  sessionId: string,
  snapshot: WorkbenchThreadViewModelWarmSnapshot,
): void {
  persistSessionTranscriptWarmEntry(sessionId, {
    sourceKey: snapshot.sourceKey,
    layoutKey: snapshot.layoutKey,
    warmKey: snapshot.warmKey,
    snapshot,
    updatedAtMs: Date.now(),
  });
}

export function primeWarmWorkbenchThreadViewModel(
  params: WorkbenchThreadViewModelWarmParams,
): WorkbenchThreadViewModelWarmSnapshot {
  const warmKey = buildWorkbenchThreadViewModelWarmKey(params);
  const existing = readWarmWorkbenchThreadViewModel(params.sessionId, warmKey);
  if (existing) {
    return existing;
  }
  incrementPretextPerfCounter("pretext_warm_viewmodel_builds");
  const snapshot = buildWorkbenchThreadViewModelWarmSnapshot(params);
  incrementPretextPerfCounter("pretext_warm_viewmodel_built_items", snapshot.listItems.length);
  persistWarmWorkbenchThreadViewModel(params.sessionId, snapshot);
  return snapshot;
}

export function getWarmWorkbenchThreadViewModelCacheSize(): number {
  return countSessionTranscriptWarmEntries();
}

export function pruneWarmWorkbenchThreadViewModelCache(retainedSessionIds: readonly string[]): void {
  pruneSessionTranscriptWarmEntries(retainedSessionIds);
}

export function resetWarmWorkbenchThreadViewModelCache(): void {
  resetSessionTranscriptWarmEntries();
}
