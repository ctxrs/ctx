import { describe, expect, it } from "vitest";
import type { SessionEvent, SessionHeadSnapshot, SessionTurn, SessionTurnToolSummary } from "../api/client";
import {
  compactActiveSessionHeadSnapshot,
  mergeSessionMessages,
  mergeSessionToolSummaries,
  mergeSessionTurns,
  sanitizeSessionHeadSnapshot,
} from "./sessionHeadState";
import { shouldReplaceSessionHead } from "./workspaceActiveSnapshot/summaryHelpers";

describe("sessionHeadState", () => {
  it("sanitizes partial head content while preserving final thought events", () => {
    const head: SessionHeadSnapshot = {
      session: {
        id: "session-1",
        task_id: "task-1",
        workspace_id: "ws-1",
        worktree_id: "wt-1",
        provider_id: "codex",
        model_id: "gpt-5",
        title: "Session",
        agent_role: "implementer",
        status: "active",
        created_at: "2026-03-09T00:00:00.000Z",
        updated_at: "2026-03-09T00:00:00.000Z",
      },
      turns: [
        {
          turn_id: "turn-1",
          session_id: "session-1",
          run_id: null,
          user_message_id: "user-1",
          status: "running",
          start_seq: 1,
          end_seq: null,
          started_at: "2026-03-09T00:00:00.000Z",
          updated_at: "2026-03-09T00:00:00.000Z",
          assistant_partial: "partial assistant",
          thought_partial: "partial thought",
          metrics_json: null,
          tool_total: 0,
          tool_pending: 0,
          tool_running: 0,
          tool_completed: 0,
          tool_failed: 0,
        },
      ],
      tool_summaries: [],
      events: [
        {
          seq: 2,
          id: "assistant-chunk",
          session_id: "session-1",
          turn_id: "turn-1",
          event_type: "assistant_chunk",
          payload_json: { content_fragment: "partial" },
          created_at: "2026-03-09T00:00:01.000Z",
        },
        {
          seq: 3,
          id: "thought-partial",
          session_id: "session-1",
          turn_id: "turn-1",
          event_type: "thought_chunk",
          payload_json: { content_fragment: "partial thought" },
          created_at: "2026-03-09T00:00:02.000Z",
        },
        {
          seq: 4,
          id: "assistant-complete",
          session_id: "session-1",
          turn_id: "turn-1",
          event_type: "assistant_complete",
          payload_json: { full_content: "final answer", message_id: "provider-msg-1", order_seq: 2 },
          created_at: "2026-03-09T00:00:02.500Z",
        },
        {
          seq: 5,
          id: "thought-final",
          session_id: "session-1",
          turn_id: "turn-1",
          event_type: "thought_chunk",
          payload_json: { is_final: true, full_content: "final thought" },
          created_at: "2026-03-09T00:00:03.000Z",
        },
      ] as SessionEvent[],
      messages: [],
      last_event_seq: 5,
      state_rev: 0,
      activity: { is_working: true },
      has_more_turns: false,
      history_cursor: null,
      has_more_history: false,
    };

    const sanitized = sanitizeSessionHeadSnapshot(head);
    expect(sanitized.turns[0]?.assistant_partial).toBeNull();
    expect(sanitized.turns[0]?.thought_partial).toBeNull();
    expect((sanitized.events ?? []).map((event) => event.id)).toEqual(["thought-final"]);
  });

  it("keeps tool summaries aligned to the visible head turns", () => {
    const turns: SessionTurn[] = [
      {
        turn_id: "turn-2",
        session_id: "session-1",
        run_id: null,
        user_message_id: "user-2",
        status: "completed",
        start_seq: 10,
        end_seq: 12,
        started_at: "2026-03-09T00:00:10.000Z",
        updated_at: "2026-03-09T00:00:12.000Z",
        assistant_partial: "",
        thought_partial: "",
        metrics_json: null,
        tool_total: 1,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 1,
        tool_failed: 0,
      },
    ];
    const previous: SessionTurnToolSummary[] = [
      {
        session_id: "session-1",
        tool_call_id: "tool-old",
        turn_id: "turn-1",
        status: "completed",
        order_seq: 10,
        created_at: "2026-03-09T00:00:01.000Z",
        updated_at: "2026-03-09T00:00:02.000Z",
      },
    ];
    const incoming: SessionTurnToolSummary[] = [
      {
        session_id: "session-1",
        tool_call_id: "tool-new",
        turn_id: "turn-2",
        status: "completed",
        order_seq: 11,
        created_at: "2026-03-09T00:00:11.000Z",
        updated_at: "2026-03-09T00:00:12.000Z",
      },
    ];

    expect(mergeSessionToolSummaries(previous, incoming, turns).map((summary) => summary.tool_call_id)).toEqual([
      "tool-new",
    ]);
  });

  it("keeps workspace-active heads bounded to a compact tail window", () => {
    const turnCount = 8;
    const head: SessionHeadSnapshot = {
      session: {
        id: "session-1",
        task_id: "task-1",
        workspace_id: "ws-1",
        worktree_id: "wt-1",
        provider_id: "codex",
        model_id: "gpt-5",
        title: "Session",
        agent_role: "implementer",
        status: "active",
        created_at: "2026-03-09T00:00:00.000Z",
        updated_at: "2026-03-09T00:00:00.000Z",
      },
      turns: Array.from({ length: turnCount }, (_, index) => ({
        turn_id: `turn-${index + 1}`,
        session_id: "session-1",
        run_id: null,
        user_message_id: `user-${index + 1}`,
        status: "completed",
        start_seq: index + 1,
        end_seq: index + 2,
        started_at: `2026-03-09T00:00:0${index}.000Z`,
        updated_at: `2026-03-09T00:00:0${index}.000Z`,
        assistant_partial: null,
        thought_partial: null,
        metrics_json: null,
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      })),
      tool_summaries: Array.from({ length: turnCount }, (_, index) => ({
        session_id: "session-1",
        tool_call_id: `tool-${index + 1}`,
        turn_id: `turn-${index + 1}`,
        status: "completed",
        order_seq: index + 1,
        created_at: `2026-03-09T00:00:0${index}.000Z`,
        updated_at: `2026-03-09T00:00:0${index}.000Z`,
      })),
      messages: Array.from({ length: turnCount }, (_, index) => ({
        id: `message-${index + 1}`,
        session_id: "session-1",
        task_id: "task-1",
        turn_id: `turn-${index + 1}`,
        role: index % 2 === 0 ? "user" : "assistant",
        content: `message-${index + 1}`,
        delivery: "immediate",
        created_at: `2026-03-09T00:00:0${index}.000Z`,
        updated_at: `2026-03-09T00:00:0${index}.000Z`,
      })),
      events: Array.from({ length: turnCount }, (_, index) => ({
        seq: index + 1,
        id: `event-${index + 1}`,
        session_id: "session-1",
        turn_id: `turn-${index + 1}`,
        event_type: "done",
        payload_json: { ok: true, index },
        created_at: `2026-03-09T00:00:0${index}.000Z`,
      })) as SessionEvent[],
      last_event_seq: turnCount,
      projection_rev: turnCount,
      state_rev: turnCount,
      activity: { is_working: false, last_turn_status: "completed" },
      head_window: {
        turn_limit: 60,
        message_limit: 1000,
        event_limit: 100,
        byte_limit: 9_999_999,
        turn_count: turnCount,
        message_count: turnCount,
        event_count: turnCount,
        bytes: 9_999_999,
        truncated: false,
      },
      has_more_turns: false,
      has_more_history: true,
      history_cursor: 1,
    };

    const compacted = compactActiveSessionHeadSnapshot(head);

    expect(compacted.turns.map((turn) => turn.turn_id)).toEqual([
      "turn-4",
      "turn-5",
      "turn-6",
      "turn-7",
      "turn-8",
    ]);
    expect(compacted.messages.map((message) => message.turn_id)).toEqual([
      "turn-4",
      "turn-5",
      "turn-6",
      "turn-7",
      "turn-8",
    ]);
    expect(compacted.tool_summaries?.map((summary) => summary.turn_id)).toEqual([
      "turn-4",
      "turn-5",
      "turn-6",
      "turn-7",
      "turn-8",
    ]);
    expect(compacted.events).toEqual([]);
    expect(compacted.has_more_turns).toBe(true);
    expect(compacted.head_window?.turn_limit).toBe(5);
    expect(compacted.head_window?.message_limit).toBe(200);
    expect(compacted.head_window?.event_limit).toBe(0);
    expect(compacted.head_window?.byte_limit).toBe(256_000);
    expect(compacted.head_window?.truncated).toBe(true);
  });

  it("does not replace a same-seq head with an older projection revision", () => {
    const prev: SessionHeadSnapshot = {
      session: {
        id: "session-1",
        task_id: "task-1",
        workspace_id: "ws-1",
        worktree_id: "wt-1",
        provider_id: "codex",
        model_id: "gpt-5",
        title: "Session",
        agent_role: "implementer",
        status: "active",
        created_at: "2026-03-09T00:00:00.000Z",
        updated_at: "2026-03-09T00:00:00.000Z",
      },
      turns: [],
      tool_summaries: [],
      events: [],
      messages: [],
      last_event_seq: 5,
      projection_rev: 8,
      state_rev: 8,
      activity: { is_working: false, last_turn_status: "completed" },
      has_more_turns: false,
      history_cursor: null,
      has_more_history: false,
    };
    const next: SessionHeadSnapshot = {
      ...prev,
      projection_rev: 7,
      state_rev: 7,
    };

    expect(shouldReplaceSessionHead(prev, next)).toBe(false);
  });

  it("keeps a bounded message tail when the message count exceeds the limit", () => {
    const head: SessionHeadSnapshot = {
      session: {
        id: "session-messages",
        task_id: "task-1",
        workspace_id: "ws-1",
        worktree_id: "wt-1",
        provider_id: "codex",
        model_id: "gpt-5",
        title: "Message-heavy session",
        agent_role: "implementer",
        status: "active",
        created_at: "2026-03-09T00:00:00.000Z",
        updated_at: "2026-03-09T00:00:00.000Z",
      },
      turns: [
        {
          turn_id: "turn-1",
          session_id: "session-messages",
          run_id: null,
          user_message_id: "user-1",
          status: "completed",
          start_seq: 1,
          end_seq: 2,
          started_at: "2026-03-09T00:00:00.000Z",
          updated_at: "2026-03-09T00:00:00.000Z",
          assistant_partial: null,
          thought_partial: null,
          metrics_json: null,
          tool_total: 0,
          tool_pending: 0,
          tool_running: 0,
          tool_completed: 0,
          tool_failed: 0,
        },
      ],
      messages: Array.from({ length: 260 }, (_, index) => ({
        id: `message-${index + 1}`,
        session_id: "session-messages",
        task_id: "task-1",
        turn_id: "turn-1",
        order_seq: index + 1,
        role: index % 2 === 0 ? "user" : "assistant",
        content: `message-${index + 1}`,
        delivery: "immediate",
        created_at: `2026-03-09T00:00:${String(index % 60).padStart(2, "0")}.000Z`,
      })),
      events: [],
      last_event_seq: 1,
      projection_rev: 1,
      state_rev: 1,
      activity: { is_working: false, last_turn_status: "completed" },
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
    };

    const compacted = compactActiveSessionHeadSnapshot(head);

    expect(compacted.turns).toHaveLength(1);
    expect(compacted.messages).toHaveLength(200);
    expect(compacted.messages[0]?.id).toBe("message-61");
    expect(compacted.messages.at(-1)?.id).toBe("message-260");
    expect(compacted.head_window?.truncated).toBe(true);
  });

  it("drops oversized message bodies before discarding retained turns for the byte limit", () => {
    const oversizedContent = "😀".repeat(500_000);
    const head: SessionHeadSnapshot = {
      session: {
        id: "session-oversized",
        task_id: "task-1",
        workspace_id: "ws-1",
        worktree_id: "wt-1",
        provider_id: "codex",
        model_id: "gpt-5",
        title: "Oversized session",
        agent_role: "implementer",
        status: "active",
        created_at: "2026-03-09T00:00:00.000Z",
        updated_at: "2026-03-09T00:00:00.000Z",
      },
      turns: [
        {
          turn_id: "turn-1",
          session_id: "session-oversized",
          run_id: null,
          user_message_id: "user-1",
          status: "completed",
          start_seq: 1,
          end_seq: 2,
          started_at: "2026-03-09T00:00:00.000Z",
          updated_at: "2026-03-09T00:00:00.000Z",
          assistant_partial: null,
          thought_partial: null,
          metrics_json: null,
          tool_total: 0,
          tool_pending: 0,
          tool_running: 0,
          tool_completed: 0,
          tool_failed: 0,
        },
      ],
      messages: [
        {
          id: "message-1",
          session_id: "session-oversized",
          task_id: "task-1",
          turn_id: "turn-1",
          role: "assistant",
          content: oversizedContent,
          delivery: "immediate",
          created_at: "2026-03-09T00:00:01.000Z",
        },
      ],
      events: [],
      last_event_seq: 1,
      projection_rev: 1,
      state_rev: 1,
      activity: { is_working: false, last_turn_status: "completed" },
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
    };

    const compacted = compactActiveSessionHeadSnapshot(head);

    expect(compacted.turns).toHaveLength(1);
    expect(compacted.messages).toEqual([]);
    expect(compacted.head_window?.message_count).toBe(0);
    expect(compacted.head_window?.bytes).toBeLessThanOrEqual(compacted.head_window?.byte_limit ?? 0);
    expect(compacted.head_window?.truncated).toBe(true);
  });

  it("trims turnless message tails to the bounded window", () => {
    const head: SessionHeadSnapshot = {
      session: {
        id: "session-turnless",
        task_id: "task-1",
        workspace_id: "ws-1",
        worktree_id: "wt-1",
        provider_id: "codex",
        model_id: "gpt-5",
        title: "Turnless messages",
        agent_role: "implementer",
        status: "active",
        created_at: "2026-03-09T00:00:00.000Z",
        updated_at: "2026-03-09T00:00:00.000Z",
      },
      turns: [
        {
          turn_id: "turn-1",
          session_id: "session-turnless",
          run_id: null,
          user_message_id: "user-1",
          status: "completed",
          start_seq: 1,
          end_seq: 2,
          started_at: "2026-03-09T00:00:00.000Z",
          updated_at: "2026-03-09T00:00:00.000Z",
          assistant_partial: null,
          thought_partial: null,
          metrics_json: null,
          tool_total: 0,
          tool_pending: 0,
          tool_running: 0,
          tool_completed: 0,
          tool_failed: 0,
        },
      ],
      tool_summaries: [],
      messages: Array.from({ length: 260 }, (_, index) => ({
        id: `message-${index + 1}`,
        session_id: "session-turnless",
        task_id: "task-1",
        turn_id: null,
        order_seq: index + 1,
        role: index % 2 === 0 ? "user" : "assistant",
        content: `message-${index + 1}`,
        delivery: "immediate",
        created_at: `2026-03-09T00:00:${String(index % 60).padStart(2, "0")}.000Z`,
      })),
      events: [],
      last_event_seq: 1,
      projection_rev: 1,
      state_rev: 1,
      activity: { is_working: false, last_turn_status: "completed" },
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
    };

    const compacted = compactActiveSessionHeadSnapshot(head);

    expect(compacted.turns).toHaveLength(1);
    expect(compacted.messages).toHaveLength(200);
    expect(compacted.messages[0]?.id).toBe("message-61");
    expect(compacted.messages.at(-1)?.id).toBe("message-260");
    expect(compacted.head_window?.truncated).toBe(true);
  });

  it("caps tool summaries in active bootstrap heads", () => {
    const head: SessionHeadSnapshot = {
      session: {
        id: "session-tool-cap",
        task_id: "task-1",
        workspace_id: "ws-1",
        worktree_id: "wt-1",
        provider_id: "codex",
        model_id: "gpt-5",
        title: "Tool cap",
        agent_role: "implementer",
        status: "active",
        created_at: "2026-03-09T00:00:00.000Z",
        updated_at: "2026-03-09T00:00:00.000Z",
      },
      turns: [
        {
          turn_id: "turn-1",
          session_id: "session-tool-cap",
          run_id: null,
          user_message_id: "user-1",
          status: "running",
          start_seq: 1,
          end_seq: null,
          started_at: "2026-03-09T00:00:00.000Z",
          updated_at: "2026-03-09T00:00:00.000Z",
          assistant_partial: null,
          thought_partial: null,
          metrics_json: null,
          tool_total: 335,
          tool_pending: 0,
          tool_running: 0,
          tool_completed: 335,
          tool_failed: 0,
        },
      ],
      tool_summaries: Array.from({ length: 335 }, (_, index) => ({
        session_id: "session-tool-cap",
        tool_call_id: `tool-${String(index).padStart(3, "0")}`,
        turn_id: "turn-1",
        status: "completed",
        order_seq: index,
        created_at: `2026-03-09T00:00:${String(index % 60).padStart(2, "0")}.000Z`,
        updated_at: `2026-03-09T00:00:${String(index % 60).padStart(2, "0")}.000Z`,
      })),
      messages: [
        {
          id: "message-latest",
          session_id: "session-tool-cap",
          task_id: "task-1",
          turn_id: "turn-1",
          order_seq: 336,
          role: "assistant",
          content: "latest assistant content",
          delivery: "immediate",
          created_at: "2026-03-09T00:01:00.000Z",
        },
      ],
      events: [],
      last_event_seq: 335,
      projection_rev: 335,
      state_rev: 335,
      activity: { is_working: true, last_turn_status: "running" },
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
    };

    const compacted = compactActiveSessionHeadSnapshot(head);

    expect(compacted.messages.map((message) => message.content)).toEqual(["latest assistant content"]);
    expect(compacted.tool_summaries).toHaveLength(96);
    expect(compacted.tool_summaries?.[0]?.tool_call_id).toBe("tool-239");
    expect(compacted.tool_summaries?.at(-1)?.tool_call_id).toBe("tool-334");
    expect(compacted.has_more_turns).toBe(false);
    expect(compacted.head_window?.truncated).toBe(true);
    expect(compacted.head_window?.bytes).toBeLessThanOrEqual(256_000);
  });

  it("shares message and turn merge ordering across snapshot consumers", () => {
    const mergedTurns = mergeSessionTurns(
      [
        {
          turn_id: "turn-1",
          session_id: "session-1",
          run_id: null,
          user_message_id: "user-1",
          status: "running",
          start_seq: 1,
          end_seq: null,
          started_at: "2026-03-09T00:00:00.000Z",
          updated_at: "2026-03-09T00:00:00.000Z",
          assistant_partial: "hel",
          thought_partial: "",
          metrics_json: null,
          tool_total: 0,
          tool_pending: 0,
          tool_running: 0,
          tool_completed: 0,
          tool_failed: 0,
        },
      ],
      [
        {
          turn_id: "turn-1",
          session_id: "session-1",
          run_id: null,
          user_message_id: "user-1",
          status: "running",
          start_seq: 1,
          end_seq: null,
          started_at: "2026-03-09T00:00:00.000Z",
          updated_at: "2026-03-09T00:00:01.000Z",
          assistant_partial: "hello",
          thought_partial: "",
          metrics_json: null,
          tool_total: 0,
          tool_pending: 0,
          tool_running: 0,
          tool_completed: 0,
          tool_failed: 0,
        },
      ],
    );
    const mergedMessages = mergeSessionMessages(
      [
        {
          id: "message-2",
          session_id: "session-1",
          task_id: "task-1",
          role: "assistant",
          content: "second",
          delivery: "immediate",
          created_at: "2026-03-09T00:00:02.000Z",
          turn_sequence: 2,
        },
      ],
      [
        {
          id: "message-1",
          session_id: "session-1",
          task_id: "task-1",
          role: "user",
          content: "first",
          delivery: "immediate",
          created_at: "2026-03-09T00:00:01.000Z",
          turn_sequence: 1,
        },
      ],
    );

    expect(mergedTurns[0]?.assistant_partial).toBeNull();
    expect(mergedMessages.map((message) => message.id)).toEqual(["message-1", "message-2"]);
  });

  it("lets terminal turn projections clear stale running tool counts", () => {
    const mergedTurns = mergeSessionTurns(
      [
        {
          turn_id: "turn-1",
          session_id: "session-1",
          run_id: null,
          user_message_id: "user-1",
          status: "running",
          start_seq: 1,
          end_seq: null,
          started_at: "2026-03-09T00:00:00.000Z",
          updated_at: "2026-03-09T00:00:00.000Z",
          assistant_partial: null,
          thought_partial: "",
          metrics_json: null,
          tool_total: 1,
          tool_pending: 0,
          tool_running: 1,
          tool_completed: 0,
          tool_failed: 0,
        },
      ],
      [
        {
          turn_id: "turn-1",
          session_id: "session-1",
          run_id: null,
          user_message_id: "user-1",
          status: "failed",
          start_seq: 1,
          end_seq: 4,
          started_at: "2026-03-09T00:00:00.000Z",
          updated_at: "2026-03-09T00:00:02.000Z",
          assistant_partial: null,
          thought_partial: "",
          metrics_json: null,
          tool_total: 1,
          tool_pending: 0,
          tool_running: 0,
          tool_completed: 0,
          tool_failed: 1,
        },
      ],
    );

    expect(mergedTurns[0]?.status).toBe("failed");
    expect(mergedTurns[0]?.tool_pending).toBe(0);
    expect(mergedTurns[0]?.tool_running).toBe(0);
    expect(mergedTurns[0]?.tool_failed).toBe(1);
  });

  it("normalizes stale live tool counts on new terminal turns", () => {
    const mergedTurns = mergeSessionTurns([], [
      {
        turn_id: "turn-1",
        session_id: "session-1",
        run_id: null,
        user_message_id: "user-1",
        status: "failed",
        start_seq: 1,
        end_seq: 4,
        started_at: "2026-03-09T00:00:00.000Z",
        updated_at: "2026-03-09T00:00:02.000Z",
        assistant_partial: null,
        thought_partial: "",
        metrics_json: null,
        tool_total: 1,
        tool_pending: 0,
        tool_running: 1,
        tool_completed: 0,
        tool_failed: 1,
      },
    ]);

    expect(mergedTurns[0]?.status).toBe("failed");
    expect(mergedTurns[0]?.tool_running).toBe(0);
  });

  it("orders messages by order_seq before created_at when both are present", () => {
    const mergedMessages = mergeSessionMessages(
      [
        {
          id: "message-2",
          session_id: "session-1",
          task_id: "task-1",
          role: "assistant",
          content: "second",
          delivery: "immediate",
          created_at: "2026-03-09T00:00:02.000Z",
          turn_sequence: 2,
          order_seq: 2,
        },
      ],
      [
        {
          id: "message-1",
          session_id: "session-1",
          task_id: "task-1",
          role: "assistant",
          content: "first",
          delivery: "immediate",
          created_at: "2026-03-09T00:00:03.000Z",
          turn_sequence: 2,
          order_seq: 1,
        },
      ],
    );

    expect(mergedMessages.map((message) => message.id)).toEqual(["message-1", "message-2"]);
  });
});
