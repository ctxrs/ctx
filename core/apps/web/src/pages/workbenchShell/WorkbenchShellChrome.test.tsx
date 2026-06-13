import React from "react";
import { fireEvent, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { describe, expect, it, vi } from "vitest";

import { WorkbenchConversationMenu, WorkbenchSidebar, WorkbenchTopbar } from "./WorkbenchShellChrome";

describe("WorkbenchTopbar", () => {
  it("renders a visible hamburger trigger for mobile task navigation", () => {
    render(
      <MemoryRouter>
        <WorkbenchTopbar
          workspaceId="workspace-1"
          workspaceTitle="ctx-monorepo"
          showDebugIds={false}
          debugIdLabel=""
          onCopyDebugIds={vi.fn()}
          onToggleSidebar={vi.fn()}
          sidebarOpen={false}
        />
      </MemoryRouter>,
    );

    const trigger = screen.getByRole("button", { name: "Open task list" });
    expect(trigger.querySelector("svg")).not.toBeNull();
  });
});

describe("WorkbenchConversationMenu", () => {
  it("renders Copy Task ID immediately after Copy Worktree Location", () => {
    render(
      <WorkbenchConversationMenu
        convoMenu={{ style: {} }}
        convoMenuRef={{ current: null }}
        activeSessionId="session-1"
        activeTaskId="task-1"
        canCopyTaskId
        copyTranscriptBusy={false}
        transcriptSpinnerDelayMs={0}
        canCopyWorktree
        archiveConversationDisabled={false}
        onExportTranscript={vi.fn()}
        onCopyTranscript={vi.fn()}
        onExportSessionLog={vi.fn()}
        onCopySessionLog={vi.fn()}
        onCopyWorktreeLocation={vi.fn()}
        onCopyTaskId={vi.fn()}
        onArchiveConversation={vi.fn()}
      />,
    );

    const menuItems = screen.getAllByRole("menuitem");
    const labels = menuItems.map((item) => item.textContent?.trim() ?? "");
    const worktreeIndex = labels.indexOf("Copy Worktree Location");
    expect(worktreeIndex).toBeGreaterThanOrEqual(0);
    expect(labels[worktreeIndex + 1]).toBe("Copy Task ID");
  });
});

describe("WorkbenchSidebar", () => {
  const renderSidebar = (overrides: Partial<React.ComponentProps<typeof WorkbenchSidebar>> = {}) => {
    const props: React.ComponentProps<typeof WorkbenchSidebar> = {
      collapsed: false,
      taskSearchRef: { current: null },
      taskQuery: "",
      onTaskQueryChange: vi.fn(),
      onNewTask: vi.fn(),
      taskListVirtuosoKey: "tasks",
      taskListItems: [],
      initialTaskListItemCount: undefined,
      computeTaskListItemKey: () => "task",
      renderTaskListItem: () => null,
      taskListContext: {
        archivedCollapsed: false,
        archivedFetchState: "idle",
        hasMoreArchived: false,
        onLoadMoreArchived: vi.fn(),
      },
      onTaskListRangeChanged: vi.fn(),
      onExpandSidebar: vi.fn(),
      onCollapseSidebar: vi.fn(),
      onSidebarResizerMouseDown: vi.fn(),
      onResetSidebarWidth: vi.fn(),
      ...overrides,
    };

    render(<WorkbenchSidebar {...props} />);
  };

  it("disables browser text assistance on task search", () => {
    renderSidebar();

    const input = screen.getByTestId("workbench-task-search");
    expect(input).toHaveAttribute("autocomplete", "off");
    expect(input).toHaveAttribute("autocorrect", "off");
    expect(input).toHaveAttribute("autocapitalize", "none");
    expect(input).toHaveAttribute("spellcheck", "false");
  });

  it("closes the mobile task drawer on a left swipe", () => {
    const onSwipeClose = vi.fn();
    renderSidebar({ mobileMode: true, onSwipeClose });

    const sidebar = document.querySelector(".wb-sidebar");
    expect(sidebar).not.toBeNull();
    if (!sidebar) throw new Error("sidebar missing");

    fireEvent.touchStart(sidebar, {
      touches: [{ clientX: 340, clientY: 120 }],
    });
    fireEvent.touchMove(sidebar, {
      touches: [{ clientX: 250, clientY: 130 }],
    });
    fireEvent.touchEnd(sidebar, {
      changedTouches: [{ clientX: 250, clientY: 130 }],
    });

    expect(onSwipeClose).toHaveBeenCalledTimes(1);
  });
});
