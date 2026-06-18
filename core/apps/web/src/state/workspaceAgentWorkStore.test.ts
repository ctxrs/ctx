import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ChangeSet, Contribution, PullRequestRef } from "@ctx/types";
import type { WorkspaceAgentWorkResponse } from "../api/clientWorkspaces";
import { getWorkspaceAgentWork } from "../api/clientWorkspaces";
import { normalizeWorkspaceAgentWork, pullRequestEndpointKey, WorkspaceAgentWorkStore } from "./workspaceAgentWorkStore";

vi.mock("../api/clientWorkspaces", () => ({
  getWorkspaceAgentWork: vi.fn(),
}));

const pr = (number: number): PullRequestRef => ({
  provider: "github",
  owner: "CtxRS",
  repo: "ctx",
  number,
});

const changeSet = (overrides: Partial<ChangeSet> & Pick<ChangeSet, "id">): ChangeSet => ({
  workspace_id: "workspace-1",
  ...overrides,
});

const contribution = (overrides: Partial<Contribution> & Pick<Contribution, "id">): Contribution => ({
  workspace_id: "workspace-1",
  subject: { kind: "system", label: "test" },
  target: { kind: "system", label: "test" },
  ...overrides,
});

const endpointIndexKey = (parts: Array<[string, string]>): string => JSON.stringify(parts);

const deferred = <T>() => {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
};

describe("workspaceAgentWorkStore normalization", () => {
  it("indexes tracked endpoints with linked change sets and contributions", () => {
    const pullRequest = pr(12);
    const graph = normalizeWorkspaceAgentWork({
      change_sets: [
        changeSet({
          id: "change-set-1",
          source_worktree_id: "worktree-1",
          pull_requests: [{ pull_request: pullRequest, kind: "result" }],
        }),
      ],
      contributions: [
        contribution({
          id: "contribution-1",
          subject: { kind: "task", task_id: "task-1" },
          target: { kind: "change_set", change_set_id: "change-set-1" },
        }),
        contribution({
          id: "contribution-2",
          change_set_id: "change-set-1",
          subject: { kind: "session", session_id: "session-1" },
          target: { kind: "task", task_id: "task-1" },
        }),
        contribution({
          id: "contribution-3",
          subject: { kind: "worktree", worktree_id: "worktree-1" },
          target: { kind: "pull_request", pull_request: pullRequest },
        }),
      ],
    });

    expect(graph.changeSetIds).toEqual(["change-set-1"]);
    expect(graph.contributionIds).toEqual(["contribution-1", "contribution-2", "contribution-3"]);
    expect(graph.endpointIndexes.tasksById["task-1"]).toEqual({
      changeSetIds: ["change-set-1"],
      contributionIds: ["contribution-1", "contribution-2"],
    });
    expect(
      graph.endpointIndexes.sessionsById[
        endpointIndexKey([
          ["session_id", "session-1"],
          ["turn_id", ""],
          ["run_id", ""],
        ])
      ],
    ).toEqual({
      changeSetIds: ["change-set-1"],
      contributionIds: ["contribution-2"],
    });
    expect(graph.endpointIndexes.worktreesById["worktree-1"]).toEqual({
      changeSetIds: ["change-set-1"],
      contributionIds: ["contribution-3"],
    });
    expect(graph.endpointIndexes.changeSetsById["change-set-1"]).toEqual({
      changeSetIds: ["change-set-1"],
      contributionIds: ["contribution-1", "contribution-2"],
    });

    const pullRequestKey = pullRequestEndpointKey(pullRequest);
    expect(graph.endpointIndexes.pullRequestsByKey[pullRequestKey]).toEqual({
      pullRequest,
      changeSetIds: ["change-set-1"],
      contributionIds: ["contribution-3"],
    });
  });

  it("indexes public endpoint aliases and digest-only artifacts", () => {
    const graph = normalizeWorkspaceAgentWork({
      change_sets: [
        changeSet({
          id: "change-set-alias",
        }),
      ],
      contributions: [
        contribution({
          id: "alias-contribution",
          subject: { kind: "task", id: "task-alias" },
          target: { kind: "change-set", id: "change-set-alias" },
        }),
        contribution({
          id: "pull-request-alias-contribution",
          subject: { kind: "task", id: "task-alias" },
          target: { kind: "pull-request", pull_request: pr(24) },
        }),
        contribution({
          id: "artifact-contribution",
          subject: {
            kind: "artifact",
            digest: "sha256:abc",
            relative_path: "transcripts/session.jsonl",
          },
          target: { kind: "check", id: "check-alias" },
        }),
        contribution({
          id: "external-session-contribution",
          subject: { kind: "session", provider: "codex", id: "thr_external" },
          target: { kind: "worktree", id: "wtr_external" },
        }),
      ],
    });

    expect(graph.endpointIndexes.tasksById["task-alias"]).toEqual({
      changeSetIds: ["change-set-alias"],
      contributionIds: ["alias-contribution", "pull-request-alias-contribution"],
    });
    expect(graph.endpointIndexes.changeSetsById["change-set-alias"]).toEqual({
      changeSetIds: ["change-set-alias"],
      contributionIds: ["alias-contribution"],
    });
    expect(
      graph.endpointIndexes.artifactsById[
        endpointIndexKey([
          ["digest", "sha256:abc"],
          ["relative_path", "transcripts/session.jsonl"],
        ])
      ],
    ).toEqual({
      changeSetIds: [],
      contributionIds: ["artifact-contribution"],
    });
    expect(graph.endpointIndexes.checksById["check-alias"]).toEqual({
      changeSetIds: [],
      contributionIds: ["artifact-contribution"],
    });
    expect(
      graph.endpointIndexes.sessionsById[
        endpointIndexKey([
          ["provider", "codex"],
          ["id", "thr_external"],
        ])
      ],
    ).toEqual({
      changeSetIds: [],
      contributionIds: ["external-session-contribution"],
    });
    expect(graph.endpointIndexes.worktreesById["wtr_external"]).toEqual({
      changeSetIds: [],
      contributionIds: ["external-session-contribution"],
    });
    expect(graph.endpointIndexes.pullRequestsByKey[pullRequestEndpointKey(pr(24))]).toEqual({
      pullRequest: pr(24),
      changeSetIds: [],
      contributionIds: ["pull-request-alias-contribution"],
    });
  });

  it("uses the same compound endpoint keys as the Rust agent-work store", () => {
    const graph = normalizeWorkspaceAgentWork({
      change_sets: [],
      contributions: [
        contribution({
          id: "compound-session",
          subject: {
            kind: "session",
            session_id: "session-1",
            turn_id: "turn-1",
            run_id: "run-1",
          },
          target: {
            kind: "run",
            run_id: "run-1",
            session_id: "session-1",
          },
        }),
        contribution({
          id: "agent-file-external",
          subject: {
            kind: "agent",
            session_id: "session-1",
            run_id: "run-1",
            label: "reviewer",
          },
          target: {
            kind: "file",
            worktree_id: "worktree-1",
            path: "src/main.ts",
          },
        }),
        contribution({
          id: "external-link",
          subject: { kind: "external", source: "linear", identifier: "ENG-123" },
          target: { kind: "artifact", artifact_id: "artifact-1" },
        }),
        contribution({
          id: "external-url-link",
          subject: { kind: "external", source: "linear", url: "https://linear.test/ENG-456" },
          target: { kind: "system", label: "external-url" },
        }),
      ],
    });

    expect(
      graph.endpointIndexes.sessionsById[
        endpointIndexKey([
          ["session_id", "session-1"],
          ["turn_id", "turn-1"],
          ["run_id", "run-1"],
        ])
      ],
    ).toEqual({
      changeSetIds: [],
      contributionIds: ["compound-session"],
    });
    expect(
      graph.endpointIndexes.runsById[
        endpointIndexKey([
          ["run_id", "run-1"],
          ["session_id", "session-1"],
        ])
      ],
    ).toEqual({
      changeSetIds: [],
      contributionIds: ["compound-session"],
    });
    expect(
      graph.endpointIndexes.agentsById[
        endpointIndexKey([
          ["run_id", "run-1"],
          ["session_id", "session-1"],
          ["label", "reviewer"],
        ])
      ],
    ).toEqual({
      changeSetIds: [],
      contributionIds: ["agent-file-external"],
    });
    expect(
      graph.endpointIndexes.filesByKey[
        endpointIndexKey([
          ["worktree_id", "worktree-1"],
          ["path", "src/main.ts"],
        ])
      ],
    ).toEqual({
      changeSetIds: [],
      contributionIds: ["agent-file-external"],
    });
    expect(
      graph.endpointIndexes.externalsByKey[
        endpointIndexKey([
          ["source", "linear"],
          ["identifier", "ENG-123"],
          ["url", ""],
        ])
      ],
    ).toEqual({
      changeSetIds: [],
      contributionIds: ["external-link"],
    });
    expect(
      graph.endpointIndexes.externalsByKey[
        endpointIndexKey([
          ["source", "linear"],
          ["identifier", ""],
          ["url", "https://linear.test/ENG-456"],
        ])
      ],
    ).toEqual({
      changeSetIds: [],
      contributionIds: ["external-url-link"],
    });
    expect(graph.endpointIndexes.artifactsById[endpointIndexKey([["artifact_id", "artifact-1"]])]).toEqual({
      changeSetIds: [],
      contributionIds: ["external-link"],
    });
  });
});

describe("WorkspaceAgentWorkStore", () => {
  beforeEach(() => {
    vi.mocked(getWorkspaceAgentWork).mockReset();
  });

  it("runs one queued refresh after the current graph load settles", async () => {
    const first = deferred<WorkspaceAgentWorkResponse>();
    const second = deferred<WorkspaceAgentWorkResponse>();
    vi.mocked(getWorkspaceAgentWork)
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise);

    const store = new WorkspaceAgentWorkStore("workspace-1");
    const firstRefresh = store.refresh();
    const queuedRefresh = store.refresh();

    expect(getWorkspaceAgentWork).toHaveBeenCalledTimes(1);
    expect(queuedRefresh).toBe(firstRefresh);

    first.resolve({ change_sets: [], contributions: [] });
    await firstRefresh;
    await Promise.resolve();

    expect(getWorkspaceAgentWork).toHaveBeenCalledTimes(2);

    second.resolve({
      change_sets: [],
      contributions: [
        contribution({
          id: "contribution-after-queued-refresh",
          subject: { kind: "task", task_id: "task-1" },
          target: { kind: "session", session_id: "session-1" },
        }),
      ],
    });
    await Promise.resolve();
    await Promise.resolve();

    expect(store.getSnapshot().graph.contributionIds).toEqual(["contribution-after-queued-refresh"]);
    store.destroy();
  });

  it("ignores refresh requests after destroy", async () => {
    const store = new WorkspaceAgentWorkStore("workspace-1");
    store.destroy();

    await store.refresh();

    expect(getWorkspaceAgentWork).not.toHaveBeenCalled();
  });
});
