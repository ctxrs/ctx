// @vitest-environment jsdom

import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { WorkbenchActiveTaskView } from "./WorkbenchActiveTaskView";

vi.mock("../../components/ArtifactsPane", () => ({
  ArtifactsPane: () => <div data-testid="artifacts-pane" />,
}));

vi.mock("../../components/DiffReviewPane", () => ({
  DiffReviewPane: () => <div data-testid="diff-review-pane" />,
}));

vi.mock("../../components/SessionsPane", () => ({
  SessionsPane: () => <div data-testid="sessions-pane" />,
}));

vi.mock("./WorkbenchPage.sessionSlot", () => ({
  WorkbenchSessionSlot: ({ sessionId }: { sessionId: string }) => <div data-testid="session-slot">{sessionId}</div>,
}));

vi.mock("./WorkbenchSessionHeader", () => ({
  WorkbenchSessionHeader: () => <div data-testid="session-header" />,
}));

vi.mock("./WorkbenchSessionLoadIssues", () => ({
  WorkbenchSessionLoadIssues: () => null,
}));

const makeProps = () => ({
  sessionsCount: 1,
  showSingleSessionHeader: false,
  singleSessionTitle: "Task",
  worktreeChip: {
    worktreeLabel: "",
    worktreePath: "",
    canCopyWorktree: false,
    canOpenTerminal: false,
  },
  worktreeCopied: false,
  showArtifactsPane: false,
  showReviewPane: false,
  terminalOpen: false,
  artifactsCount: 0,
  diffBadgeCount: 0,
  onCopyWorktreeLocation: () => {},
  onOpenWorktreeTerminal: () => {},
  onToggleArtifactsPane: () => {},
  onToggleDiffPane: () => {},
  onToggleTerminalPanel: () => {},
  onOpenConvoMenu: () => {},
  sessionLoadIssues: [],
  onRetrySessionLoads: () => {},
  activeSessionId: null,
  activeSessionRenderable: false,
  optimisticFailure: null,
  rightPaneOpen: false,
  onSplitterMouseDown: () => {},
  diffWidth: 480,
  showSessionsPane: false,
  sessionSections: [],
  activeSessionKind: "web",
  onSectionChange: () => {},
  activeWebSessionId: null,
  onSelectWebSession: () => {},
  daemonBaseUrl: "http://127.0.0.1:4399",
  webSessionsLoading: false,
  hasDiff: false,
  gitPaneModel: {
    badgeCount: 0,
    totalCount: 0,
    visibleFileCount: 0,
    available: true,
    unavailableReason: null,
    unavailableLabel: null,
    loading: false,
    computeError: null,
    listReady: true,
    inventoryDemandAllowed: false,
    largeChangeSet: false,
    largeChangeSetLabel: null,
    fileListTruncated: false,
    fileListTruncatedLabel: null,
    sections: [],
  },
  diffLoading: false,
  diffSummaryError: null,
  diffTooLarge: false,
  diffTooLargeLabel: null,
  activeSessionDiff: "",
  activeDiffContentError: null,
  diffEmptyLabel: "No diff",
  artifacts: [],
  artifactsLoading: false,
  artifactsError: null,
  onRetryArtifactsLoad: () => {},
});

describe("WorkbenchActiveTaskView", () => {
  it("renders an explicit loading state while the session is not yet renderable", () => {
    render(
      <WorkbenchActiveTaskView
        {...makeProps()}
        activeSessionId="session-1"
        activeSessionRenderable={false}
      />,
    );

    expect(screen.queryByTestId("session-slot")).not.toBeInTheDocument();
    const hydratingSlot = document.querySelector(".wb-session-slot--hydrating");
    expect(hydratingSlot).not.toBeNull();
    expect(screen.getByText("Loading conversation...")).toBeInTheDocument();
  });

  it("renders the session slot once the session is renderable", () => {
    render(
      <WorkbenchActiveTaskView
        {...makeProps()}
        activeSessionId="session-1"
        activeSessionRenderable
      />,
    );

    expect(screen.getByTestId("session-slot")).toHaveTextContent("session-1");
  });

  it("keeps the current session slot mounted through transient non-renderable refreshes", () => {
    const { rerender } = render(
      <WorkbenchActiveTaskView
        {...makeProps()}
        activeSessionId="session-1"
        activeSessionRenderable
      />,
    );

    expect(screen.getByTestId("session-slot")).toHaveTextContent("session-1");

    rerender(
      <WorkbenchActiveTaskView
        {...makeProps()}
        activeSessionId="session-1"
        activeSessionRenderable={false}
      />,
    );

    expect(screen.getByTestId("session-slot")).toHaveTextContent("session-1");
    expect(document.querySelector(".wb-session-slot--hydrating")).toBeNull();
  });

  it("shows the empty-state prompt only when no session is selected", () => {
    render(<WorkbenchActiveTaskView {...makeProps()} />);

    expect(screen.getByText("Select a session to view this task.")).toBeInTheDocument();
  });
});
