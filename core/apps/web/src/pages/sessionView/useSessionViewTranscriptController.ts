import { useCallback, useEffect, useMemo, useState, type CSSProperties, type Dispatch, type SetStateAction } from "react";
import { submitAskUserQuestion, type SessionEvent } from "../../api/client";
import {
  loadSessionViewPrefsV1,
  saveSessionViewPrefsV1,
  type SessionViewVerbosity,
} from "../../state/uiStateStore";
import type { SessionCacheEntry, SessionSupervisor } from "../../state/sessionSupervisor";
import { defaultSessionVerbosityForProvider } from "../sessionVerbosity";
import { type WorkbenchMessageListUiState } from "../sessionMessageListItemIdentity";
import { useSessionTranscriptController } from "../useSessionTranscriptController";
import { useWorkbenchThreadViewModelController } from "../useWorkbenchThreadViewModelController";
import { buildWorkbenchThreadViewModelWarmKey } from "../workbenchThreadViewModelWarmCache";
import { recordSessionThreadProjectionDebugEntry } from "../sessionThreadProjectionDebug";
import { noteSessionTranscriptWarmVerbosity } from "../sessionThread/sessionTranscriptWarmState";
import {
  noteFinalVisible,
  noteSessionSwitchFirstPaint,
} from "../../state/foregroundFreshnessTelemetry";
import { noteVisibleSessionSwitchSettled } from "../../state/visibleSessionSwitchState";
import { getLoadTestTelemetry } from "../../utils/loadTestTelemetry";
import { useStableAskUserQuestionAnswers } from "./useStableAskUserQuestionAnswers";
import { useSessionViewDebugBridge } from "./useSessionViewDebugBridge";
import type { AskUserQuestionAnswerState } from "./SessionPage.types";
import type { SessionThreadSurfaceTranscriptProps } from "./SessionThreadSurface";
import { deriveSessionError } from "../workbenchViewModel/SessionPage.workbenchViewModel";
import { selectSessionThreadProjection } from "../../state/sessionThreadProjection/selectors";
import { shouldMarkEmptySessionSwitchRendered } from "./sessionViewVisibleSwitch";

type ThreadProjection = ReturnType<typeof selectSessionThreadProjection>;

type Params = {
  sessionId: string;
  entry: SessionCacheEntry | null;
  threadProjection: ThreadProjection;
  supervisor: SessionSupervisor;
  isActive: boolean;
  showDebug: boolean;
  perfEnabled: boolean;
  listStyle: CSSProperties;
  increaseViewportBy: number;
  licenseKey: string;
};

type Result = {
  sessionError: ReturnType<typeof deriveSessionError>;
  fileOpenError: string | null;
  handleFileOpenError: (message: string | null) => void;
  verbosity: SessionViewVerbosity;
  setVerbosityPref: (next: SessionViewVerbosity) => void;
  atBottom: boolean;
  setAtBottom: Dispatch<SetStateAction<boolean>>;
  events: SessionEvent[];
  messages: ThreadProjection["messages"];
  debugEvents: SessionEvent[];
  threadListSourceKey: string;
  transcript: SessionThreadSurfaceTranscriptProps;
};

function findActiveAskToolCallId(
  events: SessionEvent[],
  optimisticAskAnswers: Record<string, AskUserQuestionAnswerState>,
): string | null {
  const answered = new Set<string>();
  for (const event of events) {
    if (event.event_type !== "notice") continue;
    if (event.payload_json?.kind !== "ask_user_question_answered") continue;
    const toolCallId = String(event.payload_json?.tool_call_id ?? "").trim();
    if (toolCallId) answered.add(toolCallId);
  }
  for (const toolCallId of Object.keys(optimisticAskAnswers)) {
    if (toolCallId) answered.add(toolCallId);
  }
  for (let index = events.length - 1; index >= 0; index -= 1) {
    const event = events[index];
    if (event?.event_type !== "notice") continue;
    if (event.payload_json?.kind !== "ask_user_question") continue;
    const toolCallId = String(event.payload_json?.tool_call_id ?? "").trim();
    if (!toolCallId || answered.has(toolCallId)) continue;
    return toolCallId;
  }
  return null;
}

export function useSessionViewTranscriptController(params: Params): Result {
  const {
    sessionId,
    entry,
    threadProjection,
    supervisor,
    isActive,
    showDebug,
    perfEnabled,
    listStyle,
    increaseViewportBy,
    licenseKey,
  } = params;
  const [verbosity, setVerbosity] = useState<SessionViewVerbosity>("default");
  const [fileOpenError, setFileOpenError] = useState<string | null>(null);
  const [atBottom, setAtBottom] = useState(true);
  const [optimisticAskAnswers, setOptimisticAskAnswers] = useState<Record<string, AskUserQuestionAnswerState>>({});
  const [expandedTurnHeaders, setExpandedTurnHeaders] = useState<Record<string, boolean>>({});
  const [expandedTurnDetailsById, setExpandedTurnDetailsById] = useState<Record<string, boolean>>({});
  const [expandedToolById, setExpandedToolById] = useState<Record<string, boolean>>({});
  const [expandedMessageById, setExpandedMessageById] = useState<Record<string, boolean>>({});

  useEffect(() => {
    setFileOpenError(null);
    setAtBottom(true);
    setOptimisticAskAnswers({});
    setExpandedTurnHeaders({});
    setExpandedTurnDetailsById({});
    setExpandedToolById({});
    setExpandedMessageById({});
  }, [sessionId]);

  useEffect(() => {
    let cancelled = false;
    loadSessionViewPrefsV1()
      .then((prefs) => {
        if (cancelled) return;
        if (prefs?.verbosity) {
          setVerbosity(prefs.verbosity);
          return;
        }
        setVerbosity(defaultSessionVerbosityForProvider(entry?.session?.provider_id));
      })
      .catch(() => {
        if (cancelled) return;
        setVerbosity(defaultSessionVerbosityForProvider(entry?.session?.provider_id));
      });
    return () => {
      cancelled = true;
    };
  }, [entry?.session?.provider_id, sessionId]);

  useEffect(() => {
    noteSessionTranscriptWarmVerbosity(verbosity);
  }, [verbosity]);

  const setVerbosityPref = useCallback((next: SessionViewVerbosity) => {
    setVerbosity(next);
    saveSessionViewPrefsV1(next).catch(() => {});
  }, []);

  const handleFileOpenError = useCallback((message: string | null) => {
    setFileOpenError(message);
  }, []);

  const askUserQuestionAnswers = useStableAskUserQuestionAnswers({
    events: threadProjection.events,
    optimisticAskAnswers,
    eventsStamp: threadProjection.eventsStamp,
  });
  const activeAskToolCallId = useMemo(
    () => findActiveAskToolCallId(threadProjection.events, optimisticAskAnswers),
    [optimisticAskAnswers, threadProjection.events],
  );
  const sessionError = useMemo(
    () => deriveSessionError(threadProjection.turns, threadProjection.events),
    [threadProjection.events, threadProjection.turns],
  );
  const {
    view: workbenchThreadView,
    listItems,
    projectionRevision,
    lastOp: rawWorkbenchThreadOp,
  } = useWorkbenchThreadViewModelController({
    sessionId,
    projectionRev: threadProjection.projectionRev,
    turnsStamp: threadProjection.turnsStamp,
    assistantStreamingStamp: threadProjection.assistantStreamingStamp,
    messagesStamp: threadProjection.messagesStamp,
    eventsStamp: threadProjection.eventsStamp,
    verbosity,
    turns: threadProjection.turns,
    assistantStreamingByTurnId: threadProjection.assistantStreamingByTurnId,
    messages: threadProjection.messages,
    events: threadProjection.events,
    toolsByTurnId: threadProjection.toolsByTurnId,
    toolSummariesReady: threadProjection.toolSummariesReady,
    askUserQuestionAnswers,
    enableDebugEvents: showDebug,
  });
  const threadListSourceKey = useMemo(
    () =>
      buildWorkbenchThreadViewModelWarmKey({
        sessionId,
        projectionRev: threadProjection.projectionRev,
        turnsStamp: threadProjection.turnsStamp,
        assistantStreamingStamp: threadProjection.assistantStreamingStamp,
        messagesStamp: threadProjection.messagesStamp,
        eventsStamp: threadProjection.eventsStamp,
        verbosity,
        turns: threadProjection.turns,
        assistantStreamingByTurnId: threadProjection.assistantStreamingByTurnId,
        messages: threadProjection.messages,
        events: threadProjection.events,
        toolsByTurnId: threadProjection.toolsByTurnId,
        toolSummariesReady: threadProjection.toolSummariesReady,
        askUserQuestionAnswers,
        enableDebugEvents: showDebug,
      }),
    [
      askUserQuestionAnswers,
      sessionId,
      showDebug,
      threadProjection.assistantStreamingByTurnId,
      threadProjection.assistantStreamingStamp,
      threadProjection.events,
      threadProjection.eventsStamp,
      threadProjection.messages,
      threadProjection.messagesStamp,
      threadProjection.projectionRev,
      threadProjection.toolSummariesReady,
      threadProjection.toolsByTurnId,
      threadProjection.turns,
      threadProjection.turnsStamp,
      verbosity,
    ],
  );
  const messageListUiState = useMemo<WorkbenchMessageListUiState>(
    () => ({
      expandedTurnHeaders,
      expandedTurnDetailsById,
      expandedToolById,
      expandedMessageById,
      turnToolsLoading: entry?.turnToolsLoading ?? [],
      verbosity,
    }),
    [
      entry?.turnToolsLoading,
      expandedMessageById,
      expandedToolById,
      expandedTurnDetailsById,
      expandedTurnHeaders,
      verbosity,
    ],
  );
  const markLoadTestSwitchRendered = useCallback(() => {
    noteSessionSwitchFirstPaint(sessionId);
    const loadTestTelemetry = getLoadTestTelemetry();
    loadTestTelemetry?.markVisibleSessionSwitchVisible(sessionId);
    if (loadTestTelemetry?.enabled && typeof requestAnimationFrame === "function") {
      requestAnimationFrame(() => {
        requestAnimationFrame(() => {
          loadTestTelemetry.markVisibleSessionSwitchStable(sessionId);
          noteVisibleSessionSwitchSettled(sessionId);
        });
      });
    } else {
      loadTestTelemetry?.markVisibleSessionSwitchStable(sessionId);
      noteVisibleSessionSwitchSettled(sessionId);
    }
  }, [sessionId]);
  const handleInitialTranscriptRendered = useCallback(() => {
    if (!isActive) return;
    markLoadTestSwitchRendered();
  }, [isActive, markLoadTestSwitchRendered]);

  useEffect(() => {
    if (!isActive || !entry?.stateLoaded) return;
    if (shouldMarkEmptySessionSwitchRendered({
      isActive,
      stateLoaded: Boolean(entry?.stateLoaded),
      listItemCount: listItems.length,
    })) {
      markLoadTestSwitchRendered();
    }
  }, [entry?.stateLoaded, isActive, listItems.length, markLoadTestSwitchRendered]);
  const {
    threadProjectionOp,
    itemIdentity,
    itemKey,
    methodsRef,
    context,
    initialData,
    initialLocation,
    onScroll,
    onRenderedDataChange,
  } = useSessionTranscriptController({
    sessionId,
    isActive,
    loaded: Boolean(entry?.stateLoaded),
    listItems,
    canLoadOlder: Boolean(sessionId && entry?.hasMoreTurns),
    loadOlder: async () => {
      if (!sessionId) return;
      await supervisor.loadMoreTurns(sessionId);
    },
    showDebug,
    onAtBottomChange: setAtBottom,
    onInitialContentRendered: handleInitialTranscriptRendered,
    uiState: messageListUiState,
    workbenchThreadOp: rawWorkbenchThreadOp,
    projectionRevision,
  });
  useSessionViewDebugBridge({
    sessionId,
    entry,
    listItems,
    threadProjection,
    workbenchThreadOp: rawWorkbenchThreadOp,
    threadProjectionOp,
    perfEnabled,
  });

  const sessionProjectionReady =
    entry?.loadState === "live" &&
    threadProjection.toolSummariesReady &&
    ["authoritative", "replica"].includes(String(entry?.freshness ?? ""));
  const terminalTurnIds = useMemo(
    () =>
      threadProjection.turns
        .filter((turn) =>
          turn.status === "completed" || turn.status === "interrupted" || turn.status === "failed",
        )
        .map((turn) => turn.turn_id),
    [threadProjection.turns],
  );

  useEffect(() => {
    if (!sessionProjectionReady || terminalTurnIds.length === 0) return;
    noteFinalVisible(sessionId, terminalTurnIds);
  }, [sessionId, sessionProjectionReady, terminalTurnIds, threadProjection.turnsStamp]);

  useEffect(() => {
    if (!showDebug || typeof window === "undefined") return;
    recordSessionThreadProjectionDebugEntry({
      sessionId,
      source: "supervisor",
      loaded: Boolean(threadProjection.loaded),
      sessionProjectionReady,
      freshness: entry?.freshness ?? null,
      loadState: entry?.loadState ?? null,
      lastTurnStatus: entry?.activity?.last_turn_status ?? null,
      turnsStamp: threadProjection.turnsStamp,
      messagesStamp: threadProjection.messagesStamp,
      eventsStamp: threadProjection.eventsStamp,
      projectionRev: threadProjection.projectionRev,
      opKind: threadProjectionOp.kind,
      listItemCount: listItems.length,
    });
  }, [
    entry?.activity?.last_turn_status,
    entry?.freshness,
    entry?.loadState,
    listItems.length,
    sessionId,
    sessionProjectionReady,
    showDebug,
    threadProjection.eventsStamp,
    threadProjection.loaded,
    threadProjection.messagesStamp,
    threadProjection.projectionRev,
    threadProjection.turnsStamp,
    threadProjectionOp.kind,
  ]);

  const onCancelAskUserQuestion = useCallback(
    async (toolCallId: string) => {
      await submitAskUserQuestion(sessionId, toolCallId, "cancelled", {});
      setOptimisticAskAnswers((prev) => ({
        ...prev,
        [toolCallId]: { outcome: "cancelled", answers: {} },
      }));
    },
    [sessionId],
  );
  const onSubmitAskUserQuestion = useCallback(
    async (toolCallId: string, answers: Record<string, string>) => {
      await submitAskUserQuestion(sessionId, toolCallId, "submitted", answers);
      setOptimisticAskAnswers((prev) => ({
        ...prev,
        [toolCallId]: { outcome: "submitted", answers },
      }));
    },
    [sessionId],
  );

  return {
    sessionError,
    fileOpenError,
    handleFileOpenError,
    verbosity,
    setVerbosityPref,
    atBottom,
    setAtBottom,
    events: threadProjection.events,
    messages: threadProjection.messages,
    debugEvents: workbenchThreadView.debugEvents,
    threadListSourceKey,
    transcript: {
      listItems,
      sourceKey: threadListSourceKey,
      liveTailItems: [],
      activeAskToolCallId,
      expandedTurnHeaders,
      setExpandedTurnHeaders,
      expandedTurnDetailsById,
      setExpandedTurnDetailsById,
      expandedToolById,
      setExpandedToolById,
      expandedMessageById,
      setExpandedMessageById,
      turnToolsLoading: entry?.turnToolsLoading ?? [],
      verbosity,
      onCancelAskUserQuestion,
      onSubmitAskUserQuestion,
      onRequestTurnTools: (turnId) => {
        supervisor.loadTurnTools(sessionId, turnId);
      },
      isActive,
      listStyle,
      itemIdentity,
      itemKey,
      increaseViewportBy,
      initialData,
      initialLocation,
      threadProjectionOp,
      context,
      onScroll,
      onRenderedDataChange,
      methodsRef,
      licenseKey,
      shortSizeAlign: atBottom ? "bottom" : "top",
    },
  };
}
