import { fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { TaskRow } from "./WorkbenchPage.taskRow";
import { HARNESS_CATALOG } from "../../utils/harnessCatalog";

function mockRaf() {
  vi.spyOn(window, "requestAnimationFrame").mockImplementation((cb: FrameRequestCallback) => {
    return window.setTimeout(() => cb(performance.now()), 0) as unknown as number;
  });
}

describe("TaskRow rename draft", () => {
  beforeEach(() => {
    mockRaf();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("persists a draft across remounts during rename", () => {
    const drafts = new Map<string, string>();
    const getRenameDraft = (taskId: string, fallback: string) => drafts.get(taskId) ?? fallback;
    const setRenameDraft = (taskId: string, nextValue: string) => {
      drafts.set(taskId, nextValue);
    };

    const baseProps = {
      taskId: "task-1",
      title: "Initial title",
      archived: false,
      archiving: false,
      archivePending: false,
      archivePendingAction: null,
      statusKind: "idle" as const,
      selected: false,
      hovered: false,
      isRenaming: true,
      working: false,
      dotKind: null,
      ageIso: new Date().toISOString(),
      providerCount: 0,
      harnesses: [] as Array<(typeof HARNESS_CATALOG)[number]>,
      getRenameDraft,
      setRenameDraft,
      onFocusTask: vi.fn(),
      onOpenMenu: vi.fn(),
      onToggleArchive: vi.fn().mockResolvedValue(undefined),
      onHoverEnter: vi.fn(),
      onHoverLeave: vi.fn(),
      onCancelRename: vi.fn(),
      onCommitRename: vi.fn(),
    };

    const { rerender } = render(<TaskRow {...baseProps} key="a" />);
    const input = screen.getByLabelText("Rename task") as HTMLInputElement;
    fireEvent.change(input, { target: { value: "Draft title" } });
    expect(input.value).toBe("Draft title");

    rerender(<TaskRow {...baseProps} key="b" title="Server update" />);
    const remountedInput = screen.getByLabelText("Rename task") as HTMLInputElement;
    expect(remountedInput.value).toBe("Draft title");
    expect(remountedInput).toHaveAttribute("autocomplete", "off");
    expect(remountedInput).toHaveAttribute("autocorrect", "off");
    expect(remountedInput).toHaveAttribute("autocapitalize", "none");
    expect(remountedInput).toHaveAttribute("spellcheck", "false");
  });

  it("opens the workbench task menu on context menu for real tasks", () => {
    const onOpenMenu = vi.fn();

    render(
      <TaskRow
        taskId="task-1"
        title="Initial title"
        archived={false}
        archivePending={false}
        archivePendingAction={null}
        statusKind="idle"
        selected={false}
        hovered={false}
        isRenaming={false}
        ageIso={new Date().toISOString()}
        providerCount={0}
        harnesses={[]}
        getRenameDraft={(_, fallback) => fallback}
        setRenameDraft={vi.fn()}
        onFocusTask={vi.fn()}
        onOpenMenu={onOpenMenu}
        onToggleArchive={vi.fn().mockResolvedValue(undefined)}
        onHoverEnter={vi.fn()}
        onHoverLeave={vi.fn()}
        onCancelRename={vi.fn()}
        onCommitRename={vi.fn()}
      />,
    );

    const row = screen.getByRole("listitem", { name: "Initial title" });
    const event = new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 120, clientY: 64 });

    row.dispatchEvent(event);

    expect(event.defaultPrevented).toBe(true);
    expect(onOpenMenu).toHaveBeenCalledWith("task-1", { x: 120, y: 64 });
  });

  it("passes the resolved session id when focusing a task row", () => {
    const onFocusTask = vi.fn();

    render(
      <TaskRow
        taskId="task-1"
        sessionId="session-1"
        title="Initial title"
        archived={false}
        archivePending={false}
        archivePendingAction={null}
        statusKind="idle"
        selected={false}
        hovered={false}
        isRenaming={false}
        ageIso={new Date().toISOString()}
        providerCount={0}
        harnesses={[]}
        getRenameDraft={(_, fallback) => fallback}
        setRenameDraft={vi.fn()}
        onFocusTask={onFocusTask}
        onOpenMenu={vi.fn()}
        onToggleArchive={vi.fn().mockResolvedValue(undefined)}
        onHoverEnter={vi.fn()}
        onHoverLeave={vi.fn()}
        onCancelRename={vi.fn()}
        onCommitRename={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("listitem", { name: "Initial title" }));

    expect(onFocusTask).toHaveBeenCalledWith("task-1", "session-1");
  });

  it("suppresses the native context menu for optimistic rows without opening task actions", () => {
    const onOpenMenu = vi.fn();

    render(
      <TaskRow
        taskId="task-1"
        title="New Task"
        archived={false}
        archivePending={false}
        archivePendingAction={null}
        statusKind="working"
        selected={false}
        hovered={false}
        isRenaming={false}
        ageIso={new Date().toISOString()}
        providerCount={0}
        harnesses={[]}
        getRenameDraft={(_, fallback) => fallback}
        setRenameDraft={vi.fn()}
        onFocusTask={vi.fn()}
        onOpenMenu={onOpenMenu}
        menuEnabled={false}
        archiveEnabled={false}
        onToggleArchive={vi.fn().mockResolvedValue(undefined)}
        onHoverEnter={vi.fn()}
        onHoverLeave={vi.fn()}
        onCancelRename={vi.fn()}
        onCommitRename={vi.fn()}
      />,
    );

    const row = screen.getByRole("listitem", { name: "New Task" });
    const title = row.querySelector(".wb-task-title");
    expect(title).not.toBeNull();
    const event = new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 88, clientY: 44 });

    title?.dispatchEvent(event);

    expect(event.defaultPrevented).toBe(true);
    expect(onOpenMenu).not.toHaveBeenCalled();
  });
});
