import { useEffect, useMemo, useSyncExternalStore } from "react";
import type { SessionSupervisorSnapshot } from "../../state/sessionSupervisor";
import type { WorkspaceActiveSnapshotState } from "../../state/workspaceActiveSnapshotStore";
import { collectWorkspaceActivePrimarySessionIds } from "../../state/workspaceActiveSnapshot/projection";
import { selectSessionThreadProjection } from "../../state/sessionThreadProjection/selectors";
import {
  addPretextPerfBucket,
  incrementPretextPerfCounter,
  readPretextPerfQueryFlag,
  recordPretextPerfEvent,
} from "../../utils/pretextPerfDiagnostics";
import { collectAskUserQuestionAnswers } from "../workbenchViewModel";
import {
  primeWarmWorkbenchThreadViewModel,
  pruneWarmWorkbenchThreadViewModelCache,
} from "../workbenchThreadViewModelWarmCache";
import {
  buildSessionPretextRuntimeLayoutKey,
  getOrCreateSessionPretextRuntime,
  pruneSessionPretextRuntimeCache,
  primeSessionPretextRuntime,
} from "../sessionThread/pretextSessionRuntimeCache";
import {
  getSessionTranscriptWarmState,
  subscribeSessionTranscriptWarmState,
} from "../sessionThread/sessionTranscriptWarmState";
import { planSessionHeadPrefetchTargets } from "./sessionHeadPrefetch";

type IdleHandle = number;

const requestIdle = (callback: () => void): IdleHandle => {
  const idleWindow = window as Window & {
    requestIdleCallback?: (cb: () => void) => number;
  };
  if (typeof idleWindow.requestIdleCallback === "function") {
    return idleWindow.requestIdleCallback(() => callback());
  }
  return window.setTimeout(callback, 16);
};

const cancelIdle = (handle: IdleHandle) => {
  const idleWindow = window as Window & {
    cancelIdleCallback?: (handle: number) => void;
  };
  if (typeof idleWindow.cancelIdleCallback === "function") {
    idleWindow.cancelIdleCallback(handle);
    return;
  }
  window.clearTimeout(handle);
};

function haveSameLoadingTurns(left: readonly string[], right: readonly string[]): boolean {
  if (left === right) return true;
  if (left.length !== right.length) return false;
  const remaining = new Set(left);
  if (remaining.size !== left.length) {
    return left.every((value, index) => value === right[index]);
  }
  for (const value of right) {
    if (!remaining.delete(value)) {
      return false;
    }
  }
  return remaining.size === 0;
}

export function useWarmSessionTranscriptRuntimes({
  workspaceSnapshot,
  sessionSnap,
  activeSessionId,
  suppressWarmSessions = false,
}: {
  workspaceSnapshot: WorkspaceActiveSnapshotState;
  sessionSnap: SessionSupervisorSnapshot;
  activeSessionId: string | null;
  suppressWarmSessions?: boolean;
}) {
  const activePrimarySessionIds = useMemo(
    () => collectWorkspaceActivePrimarySessionIds(workspaceSnapshot),
    [workspaceSnapshot],
  );
  const retainedSessionIds = useMemo(() => {
    const backgroundWarmSessionIds = suppressWarmSessions
      ? []
      : planSessionHeadPrefetchTargets({
          warmSessionIds: activePrimarySessionIds.filter((sessionId) => sessionId !== activeSessionId),
        }).targetSessionIds;
    return Array.from(
      new Set(
        [activeSessionId, ...backgroundWarmSessionIds].filter((sessionId): sessionId is string =>
          typeof sessionId === "string" && sessionId.trim().length > 0,
        ),
      ),
    );
  }, [activePrimarySessionIds, activeSessionId, suppressWarmSessions]);
  const warmState = useSyncExternalStore(
    subscribeSessionTranscriptWarmState,
    getSessionTranscriptWarmState,
    getSessionTranscriptWarmState,
  );

  useEffect(() => {
    const warmMode = readPretextPerfQueryFlag("pretextWarmMode") ?? "full";
    incrementPretextPerfCounter("pretext_warm_effect_runs");
    addPretextPerfBucket("pretext_warm_mode", warmMode);
    pruneWarmWorkbenchThreadViewModelCache(retainedSessionIds);
    pruneSessionPretextRuntimeCache(retainedSessionIds);

    if (warmState.viewportWidth <= 0) {
      incrementPretextPerfCounter("pretext_warm_skipped_missing_viewport");
      return;
    }

    if (warmMode === "off") {
      incrementPretextPerfCounter("pretext_warm_skipped_mode_off");
      return;
    }

    const sessionIds = retainedSessionIds.filter((sessionId) => sessionId !== activeSessionId);
    if (sessionIds.length === 0) {
      incrementPretextPerfCounter("pretext_warm_skipped_no_sessions");
      return;
    }

    incrementPretextPerfCounter("pretext_warm_candidate_sessions", sessionIds.length);

    let cancelled = false;
    let idleHandle: IdleHandle | null = null;
    let index = 0;

    const warmNext = () => {
      if (cancelled) return;
      while (index < sessionIds.length) {
        const sessionId = sessionIds[index]!;
        index += 1;
        incrementPretextPerfCounter("pretext_warm_session_attempts");

        const entry = sessionSnap.sessions[sessionId];
        if (!entry) continue;
        const threadProjection = selectSessionThreadProjection(entry);
        const hasProjectionData =
          threadProjection.loaded ||
          threadProjection.turns.length > 0 ||
          threadProjection.messages.length > 0 ||
          threadProjection.events.length > 0;
        if (!hasProjectionData) {
          incrementPretextPerfCounter("pretext_warm_skipped_no_projection");
          continue;
        }

        const askUserQuestionAnswers = collectAskUserQuestionAnswers(threadProjection.events, {});
        const warmedViewModel = primeWarmWorkbenchThreadViewModel({
          sessionId,
          projectionRev: threadProjection.projectionRev,
          turnsStamp: threadProjection.turnsStamp,
          assistantStreamingStamp: threadProjection.assistantStreamingStamp,
          messagesStamp: threadProjection.messagesStamp,
          eventsStamp: threadProjection.eventsStamp,
          verbosity: warmState.verbosity,
          turns: threadProjection.turns,
          assistantStreamingByTurnId: threadProjection.assistantStreamingByTurnId,
          messages: threadProjection.messages,
          events: threadProjection.events,
          toolsByTurnId: threadProjection.toolsByTurnId,
          toolSummariesReady: threadProjection.toolSummariesReady,
          askUserQuestionAnswers,
          enableDebugEvents: false,
        });
        incrementPretextPerfCounter("pretext_warm_viewmodel_ready");
        incrementPretextPerfCounter("pretext_warm_viewmodel_items", warmedViewModel.listItems.length);

        if (warmMode === "view") {
          incrementPretextPerfCounter("pretext_warm_viewmodel_only_sessions");
          recordPretextPerfEvent("warm:viewmodel-only", {
            sessionId,
            itemCount: warmedViewModel.listItems.length,
          });
          break;
        }

        const existingRuntime = getOrCreateSessionPretextRuntime(sessionId);
        const runtimeUiState =
          existingRuntime.uiState.verbosity === warmState.verbosity &&
          haveSameLoadingTurns(existingRuntime.uiState.turnToolsLoading, entry.turnToolsLoading)
            ? existingRuntime.uiState
            : {
                ...existingRuntime.uiState,
                turnToolsLoading: entry.turnToolsLoading,
                verbosity: warmState.verbosity,
              };

        primeSessionPretextRuntime({
          sessionId,
          listItems: warmedViewModel.listItems,
          uiState: runtimeUiState,
          viewportWidth: warmState.viewportWidth,
          viewportHeight: warmState.viewportHeight,
          sourceKey: warmedViewModel.warmKey,
          layoutKey: buildSessionPretextRuntimeLayoutKey({
            uiState: runtimeUiState,
            listItems: warmedViewModel.listItems,
          }),
        });
        incrementPretextPerfCounter("pretext_warm_runtime_primes");
        incrementPretextPerfCounter("pretext_warm_runtime_items", warmedViewModel.listItems.length);
        addPretextPerfBucket("pretext_warm_runtime_session", sessionId);
        break;
      }
      if (index < sessionIds.length) {
        schedule();
      }
    };

    const schedule = () => {
      idleHandle = requestIdle(() => {
        idleHandle = null;
        warmNext();
      });
    };

    schedule();
    return () => {
      cancelled = true;
      if (idleHandle != null) {
        cancelIdle(idleHandle);
      }
    };
  }, [activeSessionId, retainedSessionIds, sessionSnap, warmState]);
}
