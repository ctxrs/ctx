import React, { createContext, useContext, useEffect, useMemo, useRef, useSyncExternalStore } from "react";
import type { ChangeSet, Contribution, ContributionEndpoint, PullRequestRef } from "@ctx/types";
import { getWorkspaceAgentWork, type WorkspaceAgentWorkResponse } from "../api/clientWorkspaces";

export type AgentWorkIndexedEndpointKind =
  | "account"
  | "workspace"
  | "task"
  | "session"
  | "run"
  | "agent"
  | "system"
  | "worktree"
  | "change_set"
  | "pull_request"
  | "artifact"
  | "check"
  | "evidence"
  | "review_attestation"
  | "commit"
  | "branch"
  | "file"
  | "external";

export type AgentWorkIndexedEndpoint =
  | { kind: "account"; id: string }
  | { kind: "workspace"; id: string }
  | { kind: "task"; id: string }
  | { kind: "session"; id: string }
  | { kind: "run"; id: string }
  | { kind: "agent"; id: string }
  | { kind: "system"; id: string }
  | { kind: "worktree"; id: string }
  | { kind: "change_set"; id: string }
  | { kind: "pull_request"; id: string; pullRequest: PullRequestRef }
  | { kind: "artifact"; id: string }
  | { kind: "check"; id: string }
  | { kind: "evidence"; id: string }
  | { kind: "review_attestation"; id: string }
  | { kind: "commit"; id: string }
  | { kind: "branch"; id: string }
  | { kind: "file"; id: string }
  | { kind: "external"; id: string };

export type AgentWorkEndpointBucket = {
  changeSetIds: string[];
  contributionIds: string[];
};

export type AgentWorkPullRequestEndpointBucket = AgentWorkEndpointBucket & {
  pullRequest: PullRequestRef;
};

export type AgentWorkEndpointIndexes = {
  accountsById: Record<string, AgentWorkEndpointBucket>;
  workspacesById: Record<string, AgentWorkEndpointBucket>;
  tasksById: Record<string, AgentWorkEndpointBucket>;
  sessionsById: Record<string, AgentWorkEndpointBucket>;
  runsById: Record<string, AgentWorkEndpointBucket>;
  agentsById: Record<string, AgentWorkEndpointBucket>;
  systemsByLabel: Record<string, AgentWorkEndpointBucket>;
  worktreesById: Record<string, AgentWorkEndpointBucket>;
  changeSetsById: Record<string, AgentWorkEndpointBucket>;
  pullRequestsByKey: Record<string, AgentWorkPullRequestEndpointBucket>;
  artifactsById: Record<string, AgentWorkEndpointBucket>;
  checksById: Record<string, AgentWorkEndpointBucket>;
  evidenceById: Record<string, AgentWorkEndpointBucket>;
  reviewAttestationsById: Record<string, AgentWorkEndpointBucket>;
  commitsBySha: Record<string, AgentWorkEndpointBucket>;
  branchesByName: Record<string, AgentWorkEndpointBucket>;
  filesByKey: Record<string, AgentWorkEndpointBucket>;
  externalsByKey: Record<string, AgentWorkEndpointBucket>;
};

export type WorkspaceAgentWorkGraph = {
  changeSetIds: string[];
  contributionIds: string[];
  changeSetsById: Record<string, ChangeSet>;
  contributionsById: Record<string, Contribution>;
  endpointIndexes: AgentWorkEndpointIndexes;
};

export type WorkspaceAgentWorkStatus = "idle" | "loading" | "ready" | "error";

export type WorkspaceAgentWorkState = {
  workspaceId: string;
  status: WorkspaceAgentWorkStatus;
  error: string | null;
  loadedAtMs: number | null;
  graph: WorkspaceAgentWorkGraph;
};

type MutableEndpointBucket = {
  changeSetIds: Set<string>;
  contributionIds: Set<string>;
};

type MutablePullRequestEndpointBucket = MutableEndpointBucket & {
  pullRequest: PullRequestRef;
};

type MutableEndpointIndexes = {
  accountsById: Record<string, MutableEndpointBucket>;
  workspacesById: Record<string, MutableEndpointBucket>;
  tasksById: Record<string, MutableEndpointBucket>;
  sessionsById: Record<string, MutableEndpointBucket>;
  runsById: Record<string, MutableEndpointBucket>;
  agentsById: Record<string, MutableEndpointBucket>;
  systemsByLabel: Record<string, MutableEndpointBucket>;
  worktreesById: Record<string, MutableEndpointBucket>;
  changeSetsById: Record<string, MutableEndpointBucket>;
  pullRequestsByKey: Record<string, MutablePullRequestEndpointBucket>;
  artifactsById: Record<string, MutableEndpointBucket>;
  checksById: Record<string, MutableEndpointBucket>;
  evidenceById: Record<string, MutableEndpointBucket>;
  reviewAttestationsById: Record<string, MutableEndpointBucket>;
  commitsBySha: Record<string, MutableEndpointBucket>;
  branchesByName: Record<string, MutableEndpointBucket>;
  filesByKey: Record<string, MutableEndpointBucket>;
  externalsByKey: Record<string, MutableEndpointBucket>;
};

const emptyEndpointIndexes = (): MutableEndpointIndexes => ({
  accountsById: {},
  workspacesById: {},
  tasksById: {},
  sessionsById: {},
  runsById: {},
  agentsById: {},
  systemsByLabel: {},
  worktreesById: {},
  changeSetsById: {},
  pullRequestsByKey: {},
  artifactsById: {},
  checksById: {},
  evidenceById: {},
  reviewAttestationsById: {},
  commitsBySha: {},
  branchesByName: {},
  filesByKey: {},
  externalsByKey: {},
});

const emptyBucket = (): MutableEndpointBucket => ({
  changeSetIds: new Set(),
  contributionIds: new Set(),
});

const normalizeId = (value: string | null | undefined): string => String(value ?? "").trim();

const normalizeEndpointId = (endpoint: { id?: string | null }, fieldValue?: string | null): string =>
  normalizeId(fieldValue) || normalizeId(endpoint.id);

const endpointIndexKey = (parts: Array<[string, string]>): string => JSON.stringify(parts);

const normalizeSessionEndpointId = (endpoint: Extract<ContributionEndpoint, { kind: "session" }>): string => {
  const localId = normalizeId(endpoint.session_id);
  if (localId) {
    return endpointIndexKey([
      ["session_id", localId],
      ["turn_id", normalizeId(endpoint.turn_id)],
      ["run_id", normalizeId(endpoint.run_id)],
    ]);
  }
  const externalId = normalizeId(endpoint.id);
  if (!externalId) return "";
  const provider = normalizeId(endpoint.provider);
  return endpointIndexKey([
    ["provider", provider],
    ["id", externalId],
  ]);
};

const normalizeRunEndpointId = (endpoint: Extract<ContributionEndpoint, { kind: "run" }>): string => {
  const localId = normalizeId(endpoint.run_id);
  if (localId) {
    return endpointIndexKey([
      ["run_id", localId],
      ["session_id", normalizeId(endpoint.session_id)],
    ]);
  }
  return normalizeId(endpoint.id);
};

const normalizeAgentEndpointId = (endpoint: Extract<ContributionEndpoint, { kind: "agent" }>): string => {
  const runId = normalizeId(endpoint.run_id);
  const sessionId = normalizeId(endpoint.session_id);
  const label = normalizeId(endpoint.label);
  if (!runId && !sessionId && !label) return "";
  return endpointIndexKey([
    ["run_id", runId],
    ["session_id", sessionId],
    ["label", label],
  ]);
};

const sortedValues = (values: Set<string>): string[] => Array.from(values).sort((left, right) => left.localeCompare(right));

const freezeBucket = (bucket: MutableEndpointBucket): AgentWorkEndpointBucket => ({
  changeSetIds: sortedValues(bucket.changeSetIds),
  contributionIds: sortedValues(bucket.contributionIds),
});

const freezeBuckets = (buckets: Record<string, MutableEndpointBucket>): Record<string, AgentWorkEndpointBucket> => {
  const result: Record<string, AgentWorkEndpointBucket> = {};
  for (const key of Object.keys(buckets).sort((left, right) => left.localeCompare(right))) {
    result[key] = freezeBucket(buckets[key]);
  }
  return result;
};

const freezePullRequestBuckets = (
  buckets: Record<string, MutablePullRequestEndpointBucket>,
): Record<string, AgentWorkPullRequestEndpointBucket> => {
  const result: Record<string, AgentWorkPullRequestEndpointBucket> = {};
  for (const key of Object.keys(buckets).sort((left, right) => left.localeCompare(right))) {
    const bucket = buckets[key];
    result[key] = {
      ...freezeBucket(bucket),
      pullRequest: bucket.pullRequest,
    };
  }
  return result;
};

const endpointBucket = (buckets: Record<string, MutableEndpointBucket>, id: string): MutableEndpointBucket => {
  buckets[id] ??= emptyBucket();
  return buckets[id];
};

const pullRequestEndpointBucket = (
  buckets: Record<string, MutablePullRequestEndpointBucket>,
  key: string,
  pullRequest: PullRequestRef,
): MutablePullRequestEndpointBucket => {
  buckets[key] ??= {
    ...emptyBucket(),
    pullRequest,
  };
  return buckets[key];
};

export const pullRequestEndpointKey = (pullRequest: PullRequestRef): string => {
  const provider = normalizeId(pullRequest.provider);
  const owner = normalizeId(pullRequest.owner);
  const repo = normalizeId(pullRequest.repo);
  const number = Number.isFinite(pullRequest.number) ? String(pullRequest.number) : "";
  if (!provider && !owner && !repo && !number) return "";
  return endpointIndexKey([
    ["provider", provider],
    ["owner", owner],
    ["repo", repo],
    ["number", number],
  ]);
};

export const indexedContributionEndpoint = (
  endpoint: ContributionEndpoint | null | undefined,
): AgentWorkIndexedEndpoint | null => {
  if (!endpoint) return null;
  switch (endpoint.kind) {
    case "account": {
      const id = normalizeId(endpoint.account_id);
      return id ? { kind: "account", id } : null;
    }
    case "workspace": {
      const id = normalizeId(endpoint.workspace_id);
      return id ? { kind: "workspace", id } : null;
    }
    case "task": {
      const id = normalizeEndpointId(endpoint, endpoint.task_id);
      return id ? { kind: "task", id } : null;
    }
    case "session": {
      const id = normalizeSessionEndpointId(endpoint);
      return id ? { kind: "session", id } : null;
    }
    case "run": {
      const id = normalizeRunEndpointId(endpoint);
      return id ? { kind: "run", id } : null;
    }
    case "agent": {
      const id = normalizeAgentEndpointId(endpoint);
      return id ? { kind: "agent", id } : null;
    }
    case "system": {
      const id = normalizeId(endpoint.label);
      return id ? { kind: "system", id } : null;
    }
    case "worktree": {
      const id = normalizeEndpointId(endpoint, endpoint.worktree_id);
      return id ? { kind: "worktree", id } : null;
    }
    case "change_set":
    case "change-set": {
      const id = normalizeEndpointId(endpoint, endpoint.change_set_id);
      return id ? { kind: "change_set", id } : null;
    }
    case "pull_request":
    case "pull-request": {
      const id = pullRequestEndpointKey(endpoint.pull_request);
      return id ? { kind: "pull_request", id, pullRequest: endpoint.pull_request } : null;
    }
    case "artifact": {
      const explicitId = normalizeEndpointId(endpoint, endpoint.artifact_id);
      if (explicitId) {
        return { kind: "artifact", id: endpointIndexKey([["artifact_id", explicitId]]) };
      }
      const digest = normalizeId(endpoint.digest);
      const relativePath = normalizeId(endpoint.relative_path);
      if (digest || relativePath) {
        return {
          kind: "artifact",
          id: endpointIndexKey([
            ["digest", digest],
            ["relative_path", relativePath],
          ]),
        };
      }
      return null;
    }
    case "check": {
      const id = normalizeEndpointId(endpoint, endpoint.check_id);
      return id ? { kind: "check", id } : null;
    }
    case "evidence": {
      const id = normalizeId(endpoint.id);
      return id ? { kind: "evidence", id } : null;
    }
    case "review_attestation":
    case "review-attestation": {
      const id = normalizeId(endpoint.id);
      return id ? { kind: "review_attestation", id } : null;
    }
    case "commit": {
      const id = normalizeId(endpoint.sha);
      return id ? { kind: "commit", id } : null;
    }
    case "branch": {
      const id = normalizeId(endpoint.name);
      return id ? { kind: "branch", id } : null;
    }
    case "file": {
      const path = normalizeId(endpoint.path);
      if (!path) return null;
      return {
        kind: "file",
        id: endpointIndexKey([
          ["worktree_id", normalizeId(endpoint.worktree_id)],
          ["path", path],
        ]),
      };
    }
    case "external": {
      const source = normalizeId(endpoint.source);
      const identifier = normalizeId(endpoint.identifier);
      const url = normalizeId(endpoint.url);
      if (!source || (!identifier && !url)) return null;
      return {
        kind: "external",
        id: endpointIndexKey([
          ["source", source],
          ["identifier", identifier],
          ["url", url],
        ]),
      };
    }
    default:
      return null;
  }
};

const addToBucket = (
  bucket: MutableEndpointBucket,
  links: { changeSetIds?: Iterable<string>; contributionId?: string },
): void => {
  for (const changeSetId of links.changeSetIds ?? []) {
    const id = normalizeId(changeSetId);
    if (id) bucket.changeSetIds.add(id);
  }
  const contributionId = normalizeId(links.contributionId);
  if (contributionId) bucket.contributionIds.add(contributionId);
};

const addEndpointLink = (
  indexes: MutableEndpointIndexes,
  endpoint: AgentWorkIndexedEndpoint,
  links: { changeSetIds?: Iterable<string>; contributionId?: string },
): void => {
  switch (endpoint.kind) {
    case "account":
      addToBucket(endpointBucket(indexes.accountsById, endpoint.id), links);
      break;
    case "workspace":
      addToBucket(endpointBucket(indexes.workspacesById, endpoint.id), links);
      break;
    case "task":
      addToBucket(endpointBucket(indexes.tasksById, endpoint.id), links);
      break;
    case "session":
      addToBucket(endpointBucket(indexes.sessionsById, endpoint.id), links);
      break;
    case "run":
      addToBucket(endpointBucket(indexes.runsById, endpoint.id), links);
      break;
    case "agent":
      addToBucket(endpointBucket(indexes.agentsById, endpoint.id), links);
      break;
    case "system":
      addToBucket(endpointBucket(indexes.systemsByLabel, endpoint.id), links);
      break;
    case "worktree":
      addToBucket(endpointBucket(indexes.worktreesById, endpoint.id), links);
      break;
    case "change_set":
      addToBucket(endpointBucket(indexes.changeSetsById, endpoint.id), links);
      break;
    case "pull_request":
      addToBucket(pullRequestEndpointBucket(indexes.pullRequestsByKey, endpoint.id, endpoint.pullRequest), links);
      break;
    case "artifact":
      addToBucket(endpointBucket(indexes.artifactsById, endpoint.id), links);
      break;
    case "check":
      addToBucket(endpointBucket(indexes.checksById, endpoint.id), links);
      break;
    case "evidence":
      addToBucket(endpointBucket(indexes.evidenceById, endpoint.id), links);
      break;
    case "review_attestation":
      addToBucket(endpointBucket(indexes.reviewAttestationsById, endpoint.id), links);
      break;
    case "commit":
      addToBucket(endpointBucket(indexes.commitsBySha, endpoint.id), links);
      break;
    case "branch":
      addToBucket(endpointBucket(indexes.branchesByName, endpoint.id), links);
      break;
    case "file":
      addToBucket(endpointBucket(indexes.filesByKey, endpoint.id), links);
      break;
    case "external":
      addToBucket(endpointBucket(indexes.externalsByKey, endpoint.id), links);
      break;
  }
};

const contributionChangeSetIds = (contribution: Contribution): Set<string> => {
  const ids = new Set<string>();
  const declared = normalizeId(contribution.change_set_id);
  if (declared) ids.add(declared);
  const subject = indexedContributionEndpoint(contribution.subject);
  const target = indexedContributionEndpoint(contribution.target);
  if (subject?.kind === "change_set") ids.add(subject.id);
  if (target?.kind === "change_set") ids.add(target.id);
  return ids;
};

const freezeEndpointIndexes = (indexes: MutableEndpointIndexes): AgentWorkEndpointIndexes => ({
  accountsById: freezeBuckets(indexes.accountsById),
  workspacesById: freezeBuckets(indexes.workspacesById),
  tasksById: freezeBuckets(indexes.tasksById),
  sessionsById: freezeBuckets(indexes.sessionsById),
  runsById: freezeBuckets(indexes.runsById),
  agentsById: freezeBuckets(indexes.agentsById),
  systemsByLabel: freezeBuckets(indexes.systemsByLabel),
  worktreesById: freezeBuckets(indexes.worktreesById),
  changeSetsById: freezeBuckets(indexes.changeSetsById),
  pullRequestsByKey: freezePullRequestBuckets(indexes.pullRequestsByKey),
  artifactsById: freezeBuckets(indexes.artifactsById),
  checksById: freezeBuckets(indexes.checksById),
  evidenceById: freezeBuckets(indexes.evidenceById),
  reviewAttestationsById: freezeBuckets(indexes.reviewAttestationsById),
  commitsBySha: freezeBuckets(indexes.commitsBySha),
  branchesByName: freezeBuckets(indexes.branchesByName),
  filesByKey: freezeBuckets(indexes.filesByKey),
  externalsByKey: freezeBuckets(indexes.externalsByKey),
});

export const normalizeWorkspaceAgentWork = (data: WorkspaceAgentWorkResponse): WorkspaceAgentWorkGraph => {
  const changeSetsById: Record<string, ChangeSet> = {};
  const contributionsById: Record<string, Contribution> = {};
  const indexes = emptyEndpointIndexes();

  for (const changeSet of data.change_sets ?? []) {
    const changeSetId = normalizeId(changeSet.id);
    if (!changeSetId) continue;
    changeSetsById[changeSetId] = changeSet;
    addEndpointLink(indexes, { kind: "change_set", id: changeSetId }, { changeSetIds: [changeSetId] });

    const worktreeId = normalizeId(changeSet.source_worktree_id);
    if (worktreeId) {
      addEndpointLink(indexes, { kind: "worktree", id: worktreeId }, { changeSetIds: [changeSetId] });
    }

    for (const link of changeSet.pull_requests ?? []) {
      const key = pullRequestEndpointKey(link.pull_request);
      if (!key) continue;
      addEndpointLink(
        indexes,
        { kind: "pull_request", id: key, pullRequest: link.pull_request },
        { changeSetIds: [changeSetId] },
      );
    }
  }

  for (const contribution of data.contributions ?? []) {
    const contributionId = normalizeId(contribution.id);
    if (!contributionId) continue;
    contributionsById[contributionId] = contribution;

    const changeSetIds = contributionChangeSetIds(contribution);
    const endpoints = [
      indexedContributionEndpoint(contribution.subject),
      indexedContributionEndpoint(contribution.target),
    ].filter((endpoint): endpoint is AgentWorkIndexedEndpoint => Boolean(endpoint));

    for (const changeSetId of changeSetIds) {
      addEndpointLink(
        indexes,
        { kind: "change_set", id: changeSetId },
        { changeSetIds: [changeSetId], contributionId },
      );
    }

    for (const endpoint of endpoints) {
      const endpointChangeSetIds = new Set(changeSetIds);
      if (endpoint.kind === "change_set") endpointChangeSetIds.add(endpoint.id);
      addEndpointLink(indexes, endpoint, { changeSetIds: endpointChangeSetIds, contributionId });
    }
  }

  return {
    changeSetIds: Object.keys(changeSetsById).sort((left, right) => left.localeCompare(right)),
    contributionIds: Object.keys(contributionsById).sort((left, right) => left.localeCompare(right)),
    changeSetsById,
    contributionsById,
    endpointIndexes: freezeEndpointIndexes(indexes),
  };
};

export const EMPTY_WORKSPACE_AGENT_WORK_GRAPH = normalizeWorkspaceAgentWork({
  change_sets: [],
  contributions: [],
});

const errorMessage = (error: unknown): string => {
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === "string" && error) return error;
  return "Failed to load workspace agent work.";
};

export class WorkspaceAgentWorkStore {
  private listeners = new Set<() => void>();
  private destroyed = false;
  private requestGeneration = 0;
  private loadingPromise: Promise<void> | null = null;
  private refreshQueued = false;
  private snapshot: WorkspaceAgentWorkState;

  constructor(private readonly workspaceId: string) {
    this.snapshot = {
      workspaceId,
      status: "idle",
      error: null,
      loadedAtMs: null,
      graph: EMPTY_WORKSPACE_AGENT_WORK_GRAPH,
    };
  }

  init = (): void => {
    this.destroyed = false;
    this.refresh().catch(() => {});
  };

  destroy = (): void => {
    this.destroyed = true;
    this.requestGeneration += 1;
    this.loadingPromise = null;
    this.refreshQueued = false;
    this.listeners.clear();
  };

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  getSnapshot = (): WorkspaceAgentWorkState => this.snapshot;

  refresh = (): Promise<void> => {
    if (this.destroyed) return Promise.resolve();
    if (this.loadingPromise) {
      this.refreshQueued = true;
      return this.loadingPromise;
    }
    this.refreshQueued = false;
    const generation = ++this.requestGeneration;
    this.setSnapshot({
      ...this.snapshot,
      status: "loading",
      error: null,
    });

    this.loadingPromise = getWorkspaceAgentWork(this.workspaceId)
      .then((data) => {
        if (this.destroyed || generation !== this.requestGeneration) return;
        this.setSnapshot({
          workspaceId: this.workspaceId,
          status: "ready",
          error: null,
          loadedAtMs: Date.now(),
          graph: normalizeWorkspaceAgentWork(data),
        });
      })
      .catch((error: unknown) => {
        if (this.destroyed || generation !== this.requestGeneration) return;
        this.setSnapshot({
          ...this.snapshot,
          status: "error",
          error: errorMessage(error),
        });
      })
      .finally(() => {
        if (generation === this.requestGeneration) {
          this.loadingPromise = null;
        }
        if (!this.destroyed && this.refreshQueued && generation === this.requestGeneration) {
          this.refreshQueued = false;
          this.refresh().catch(() => {});
        }
      });

    return this.loadingPromise;
  };

  private setSnapshot(next: WorkspaceAgentWorkState): void {
    this.snapshot = next;
    for (const listener of this.listeners) {
      listener();
    }
  }
}

const WorkspaceAgentWorkContext = createContext<WorkspaceAgentWorkStore | null>(null);

export function WorkspaceAgentWorkProvider({
  workspaceId,
  children,
}: {
  workspaceId: string;
  children: React.ReactNode;
}) {
  const storeRef = useRef<WorkspaceAgentWorkStore | null>(null);
  const lastWorkspaceRef = useRef<string | null>(null);
  if (!storeRef.current || lastWorkspaceRef.current !== workspaceId) {
    storeRef.current?.destroy();
    storeRef.current = new WorkspaceAgentWorkStore(workspaceId);
    lastWorkspaceRef.current = workspaceId;
  }

  useEffect(() => {
    const store = storeRef.current;
    store?.init();
    return () => store?.destroy();
  }, [workspaceId]);

  return (
    <WorkspaceAgentWorkContext.Provider value={storeRef.current}>
      {children}
    </WorkspaceAgentWorkContext.Provider>
  );
}

export function useWorkspaceAgentWorkStore(): WorkspaceAgentWorkStore {
  const store = useContext(WorkspaceAgentWorkContext);
  if (!store) throw new Error("WorkspaceAgentWorkProvider missing");
  return store;
}

export function useWorkspaceAgentWorkSnapshot(): WorkspaceAgentWorkState {
  const store = useWorkspaceAgentWorkStore();
  return useSyncExternalStore(store.subscribe, store.getSnapshot, store.getSnapshot);
}

export function useWorkspaceAgentWorkGraph(): WorkspaceAgentWorkGraph {
  const snapshot = useWorkspaceAgentWorkSnapshot();
  return useMemo(() => snapshot.graph, [snapshot.graph]);
}
