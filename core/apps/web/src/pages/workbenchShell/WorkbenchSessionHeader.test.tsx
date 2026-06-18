import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { WorkbenchSessionHeader } from "./WorkbenchSessionHeader";

const baseProps = {
  busy: false,
  title: "Task title",
  worktreeChip: {
    worktreeLabel: "main",
    worktreePath: "/tmp/repo",
    canCopyWorktree: true,
    canOpenTerminal: true,
  },
  worktreeCopied: false,
  showArtifactsPane: false,
  showReviewPane: false,
  terminalOpen: false,
  artifactsCount: 0,
  diffBadgeCount: 0,
  onCopyWorktreeLocation: vi.fn(),
  onOpenWorktreeTerminal: vi.fn(),
  onToggleArtifactsPane: vi.fn(),
  onToggleDiffPane: vi.fn(),
  onToggleTerminalPanel: vi.fn(),
  onOpenConvoMenu: vi.fn(),
};

describe("WorkbenchSessionHeader", () => {
  it("renders active task agent work summary chips", () => {
    render(
      <WorkbenchSessionHeader
        {...baseProps}
        agentWorkSummary={{
          taskId: "task-1",
          changeSetCount: 1,
          contributionCount: 2,
          linkedPullRequestCount: 1,
          latestUpdateTimestamp: null,
        }}
      />,
    );

    expect(screen.getByLabelText("Agent work summary")).toHaveTextContent("1 change");
    expect(screen.getByLabelText("Agent work summary")).toHaveTextContent("1 PR");
  });
});
