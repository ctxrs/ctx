import { describe, expect, it, vi } from "vitest";
import type { Message, SessionEvent, SessionTurn, SessionTurnTool } from "../../api/client";
import { buildWorkbenchThreadViewModel } from "./SessionPage.workbenchViewModel";
import type { ThreadItem } from "../sessionView/SessionPage.types";

vi.mock("react-syntax-highlighter", () => ({ Prism: () => null }));
vi.mock("react-syntax-highlighter/dist/esm/styles/prism", () => ({ oneDark: {} }));

const isTurnStatusItem = (item: ThreadItem): item is Extract<ThreadItem, { kind: "turn_status" }> =>
  item.kind === "turn_status";

const isToolItem = (item: ThreadItem): item is Extract<ThreadItem, { kind: "tool" }> =>
  item.kind === "tool";

const isMessageItem = (item: ThreadItem): item is Extract<ThreadItem, { kind: "message" }> =>
  item.kind === "message";

describe("buildWorkbenchThreadViewModel", () => {
  it("does not synthesize a thread from raw events when turns are missing", async () => {
    const events = [
      {
        id: "e1",
        session_id: "s1",
        run_id: "r1",
        turn_id: "t1",
        event_type: "user_message",
        payload_json: { message_id: "m1", content: "hello", attachments: [], order_seq: 1 },
        created_at: "2025-12-15T00:00:00.000Z",
      },
      {
        id: "e2",
        session_id: "s1",
        run_id: "r1",
        turn_id: "t1",
        event_type: "assistant_chunk",
        payload_json: { content_fragment: "Hi", order_seq: 2 },
        created_at: "2025-12-15T00:00:01.000Z",
      },
    ];

    const out = buildWorkbenchThreadViewModel([], [] as unknown as Message[], {}, events as unknown as SessionEvent[]);
    expect(out.groups).toEqual([]);
    expect(out.debugEvents).toEqual([]);
  }, 30000);

  it("does not infer user headers when turn.user_message_id is missing", async () => {
    const turns = [
      {
        turn_id: "t1",
        session_id: "s1",
        status: "running",
        started_at: "2025-12-15T00:00:00.000Z",
        updated_at: "2025-12-15T00:00:01.000Z",
        tool_total: 1,
        tool_pending: 0,
        tool_running: 1,
        tool_completed: 0,
        tool_failed: 0,
      },
    ];

    const messages = [
      {
        id: "m1",
        session_id: "s1",
        role: "user",
        content: "hello",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:00.000Z",
        turn_id: "t1",
        turn_sequence: 1,
      },
    ];

    const events = [
      {
        id: "e1",
        session_id: "s1",
        run_id: "r1",
        turn_id: "t1",
        event_type: "tool_call",
        payload_json: { tool_call_id: "tool-1", title: "ls", order_seq: 2 },
        created_at: "2025-12-15T00:00:01.000Z",
      },
    ];

    const out = buildWorkbenchThreadViewModel(turns as unknown as SessionTurn[], messages as unknown as Message[], {}, events as unknown as SessionEvent[]);
    expect(out.groups.length).toBe(1);
    expect(out.groups[0]?.header).toBeNull();
  }, 10000);

  it("skips turns without any stable order_seq anchor", async () => {
    const turns = [
      {
        turn_id: "t1",
        session_id: "s1",
        user_message_id: "m1",
        status: "running",
        started_at: "2025-12-15T00:00:00.000Z",
        updated_at: "2025-12-15T00:00:01.000Z",
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
    ];

    const messages = [
      {
        id: "m1",
        session_id: "s1",
        role: "user",
        content: "hello",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:00.000Z",
        turn_id: "t1",
      },
      {
        id: "a1",
        session_id: "s1",
        role: "assistant",
        content: "partial",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:01.000Z",
        turn_id: "t1",
      },
    ];

    const events = [
      {
        id: "e1",
        session_id: "s1",
        run_id: "r1",
        turn_id: "t1",
        event_type: "assistant_chunk",
        payload_json: { content_fragment: "x" },
        created_at: "2025-12-15T00:00:01.000Z",
      },
    ];

    const out = buildWorkbenchThreadViewModel(turns as unknown as SessionTurn[], messages as unknown as Message[], {}, events as unknown as SessionEvent[]);
    expect(out.groups.length).toBe(0);
  }, 10000);

  it("interleaves tool + thought activity and appends a status row", async () => {
    const turns = [
      {
        turn_id: "t1",
        session_id: "s1",
        user_message_id: "m1",
        status: "running",
        started_at: "2025-12-15T00:00:00.000Z",
        updated_at: "2025-12-15T00:00:05.000Z",
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
    ];

    const messages = [
      {
        id: "m1",
        session_id: "s1",
        role: "user",
        content: "hello",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:00.000Z",
        turn_id: "t1",
        order_seq: 1,
      },
    ];

    const events = [
      {
        seq: 1,
        id: "e1",
        session_id: "s1",
        run_id: "r1",
        turn_id: "t1",
        event_type: "tool_call",
        payload_json: { tool_call_id: "tool-1", title: "ls", order_seq: 1 },
        created_at: "2025-12-15T00:00:01.000Z",
      },
      {
        seq: 2,
        id: "e2",
        session_id: "s1",
        run_id: "r1",
        turn_id: "t1",
        event_type: "thought_chunk",
        payload_json: { content_fragment: "thinking", order_seq: 2 },
        created_at: "2025-12-15T00:00:02.000Z",
      },
      {
        seq: 3,
        id: "e3",
        session_id: "s1",
        run_id: "r1",
        turn_id: "t1",
        event_type: "tool_call",
        payload_json: { tool_call_id: "tool-2", title: "pwd", order_seq: 3 },
        created_at: "2025-12-15T00:00:03.000Z",
      },
    ];

    const out = buildWorkbenchThreadViewModel(turns as unknown as SessionTurn[], messages as unknown as Message[], {}, events as unknown as SessionEvent[]);
    const items = out.groups[0]?.items ?? [];
    expect(items.map((it) => it.kind)).toEqual(["tool", "thought", "tool", "turn_status"]);
  }, 10000);

  it("renders pending assistant overlay from its own order sequence without durable chunk events", async () => {
    const turns = [
      {
        turn_id: "t1",
        session_id: "s1",
        user_message_id: "m1",
        status: "running",
        started_at: "2025-12-15T00:00:00.000Z",
        updated_at: "2025-12-15T00:00:01.000Z",
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
    ];
    const messages = [
      {
        id: "m1",
        session_id: "s1",
        role: "user",
        content: "hello",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:00.000Z",
        turn_id: "t1",
        order_seq: 1,
      },
    ];

    const out = buildWorkbenchThreadViewModel(
      turns as unknown as SessionTurn[],
      messages as unknown as Message[],
      {},
      [],
      {
        t1: {
          content: "streaming partial",
          providerMessageId: "provider-msg-1",
          orderSeq: 2,
        },
      },
    );

    const assistant = out.groups[0]?.items.find(
      (item): item is Extract<ThreadItem, { kind: "assistant" }> => item.kind === "assistant",
    );
    expect(assistant).toMatchObject({
      id: "assistant-t1-pending",
      content: "streaming partial",
      is_complete: false,
    });
  }, 10000);

  it("suppresses a stale pending assistant row when the same assistant reply is already persisted", async () => {
    const turns = [
      {
        turn_id: "t1",
        session_id: "s1",
        user_message_id: "m1",
        status: "running",
        started_at: "2025-12-15T00:00:00.000Z",
        updated_at: "2025-12-15T00:00:02.000Z",
        assistant_partial: null,
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
    ];

    const messages = [
      {
        id: "m1",
        session_id: "s1",
        role: "user",
        content: "hi",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:00.000Z",
        turn_id: "t1",
        order_seq: 1,
      },
      {
        id: "a1",
        session_id: "s1",
        role: "assistant",
        content: "pong",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:01.000Z",
        turn_id: "t1",
        order_seq: 2,
      },
    ];

    const events = [
      {
        seq: 1,
        id: "e1",
        session_id: "s1",
        run_id: "r1",
        turn_id: "t1",
        event_type: "assistant_complete",
        payload_json: { full_content: "pong", message_id: "provider-msg-1", order_seq: 2 },
        created_at: "2025-12-15T00:00:01.000Z",
      },
    ];

    const out = buildWorkbenchThreadViewModel(
      turns as unknown as SessionTurn[],
      messages as unknown as Message[],
      {},
      events as unknown as SessionEvent[],
      {
        t1: {
          content: "pong",
          providerMessageId: "provider-msg-1",
          orderSeq: 2,
        },
      },
    );
    const items = out.groups[0]?.items ?? [];
    const assistantItems = items.filter(
      (item): item is Extract<ThreadItem, { kind: "assistant" }> => item.kind === "assistant",
    );

    expect(assistantItems).toHaveLength(1);
    expect(assistantItems[0]).toMatchObject({
      id: "assistant-msg-a1",
      content: "pong",
      is_complete: true,
    });
  }, 10000);

  it("uses CRP reasoning summaries for status and keeps trace chunks in thought rows", async () => {
    const turns = [
      {
        turn_id: "t1",
        session_id: "s1",
        user_message_id: "m1",
        status: "running",
        started_at: "2025-12-15T00:00:00.000Z",
        updated_at: "2025-12-15T00:00:05.000Z",
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
    ];

    const messages = [
      {
        id: "m1",
        session_id: "s1",
        role: "user",
        content: "hello",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:00.000Z",
        turn_id: "t1",
        order_seq: 1,
      },
    ];

    const events = [
      {
        seq: 1,
        id: "e1",
        session_id: "s1",
        run_id: "r1",
        turn_id: "t1",
        event_type: "notice",
        payload_json: { kind: "reasoning_summary", text: "Reading foo", crp_seq: 1, order_seq: 1 },
        created_at: "2025-12-15T00:00:01.000Z",
      },
      {
        seq: 2,
        id: "e2",
        session_id: "s1",
        run_id: "r1",
        turn_id: "t1",
        event_type: "thought_chunk",
        payload_json: {
          content_fragment: "Thinking about bar",
          crp_seq: 2,
          crp_channel: "data",
          item_id: "thought-item-1",
          summary_index: 0,
          order_seq: 2,
        },
        created_at: "2025-12-15T00:00:02.000Z",
      },
    ];

    const out = buildWorkbenchThreadViewModel(turns as unknown as SessionTurn[], messages as unknown as Message[], {}, events as unknown as SessionEvent[]);
    const items = out.groups[0]?.items ?? [];
    type ThoughtItem = { kind: "thought"; id: string; turn_id: string; created_at: string; content: string };
    const thoughtItems = items.filter((it): it is ThoughtItem => it.kind === "thought");
    expect(thoughtItems.length).toBe(1);
    expect(String(thoughtItems[0]?.content)).toContain("Thinking about bar");
    expect(String(thoughtItems[0]?.content)).not.toContain("Reading foo");

    const statusItem = items.find(isTurnStatusItem);
    expect(statusItem?.custom_status).toBe("Reading foo");
  }, 10000);

  it("renders timeline notices for unknown runtime events marked for display", async () => {
    const turns = [
      {
        turn_id: "t1",
        session_id: "s1",
        user_message_id: "m1",
        status: "running",
        started_at: "2025-12-15T00:00:00.000Z",
        updated_at: "2025-12-15T00:00:05.000Z",
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
    ];

    const messages = [
      {
        id: "m1",
        session_id: "s1",
        role: "user",
        content: "hello",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:00.000Z",
        turn_id: "t1",
        order_seq: 1,
      },
    ];

    const events = [
      {
        seq: 1,
        id: "e1",
        session_id: "s1",
        run_id: "r1",
        turn_id: "t1",
        event_type: "notice",
        payload_json: {
          kind: "crp_unknown_event",
          original_type: "tool.progress",
          message: "Unknown runtime event: Scanning files",
          display_in_timeline: true,
          order_seq: 2,
        },
        created_at: "2025-12-15T00:00:01.000Z",
      },
    ];

    const out = buildWorkbenchThreadViewModel(
      turns as unknown as SessionTurn[],
      messages as unknown as Message[],
      {},
      events as unknown as SessionEvent[],
    );
    const items = out.groups[0]?.items ?? [];
    const noticeItem = items.find(isMessageItem);
    expect(noticeItem?.role).toBe("system");
    expect(noticeItem?.content).toContain("Unknown runtime event: Scanning files");
  }, 10000);

  it("prefers tool names when rendering unknown runtime tool notices", async () => {
    const turns = [
      {
        turn_id: "t1",
        session_id: "s1",
        user_message_id: "m1",
        status: "running",
        started_at: "2025-12-15T00:00:00.000Z",
        updated_at: "2025-12-15T00:00:05.000Z",
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
    ];

    const messages = [
      {
        id: "m1",
        session_id: "s1",
        role: "user",
        content: "hello",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:00.000Z",
        turn_id: "t1",
        order_seq: 1,
      },
    ];

    const events = [
      {
        seq: 1,
        id: "e1",
        session_id: "s1",
        run_id: "r1",
        turn_id: "t1",
        event_type: "notice",
        payload_json: {
          kind: "crp_unknown_event",
          original_type: "tool.progress",
          display_in_timeline: true,
          raw: {
            tool_name: "Agent",
            description: "Read agent basics context",
          },
          order_seq: 2,
        },
        created_at: "2025-12-15T00:00:01.000Z",
      },
    ];

    const out = buildWorkbenchThreadViewModel(
      turns as unknown as SessionTurn[],
      messages as unknown as Message[],
      {},
      events as unknown as SessionEvent[],
    );
    const items = out.groups[0]?.items ?? [];
    const noticeItem = items.find(isMessageItem);
    expect(noticeItem?.role).toBe("system");
    expect(noticeItem?.content).toContain("Unknown tool event: Subagent · Read agent basics context");
  }, 10000);

  it("splits CRP thought chunks into blocks between control events", async () => {
    const turns = [
      {
        turn_id: "t1",
        session_id: "s1",
        user_message_id: "m1",
        status: "running",
        started_at: "2025-12-15T00:00:00.000Z",
        updated_at: "2025-12-15T00:00:05.000Z",
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
    ];

    const messages = [
      {
        id: "m1",
        session_id: "s1",
        role: "user",
        content: "hello",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:00.000Z",
        turn_id: "t1",
        order_seq: 1,
      },
    ];

    const events = [
      {
        seq: 1,
        id: "e1",
        session_id: "s1",
        run_id: "r1",
        turn_id: "t1",
        event_type: "thought_chunk",
        payload_json: {
          content_fragment: "first",
          crp_seq: 1,
          crp_channel: "data",
          item_id: "thought-item-1",
          summary_index: 0,
          order_seq: 1,
        },
        created_at: "2025-12-15T00:00:01.000Z",
      },
      {
        seq: 2,
        id: "e2",
        session_id: "s1",
        run_id: "r1",
        turn_id: "t1",
        event_type: "tool_call",
        payload_json: { tool_call_id: "tool-1", title: "ls", order_seq: 2 },
        created_at: "2025-12-15T00:00:02.000Z",
      },
      {
        seq: 3,
        id: "e3",
        session_id: "s1",
        run_id: "r1",
        turn_id: "t1",
        event_type: "thought_chunk",
        payload_json: {
          content_fragment: "second",
          crp_seq: 3,
          crp_channel: "data",
          item_id: "thought-item-1",
          summary_index: 1,
          order_seq: 3,
        },
        created_at: "2025-12-15T00:00:03.000Z",
      },
    ];

    const out = buildWorkbenchThreadViewModel(turns as unknown as SessionTurn[], messages as unknown as Message[], {}, events as unknown as SessionEvent[]);
    const items = out.groups[0]?.items ?? [];
    const kinds = items.map((it) => it.kind);
    expect(kinds).toEqual(["thought", "tool", "thought", "turn_status"]);
  }, 10000);

  it("orders tool activity by order_seq", async () => {
    const turns = [
      {
        turn_id: "t1",
        session_id: "s1",
        status: "running",
        started_at: "2025-12-15T00:00:00.000Z",
        updated_at: "2025-12-15T00:00:05.000Z",
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
    ];

    const messages = [
      {
        id: "m1",
        session_id: "s1",
        role: "user",
        content: "hello",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:00.000Z",
        turn_id: "t1",
        order_seq: 1,
      },
    ];

    const events = [
      {
        id: "e1",
        session_id: "s1",
        run_id: "r1",
        turn_id: "t1",
        event_type: "tool_call",
        payload_json: { tool_call_id: "tool-1", title: "first", order_seq: 1 },
        created_at: "2025-12-15T00:00:02.000Z",
      },
      {
        id: "e2",
        session_id: "s1",
        run_id: "r1",
        turn_id: "t1",
        event_type: "tool_call",
        payload_json: { tool_call_id: "tool-2", title: "second", order_seq: 2 },
        created_at: "2025-12-15T00:00:01.000Z",
      },
    ];

    const out = buildWorkbenchThreadViewModel(turns as unknown as SessionTurn[], messages as unknown as Message[], {}, events as unknown as SessionEvent[]);
    const tools = (out.groups[0]?.items ?? []).filter(isToolItem);
    expect(tools.map((it) => it.tool_call_id)).toEqual(["tool-1", "tool-2"]);
  }, 10000);

  it("orders assistant messages by message order_seq instead of turn_sequence fallback", async () => {
    const turns = [
      {
        turn_id: "t1",
        session_id: "s1",
        user_message_id: "m1",
        status: "completed",
        started_at: "2025-12-15T00:00:00.000Z",
        updated_at: "2025-12-15T00:00:03.000Z",
        tool_total: 1,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 1,
        tool_failed: 0,
      },
    ];

    const messages = [
      {
        id: "m1",
        session_id: "s1",
        role: "user",
        content: "hello",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:00.000Z",
        turn_id: "t1",
        order_seq: 1,
      },
      {
        id: "m2",
        session_id: "s1",
        role: "assistant",
        content: "done",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:03.000Z",
        turn_id: "t1",
        turn_sequence: 1,
        order_seq: 3,
      },
    ];

    const events = [
      {
        id: "e1",
        session_id: "s1",
        run_id: "r1",
        turn_id: "t1",
        event_type: "tool_call",
        payload_json: { tool_call_id: "tool-1", title: "ls", order_seq: 2 },
        created_at: "2025-12-15T00:00:01.000Z",
      },
    ];

    const out = buildWorkbenchThreadViewModel(
      turns as unknown as SessionTurn[],
      messages as unknown as Message[],
      {},
      events as unknown as SessionEvent[],
    );

    expect(out.groups[0]?.items.map((item) => item.kind)).toEqual([
      "tool",
      "assistant",
      "turn_status",
    ]);
  }, 10000);

  it("keeps summary-backed tools ahead of a later final assistant using tool order_seq", async () => {
    const turns = [
      {
        turn_id: "t1",
        session_id: "s1",
        user_message_id: "m1",
        status: "completed",
        started_at: "2025-12-15T00:00:00.000Z",
        updated_at: "2025-12-15T00:00:04.000Z",
        tool_total: 2,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 2,
        tool_failed: 0,
      },
    ];

    const messages = [
      {
        id: "m1",
        session_id: "s1",
        role: "user",
        content: "hello",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:00.000Z",
        turn_id: "t1",
        order_seq: 1,
      },
      {
        id: "m2",
        session_id: "s1",
        role: "assistant",
        content: "final answer",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:03.000Z",
        turn_id: "t1",
        turn_sequence: 1,
        order_seq: 3,
      },
    ];

    const toolsByTurnId = {
      t1: [
        {
          session_id: "s1",
          turn_id: "t1",
          tool_call_id: "tool-1",
          tool_kind: "exec",
          provider_tool_name: "exec_command",
          title: "Ran",
          subtitle: "pwd",
          status: "completed",
          input_json: { cmd: "pwd" },
          output_text: "",
          order_seq: 2,
          first_event_seq: null,
          created_at: "2025-12-15T00:00:01.000Z",
          updated_at: "2025-12-15T00:00:01.100Z",
          summary_only: true,
        },
        {
          session_id: "s1",
          turn_id: "t1",
          tool_call_id: "tool-2",
          tool_kind: "exec",
          provider_tool_name: "exec_command",
          title: "Ran",
          subtitle: "ls",
          status: "completed",
          input_json: { cmd: "ls" },
          output_text: "",
          order_seq: 3,
          first_event_seq: null,
          created_at: "2025-12-15T00:00:02.000Z",
          updated_at: "2025-12-15T00:00:02.100Z",
          summary_only: true,
        },
      ],
    };

    const out = buildWorkbenchThreadViewModel(
      turns as unknown as SessionTurn[],
      messages as unknown as Message[],
      toolsByTurnId as unknown as Record<string, SessionTurnTool[]>,
      [] as unknown as SessionEvent[],
    );

    expect(out.groups[0]?.items.map((item) => item.kind)).toEqual([
      "tool",
      "tool",
      "assistant",
      "turn_status",
    ]);
  }, 10000);

  it("keeps thought ordering stable when final chunks arrive", async () => {
    const turns = [
      {
        turn_id: "t1",
        session_id: "s1",
        status: "running",
        started_at: "2025-12-15T00:00:00.000Z",
        updated_at: "2025-12-15T00:00:05.000Z",
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
    ];

    const messages = [
      {
        id: "m1",
        session_id: "s1",
        role: "user",
        content: "hello",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:00.000Z",
        turn_id: "t1",
        order_seq: 1,
      },
    ];

    const events = [
      {
        seq: 1,
        id: "e1",
        session_id: "s1",
        run_id: "r1",
        turn_id: "t1",
        event_type: "thought_chunk",
        payload_json: { content_fragment: "draft", item_id: "thought-1", order_seq: 1 },
        created_at: "2025-12-15T00:00:01.000Z",
      },
      {
        seq: 2,
        id: "e2",
        session_id: "s1",
        run_id: "r1",
        turn_id: "t1",
        event_type: "tool_call",
        payload_json: { tool_call_id: "tool-1", title: "ls", order_seq: 2 },
        created_at: "2025-12-15T00:00:02.000Z",
      },
      {
        seq: 3,
        id: "e3",
        session_id: "s1",
        run_id: "r1",
        turn_id: "t1",
        event_type: "thought_chunk",
        payload_json: { full_content: "final", is_final: true, item_id: "thought-1", order_seq: 1 },
        created_at: "2025-12-15T00:00:03.000Z",
      },
    ];

    const out = buildWorkbenchThreadViewModel(turns as unknown as SessionTurn[], messages as unknown as Message[], {}, events as unknown as SessionEvent[]);
    const items = out.groups[0]?.items ?? [];
    expect(items.map((it) => it.kind)).toEqual(["thought", "tool", "turn_status"]);
  }, 10000);

  it("keeps tool interleaving stable across tool updates", async () => {
    const turns = [
      {
        turn_id: "t1",
        session_id: "s1",
        status: "running",
        started_at: "2025-12-15T00:00:00.000Z",
        updated_at: "2025-12-15T00:00:05.000Z",
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
    ];

    const messages = [
      {
        id: "m1",
        session_id: "s1",
        role: "user",
        content: "hello",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:00.000Z",
        turn_id: "t1",
        order_seq: 1,
      },
    ];

    const events = [
      {
        seq: 1,
        id: "e1",
        session_id: "s1",
        run_id: "r1",
        turn_id: "t1",
        event_type: "thought_chunk",
        payload_json: { content_fragment: "thinking", order_seq: 2 },
        created_at: "2025-12-15T00:00:01.000Z",
      },
      {
        seq: 2,
        id: "e2",
        session_id: "s1",
        run_id: "r1",
        turn_id: "t1",
        event_type: "tool_call",
        payload_json: { tool_call_id: "tool-1", title: "search", order_seq: 1 },
        created_at: "2025-12-15T00:00:03.000Z",
      },
      {
        seq: 3,
        id: "e3",
        session_id: "s1",
        run_id: "r1",
        turn_id: "t1",
        event_type: "tool_call_update",
        payload_json: { tool_call_id: "tool-1", outputText: "partial", order_seq: 1 },
        created_at: "2025-12-15T00:00:04.000Z",
      },
      {
        seq: 4,
        id: "e4",
        session_id: "s1",
        run_id: "r1",
        turn_id: "t1",
        event_type: "tool_result",
        payload_json: { tool_call_id: "tool-1", outputText: "done", order_seq: 1 },
        created_at: "2025-12-15T00:00:05.000Z",
      },
    ];

    const out = buildWorkbenchThreadViewModel(turns as unknown as SessionTurn[], messages as unknown as Message[], {}, events as unknown as SessionEvent[]);
    const items = out.groups[0]?.items ?? [];
    expect(items.map((it) => it.kind)).toEqual(["tool", "thought", "turn_status"]);
  }, 10000);

  it("does not synthesize empty object input when tool update omits input fields", async () => {
    const turns = [
      {
        turn_id: "t1",
        session_id: "s1",
        user_message_id: "m1",
        status: "running",
        started_at: "2025-12-15T00:00:00.000Z",
        updated_at: "2025-12-15T00:00:05.000Z",
        tool_total: 1,
        tool_pending: 1,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
    ];

    const messages = [
      {
        id: "m1",
        session_id: "s1",
        role: "user",
        content: "hello",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:00.000Z",
        turn_id: "t1",
        order_seq: 1,
      },
    ];

    const toolsByTurnId = {
      t1: [
        {
          session_id: "s1",
          turn_id: "t1",
          tool_call_id: "tool-1",
          tool_kind: "search",
          title: "search",
          status: "pending",
          created_at: "2025-12-15T00:00:01.000Z",
          updated_at: "2025-12-15T00:00:01.000Z",
          order_seq: 2,
          input_json: null,
          output_text: "",
        },
      ],
    } as unknown as Record<string, SessionTurnTool[]>;

    const events = [
      {
        seq: 1,
        id: "e1",
        session_id: "s1",
        run_id: "r1",
        turn_id: "t1",
        event_type: "tool_call_update",
        payload_json: { tool_call_id: "tool-1", status: "running", order_seq: 2 },
        created_at: "2025-12-15T00:00:02.000Z",
      },
    ];

    const out = buildWorkbenchThreadViewModel(
      turns as unknown as SessionTurn[],
      messages as unknown as Message[],
      toolsByTurnId,
      events as unknown as SessionEvent[],
    );
    const tool = out.groups[0]?.items.find(isToolItem);
    expect(tool?.input).toBeNull();
    expect(tool?.has_details).toBe(false);
  }, 10000);

  it("ignores notice status text for turn status rows", async () => {
    const turns = [
      {
        turn_id: "t1",
        session_id: "s1",
        user_message_id: "m1",
        status: "running",
        started_at: "2025-12-15T00:00:00.000Z",
        updated_at: "2025-12-15T00:00:05.000Z",
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
    ];

    const messages = [
      {
        id: "m1",
        session_id: "s1",
        role: "user",
        content: "hello",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:00.000Z",
        turn_id: "t1",
        order_seq: 1,
      },
    ];

    const events = [
      {
        seq: 1,
        id: "e1",
        session_id: "s1",
        run_id: "r1",
        turn_id: "t1",
        event_type: "notice",
        payload_json: { _meta: { statusText: "Preparing instructions" } },
        created_at: "2025-12-15T00:00:01.000Z",
      },
      {
        seq: 2,
        id: "e2",
        session_id: "s1",
        run_id: "r1",
        turn_id: "t1",
        event_type: "notice",
        payload_json: { _meta: { statusText: "Preparing specs" } },
        created_at: "2025-12-15T00:00:02.000Z",
      },
    ];

    const out = buildWorkbenchThreadViewModel(turns as unknown as SessionTurn[], messages as unknown as Message[], {}, events as unknown as SessionEvent[]);
    const statusItem = out.groups[0]?.items.find(isTurnStatusItem);
    expect(statusItem?.custom_status).toBeUndefined();
  }, 10000);

  it("ignores notice status text even when tool events are present", async () => {
    const turns = [
      {
        turn_id: "t1",
        session_id: "s1",
        user_message_id: "m1",
        status: "running",
        started_at: "2025-12-15T00:00:00.000Z",
        updated_at: "2025-12-15T00:00:02.000Z",
        tool_total: 1,
        tool_pending: 1,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
    ];

    const messages = [
      {
        id: "m1",
        session_id: "s1",
        role: "user",
        content: "hello",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:00.000Z",
        turn_id: "t1",
        order_seq: 1,
      },
    ];

    const events = [
      {
        seq: 1,
        id: "e1",
        session_id: "s1",
        run_id: "r1",
        turn_id: "t1",
        event_type: "tool_call",
        payload_json: {
          tool_call_id: "tool-1",
          kind: "search",
          status: "running",
          order_seq: 2,
          input: { query: "alpha" },
        },
        created_at: "2025-12-15T00:00:01.000Z",
      },
      {
        seq: 2,
        id: "e2",
        session_id: "s1",
        run_id: "r1",
        turn_id: "t1",
        event_type: "notice",
        payload_json: { _meta: { statusText: "Considering" } },
        created_at: "2025-12-15T00:00:02.000Z",
      },
    ];

    const out = buildWorkbenchThreadViewModel(turns as unknown as SessionTurn[], messages as unknown as Message[], {}, events as unknown as SessionEvent[]);
    const statusItem = out.groups[0]?.items.find(isTurnStatusItem);
    expect(statusItem?.custom_status).toBeUndefined();
  }, 10000);

  it("uses provider tool names for tool rows without changing turn status text", async () => {
    const turns = [
      {
        turn_id: "t1",
        session_id: "s1",
        user_message_id: "m1",
        status: "running",
        started_at: "2025-12-15T00:00:00.000Z",
        updated_at: "2025-12-15T00:00:02.000Z",
        tool_total: 1,
        tool_pending: 1,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
    ];

    const messages = [
      {
        id: "m1",
        session_id: "s1",
        role: "user",
        content: "hello",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:00.000Z",
        turn_id: "t1",
        order_seq: 1,
      },
    ];

    const events = [
      {
        seq: 1,
        id: "e1",
        session_id: "s1",
        run_id: "r1",
        turn_id: "t1",
        event_type: "tool_call",
        payload_json: {
          tool_call_id: "tool-1",
          kind: "execute",
          tool_name: "Bash",
          subtitle: "Print working directory",
          status: "running",
          order_seq: 2,
          toolCall: {
            name: "Bash",
            kind: "execute",
          },
          rawInput: { command: "pwd" },
        },
        created_at: "2025-12-15T00:00:01.000Z",
      },
    ];

    const out = buildWorkbenchThreadViewModel(
      turns as unknown as SessionTurn[],
      messages as unknown as Message[],
      {},
      events as unknown as SessionEvent[],
    );
    const toolItem = out.groups[0]?.items.find(isToolItem);
    const statusItem = out.groups[0]?.items.find(isTurnStatusItem);
    expect(toolItem?.title).toBe("Bash");
    expect(toolItem?.subtitle).toBe("Print working directory");
    expect(statusItem?.custom_status).toBeUndefined();
  }, 10000);

  it("renders tool summaries as tool rows when per-turn events are absent", async () => {
    const turns = [
      {
        turn_id: "t1",
        session_id: "s1",
        user_message_id: "m1",
        status: "completed",
        start_seq: 1,
        end_seq: 4,
        started_at: "2025-12-15T00:00:00.000Z",
        updated_at: "2025-12-15T00:00:03.000Z",
        tool_total: 1,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 1,
        tool_failed: 0,
      },
    ];

    const messages = [
      {
        id: "m1",
        session_id: "s1",
        role: "user",
        content: "hello",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:00.000Z",
        turn_id: "t1",
        order_seq: 1,
      },
      {
        id: "m2",
        session_id: "s1",
        role: "assistant",
        content: "done",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:03.000Z",
        turn_id: "t1",
        order_seq: 3,
      },
    ];

    const toolsByTurnId = {
      t1: [
        {
          session_id: "s1",
          tool_call_id: "tool-1",
          turn_id: "t1",
          tool_kind: "Grep",
          provider_tool_name: "Grep",
          title: "Grep",
          subtitle: "SessionPage",
          status: "completed",
          input_json: { pattern: "SessionPage" },
          output_text: "Found 29 files",
          order_seq: 2,
          first_event_seq: 2,
          created_at: "2025-12-15T00:00:01.000Z",
          updated_at: "2025-12-15T00:00:02.000Z",
        },
      ],
    };

    const out = buildWorkbenchThreadViewModel(
      turns as unknown as SessionTurn[],
      messages as unknown as Message[],
      toolsByTurnId,
      [],
    );

    const toolItems = out.groups[0]?.items.filter(isToolItem) ?? [];
    expect(toolItems).toHaveLength(1);
    expect(toolItems[0]?.title).toBe("Grep");
    expect(toolItems[0]?.subtitle).toBe("SessionPage");
  }, 10000);

  it("normalizes context-window metrics from canonical keys only", async () => {
    const { normalizeContextWindowMetrics } = await import("./SessionPage.workbenchViewModel");

    const canonical = normalizeContextWindowMetrics({
      context_tokens_estimate: 40,
      context_window_tokens: 100,
      remaining_tokens_estimate: 60,
      remaining_fraction: 0.6,
    });
    expect(canonical).toEqual({
      windowTokens: 100,
      usedTokens: 40,
      remainingTokens: 60,
      remainingFraction: 0.6,
    });

    const legacy = normalizeContextWindowMetrics({
      context_window: 100,
      total_tokens: 40,
      remaining_tokens: 60,
    });
    expect(legacy).toBeNull();
  }, 10000);

  it("produces a messagesKey that changes when message content changes with same length", async () => {
    const { deriveMessagesKey } = await import("./SessionPage.workbenchViewModel");

    const base = {
      id: "m1",
      session_id: "s1",
      role: "user",
      attachments: [],
      delivery: "immediate",
      created_at: "2025-12-15T00:00:00.000Z",
    };

    const k1 = deriveMessagesKey([{ ...base, content: "hello" }] as unknown as Message[]);
    const k2 = deriveMessagesKey([{ ...base, content: "world" }] as unknown as Message[]);
    expect(k1).not.toBe(k2);
  }, 10000);

  it("produces a messagesKey that changes when message attachments change in place", async () => {
    const { deriveMessagesKey } = await import("./SessionPage.workbenchViewModel");

    const base = {
      id: "m1",
      session_id: "s1",
      role: "assistant",
      content: "same",
      delivery: "immediate",
      created_at: "2025-12-15T00:00:00.000Z",
    };

    const k1 = deriveMessagesKey([{ ...base, attachments: [] }] as unknown as Message[]);
    const k2 = deriveMessagesKey([
      {
        ...base,
        attachments: [{ blob_id: "blob-1", mime_type: "image/png" }],
      },
    ] as unknown as Message[]);
    expect(k1).not.toBe(k2);
  }, 10000);

  it("produces a messagesKey that changes when queued delivery changes in place", async () => {
    const { deriveMessagesKey } = await import("./SessionPage.workbenchViewModel");

    const base = {
      id: "m1",
      session_id: "s1",
      role: "user",
      content: "same",
      attachments: [],
      created_at: "2025-12-15T00:00:00.000Z",
    };

    const k1 = deriveMessagesKey([{ ...base, delivery: "immediate" }] as unknown as Message[]);
    const k2 = deriveMessagesKey([{ ...base, delivery: "queued" }] as unknown as Message[]);
    expect(k1).not.toBe(k2);
  }, 10000);

  it("produces a turnsKey that changes when a non-tail turn updates in place", async () => {
    const { deriveTurnsKey } = await import("./SessionPage.workbenchViewModel");

    const turns = [
      {
        turn_id: "turn-1",
        start_seq: 1,
        updated_at: "2025-12-15T00:00:00.000Z",
      },
      {
        turn_id: "turn-2",
        start_seq: 2,
        updated_at: "2025-12-15T00:00:01.000Z",
      },
    ];

    const k1 = deriveTurnsKey(turns as unknown as SessionTurn[]);
    const k2 = deriveTurnsKey([
      {
        ...turns[0],
        updated_at: "2025-12-15T00:01:00.000Z",
      },
      turns[1],
    ] as unknown as SessionTurn[]);
    expect(k1).not.toBe(k2);
  }, 10000);

  it("produces a turnsKey that changes when turn status changes with the same timestamp", async () => {
    const { deriveTurnsKey } = await import("./SessionPage.workbenchViewModel");

    const turns = [
      {
        turn_id: "turn-1",
        start_seq: 1,
        updated_at: "2025-12-15T00:00:00.000Z",
        status: "running",
      },
    ];

    const k1 = deriveTurnsKey(turns as unknown as SessionTurn[]);
    const k2 = deriveTurnsKey([{ ...turns[0], status: "completed" }] as unknown as SessionTurn[]);
    expect(k1).not.toBe(k2);
  }, 10000);

  it("merges optimistic queued messages into the queue panel list", async () => {
    const { mergeQueuedMessagesForPanel } = await import("./SessionPage.workbenchViewModel");

    const pending = [
      {
        id: "m-pending-1",
        session_id: "s1",
        task_id: "t1",
        role: "user",
        content: "queued",
        delivery: "queued",
        created_at: "2025-12-15T00:00:00.000Z",
      },
    ];

    const merged = mergeQueuedMessagesForPanel(
      [] as unknown as Message[],
      pending as Message[],
    );
    expect(merged).toHaveLength(1);
    expect(String(merged[0]?.id)).toBe("m-pending-1");
  }, 10000);

  it("filters queued panel items once a turn starts running", async () => {
    const { filterQueuedMessagesForPanel } = await import("./SessionPage.workbenchViewModel");

    const queue = [
      {
        id: "m1",
        session_id: "s1",
        task_id: "t1",
        role: "user",
        content: "queued",
        delivery: "queued",
        created_at: "2025-12-15T00:00:00.000Z",
      },
    ];

    const turns = [
      {
        turn_id: "t1",
        session_id: "s1",
        user_message_id: "m1",
        status: "running",
        started_at: "2025-12-15T00:00:01.000Z",
        updated_at: "2025-12-15T00:00:02.000Z",
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
    ];

    const filtered = filterQueuedMessagesForPanel(queue as unknown as Message[], turns as unknown as SessionTurn[]);
    expect(filtered).toEqual([]);
  }, 10000);

  it("drops queued turns from the thread list when message ids are hidden", async () => {
    const { buildWorkbenchThreadViewModelFromTurns, filterTurnsForQueuedMessages } =
      await import("./SessionPage.workbenchViewModel");

    const turns = [
      {
        turn_id: "t-queued",
        session_id: "s1",
        user_message_id: "m-queued",
        status: "queued",
        started_at: "2025-12-15T00:00:00.000Z",
        updated_at: "2025-12-15T00:00:01.000Z",
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
      {
        turn_id: "t-live",
        session_id: "s1",
        user_message_id: "m-live",
        status: "running",
        started_at: "2025-12-15T00:00:02.000Z",
        updated_at: "2025-12-15T00:00:03.000Z",
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
    ];

    const messages = [
      {
        id: "m-live",
        session_id: "s1",
        role: "user",
        content: "hello",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:02.000Z",
        turn_id: "t-live",
        order_seq: 1,
      },
    ];

    const filteredTurns = filterTurnsForQueuedMessages(turns as unknown as SessionTurn[], new Set(["m-queued"]));
    const out = buildWorkbenchThreadViewModelFromTurns(filteredTurns, messages as unknown as Message[], {}, [], new Map());
    expect(out.groups.length).toBe(1);
    expect(out.groups[0]?.header?.id).toBe("t-live");
  }, 10000);

  it("keeps a turn header visible when the user message loses order_seq but the turn has start_seq", async () => {
    const { buildWorkbenchThreadViewModelFromTurns } = await import("./SessionPage.workbenchViewModel");

    const turns = [
      {
        turn_id: "t1",
        session_id: "s1",
        user_message_id: "m1",
        status: "running",
        start_seq: 1,
        end_seq: null,
        started_at: "2025-12-15T00:00:00.000Z",
        updated_at: "2025-12-15T00:00:01.000Z",
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
    ];

    const messages = [
      {
        id: "m1",
        session_id: "s1",
        task_id: "task-1",
        role: "user",
        content: "hello",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:00.000Z",
        turn_id: "t1",
      },
    ];

    const out = buildWorkbenchThreadViewModelFromTurns(
      turns as unknown as SessionTurn[],
      messages as unknown as Message[],
      {},
      [],
      new Map(),
    );

    expect(out.groups).toHaveLength(1);
    expect(out.groups[0]?.header?.id).toBe("t1");
    expect(out.groups[0]?.header?.content).toBe("hello");
  }, 10000);
});
