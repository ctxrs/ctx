import { describe, expect, it } from "vitest";
import type { ChangeSet, Contribution, PullRequestRef } from "@ctx/types";
import { normalizeWorkspaceAgentWork } from "../../state/workspaceAgentWorkStore";
import { summarizeAgentWorkForTask } from "./agentWorkProjection";

const pr = (number: number): PullRequestRef => ({
  provider: "github",
  owner: "ctxrs",
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

describe("agentWorkProjection", () => {
  it("summarizes task-linked change sets, contributions, pull requests, and latest timestamp", () => {
    const graph = normalizeWorkspaceAgentWork({
      change_sets: [
        changeSet({
          id: "change-set-1",
          created_at: "2026-01-01T10:00:00Z",
          updated_at: "2026-01-02T10:00:00Z",
          pull_requests: [{ pull_request: pr(41), kind: "result" }],
        }),
        changeSet({
          id: "change-set-2",
          created_at: "2026-01-03T10:00:00Z",
          updated_at: "2026-01-03T11:00:00Z",
        }),
      ],
      contributions: [
        contribution({
          id: "contribution-1",
          created_at: "2026-01-01T12:00:00Z",
          subject: { kind: "task", task_id: "task-1" },
          target: { kind: "change_set", change_set_id: "change-set-1" },
        }),
        contribution({
          id: "contribution-2",
          change_set_id: "change-set-2",
          updated_at: "2026-01-03T12:00:00Z",
          subject: { kind: "task", task_id: "task-1" },
          target: { kind: "session", session_id: "session-1" },
        }),
        contribution({
          id: "contribution-3",
          updated_at: "2026-01-04T09:00:00Z",
          subject: { kind: "task", task_id: "task-1" },
          target: { kind: "pull_request", pull_request: pr(42) },
        }),
        contribution({
          id: "contribution-4",
          subject: { kind: "task", task_id: "task-2" },
          target: { kind: "change_set", change_set_id: "change-set-1" },
        }),
      ],
    });

    expect(summarizeAgentWorkForTask(graph, "task-1")).toEqual({
      taskId: "task-1",
      changeSetCount: 2,
      contributionCount: 3,
      linkedPullRequestCount: 2,
      latestUpdateTimestamp: "2026-01-04T09:00:00Z",
    });
  });

  it("returns an empty summary for tasks without indexed agent work", () => {
    const graph = normalizeWorkspaceAgentWork({
      change_sets: [],
      contributions: [],
    });

    expect(summarizeAgentWorkForTask(graph, "task-missing")).toEqual({
      taskId: "task-missing",
      changeSetCount: 0,
      contributionCount: 0,
      linkedPullRequestCount: 0,
      latestUpdateTimestamp: null,
    });
  });

  it("summarizes work reachable through parent and child agent sessions", () => {
    const graph = normalizeWorkspaceAgentWork({
      change_sets: [
        changeSet({
          id: "change-set-from-child",
          updated_at: "2026-01-05T10:00:00Z",
        }),
      ],
      contributions: [
        contribution({
          id: "task-parent-session",
          subject: { kind: "task", task_id: "task-with-subagents" },
          target: { kind: "session", session_id: "session-parent" },
        }),
        contribution({
          id: "parent-child-session",
          subject: { kind: "session", session_id: "session-parent" },
          target: { kind: "session", session_id: "session-child" },
        }),
        contribution({
          id: "child-produced-change-set",
          updated_at: "2026-01-05T11:00:00Z",
          subject: { kind: "session", session_id: "session-child" },
          target: { kind: "change_set", change_set_id: "change-set-from-child" },
        }),
      ],
    });

    expect(summarizeAgentWorkForTask(graph, "task-with-subagents")).toEqual({
      taskId: "task-with-subagents",
      changeSetCount: 1,
      contributionCount: 3,
      linkedPullRequestCount: 0,
      latestUpdateTimestamp: "2026-01-05T11:00:00Z",
    });
  });

  it("summarizes change sets captured from a session worktree", () => {
    const graph = normalizeWorkspaceAgentWork({
      change_sets: [
        changeSet({
          id: "change-set-from-worktree",
          source_worktree_id: "worktree-1",
          updated_at: "2026-01-06T10:00:00Z",
          pull_requests: [{ pull_request: pr(43), kind: "result" }],
        }),
      ],
      contributions: [
        contribution({
          id: "task-session",
          subject: { kind: "task", task_id: "task-with-worktree" },
          target: { kind: "session", session_id: "session-1" },
        }),
        contribution({
          id: "session-worktree",
          updated_at: "2026-01-06T11:00:00Z",
          subject: { kind: "session", session_id: "session-1" },
          target: { kind: "worktree", worktree_id: "worktree-1" },
        }),
      ],
    });

    expect(summarizeAgentWorkForTask(graph, "task-with-worktree")).toEqual({
      taskId: "task-with-worktree",
      changeSetCount: 1,
      contributionCount: 2,
      linkedPullRequestCount: 1,
      latestUpdateTimestamp: "2026-01-06T11:00:00Z",
    });
  });

  it("does not summarize another task through a shared worktree", () => {
    const graph = normalizeWorkspaceAgentWork({
      change_sets: [
        changeSet({
          id: "change-set-task-1",
          source_worktree_id: "shared-worktree",
          updated_at: "2026-01-07T10:00:00Z",
          pull_requests: [{ pull_request: pr(44), kind: "result" }],
        }),
        changeSet({
          id: "change-set-task-2",
          source_worktree_id: "shared-worktree",
          updated_at: "2026-01-08T10:00:00Z",
          pull_requests: [{ pull_request: pr(45), kind: "result" }],
        }),
      ],
      contributions: [
        contribution({
          id: "task-1-session",
          subject: { kind: "task", task_id: "task-1" },
          target: { kind: "session", session_id: "session-1" },
        }),
        contribution({
          id: "session-1-worktree",
          updated_at: "2026-01-07T11:00:00Z",
          subject: { kind: "session", session_id: "session-1" },
          target: { kind: "worktree", worktree_id: "shared-worktree" },
        }),
        contribution({
          id: "session-1-change-set",
          subject: { kind: "session", session_id: "session-1" },
          target: { kind: "change_set", change_set_id: "change-set-task-1" },
        }),
        contribution({
          id: "task-2-session",
          subject: { kind: "task", task_id: "task-2" },
          target: { kind: "session", session_id: "session-2" },
        }),
        contribution({
          id: "session-2-worktree",
          updated_at: "2026-01-08T11:00:00Z",
          subject: { kind: "session", session_id: "session-2" },
          target: { kind: "worktree", worktree_id: "shared-worktree" },
        }),
        contribution({
          id: "session-2-change-set",
          subject: { kind: "session", session_id: "session-2" },
          target: { kind: "change_set", change_set_id: "change-set-task-2" },
        }),
      ],
    });

    expect(summarizeAgentWorkForTask(graph, "task-1")).toEqual({
      taskId: "task-1",
      changeSetCount: 1,
      contributionCount: 3,
      linkedPullRequestCount: 1,
      latestUpdateTimestamp: "2026-01-07T11:00:00Z",
    });
  });
});
