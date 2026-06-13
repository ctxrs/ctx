import { useMemo, useRef } from "react";
import { type SessionEvent } from "../../api/client";
import {
  collectAskUserQuestionAnswers,
} from "../workbenchViewModel";
import type { AskUserQuestionAnswerState } from "./SessionPage.types";

function areAnswerRecordsEqual(
  a: Record<string, string>,
  b: Record<string, string>,
): boolean {
  const aKeys = Object.keys(a);
  const bKeys = Object.keys(b);
  if (aKeys.length !== bKeys.length) return false;
  for (const key of aKeys) {
    if (a[key] !== b[key]) return false;
  }
  return true;
}

function areAskUserAnswerStatesEqual(
  a: AskUserQuestionAnswerState,
  b: AskUserQuestionAnswerState,
): boolean {
  return a.outcome === b.outcome && areAnswerRecordsEqual(a.answers, b.answers);
}

function areAskUserAnswerMapsEqual(
  a: Map<string, AskUserQuestionAnswerState>,
  b: Map<string, AskUserQuestionAnswerState>,
): boolean {
  if (a === b) return true;
  if (a.size !== b.size) return false;
  for (const [toolCallId, state] of a.entries()) {
    const next = b.get(toolCallId);
    if (!next || !areAskUserAnswerStatesEqual(state, next)) {
      return false;
    }
  }
  return true;
}

type UseStableAskUserQuestionAnswersArgs = {
  events: SessionEvent[];
  optimisticAskAnswers: Record<string, AskUserQuestionAnswerState>;
  eventsStamp: string;
};

export function useStableAskUserQuestionAnswers({
  events,
  optimisticAskAnswers,
  eventsStamp,
}: UseStableAskUserQuestionAnswersArgs) {
  const stableAskUserQuestionAnswersRef = useRef(
    new Map<string, AskUserQuestionAnswerState>(),
  );

  return useMemo(() => {
    const next = collectAskUserQuestionAnswers(events, optimisticAskAnswers);
    const previous = stableAskUserQuestionAnswersRef.current;
    if (areAskUserAnswerMapsEqual(previous, next)) {
      return previous;
    }
    stableAskUserQuestionAnswersRef.current = next;
    return next;
  }, [events, eventsStamp, optimisticAskAnswers]);
}
