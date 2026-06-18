import type { ChangeSet, Contribution, ContributionEndpoint } from "@ctx/types";
import type {
  AgentWorkEndpointBucket,
  AgentWorkIndexedEndpoint,
  WorkspaceAgentWorkGraph,
} from "../../state/workspaceAgentWorkStore";
import { indexedContributionEndpoint, pullRequestEndpointKey } from "../../state/workspaceAgentWorkStore";

export type AgentWorkTaskSummary = {
  taskId: string;
  changeSetCount: number;
  contributionCount: number;
  linkedPullRequestCount: number;
  latestUpdateTimestamp: string | null;
};

const emptyTaskSummary = (taskId: string): AgentWorkTaskSummary => ({
  taskId,
  changeSetCount: 0,
  contributionCount: 0,
  linkedPullRequestCount: 0,
  latestUpdateTimestamp: null,
});

export const formatAgentWorkSummaryChips = (
  summary: AgentWorkTaskSummary | null | undefined,
): string[] => {
  const chips: string[] = [];
  if (summary?.changeSetCount) {
    chips.push(`${summary.changeSetCount} change${summary.changeSetCount === 1 ? "" : "s"}`);
  }
  if (summary?.linkedPullRequestCount) {
    chips.push(`${summary.linkedPullRequestCount} PR${summary.linkedPullRequestCount === 1 ? "" : "s"}`);
  }
  if (chips.length === 0 && summary?.contributionCount) {
    chips.push(`${summary.contributionCount} link${summary.contributionCount === 1 ? "" : "s"}`);
  }
  return chips;
};

const addLatestTimestamp = (current: string | null, candidate: string | null | undefined): string | null => {
  if (!candidate) return current;
  if (!current) return candidate;
  const currentMs = Date.parse(current);
  const candidateMs = Date.parse(candidate);
  if (Number.isFinite(currentMs) && Number.isFinite(candidateMs)) {
    return candidateMs > currentMs ? candidate : current;
  }
  return candidate > current ? candidate : current;
};

const addRecordTimestamps = (
  latest: string | null,
  record: Pick<ChangeSet | Contribution, "created_at" | "updated_at">,
): string | null => addLatestTimestamp(addLatestTimestamp(latest, record.created_at), record.updated_at);

const endpointPullRequestKey = (endpoint: ContributionEndpoint): string | null => {
  const indexed = indexedContributionEndpoint(endpoint);
  return indexed?.kind === "pull_request" ? indexed.id : null;
};

const addContributionPullRequestKeys = (keys: Set<string>, contribution: Contribution): void => {
  const subjectKey = endpointPullRequestKey(contribution.subject);
  const targetKey = endpointPullRequestKey(contribution.target);
  if (subjectKey) keys.add(subjectKey);
  if (targetKey) keys.add(targetKey);
};

const addChangeSetPullRequestKeys = (keys: Set<string>, changeSet: ChangeSet): void => {
  for (const link of changeSet.pull_requests ?? []) {
    const key = pullRequestEndpointKey(link.pull_request);
    if (key) keys.add(key);
  }
};

const endpointQueueKey = (endpoint: AgentWorkIndexedEndpoint): string => `${endpoint.kind}:${endpoint.id}`;

type TraversableAgentWorkEndpoint = Extract<AgentWorkIndexedEndpoint, { kind: "task" | "session" | "run" | "worktree" }>;

const isTraversableEndpoint = (
  endpoint: AgentWorkIndexedEndpoint | null | undefined,
): endpoint is TraversableAgentWorkEndpoint =>
  endpoint?.kind === "task" || endpoint?.kind === "session" || endpoint?.kind === "run" || endpoint?.kind === "worktree";

const bucketForEndpoint = (
  graph: WorkspaceAgentWorkGraph,
  endpoint: TraversableAgentWorkEndpoint,
): AgentWorkEndpointBucket | undefined => {
  switch (endpoint.kind) {
    case "task":
      return graph.endpointIndexes.tasksById[endpoint.id];
    case "session":
      return graph.endpointIndexes.sessionsById[endpoint.id];
    case "run":
      return graph.endpointIndexes.runsById[endpoint.id];
    case "worktree":
      return graph.endpointIndexes.worktreesById[endpoint.id];
  }
};

const shouldIncludeWorktreeChangeSet = (
  graph: WorkspaceAgentWorkGraph,
  changeSetId: string,
  reachableContributionIds: Set<string>,
): boolean => {
  const changeSetBucket = graph.endpointIndexes.changeSetsById[changeSetId];
  if (!changeSetBucket || changeSetBucket.contributionIds.length === 0) return true;
  return changeSetBucket.contributionIds.some((contributionId) => reachableContributionIds.has(contributionId));
};

export const summarizeAgentWorkForTask = (
  graph: WorkspaceAgentWorkGraph,
  taskId: string | null | undefined,
): AgentWorkTaskSummary => {
  const normalizedTaskId = String(taskId ?? "").trim();
  if (!normalizedTaskId) return emptyTaskSummary("");

  const changeSetIds = new Set<string>();
  const contributionIds = new Set<string>();
  const pullRequestKeys = new Set<string>();
  let latestUpdateTimestamp: string | null = null;
  const queuedEndpointKeys = new Set<string>();
  const queue: TraversableAgentWorkEndpoint[] = [{ kind: "task", id: normalizedTaskId }];
  queuedEndpointKeys.add(endpointQueueKey(queue[0]));

  for (let cursor = 0; cursor < queue.length; cursor += 1) {
    const endpoint = queue[cursor];
    const bucket = bucketForEndpoint(graph, endpoint);
    if (!bucket) continue;

    for (const changeSetId of bucket.changeSetIds) {
      if (endpoint.kind === "worktree" && !shouldIncludeWorktreeChangeSet(graph, changeSetId, contributionIds)) {
        continue;
      }
      changeSetIds.add(changeSetId);
    }

    if (endpoint.kind === "worktree") continue;

    for (const contributionId of bucket.contributionIds) {
      if (contributionIds.has(contributionId)) continue;
      contributionIds.add(contributionId);
      const contribution = graph.contributionsById[contributionId];
      if (!contribution) continue;
      latestUpdateTimestamp = addRecordTimestamps(latestUpdateTimestamp, contribution);
      addContributionPullRequestKeys(pullRequestKeys, contribution);
      if (contribution.change_set_id) changeSetIds.add(contribution.change_set_id);

      const subject = indexedContributionEndpoint(contribution.subject);
      const target = indexedContributionEndpoint(contribution.target);
      if (subject?.kind === "change_set") changeSetIds.add(subject.id);
      if (target?.kind === "change_set") changeSetIds.add(target.id);

      for (const linkedEndpoint of [subject, target]) {
        if (!isTraversableEndpoint(linkedEndpoint)) continue;
        const key = endpointQueueKey(linkedEndpoint);
        if (queuedEndpointKeys.has(key)) continue;
        queuedEndpointKeys.add(key);
        queue.push(linkedEndpoint);
      }
    }
  }

  for (const changeSetId of changeSetIds) {
    const changeSet = graph.changeSetsById[changeSetId];
    if (!changeSet) continue;
    latestUpdateTimestamp = addRecordTimestamps(latestUpdateTimestamp, changeSet);
    addChangeSetPullRequestKeys(pullRequestKeys, changeSet);
  }

  return {
    taskId: normalizedTaskId,
    changeSetCount: changeSetIds.size,
    contributionCount: contributionIds.size,
    linkedPullRequestCount: pullRequestKeys.size,
    latestUpdateTimestamp,
  };
};
