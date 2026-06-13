import { mergeOrderedIds, sameIdList } from "./idList";

type SessionSubscriptionPlanInput = {
  openSessionIds: string[];
  activeTaskSessionIds: string[];
  warmSessionIds: string[];
  previousSubscribedSessionIds: string[];
};

export type SessionSubscriptionPlan = {
  openSessionIds: string[];
  nextSubscribedSessionIds: string[];
  addedSessionIds: string[];
  removedSessionIds: string[];
  changed: boolean;
};

export const buildSessionSubscriptionPlan = ({
  openSessionIds,
  activeTaskSessionIds,
  warmSessionIds,
  previousSubscribedSessionIds,
}: SessionSubscriptionPlanInput): SessionSubscriptionPlan => {
  const nextSubscribedSessionIds = mergeOrderedIds(activeTaskSessionIds, openSessionIds, warmSessionIds);
  const previousSet = new Set(previousSubscribedSessionIds);
  const nextSet = new Set(nextSubscribedSessionIds);
  return {
    openSessionIds,
    nextSubscribedSessionIds,
    addedSessionIds: nextSubscribedSessionIds.filter((sessionId) => !previousSet.has(sessionId)),
    removedSessionIds: previousSubscribedSessionIds.filter((sessionId) => !nextSet.has(sessionId)),
    changed: !sameIdList(nextSubscribedSessionIds, previousSubscribedSessionIds),
  };
};
