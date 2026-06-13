import { describe, expect, it } from "vitest";
import type { WorkbenchListItem } from "./SessionPage.types";
import { classifyWorkbenchThreadProjectionOp, createWorkbenchLayoutProjectionOp } from "./sessionThreadProjection";

const makeAssistant = (content: string): WorkbenchListItem => ({
  id: "assistant-turn-1-pending",
  kind: "assistant",
  turn_id: "turn-1",
  created_at: "2026-03-19T00:00:00.000Z",
  content,
  thought: "",
  is_complete: false,
});

const makeTurnStatus = (): WorkbenchListItem => ({
  id: "turn-status-turn-1",
  kind: "turn_status",
  turn_id: "turn-1",
  created_at: "2026-03-19T00:00:01.000Z",
  started_at: "2026-03-19T00:00:00.000Z",
  updated_at: "2026-03-19T00:00:01.000Z",
  status: "running",
  custom_status: null,
  assistant_messages_content: "",
});

const makeUserMessage = (expanded = false): WorkbenchListItem => ({
  id: "message-user-1",
  kind: "message",
  role: "user",
  created_at: "2026-03-19T00:00:02.000Z",
  content: expanded ? "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8\nline 9\nline 10\nline 11\nline 12\nline 13\nline 14\nline 15\nline 16\nline 17\nline 18\nline 19\nline 20\nline 21" : "short",
  attachments: [],
});

const makeToolGroup = (tools: Array<Extract<WorkbenchListItem, { kind: "tool" }>> = []): WorkbenchListItem => ({
  id: "tool-group-turn-1",
  kind: "tool_group",
  turn_id: "turn-1",
  created_at: "2026-03-19T00:00:00.000Z",
  updated_at: "2026-03-19T00:00:00.000Z",
  thought: "",
  tool_total: 1,
  tool_pending: 0,
  tool_running: 0,
  tool_completed: 0,
  tool_failed: 0,
  tools,
});

const makeTool = (): Extract<WorkbenchListItem, { kind: "tool" }> => ({
  id: "tool-1",
  kind: "tool",
  created_at: "2026-03-19T00:00:00.000Z",
  updated_at: "2026-03-19T00:00:00.000Z",
  tool_call_id: "tool-call-1",
  tool_kind: "shell",
  provider_tool_name: "shell",
  title: "pnpm test",
  subtitle: "",
  status: "completed",
  input: "",
  output_text: "",
  locations: [],
  raw: null,
  updates_seen: 1,
});

describe("classifyWorkbenchThreadProjectionOp", () => {
  it("expands localized streaming remeasure to include the adjacent row boundary", () => {
    const status = makeTurnStatus();
    const current = [makeAssistant("short reply"), status];
    const next = [makeAssistant("short reply\nwith another line"), status];

    const op = classifyWorkbenchThreadProjectionOp({
      current,
      next,
      projectionRevision: 2,
    });

    expect(op.kind).toBe("reconcile");
    expect(op.changedItemIds).toEqual(["assistant-turn-1-pending"]);
    expect(op.remeasureItemIds).toEqual(["assistant-turn-1-pending", "turn-status-turn-1"]);
  });

  it("does not classify a mixed streaming growth plus tail append as append-only", () => {
    const status = makeTurnStatus();
    const appendedTool = makeTool();
    const current = [makeAssistant("short reply"), status];
    const next = [makeAssistant("short reply\nwith another line"), status, appendedTool];

    const op = classifyWorkbenchThreadProjectionOp({
      current,
      next,
      projectionRevision: 3,
    });

    expect(op.kind).toBe("reconcile");
    expect(op.changedItemIds).toEqual([
      "assistant-turn-1-pending",
      "tool-1",
    ]);
    expect(op.remeasureItemIds).toEqual([
      "assistant-turn-1-pending",
      "turn-status-turn-1",
      "tool-1",
    ]);
  });

  it("keeps tool hydration remeasurement localized to the changed rows", () => {
    const current = [
      makeAssistant("stable markdown"),
      makeToolGroup(),
      makeTurnStatus(),
    ];
    const next = [
      current[0]!,
      makeToolGroup([makeTool()]),
      current[2]!,
    ];

    const op = classifyWorkbenchThreadProjectionOp({
      current,
      next,
      projectionRevision: 4,
    });

    expect(op.kind).toBe("hydrate_tools");
    expect(op.changedItemIds).toEqual(["tool-group-turn-1"]);
    expect(op.remeasureItemIds).toEqual(["tool-group-turn-1"]);
  });

  it("classifies pure prefix growth as prepend history", () => {
    const current = [
      makeUserMessage(false),
      makeTurnStatus(),
    ];
    const next = [
      {
        ...makeUserMessage(false),
        id: "message-user-0",
        created_at: "2026-03-18T23:59:59.000Z",
      },
      ...current,
    ];

    const op = classifyWorkbenchThreadProjectionOp({
      current,
      next,
      projectionRevision: 5,
    });

    expect(op.kind).toBe("prepend_history");
    expect(op.changedItemIds).toEqual(["message-user-0"]);
    expect(op.remeasureItemIds).toEqual(["message-user-0", "message-user-1"]);
  });
});

describe("createWorkbenchLayoutProjectionOp", () => {
  it("expands layout toggles to include adjacent rows for remeasurement", () => {
    const listItems = [makeUserMessage(false), makeTurnStatus()];

    const op = createWorkbenchLayoutProjectionOp({
      listItems,
      previousUiState: {
        expandedTurnHeaders: {},
        expandedTurnDetailsById: {},
        expandedToolById: {},
        expandedMessageById: {},
        turnToolsLoading: [],
      },
      nextUiState: {
        expandedTurnHeaders: {},
        expandedTurnDetailsById: {},
        expandedToolById: {},
        expandedMessageById: { "message-user-1": true },
        turnToolsLoading: [],
      },
      projectionRevision: 3,
    });

    expect(op.kind).toBe("toggle_expansion");
    expect(op.changedItemIds).toEqual(["message-user-1"]);
    expect(op.remeasureItemIds).toEqual(["message-user-1", "turn-status-turn-1"]);
  });
});
