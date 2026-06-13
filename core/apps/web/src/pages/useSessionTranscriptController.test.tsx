// @vitest-environment jsdom

import { renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { WorkbenchListItem } from "./SessionPage.types";
import { useSessionTranscriptController } from "./useSessionTranscriptController";

const usePretextVirtualizerSessionControllerMock = vi.fn(
  (_args: unknown) => ({
    methodsRef: { current: null },
    context: { loaded: true, loadingOlder: false },
    initialData: [],
    initialLocation: null,
    onScroll: vi.fn(),
    onRenderedDataChange: vi.fn(),
  }),
);

vi.mock("./usePretextVirtualizerSessionController", () => ({
  usePretextVirtualizerSessionController: (args: unknown) =>
    usePretextVirtualizerSessionControllerMock(args),
}));

const listItems: WorkbenchListItem[] = [
  {
    kind: "message",
    id: "message-1",
    role: "user",
    content: Array.from({ length: 24 }, (_, index) => `line ${index + 1}`).join("\n"),
    attachments: [],
    created_at: "2026-04-06T00:00:00Z",
  },
];

const toolListItems: WorkbenchListItem[] = [
  {
    kind: "tool",
    id: "tool-1",
    created_at: "2026-04-06T00:00:00Z",
    updated_at: "2026-04-06T00:00:00Z",
    tool_call_id: "call-1",
    tool_kind: "execute",
    title: "Run pwd",
    status: "completed",
    locations: [],
    input: { command: "pwd" },
    output_text: "output",
    raw: {},
    updates_seen: 1,
  },
];

describe("useSessionTranscriptController", () => {
  it("merges layout-toggle revisions into the transcript projection op and delegates scroll control", () => {
    const baseUiState = {
      expandedTurnHeaders: {},
      expandedTurnDetailsById: {},
      expandedToolById: {},
      expandedMessageById: {},
      turnToolsLoading: [],
    };
    const { result, rerender } = renderHook(
      ({ expanded }: { expanded: boolean }) =>
        useSessionTranscriptController({
          sessionId: "session-1",
          isActive: true,
          loaded: true,
          listItems,
          canLoadOlder: false,
          loadOlder: vi.fn(async () => {}),
          showDebug: false,
          onAtBottomChange: vi.fn(),
          uiState: {
            ...baseUiState,
            expandedMessageById: expanded ? { "message-1": true } : {},
          },
          workbenchThreadOp: null,
          projectionRevision: 7,
        }),
      { initialProps: { expanded: false } },
    );

    expect(result.current.threadProjectionOp.kind).toBe("noop");
    expect(result.current.itemIdentity(listItems[0]!)).toBe("message-1");
    expect(result.current.itemKey(listItems[0]!)).toBe("message-1");
    expect(usePretextVirtualizerSessionControllerMock).toHaveBeenCalledWith(
      expect.objectContaining({
        sessionId: "session-1",
        listItems,
        canLoadOlder: false,
      }),
    );

    rerender({ expanded: true });

    expect(result.current.threadProjectionOp.kind).toBe("toggle_expansion");
    expect(result.current.threadProjectionOp.changedItemIds).toEqual(["message-1"]);
    expect(result.current.threadProjectionOp.remeasureItemIds).toEqual(["message-1"]);
  });

  it("emits a layout projection op when verbosity changes tool row height", () => {
    const baseUiState = {
      expandedTurnHeaders: {},
      expandedTurnDetailsById: {},
      expandedToolById: { "tool-1": true },
      expandedMessageById: {},
      turnToolsLoading: [],
      verbosity: "default",
    };

    const { result, rerender } = renderHook(
      ({ verbosity }: { verbosity: string }) =>
        useSessionTranscriptController({
          sessionId: "session-1",
          isActive: true,
          loaded: true,
          listItems: toolListItems,
          canLoadOlder: false,
          loadOlder: vi.fn(async () => {}),
          showDebug: false,
          onAtBottomChange: vi.fn(),
          uiState: {
            ...baseUiState,
            verbosity,
          },
          workbenchThreadOp: null,
          projectionRevision: 11,
        }),
      { initialProps: { verbosity: "default" } },
    );

    rerender({ verbosity: "verbose" });

    expect(result.current.threadProjectionOp.kind).toBe("toggle_expansion");
    expect(result.current.threadProjectionOp.changedItemIds).toEqual(["tool-1"]);
    expect(result.current.threadProjectionOp.remeasureItemIds).toEqual(["tool-1"]);
  });
});
