import { describe, expect, it } from "vitest";

import type { WorktreeVcsSnapshot } from "@ctx/types";

import { buildGitPaneModel, GIT_PANE_REVIEWABLE_FILE_LIMIT } from "./worktreeGitPaneModel";

const makeSnapshot = (overrides?: Partial<WorktreeVcsSnapshot>): WorktreeVcsSnapshot => ({
  worktree_id: "wt-1",
  rev: 1,
  emitted_at_ms: 1,
  base_commit_sha: "base",
  head_commit_sha: "head",
  base_resolution: { kind: "merge_base", target_source: "primary_branch_config", error: null },
  compute_state: "ready",
  summary: { file_count: 0, line_additions: 0, line_deletions: 0, line_count: 0 },
  git_status: {
    branch: "main",
    upstream: "origin/main",
    ahead: 0,
    behind: 0,
    detached: false,
    staged: 0,
    unstaged: 0,
    untracked: 0,
    entries: [],
  },
  touched_files: {
    items: [],
    truncated: false,
    total_count: 0,
  },
  touched_files_state: "not_loaded",
  freshness: "fresh",
  available: true,
  unavailable_reason: null,
  schema_version: 2,
  ...overrides,
});

describe("buildGitPaneModel", () => {
  it("keeps pane inventory non-empty when the badge summary is non-zero but diff detail is absent", () => {
    const model = buildGitPaneModel(
      makeSnapshot({
        summary: { file_count: 1, line_additions: 3, line_deletions: 1, line_count: 4 },
        git_status: {
          branch: "main",
          upstream: "origin/main",
          ahead: 0,
          behind: 0,
          detached: false,
          staged: 0,
          unstaged: 1,
          untracked: 0,
          entries: [{ path: "file.txt", index_status: " ", worktree_status: "M", orig_path: null }],
        },
        touched_files: {
          items: [{ path: "file.txt", index_status: " ", worktree_status: "M", orig_path: null }],
          truncated: false,
          total_count: 1,
        },
        touched_files_state: "ready",
      }),
    );

    expect(model.badgeCount).toBe(1);
    expect(model.totalCount).toBe(1);
    expect(model.sections).toHaveLength(1);
    expect(model.sections[0]?.key).toBe("unstaged");
    expect(model.sections[0]?.files[0]?.path).toBe("file.txt");
  });

  it("falls back to summary count without claiming the pane is empty when inventory is not ready yet", () => {
    const model = buildGitPaneModel(
      makeSnapshot({
        summary: { file_count: 2, line_additions: 4, line_deletions: 0, line_count: 4 },
        touched_files: { items: [], truncated: false, total_count: 2 },
        touched_files_state: "loading",
      }),
    );

    expect(model.badgeCount).toBe(2);
    expect(model.totalCount).toBe(2);
    expect(model.listReady).toBe(false);
    expect(model.loading).toBe(true);
    expect(model.inventoryDemandAllowed).toBe(true);
  });

  it("disables file-by-file inventory demand for large change sets", () => {
    const model = buildGitPaneModel(
      makeSnapshot({
        summary: {
          file_count: GIT_PANE_REVIEWABLE_FILE_LIMIT + 1,
          line_additions: 4,
          line_deletions: 0,
          line_count: 4,
        },
        git_status: {
          branch: "main",
          upstream: "origin/main",
          ahead: 0,
          behind: 0,
          detached: false,
          staged: 0,
          unstaged: 1,
          untracked: 0,
          entries: [{ path: "file.txt", index_status: " ", worktree_status: "M", orig_path: null }],
        },
        touched_files: {
          items: [{ path: "file.txt", index_status: " ", worktree_status: "M", orig_path: null }],
          truncated: true,
          total_count: GIT_PANE_REVIEWABLE_FILE_LIMIT + 1,
        },
        touched_files_state: "ready",
      }),
    );

    expect(model.badgeCount).toBe(GIT_PANE_REVIEWABLE_FILE_LIMIT + 1);
    expect(model.largeChangeSet).toBe(true);
    expect(model.inventoryDemandAllowed).toBe(false);
    expect(model.listReady).toBe(true);
    expect(model.sections).toEqual([]);
    expect(model.largeChangeSetLabel).toContain("301 changed files");
    expect(model.largeChangeSetLabel).toContain("keep the app responsive");
  });

  it("reports capped file inventory without suppressing reviewable change sets", () => {
    const model = buildGitPaneModel(
      makeSnapshot({
        summary: { file_count: 250, line_additions: 4, line_deletions: 0, line_count: 4 },
        touched_files: {
          items: [{ path: "file.txt", index_status: "M", worktree_status: null, orig_path: null }],
          truncated: true,
          total_count: 250,
        },
        touched_files_state: "ready",
      }),
    );

    expect(model.largeChangeSet).toBe(false);
    expect(model.fileListTruncated).toBe(true);
    expect(model.fileListTruncatedLabel).toBe("Showing 1 of 250 changed files.");
    expect(model.inventoryDemandAllowed).toBe(true);
  });

  it("groups staged, unstaged, and untracked files deterministically", () => {
    const model = buildGitPaneModel(
      makeSnapshot({
        summary: { file_count: 3, line_additions: 3, line_deletions: 0, line_count: 3 },
        git_status: {
          branch: "main",
          upstream: "origin/main",
          ahead: 0,
          behind: 0,
          detached: false,
          staged: 1,
          unstaged: 1,
          untracked: 1,
          entries: [
            { path: "staged.txt", index_status: "M", worktree_status: " ", orig_path: null },
            { path: "unstaged.txt", index_status: " ", worktree_status: "M", orig_path: null },
            { path: "new.txt", index_status: "?", worktree_status: null, orig_path: null },
          ],
        },
        touched_files: {
          items: [],
          truncated: false,
          total_count: 3,
        },
        touched_files_state: "ready",
      }),
    );

    expect(model.sections.map((section) => section.key)).toEqual(["staged", "unstaged", "untracked"]);
    expect(model.sections.map((section) => section.files[0]?.path)).toEqual([
      "staged.txt",
      "unstaged.txt",
      "new.txt",
    ]);
  });

  it("does not use working-tree status counts as the merge-base badge fallback", () => {
    const model = buildGitPaneModel(
      makeSnapshot({
        summary: {},
        git_status: {
          branch: "main",
          upstream: "origin/main",
          ahead: 0,
          behind: 0,
          detached: false,
          staged: 1,
          unstaged: 1,
          untracked: 1,
          entries: [
            { path: "staged.txt", index_status: "M", worktree_status: " ", orig_path: null },
            { path: "unstaged.txt", index_status: " ", worktree_status: "M", orig_path: null },
            { path: "new.txt", index_status: "?", worktree_status: null, orig_path: null },
          ],
        },
        touched_files: {
          items: [],
          truncated: false,
          total_count: null,
        },
        touched_files_state: "not_loaded",
      }),
    );

    expect(model.badgeCount).toBe(0);
    expect(model.totalCount).toBe(3);
  });

  it("reports unavailable no-repo worktrees as unavailable instead of empty", () => {
    const model = buildGitPaneModel(
      makeSnapshot({
        available: false,
        unavailable_reason: "no_repo",
      }),
    );

    expect(model.badgeCount).toBe(0);
    expect(model.totalCount).toBe(0);
    expect(model.unavailableLabel).toBe("No Git repository detected for this task yet.");
    expect(model.listReady).toBe(true);
  });

  it("reports unavailable missing primary branch with action-oriented copy", () => {
    const model = buildGitPaneModel(
      makeSnapshot({
        available: false,
        unavailable_reason: "no_target_branch",
      }),
    );

    expect(model.unavailableLabel).toBe("Set a primary branch to compare changes.");
    expect(model.listReady).toBe(true);
  });
});
