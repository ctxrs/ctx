import { describe, expect, it } from "vitest";
import type { WorkbenchListItem } from "./SessionPage.types";
import { SESSION_TRANSCRIPT_LAYOUT_ENGINE_REVISION } from "./sessionThread/sessionMarkdownMeasurement";
import { getWorkbenchTurnHeaderDisplayPlainText } from "./sessionThread/transcriptRowLayoutModel";
import {
  collectWorkbenchToolGroupExpansionIds,
  getWorkbenchMessageListLayoutRevision,
  getWorkbenchListItemKey,
  getWorkbenchListItemSizeCacheKey,
  resolveWorkbenchMessageExpanded,
  resolveWorkbenchTurnHeaderExpanded,
  type WorkbenchMessageListUiState,
} from "./sessionMessageListItemIdentity";

const baseUiState: WorkbenchMessageListUiState = {
  expandedTurnHeaders: {},
  expandedTurnDetailsById: {},
  expandedToolById: {},
  expandedMessageById: {},
  turnToolsLoading: [],
};

describe("sessionMessageListItemIdentity", () => {
  it("defaults long user messages to collapsed and toggles their height key when expanded", () => {
    const content = Array.from({ length: 24 }, (_, index) => `line ${index + 1}`).join("\n");
    const item: Extract<WorkbenchListItem, { kind: "message" }> = {
      kind: "message",
      id: "message-1",
      role: "user",
      content,
      attachments: [],
      created_at: "2025-01-01T00:00:00.000Z",
    };

    expect(resolveWorkbenchMessageExpanded(item, baseUiState.expandedMessageById)).toBe(false);
    expect(getWorkbenchListItemKey(item, baseUiState)).toContain(":message:collapsed:");

    const expandedUiState: WorkbenchMessageListUiState = {
      ...baseUiState,
      expandedMessageById: { "message-1": true },
    };
    expect(resolveWorkbenchMessageExpanded(item, expandedUiState.expandedMessageById)).toBe(true);
    expect(getWorkbenchListItemKey(item, expandedUiState)).toContain(":message:expanded:");
  });

  it("keeps short messages fixed", () => {
    const item: Extract<WorkbenchListItem, { kind: "message" }> = {
      kind: "message",
      id: "message-1",
      role: "user",
      content: "short",
      attachments: [],
      created_at: "2025-01-01T00:00:00.000Z",
    };

    expect(resolveWorkbenchMessageExpanded(item, baseUiState.expandedMessageById)).toBe(true);
    expect(getWorkbenchListItemKey(item, baseUiState)).toContain(":message:fixed:");
  });

  it("changes expandable message height keys when visible content or attachments change", () => {
    const content = Array.from({ length: 24 }, (_, index) => `line ${index + 1}`).join("\n");
    const baseItem: Extract<WorkbenchListItem, { kind: "message" }> = {
      kind: "message",
      id: "message-1",
      role: "user",
      content,
      attachments: [],
      created_at: "2025-01-01T00:00:00.000Z",
    };
    const attachmentItem: Extract<WorkbenchListItem, { kind: "message" }> = {
      ...baseItem,
      attachments: [{ kind: "image_ref", blob_id: "blob-1", mime_type: "image/png", name: "one.png" }],
    };
    const visibleContentItem: Extract<WorkbenchListItem, { kind: "message" }> = {
      ...baseItem,
      content: `${content}\nline 25`,
    };

    expect(getWorkbenchListItemKey(baseItem, baseUiState)).not.toBe(
      getWorkbenchListItemKey(attachmentItem, baseUiState),
    );
    expect(getWorkbenchListItemSizeCacheKey(baseItem, baseUiState)).not.toBe(
      getWorkbenchListItemSizeCacheKey(attachmentItem, baseUiState),
    );
    expect(getWorkbenchListItemKey(baseItem, baseUiState)).toBe(
      getWorkbenchListItemKey(visibleContentItem, baseUiState),
    );

    const expandedUiState: WorkbenchMessageListUiState = {
      ...baseUiState,
      expandedMessageById: { "message-1": true },
    };
    expect(getWorkbenchListItemKey(baseItem, expandedUiState)).not.toBe(
      getWorkbenchListItemKey(visibleContentItem, expandedUiState),
    );
    expect(getWorkbenchListItemSizeCacheKey(baseItem, expandedUiState)).not.toBe(
      getWorkbenchListItemSizeCacheKey(visibleContentItem, expandedUiState),
    );
  });

  it("treats long wrapped messages with unchanged collapsed content as fixed-height identity", () => {
    const content = ["there are a few different issues on latest ctx app that i want to investigate with you", "", "wrapped paragraph ".repeat(160)].join("\n");
    const item: Extract<WorkbenchListItem, { kind: "message" }> = {
      kind: "message",
      id: "message-wrapped",
      role: "user",
      content,
      attachments: [
        { kind: "image_ref", blob_id: "blob-1", mime_type: "image/png", name: "one.png" },
        { kind: "image_ref", blob_id: "blob-2", mime_type: "image/png", name: "two.png" },
        { kind: "image_ref", blob_id: "blob-3", mime_type: "image/png", name: "three.png" },
      ],
      created_at: "2025-01-01T00:00:00.000Z",
    };
    const expandedUiState: WorkbenchMessageListUiState = {
      ...baseUiState,
      expandedMessageById: { "message-wrapped": true },
    };

    expect(resolveWorkbenchMessageExpanded(item, baseUiState.expandedMessageById)).toBe(true);
    expect(resolveWorkbenchMessageExpanded(item, expandedUiState.expandedMessageById)).toBe(true);
    expect(getWorkbenchListItemKey(item, baseUiState)).toContain(":message:fixed:");
    expect(getWorkbenchListItemKey(item, baseUiState)).toBe(getWorkbenchListItemKey(item, expandedUiState));
    expect(getWorkbenchListItemSizeCacheKey(item, baseUiState)).toBe(
      getWorkbenchListItemSizeCacheKey(item, expandedUiState),
    );
  });

  it("uses header expansion state only for long turn headers", () => {
    const item: Extract<WorkbenchListItem, { kind: "turn_header" }> = {
      kind: "turn_header",
      id: "turn-header-1",
      header: {
        id: "header-1",
        content: "line 1\nline 2\nline 3\nline 4\nline 5",
        plain_text: "line 1\nline 2\nline 3\nline 4\nline 5",
        attachments: [],
        created_at: "2025-01-01T00:00:00.000Z",
      },
    };

    expect(resolveWorkbenchTurnHeaderExpanded(item, baseUiState.expandedTurnHeaders)).toBe(false);
    expect(getWorkbenchListItemKey(item, baseUiState)).toContain(":turn-header:collapsed:");

    const expandedUiState: WorkbenchMessageListUiState = {
      ...baseUiState,
      expandedTurnHeaders: { "header-1": true },
    };
    expect(resolveWorkbenchTurnHeaderExpanded(item, expandedUiState.expandedTurnHeaders)).toBe(true);
    expect(getWorkbenchListItemKey(item, expandedUiState)).toContain(":turn-header:expanded:");
  });

  it("normalizes markdown turn-header content into display plain text when plain_text is absent", () => {
    const item: Extract<WorkbenchListItem, { kind: "turn_header" }> = {
      kind: "turn_header",
      id: "turn-header-markdown",
      header: {
        id: "header-markdown",
        content: "# Heading\n\n- bullet",
        attachments: [],
        created_at: "2025-01-01T00:00:00.000Z",
      },
    };

    expect(getWorkbenchListItemKey(item, baseUiState)).toContain(":turn-header:collapsed:");
  });

  it("normalizes markdown-like explicit turn-header plain text into visible label text", () => {
    const header = {
      id: "header-explicit-markdown",
      content: "",
      plain_text:
        "- [Predicting the Popularity of Social News Posts](https://cs229.stanford.edu/proj2012/MaguireMichelson-PredictingThePopularityOfSocialNewsPosts.pdf) reports `85% accuracy`",
      attachments: [],
      created_at: "2025-01-01T00:00:00.000Z",
    };

    expect(getWorkbenchTurnHeaderDisplayPlainText(header)).toBe(
      "- Predicting the Popularity of Social News Posts reports 85% accuracy",
    );
  });

  it("captures nested tool expansion inside a tool-group height key", () => {
    const item: Extract<WorkbenchListItem, { kind: "tool_group" }> = {
      kind: "tool_group",
      id: "tool-group-1",
      turn_id: "turn-1",
      created_at: "2025-01-01T00:00:00.000Z",
      updated_at: "2025-01-01T00:00:00.000Z",
      tool_total: 1,
      tool_pending: 0,
      tool_running: 0,
      tool_completed: 1,
      tool_failed: 0,
      thought: "",
      tools: [
        {
          kind: "tool",
          id: "tool-1",
          created_at: "2025-01-01T00:00:00.000Z",
          updated_at: "2025-01-01T00:00:00.000Z",
          tool_call_id: "tool-call-1",
          tool_kind: "execute",
          title: "Run",
          status: "completed",
          locations: [],
          input: null,
          output_text: "",
          raw: null,
          updates_seen: 1,
        },
      ],
    };

    const collapsedUiState: WorkbenchMessageListUiState = {
      ...baseUiState,
      expandedTurnDetailsById: { "turn-1": false },
    };
    expect(getWorkbenchListItemKey(item, collapsedUiState)).toContain(":tool-group:collapsed");

    const expandedUiState: WorkbenchMessageListUiState = {
      ...baseUiState,
      expandedTurnDetailsById: { "turn-1": true },
      expandedToolById: { "tool-1": true },
    };
    expect(getWorkbenchListItemKey(item, expandedUiState)).toContain(":tool-group:expanded:ready:");
    expect(getWorkbenchListItemKey(item, expandedUiState)).toContain("tool-1:open");
  });

  it("keeps standalone tool height identity tied to the summary row only", () => {
    const tool: Extract<WorkbenchListItem, { kind: "tool" }> = {
      kind: "tool",
      id: "tool-1",
      created_at: "2025-01-01T00:00:00.000Z",
      updated_at: "2025-01-01T00:00:01.000Z",
      tool_call_id: "tool-call-1",
      tool_kind: "execute",
      title: "Ran",
      subtitle: "pwd",
      status: "completed",
      locations: [],
      input: { command: "pwd" },
      output_text: "first output",
      raw: null,
      updates_seen: 1,
      has_details: true,
    };
    const expandedUiState: WorkbenchMessageListUiState = {
      ...baseUiState,
      expandedToolById: { "tool-1": true },
    };
    const nextOutput = {
      ...tool,
      output_text: "second output\nwith more lines",
    };

    expect(getWorkbenchListItemKey(tool, baseUiState)).toBe(
      getWorkbenchListItemKey(tool, expandedUiState, { verbosity: "verbose" }),
    );
    expect(getWorkbenchListItemSizeCacheKey(tool, baseUiState)).toBe(
      getWorkbenchListItemSizeCacheKey(tool, expandedUiState, { verbosity: "verbose" }),
    );
    expect(getWorkbenchListItemKey(tool, baseUiState)).toBe(
      getWorkbenchListItemKey(nextOutput, baseUiState, { verbosity: "verbose" }),
    );
    expect(getWorkbenchListItemSizeCacheKey(tool, baseUiState)).toBe(
      getWorkbenchListItemSizeCacheKey(nextOutput, baseUiState, { verbosity: "verbose" }),
    );
  });

  it("builds a stable layout revision from layout-affecting UI state and verbosity", () => {
    const revisionA = getWorkbenchMessageListLayoutRevision(
      {
        expandedTurnHeaders: { b: true, a: true, ignored: false },
        expandedTurnDetailsById: { turn2: true, turn1: true },
        expandedToolById: { tool2: true, tool1: true },
        expandedMessageById: { message2: true, message1: true },
        turnToolsLoading: ["turn2", "turn1"],
      },
      { verbosity: "verbose" },
    );
    const revisionB = getWorkbenchMessageListLayoutRevision(
      {
        expandedTurnHeaders: { a: true, b: true },
        expandedTurnDetailsById: { turn1: true, turn2: true },
        expandedToolById: { tool1: true, tool2: true },
        expandedMessageById: { message1: true, message2: true },
        turnToolsLoading: ["turn1", "turn2"],
      },
      { verbosity: "verbose" },
    );
    const revisionC = getWorkbenchMessageListLayoutRevision(baseUiState, { verbosity: "normal" });

    expect(revisionA).toBe(revisionB);
    expect(revisionA).not.toBe(revisionC);
  });

  it("filters global tool expansion revision churn to tool-group children", () => {
    const uiState: WorkbenchMessageListUiState = {
      ...baseUiState,
      expandedToolById: { "tool-group-child": true, "standalone-tool": true },
    };
    const toolGroupItems: WorkbenchListItem[] = [
      {
        kind: "tool_group",
        id: "tool-group-1",
        turn_id: "turn-1",
        created_at: "2025-01-01T00:00:00.000Z",
        updated_at: "2025-01-01T00:00:00.000Z",
        tool_total: 1,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 1,
        tool_failed: 0,
        thought: "",
        tools: [
          {
            kind: "tool",
            id: "tool-group-child",
            created_at: "2025-01-01T00:00:00.000Z",
            updated_at: "2025-01-01T00:00:00.000Z",
            tool_call_id: "tool-call-1",
            tool_kind: "execute",
            title: "Run",
            status: "completed",
            locations: [],
            input: null,
            output_text: "",
            raw: null,
            updates_seen: 1,
          },
        ],
      },
    ];

    expect(collectWorkbenchToolGroupExpansionIds(toolGroupItems)).toEqual(["tool-group-child"]);
    expect(
      getWorkbenchMessageListLayoutRevision(uiState, {
        toolExpansionIds: [],
      }),
    ).toBe(getWorkbenchMessageListLayoutRevision(baseUiState, { toolExpansionIds: [] }));
    expect(
      getWorkbenchMessageListLayoutRevision(uiState, {
        toolExpansionIds: collectWorkbenchToolGroupExpansionIds(toolGroupItems),
      }),
    ).not.toBe(
      getWorkbenchMessageListLayoutRevision(baseUiState, {
        toolExpansionIds: collectWorkbenchToolGroupExpansionIds(toolGroupItems),
      }),
    );
  });

  it("scopes layout and height identity to the transcript layout engine revision", () => {
    const item: Extract<WorkbenchListItem, { kind: "message" }> = {
      kind: "message",
      id: "message-1",
      role: "user",
      content: "short",
      attachments: [],
      created_at: "2025-01-01T00:00:00.000Z",
    };

    expect(getWorkbenchMessageListLayoutRevision(baseUiState)).toContain(
      SESSION_TRANSCRIPT_LAYOUT_ENGINE_REVISION,
    );
    expect(getWorkbenchListItemSizeCacheKey(item, baseUiState)).toContain(
      SESSION_TRANSCRIPT_LAYOUT_ENGINE_REVISION,
    );
    expect(getWorkbenchListItemKey(item, baseUiState)).toContain(
      SESSION_TRANSCRIPT_LAYOUT_ENGINE_REVISION,
    );
  });

  it("changes assistant and thought keys when their content changes", () => {
    const assistantA: Extract<WorkbenchListItem, { kind: "assistant" }> = {
      kind: "assistant",
      id: "assistant-1",
      turn_id: "turn-1",
      created_at: "2025-01-01T00:00:00.000Z",
      content: "short reply",
      thought: "",
      is_complete: true,
    };
    const assistantB = { ...assistantA, content: "longer reply\nwith another line" };
    const thoughtA: Extract<WorkbenchListItem, { kind: "thought" }> = {
      kind: "thought",
      id: "thought-1",
      turn_id: "turn-1",
      created_at: "2025-01-01T00:00:00.000Z",
      content: "thinking",
    };
    const thoughtB = { ...thoughtA, content: "thinking more" };

    expect(getWorkbenchListItemKey(assistantA, baseUiState)).not.toBe(getWorkbenchListItemKey(assistantB, baseUiState));
    expect(getWorkbenchListItemKey(thoughtA, baseUiState)).not.toBe(getWorkbenchListItemKey(thoughtB, baseUiState));
  });

  it("treats completed assistant rows as fixed-height identity", () => {
    const content = Array.from({ length: 24 }, (_, index) => `assistant line ${index + 1}`).join("\n");
    const item: Extract<WorkbenchListItem, { kind: "assistant" }> = {
      kind: "assistant",
      id: "assistant-1",
      turn_id: "turn-1",
      created_at: "2025-01-01T00:00:00.000Z",
      content,
      thought: "",
      is_complete: true,
    };

    expect(getWorkbenchListItemKey(item, baseUiState)).toContain(":assistant:fixed:");
  });

  it("keys pending assistant rows on the same content identity as completed rows", () => {
    const content = Array.from({ length: 24 }, (_, index) => `assistant line ${index + 1}`).join("\n");
    const item: Extract<WorkbenchListItem, { kind: "assistant" }> = {
      kind: "assistant",
      id: "assistant-1",
      turn_id: "turn-1",
      created_at: "2025-01-01T00:00:00.000Z",
      content,
      thought: "",
      is_complete: false,
    };

    expect(getWorkbenchListItemKey(item, baseUiState)).toContain(":assistant:fixed:");
  });

  it("changes standalone tool keys only when the visible summary changes", () => {
    const item: Extract<WorkbenchListItem, { kind: "tool" }> = {
      kind: "tool",
      id: "tool-1",
      created_at: "2025-01-01T00:00:00.000Z",
      updated_at: "2025-01-01T00:00:00.000Z",
      tool_call_id: "tool-call-1",
      tool_kind: "execute",
      title: "Run",
      subtitle: "first summary",
      status: "completed",
      locations: [],
      input: { command: ["echo", "hi"] },
      output_text: "first output",
      raw: null,
      updates_seen: 1,
    };

    const collapsedA = getWorkbenchListItemKey(item, baseUiState, { verbosity: "default" });
    const collapsedB = getWorkbenchListItemKey({ ...item, subtitle: "second summary" }, baseUiState, {
      verbosity: "default",
    });
    const expandedState: WorkbenchMessageListUiState = {
      ...baseUiState,
      expandedToolById: { "tool-1": true },
    };
    const expandedA = getWorkbenchListItemKey(item, expandedState, { verbosity: "verbose" });
    const expandedB = getWorkbenchListItemKey({ ...item, output_text: "second output" }, expandedState, {
      verbosity: "verbose",
    });

    expect(collapsedA).not.toBe(collapsedB);
    expect(expandedA).toBe(expandedB);
  });

  it("tracks size-cache keys only for stable rows and includes terminal turn status layout", () => {
    const pendingAssistant: Extract<WorkbenchListItem, { kind: "assistant" }> = {
      kind: "assistant",
      id: "assistant-pending",
      turn_id: "turn-1",
      created_at: "2025-01-01T00:00:00.000Z",
      content: "still streaming",
      thought: "",
      is_complete: false,
    };
    const completedAssistant: Extract<WorkbenchListItem, { kind: "assistant" }> = {
      ...pendingAssistant,
      id: "assistant-complete",
      is_complete: true,
    };
    const runningStatus: Extract<WorkbenchListItem, { kind: "turn_status" }> = {
      kind: "turn_status",
      id: "turn-status-1",
      turn_id: "turn-1",
      created_at: "2025-01-01T00:00:00.000Z",
      started_at: "2025-01-01T00:00:00.000Z",
      updated_at: "2025-01-01T00:00:05.000Z",
      status: "running",
      custom_status: "Working through files",
      assistant_messages_content: "",
    };
    const completedStatus: Extract<WorkbenchListItem, { kind: "turn_status" }> = {
      ...runningStatus,
      status: "completed",
      assistant_messages_content: "final response",
    };

    expect(getWorkbenchListItemSizeCacheKey(pendingAssistant, baseUiState)).toBeNull();
    expect(getWorkbenchListItemSizeCacheKey(completedAssistant, baseUiState)).toContain("assistant:fixed:");
    expect(getWorkbenchListItemSizeCacheKey(runningStatus, baseUiState)).toBeNull();
    expect(getWorkbenchListItemSizeCacheKey(completedStatus, baseUiState)).toContain("turn-status:");
    expect(getWorkbenchListItemSizeCacheKey(completedStatus, baseUiState)).toContain(":copy");
  });

  it("changes collapsed turn-header size keys when the rendered header text changes", () => {
    const itemA: Extract<WorkbenchListItem, { kind: "turn_header" }> = {
      kind: "turn_header",
      id: "turn-header-1",
      header: {
        id: "header-1",
        content: "first line\nsecond line",
        plain_text: "first line\nsecond line",
        attachments: [],
        created_at: "2025-01-01T00:00:00.000Z",
      },
    };
    const itemB: Extract<WorkbenchListItem, { kind: "turn_header" }> = {
      ...itemA,
      header: {
        ...itemA.header,
        content: "updated header text\nsecond line",
        plain_text: "updated header text\nsecond line",
      },
    };

    expect(getWorkbenchListItemKey(itemA, baseUiState)).not.toBe(getWorkbenchListItemKey(itemB, baseUiState));
    expect(getWorkbenchListItemSizeCacheKey(itemA, baseUiState)).not.toBe(
      getWorkbenchListItemSizeCacheKey(itemB, baseUiState),
    );
  });
});
