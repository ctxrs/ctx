import { cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { Message, SessionEvent, SessionTurn, SessionTurnTool } from "../api/client";
import type { AskUserQuestionAnswerState } from "./SessionPage.types";
import { deriveMessagesKey, deriveTurnsKey } from "./SessionPage.workbenchViewModel";
import { useWorkbenchThreadViewModelController } from "./useWorkbenchThreadViewModelController";
import {
  buildWorkbenchThreadViewModelWarmKey,
  readWarmWorkbenchThreadViewModel,
  resetWarmWorkbenchThreadViewModelCache,
} from "./workbenchThreadViewModelWarmCache";

type ControllerProps = Parameters<typeof useWorkbenchThreadViewModelController>[0];
type ControllerResult = ReturnType<typeof useWorkbenchThreadViewModelController>;
type HarnessProps = Omit<ControllerProps, "assistantStreamingStamp"> & {
  assistantStreamingStamp?: string;
};

let latestResult: ControllerResult | null = null;

const isToolItem = (
  item: ControllerResult["listItems"][number],
): item is Extract<ControllerResult["listItems"][number], { kind: "tool" }> => item.kind === "tool";

const isAssistantItem = (
  item: ControllerResult["listItems"][number],
): item is Extract<ControllerResult["listItems"][number], { kind: "assistant" }> => item.kind === "assistant";

const isTurnStatusItem = (
  item: ControllerResult["listItems"][number],
): item is Extract<ControllerResult["listItems"][number], { kind: "turn_status" }> => item.kind === "turn_status";

function Harness({ assistantStreamingStamp = "0:0", ...props }: HarnessProps) {
  latestResult = useWorkbenchThreadViewModelController({ ...props, assistantStreamingStamp });
  return null;
}

afterEach(() => {
  cleanup();
  latestResult = null;
  resetWarmWorkbenchThreadViewModelCache();
});

const turns: SessionTurn[] = [
  {
    turn_id: "turn-1",
    session_id: "session-1",
    run_id: null,
    user_message_id: "message-1",
    status: "completed",
    start_seq: 1,
    end_seq: 2,
    started_at: "2025-12-15T00:00:00.000Z",
    updated_at: "2025-12-15T00:00:01.000Z",
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

const messages = [
  {
    id: "message-1",
    session_id: "session-1",
    task_id: "task-1",
    turn_id: "turn-1",
    turn_sequence: 1,
    role: "user",
    content: "Run the check",
    attachments: [],
    delivery: "immediate",
    created_at: "2025-12-15T00:00:00.000Z",
    order_seq: 1,
  },
 ] as unknown as Message[];

const events: SessionEvent[] = [
  {
    seq: 1,
    id: "event-1",
    session_id: "session-1",
    run_id: "run-1",
    turn_id: "turn-1",
    event_type: "tool_call",
    payload_json: {
      tool_call_id: "tool-1",
      order_seq: 2,
    },
    created_at: "2025-12-15T00:00:00.500Z",
  },
];

const toolsByTurnId: Record<string, SessionTurnTool[]> = {
  "turn-1": [
    {
      session_id: "session-1",
      tool_call_id: "tool-1",
      turn_id: "turn-1",
      tool_kind: "shell",
      title: "pnpm test",
      status: "completed",
      input_json: { command: "pnpm test" },
      output_text: "ok",
      order_seq: 2,
      input_truncated: false,
      input_original_bytes: 9,
      output_truncated: false,
      output_original_bytes: 2,
      created_at: "2025-12-15T00:00:00.500Z",
      updated_at: "2025-12-15T00:00:01.000Z",
    },
  ],
};

const buildTurnsStamp = (value: SessionTurn[], rev = 0) => `${rev}:${deriveTurnsKey(value)}`;
const buildMessagesStamp = (value: Message[], rev = 0) => `${rev}:${deriveMessagesKey(value)}`;

function renderController(
  overrides: Partial<ControllerProps> = {},
): ReturnType<typeof render> {
  const baseProps: ControllerProps = {
    sessionId: "session-1",
    turnsStamp: buildTurnsStamp(turns),
    assistantStreamingStamp: "0:0",
    messagesStamp: buildMessagesStamp(messages),
    eventsStamp: "0:1",
    verbosity: "default",
    turns,
    messages,
    events,
    toolsByTurnId,
    toolSummariesReady: true,
    askUserQuestionAnswers: new Map<string, AskUserQuestionAnswerState>(),
    enableDebugEvents: false,
  };
  return render(<Harness {...baseProps} {...overrides} />);
}

function expectGroup(key: string) {
  const group = latestResult?.view.groups.find((candidate) => candidate.key === key);
  expect(group).toBeTruthy();
  return group!;
}

function expectListItem(id: string) {
  const item = latestResult?.listItems.find((candidate) => candidate.id === id);
  expect(item).toBeTruthy();
  return item!;
}

describe("useWorkbenchThreadViewModelController", () => {
  it("rebuilds when explicit message stamps change without structural length changes", async () => {
    const { rerender } = renderController();

    await waitFor(() => {
      expect(latestResult?.view.groups[0]?.header?.content).toBe("Run the check");
    });

    const nextMessages = [
      {
        ...messages[0],
        content: "Run the build",
      },
    ] as unknown as Message[];

    rerender(
      <Harness
        sessionId="session-1"
        turnsStamp={buildTurnsStamp(turns)}
        messagesStamp={buildMessagesStamp(nextMessages, 1)}
        eventsStamp="0:1"
        verbosity="default"
        turns={turns}
        messages={nextMessages}
        events={events}
        toolsByTurnId={toolsByTurnId}
        toolSummariesReady
        askUserQuestionAnswers={new Map<string, AskUserQuestionAnswerState>()}
        enableDebugEvents={false}
      />,
    );

    await waitFor(() => {
      expect(latestResult?.view.groups[0]?.header?.content).toBe("Run the build");
    });
  });

  it("rebuilds when tool summaries become ready without transcript stamp changes", async () => {
    const { rerender } = renderController({ toolSummariesReady: false });

    await waitFor(() => {
      const tool = latestResult?.listItems.find(isToolItem);
      expect(tool?.title).not.toBe("pnpm test");
    });

    rerender(
      <Harness
        sessionId="session-1"
        turnsStamp={buildTurnsStamp(turns)}
        messagesStamp={buildMessagesStamp(messages)}
        eventsStamp="0:1"
        verbosity="default"
        turns={turns}
        messages={messages}
        events={events}
        toolsByTurnId={toolsByTurnId}
        toolSummariesReady
        askUserQuestionAnswers={new Map<string, AskUserQuestionAnswerState>()}
        enableDebugEvents={false}
      />,
    );

    await waitFor(() => {
      const tool = latestResult?.listItems.find(isToolItem);
      expect(tool?.title).toBe("pnpm test");
    });
  });

  it("prefers structural reconcile when head repair changes messages and tool summaries together", async () => {
    const { rerender } = renderController({
      toolsByTurnId: {},
      toolSummariesReady: false,
    });

    await waitFor(() => {
      expect(latestResult?.listItems.filter(isAssistantItem)).toHaveLength(0);
    });

    const repairedMessages = [
      ...messages,
      {
        id: "message-2",
        session_id: "session-1",
        task_id: "task-1",
        turn_id: "turn-1",
        turn_sequence: 3,
        role: "assistant",
        content: "Recovered final content",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:01.000Z",
        order_seq: 3,
      },
    ] as unknown as Message[];

    rerender(
      <Harness
        sessionId="session-1"
        turnsStamp={buildTurnsStamp(turns)}
        messagesStamp={buildMessagesStamp(repairedMessages, 1)}
        eventsStamp="0:1"
        verbosity="default"
        turns={turns}
        messages={repairedMessages}
        events={events}
        toolsByTurnId={toolsByTurnId}
        toolSummariesReady
        askUserQuestionAnswers={new Map<string, AskUserQuestionAnswerState>()}
        enableDebugEvents={false}
      />,
    );

    await waitFor(() => {
      expect(latestResult?.listItems.filter(isAssistantItem).map((item) => item.content)).toContain(
        "Recovered final content",
      );
    });
    expect(latestResult?.lastOp.kind).toBe("reconcile");
  });

  it("ignores outer tool-map churn when per-turn tool arrays are unchanged", async () => {
    const askUserQuestionAnswers = new Map<string, AskUserQuestionAnswerState>();
    const { rerender } = renderController({ askUserQuestionAnswers });

    await waitFor(() => {
      const tool = latestResult?.listItems.find(isToolItem);
      expect(tool?.title).toBe("pnpm test");
    });

    const warmKey = buildWorkbenchThreadViewModelWarmKey({
      sessionId: "session-1",
      projectionRev: 0,
      turnsStamp: buildTurnsStamp(turns),
      assistantStreamingStamp: "0:0",
      messagesStamp: buildMessagesStamp(messages),
      eventsStamp: "0:1",
      verbosity: "default",
      turns,
      messages,
      events,
      toolsByTurnId,
      toolSummariesReady: true,
      askUserQuestionAnswers,
      enableDebugEvents: false,
    });
    const beforeWarmSnapshot = readWarmWorkbenchThreadViewModel("session-1", warmKey);

    rerender(
      <Harness
        sessionId="session-1"
        turnsStamp={buildTurnsStamp(turns)}
        messagesStamp={buildMessagesStamp(messages)}
        eventsStamp="0:1"
        verbosity="default"
        turns={turns}
        messages={messages}
        events={events}
        toolsByTurnId={{ ...toolsByTurnId }}
        toolSummariesReady
        askUserQuestionAnswers={askUserQuestionAnswers}
        enableDebugEvents={false}
      />,
    );

    await waitFor(() => {
      const tool = latestResult?.listItems.find(isToolItem);
      expect(tool?.title).toBe("pnpm test");
    });

    expect(latestResult?.listItems.filter(isToolItem).map((item) => item.title)).toEqual(["pnpm test"]);
    expect(latestResult?.lastOp.kind).toBe("noop");
    expect(readWarmWorkbenchThreadViewModel("session-1", warmKey)).toBe(beforeWarmSnapshot);
  });

  it("rebuilds filtered thread items when verbosity changes without transcript stamp changes", async () => {
    const { rerender } = renderController();

    await waitFor(() => {
      expect(latestResult?.listItems.some(isToolItem)).toBe(true);
    });

    rerender(
      <Harness
        sessionId="session-1"
        turnsStamp={buildTurnsStamp(turns)}
        messagesStamp={buildMessagesStamp(messages)}
        eventsStamp="0:1"
        verbosity="terse"
        turns={turns}
        messages={messages}
        events={events}
        toolsByTurnId={toolsByTurnId}
        toolSummariesReady
        askUserQuestionAnswers={new Map<string, AskUserQuestionAnswerState>()}
        enableDebugEvents={false}
      />,
    );

    await waitFor(() => {
      expect(latestResult?.listItems.some(isToolItem)).toBe(false);
    });
  });

  it("updates only the dirty turn group on event-only appends when transcript stamps stay stable", async () => {
    const askUserQuestionAnswers = new Map<string, AskUserQuestionAnswerState>();
    const emptyToolsByTurnId: Record<string, SessionTurnTool[]> = {};
    const multiTurns = [
      {
        turn_id: "turn-1",
        session_id: "session-1",
        run_id: null,
        user_message_id: "message-1",
        status: "running",
        start_seq: 1,
        end_seq: 2,
        started_at: "2025-12-15T00:00:00.000Z",
        updated_at: "2025-12-15T00:00:01.000Z",
        assistant_partial: "",
        thought_partial: "",
        metrics_json: null,
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
      {
        turn_id: "turn-2",
        session_id: "session-1",
        run_id: null,
        user_message_id: "message-2",
        status: "completed",
        start_seq: 3,
        end_seq: 4,
        started_at: "2025-12-15T00:00:02.000Z",
        updated_at: "2025-12-15T00:00:03.000Z",
        assistant_partial: "",
        thought_partial: "",
        metrics_json: null,
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
    ] as SessionTurn[];
    const multiMessages = [
      {
        id: "message-1",
        session_id: "session-1",
        task_id: "task-1",
        turn_id: "turn-1",
        turn_sequence: 1,
        role: "user",
        content: "First turn",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:00.000Z",
        order_seq: 1,
      },
      {
        id: "message-2",
        session_id: "session-1",
        task_id: "task-1",
        turn_id: "turn-2",
        turn_sequence: 2,
        role: "user",
        content: "Second turn",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:02.000Z",
        order_seq: 3,
      },
    ] as unknown as Message[];
    const turnsStamp = buildTurnsStamp(multiTurns);
    const messagesStamp = buildMessagesStamp(multiMessages);
    const { rerender } = renderController({
      turns: multiTurns,
      messages: multiMessages,
      events: [],
      eventsStamp: "0:0",
      turnsStamp,
      messagesStamp,
      toolsByTurnId: emptyToolsByTurnId,
      askUserQuestionAnswers,
    });

    await waitFor(() => {
      expect(latestResult?.view.groups.map((group) => group.key)).toEqual(["turn-turn-1", "turn-turn-2"]);
    });

    const firstGroupBefore = expectGroup("turn-turn-1");
    const secondGroupBefore = expectGroup("turn-turn-2");
    const secondHeaderBefore = expectListItem("turn-header-turn-2");

    const appendedEvents: SessionEvent[] = [
      {
        seq: 1,
        id: "event-tool-1",
        session_id: "session-1",
        run_id: "run-1",
        turn_id: "turn-1",
        event_type: "tool_call",
        payload_json: {
          tool_call_id: "tool-1",
          title: "ls -la",
          order_seq: 2,
        },
        created_at: "2025-12-15T00:00:01.500Z",
      },
    ];

    rerender(
      <Harness
        sessionId="session-1"
        turnsStamp={turnsStamp}
        messagesStamp={messagesStamp}
        eventsStamp="1:1"
        verbosity="default"
        turns={multiTurns}
        messages={multiMessages}
        events={appendedEvents}
        toolsByTurnId={emptyToolsByTurnId}
        toolSummariesReady
        askUserQuestionAnswers={askUserQuestionAnswers}
        enableDebugEvents={false}
      />,
    );

    await waitFor(() => {
      const firstGroup = expectGroup("turn-turn-1");
      expect(firstGroup.items.some(isToolItem)).toBe(true);
    });

    expect(expectGroup("turn-turn-1")).not.toBe(firstGroupBefore);
    expect(expectGroup("turn-turn-2")).toBe(secondGroupBefore);
    expect(expectListItem("turn-header-turn-2")).toBe(secondHeaderBefore);
  });

  it("patches streaming overlay changes without structural transcript stamps", async () => {
    const askUserQuestionAnswers = new Map<string, AskUserQuestionAnswerState>();
    const emptyToolsByTurnId: Record<string, SessionTurnTool[]> = {};
    const multiTurns = [
      {
        turn_id: "turn-1",
        session_id: "session-1",
        run_id: null,
        user_message_id: "message-1",
        status: "running",
        start_seq: 1,
        end_seq: null,
        started_at: "2025-12-15T00:00:00.000Z",
        updated_at: "2025-12-15T00:00:01.000Z",
        assistant_partial: "",
        thought_partial: "",
        metrics_json: null,
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
      {
        turn_id: "turn-2",
        session_id: "session-1",
        run_id: null,
        user_message_id: "message-2",
        status: "completed",
        start_seq: 3,
        end_seq: 4,
        started_at: "2025-12-15T00:00:02.000Z",
        updated_at: "2025-12-15T00:00:03.000Z",
        assistant_partial: "",
        thought_partial: "",
        metrics_json: null,
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
    ] as SessionTurn[];
    const multiMessages = [
      {
        id: "message-1",
        session_id: "session-1",
        task_id: "task-1",
        turn_id: "turn-1",
        turn_sequence: 1,
        role: "user",
        content: "First turn",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:00.000Z",
        order_seq: 1,
      },
      {
        id: "message-2",
        session_id: "session-1",
        task_id: "task-1",
        turn_id: "turn-2",
        turn_sequence: 2,
        role: "user",
        content: "Second turn",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:02.000Z",
        order_seq: 3,
      },
    ] as unknown as Message[];
    const streamingEvent: SessionEvent = {
      seq: 2,
      id: "event-assistant-chunk-1",
      session_id: "session-1",
      run_id: "run-1",
      turn_id: "turn-1",
      event_type: "assistant_chunk",
      payload_json: {
        content_fragment: "Hello",
        message_id: "provider-message-1",
        order_seq: 2,
      },
      created_at: "2025-12-15T00:00:01.500Z",
    };
    const turnsStamp = buildTurnsStamp(multiTurns);
    const messagesStamp = buildMessagesStamp(multiMessages);
    const { rerender } = renderController({
      turns: multiTurns,
      messages: multiMessages,
      events: [streamingEvent],
      eventsStamp: "1:2",
      turnsStamp,
      assistantStreamingStamp: "1:1",
      assistantStreamingByTurnId: {
        "turn-1": { content: "Hello", providerMessageId: "provider-message-1", orderSeq: 2 },
      },
      messagesStamp,
      toolsByTurnId: emptyToolsByTurnId,
      askUserQuestionAnswers,
    });

    await waitFor(() => {
      const pendingAssistant = expectListItem("assistant-turn-1-pending");
      expect(pendingAssistant.kind).toBe("assistant");
      if (pendingAssistant.kind === "assistant") {
        expect(pendingAssistant.content).toBe("Hello");
      }
    });

    const firstGroupBefore = expectGroup("turn-turn-1");
    const secondGroupBefore = expectGroup("turn-turn-2");

    rerender(
      <Harness
        sessionId="session-1"
        turnsStamp={turnsStamp}
        assistantStreamingStamp="2:1"
        messagesStamp={messagesStamp}
        eventsStamp="1:2"
        verbosity="default"
        turns={multiTurns}
        assistantStreamingByTurnId={{
          "turn-1": { content: "Hello world", providerMessageId: "provider-message-1", orderSeq: 2 },
        }}
        messages={multiMessages}
        events={[streamingEvent]}
        toolsByTurnId={emptyToolsByTurnId}
        toolSummariesReady
        askUserQuestionAnswers={askUserQuestionAnswers}
        enableDebugEvents={false}
      />,
    );

    await waitFor(() => {
      const pendingAssistant = expectListItem("assistant-turn-1-pending");
      expect(pendingAssistant.kind).toBe("assistant");
      if (pendingAssistant.kind === "assistant") {
        expect(pendingAssistant.content).toBe("Hello world");
      }
    });

    expect(expectGroup("turn-turn-1")).not.toBe(firstGroupBefore);
    expect(expectGroup("turn-turn-2")).toBe(secondGroupBefore);
    expect(latestResult?.lastOp.kind).toBe("append_stream");
  });

  it("patches assistant stream order-seq backfills without structural transcript stamps", async () => {
    const askUserQuestionAnswers = new Map<string, AskUserQuestionAnswerState>();
    const runningTurns = [
      {
        turn_id: "turn-1",
        session_id: "session-1",
        run_id: null,
        user_message_id: "message-1",
        status: "running",
        start_seq: 1,
        end_seq: null,
        started_at: "2025-12-15T00:00:00.000Z",
        updated_at: "2025-12-15T00:00:01.000Z",
        assistant_partial: "",
        thought_partial: "",
        metrics_json: null,
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
    ] as SessionTurn[];
    const runningMessages = [
      {
        id: "message-1",
        session_id: "session-1",
        task_id: "task-1",
        turn_id: "turn-1",
        turn_sequence: 1,
        role: "user",
        content: "First turn",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:00.000Z",
        order_seq: 1,
      },
    ] as unknown as Message[];
    const turnsStamp = buildTurnsStamp(runningTurns);
    const messagesStamp = buildMessagesStamp(runningMessages);
    const { rerender } = renderController({
      turns: runningTurns,
      messages: runningMessages,
      events: [],
      eventsStamp: "0:0",
      turnsStamp,
      assistantStreamingStamp: "1:no-order",
      assistantStreamingByTurnId: {
        "turn-1": { content: "Hello", providerMessageId: "provider-message-1", orderSeq: null },
      },
      messagesStamp,
      toolsByTurnId: {},
      askUserQuestionAnswers,
    });

    await waitFor(() => {
      expect(latestResult?.listItems.some((item) => item.id === "assistant-turn-1-pending")).toBe(false);
    });

    const firstGroupBefore = expectGroup("turn-turn-1");

    rerender(
      <Harness
        sessionId="session-1"
        turnsStamp={turnsStamp}
        assistantStreamingStamp="2:with-order"
        messagesStamp={messagesStamp}
        eventsStamp="0:0"
        verbosity="default"
        turns={runningTurns}
        assistantStreamingByTurnId={{
          "turn-1": { content: "Hello", providerMessageId: "provider-message-1", orderSeq: 2 },
        }}
        messages={runningMessages}
        events={[]}
        toolsByTurnId={{}}
        toolSummariesReady
        askUserQuestionAnswers={askUserQuestionAnswers}
        enableDebugEvents={false}
      />,
    );

    await waitFor(() => {
      const pendingAssistant = expectListItem("assistant-turn-1-pending");
      expect(pendingAssistant.kind).toBe("assistant");
      if (pendingAssistant.kind === "assistant") {
        expect(pendingAssistant.content).toBe("Hello");
      }
    });

    expect(expectGroup("turn-turn-1")).not.toBe(firstGroupBefore);
    expect(latestResult?.lastOp.kind).toBe("append_stream");
  });

  it("updates only the dirty turn group on tool-summary changes when transcript stamps stay stable", async () => {
    const askUserQuestionAnswers = new Map<string, AskUserQuestionAnswerState>();
    const multiTurns = [
      {
        turn_id: "turn-1",
        session_id: "session-1",
        run_id: null,
        user_message_id: "message-1",
        status: "completed",
        start_seq: 1,
        end_seq: 2,
        started_at: "2025-12-15T00:00:00.000Z",
        updated_at: "2025-12-15T00:00:01.000Z",
        assistant_partial: "",
        thought_partial: "",
        metrics_json: null,
        tool_total: 2,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 2,
        tool_failed: 0,
      },
      {
        turn_id: "turn-2",
        session_id: "session-1",
        run_id: null,
        user_message_id: "message-2",
        status: "completed",
        start_seq: 3,
        end_seq: 4,
        started_at: "2025-12-15T00:00:02.000Z",
        updated_at: "2025-12-15T00:00:03.000Z",
        assistant_partial: "",
        thought_partial: "",
        metrics_json: null,
        tool_total: 1,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 1,
        tool_failed: 0,
      },
    ] as SessionTurn[];
    const multiMessages = [
      {
        id: "message-1",
        session_id: "session-1",
        task_id: "task-1",
        turn_id: "turn-1",
        turn_sequence: 1,
        role: "user",
        content: "First turn",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:00.000Z",
        order_seq: 1,
      },
      {
        id: "message-2",
        session_id: "session-1",
        task_id: "task-1",
        turn_id: "turn-2",
        turn_sequence: 2,
        role: "user",
        content: "Second turn",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:02.000Z",
        order_seq: 3,
      },
    ] as unknown as Message[];
    const baseToolsByTurnId: Record<string, SessionTurnTool[]> = {
      "turn-1": [
        {
          session_id: "session-1",
          tool_call_id: "tool-1",
          turn_id: "turn-1",
          tool_kind: "shell",
          title: "pwd",
          status: "completed",
          input_json: { command: "pwd" },
          output_text: "/tmp",
          order_seq: 2,
          input_truncated: false,
          input_original_bytes: 3,
          output_truncated: false,
          output_original_bytes: 4,
          created_at: "2025-12-15T00:00:00.500Z",
          updated_at: "2025-12-15T00:00:01.000Z",
        },
      ],
      "turn-2": [
        {
          session_id: "session-1",
          tool_call_id: "tool-2",
          turn_id: "turn-2",
          tool_kind: "shell",
          title: "echo hi",
          status: "completed",
          input_json: { command: "echo hi" },
          output_text: "hi",
          order_seq: 4,
          input_truncated: false,
          input_original_bytes: 7,
          output_truncated: false,
          output_original_bytes: 2,
          created_at: "2025-12-15T00:00:02.500Z",
          updated_at: "2025-12-15T00:00:03.000Z",
        },
      ],
    };
    const turnsStamp = buildTurnsStamp(multiTurns);
    const messagesStamp = buildMessagesStamp(multiMessages);
    const { rerender } = renderController({
      turns: multiTurns,
      messages: multiMessages,
      events: [],
      eventsStamp: "0:0",
      turnsStamp,
      messagesStamp,
      toolsByTurnId: baseToolsByTurnId,
      askUserQuestionAnswers,
    });

    await waitFor(() => {
      expect(latestResult?.listItems.filter(isToolItem).map((item) => item.title)).toEqual(["pwd", "echo hi"]);
    });

    const firstGroupBefore = expectGroup("turn-turn-1");
    const secondGroupBefore = expectGroup("turn-turn-2");

    rerender(
      <Harness
        sessionId="session-1"
        turnsStamp={turnsStamp}
        messagesStamp={messagesStamp}
        eventsStamp="0:0"
        verbosity="default"
        turns={multiTurns}
        messages={multiMessages}
        events={[]}
        toolsByTurnId={{
          "turn-1": [
            ...baseToolsByTurnId["turn-1"]!,
            {
              session_id: "session-1",
              tool_call_id: "tool-3",
              turn_id: "turn-1",
              tool_kind: "shell",
              title: "ls",
              status: "completed",
              input_json: { command: "ls" },
              output_text: "file.txt",
              order_seq: 5,
              input_truncated: false,
              input_original_bytes: 2,
              output_truncated: false,
              output_original_bytes: 8,
              created_at: "2025-12-15T00:00:01.500Z",
              updated_at: "2025-12-15T00:00:01.750Z",
            },
          ],
          "turn-2": baseToolsByTurnId["turn-2"]!,
        }}
        toolSummariesReady
        askUserQuestionAnswers={askUserQuestionAnswers}
        enableDebugEvents={false}
      />,
    );

    await waitFor(() => {
      expect(latestResult?.listItems.filter(isToolItem).map((item) => item.title)).toEqual(["pwd", "ls", "echo hi"]);
    });

    expect(expectGroup("turn-turn-1")).not.toBe(firstGroupBefore);
    expect(expectGroup("turn-turn-2")).toBe(secondGroupBefore);
    expect(latestResult?.lastOp.kind).toBe("hydrate_tools");
  });

  it("seeds per-turn caches on mount so the first append-only update preserves prior turn context", async () => {
    const askUserQuestionAnswers = new Map<string, AskUserQuestionAnswerState>();
    const emptyToolsByTurnId: Record<string, SessionTurnTool[]> = {};
    const singleTurn = [
      {
        turn_id: "turn-1",
        session_id: "session-1",
        run_id: null,
        user_message_id: "message-1",
        status: "running",
        start_seq: 1,
        end_seq: 2,
        started_at: "2025-12-15T00:00:00.000Z",
        updated_at: "2025-12-15T00:00:01.000Z",
        assistant_partial: "",
        thought_partial: "",
        metrics_json: null,
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
    ] as SessionTurn[];
    const singleMessages = [
      {
        id: "message-1",
        session_id: "session-1",
        task_id: "task-1",
        turn_id: "turn-1",
        turn_sequence: 1,
        role: "user",
        content: "First turn",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:00.000Z",
        order_seq: 1,
      },
    ] as unknown as Message[];
    const baseEvents: SessionEvent[] = [
      {
        seq: 1,
        id: "event-tool-1",
        session_id: "session-1",
        run_id: "run-1",
        turn_id: "turn-1",
        event_type: "tool_call",
        payload_json: {
          tool_call_id: "tool-1",
          title: "ls -la",
          order_seq: 2,
        },
        created_at: "2025-12-15T00:00:01.500Z",
      },
    ];
    const turnsStamp = buildTurnsStamp(singleTurn);
    const messagesStamp = buildMessagesStamp(singleMessages);
    const { rerender } = renderController({
      turns: singleTurn,
      messages: singleMessages,
      events: baseEvents,
      eventsStamp: "1:1",
      turnsStamp,
      messagesStamp,
      toolsByTurnId: emptyToolsByTurnId,
      askUserQuestionAnswers,
    });

    await waitFor(() => {
      expect(expectGroup("turn-turn-1").header?.content).toBe("First turn");
      expect(latestResult?.listItems.filter(isToolItem).map((item) => item.title)).toEqual(["ls -la"]);
    });

    const nextEvents: SessionEvent[] = [
      ...baseEvents,
      {
        seq: 2,
        id: "event-tool-2",
        session_id: "session-1",
        run_id: "run-1",
        turn_id: "turn-1",
        event_type: "tool_call",
        payload_json: {
          tool_call_id: "tool-2",
          title: "echo hi",
          order_seq: 3,
        },
        created_at: "2025-12-15T00:00:01.750Z",
      },
    ];

    rerender(
      <Harness
        sessionId="session-1"
        turnsStamp={turnsStamp}
        messagesStamp={messagesStamp}
        eventsStamp="2:2"
        verbosity="default"
        turns={singleTurn}
        messages={singleMessages}
        events={nextEvents}
        toolsByTurnId={emptyToolsByTurnId}
        toolSummariesReady
        askUserQuestionAnswers={askUserQuestionAnswers}
        enableDebugEvents={false}
      />,
    );

    await waitFor(() => {
      expect(expectGroup("turn-turn-1").header?.content).toBe("First turn");
      expect(latestResult?.listItems.filter(isToolItem).map((item) => item.title)).toEqual(["ls -la", "echo hi"]);
    });
  });

  it("persists localized append updates into the warm snapshot cache", async () => {
    const askUserQuestionAnswers = new Map<string, AskUserQuestionAnswerState>();
    const emptyToolsByTurnId: Record<string, SessionTurnTool[]> = {};
    const singleTurn = [
      {
        turn_id: "turn-1",
        session_id: "session-1",
        run_id: null,
        user_message_id: "message-1",
        status: "running",
        start_seq: 1,
        end_seq: 2,
        started_at: "2025-12-15T00:00:00.000Z",
        updated_at: "2025-12-15T00:00:01.000Z",
        assistant_partial: "",
        thought_partial: "",
        metrics_json: null,
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
    ] as SessionTurn[];
    const singleMessages = [
      {
        id: "message-1",
        session_id: "session-1",
        task_id: "task-1",
        turn_id: "turn-1",
        turn_sequence: 1,
        role: "user",
        content: "First turn",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:00.000Z",
        order_seq: 1,
      },
    ] as unknown as Message[];
    const baseEvents: SessionEvent[] = [
      {
        seq: 1,
        id: "event-tool-1",
        session_id: "session-1",
        run_id: "run-1",
        turn_id: "turn-1",
        event_type: "tool_call",
        payload_json: {
          tool_call_id: "tool-1",
          title: "ls -la",
          order_seq: 2,
        },
        created_at: "2025-12-15T00:00:01.500Z",
      },
    ];
    const turnsStamp = buildTurnsStamp(singleTurn);
    const messagesStamp = buildMessagesStamp(singleMessages);
    const { rerender } = renderController({
      turns: singleTurn,
      messages: singleMessages,
      events: baseEvents,
      eventsStamp: "1:1",
      turnsStamp,
      messagesStamp,
      toolsByTurnId: emptyToolsByTurnId,
      askUserQuestionAnswers,
    });

    await waitFor(() => {
      expect(latestResult?.listItems.filter(isToolItem).map((item) => item.title)).toEqual(["ls -la"]);
    });

    const nextEvents: SessionEvent[] = [
      ...baseEvents,
      {
        seq: 2,
        id: "event-tool-2",
        session_id: "session-1",
        run_id: "run-1",
        turn_id: "turn-1",
        event_type: "tool_call",
        payload_json: {
          tool_call_id: "tool-2",
          title: "echo hi",
          order_seq: 3,
        },
        created_at: "2025-12-15T00:00:01.750Z",
      },
    ];

    rerender(
      <Harness
        sessionId="session-1"
        turnsStamp={turnsStamp}
        messagesStamp={messagesStamp}
        eventsStamp="2:2"
        verbosity="default"
        turns={singleTurn}
        messages={singleMessages}
        events={nextEvents}
        toolsByTurnId={emptyToolsByTurnId}
        toolSummariesReady
        askUserQuestionAnswers={askUserQuestionAnswers}
        enableDebugEvents={false}
      />,
    );

    await waitFor(() => {
      expect(latestResult?.listItems.filter(isToolItem).map((item) => item.title)).toEqual(["ls -la", "echo hi"]);
    });

    const warmSnapshot = readWarmWorkbenchThreadViewModel(
      "session-1",
      buildWorkbenchThreadViewModelWarmKey({
        sessionId: "session-1",
        projectionRev: 0,
        turnsStamp,
        assistantStreamingStamp: "0:0",
        messagesStamp,
        eventsStamp: "2:2",
        verbosity: "default",
        turns: singleTurn,
        messages: singleMessages,
        events: nextEvents,
        toolsByTurnId: emptyToolsByTurnId,
        toolSummariesReady: true,
        askUserQuestionAnswers,
        enableDebugEvents: false,
      }),
    );

    expect(warmSnapshot?.listItems.filter(isToolItem).map((item) => item.title)).toEqual(["ls -la", "echo hi"]);
  });

  it("falls back to a full rebuild when an existing event changes during an append tick", async () => {
    const askUserQuestionAnswers = new Map<string, AskUserQuestionAnswerState>();
    const emptyToolsByTurnId: Record<string, SessionTurnTool[]> = {};
    const multiTurns = [
      {
        turn_id: "turn-1",
        session_id: "session-1",
        run_id: null,
        user_message_id: "message-1",
        status: "running",
        start_seq: 1,
        end_seq: 2,
        started_at: "2025-12-15T00:00:00.000Z",
        updated_at: "2025-12-15T00:00:01.000Z",
        assistant_partial: "",
        thought_partial: "",
        metrics_json: null,
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
    ] as SessionTurn[];
    const multiMessages = [
      {
        id: "message-1",
        session_id: "session-1",
        task_id: "task-1",
        turn_id: "turn-1",
        turn_sequence: 1,
        role: "user",
        content: "First turn",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:00.000Z",
        order_seq: 1,
      },
    ] as unknown as Message[];
    const baseEvents: SessionEvent[] = [
      {
        seq: 1,
        id: "event-tool-1",
        session_id: "session-1",
        run_id: "run-1",
        turn_id: "turn-1",
        event_type: "tool_call",
        payload_json: {
          tool_call_id: "tool-1",
          title: "ls -la",
          order_seq: 2,
        },
        created_at: "2025-12-15T00:00:01.500Z",
      },
    ];
    const { rerender } = renderController({
      turns: multiTurns,
      messages: multiMessages,
      events: baseEvents,
      eventsStamp: "1:1",
      turnsStamp: buildTurnsStamp(multiTurns),
      messagesStamp: buildMessagesStamp(multiMessages),
      toolsByTurnId: emptyToolsByTurnId,
      askUserQuestionAnswers,
    });

    await waitFor(() => {
      const tool = latestResult?.listItems.find(isToolItem);
      expect(tool?.title).toBe("ls -la");
    });

    const nextEvents: SessionEvent[] = [
      {
        ...baseEvents[0],
        payload_json: {
          ...baseEvents[0].payload_json,
          title: "pwd",
        },
      },
      {
        seq: 2,
        id: "event-tool-2",
        session_id: "session-1",
        run_id: "run-1",
        turn_id: "turn-1",
        event_type: "tool_call",
        payload_json: {
          tool_call_id: "tool-2",
          title: "echo hi",
          order_seq: 3,
        },
        created_at: "2025-12-15T00:00:01.750Z",
      },
    ];

    rerender(
      <Harness
        sessionId="session-1"
        turnsStamp={buildTurnsStamp(multiTurns)}
        messagesStamp={buildMessagesStamp(multiMessages)}
        eventsStamp="2:2"
        verbosity="default"
        turns={multiTurns}
        messages={multiMessages}
        events={nextEvents}
        toolsByTurnId={emptyToolsByTurnId}
        toolSummariesReady
        askUserQuestionAnswers={askUserQuestionAnswers}
        enableDebugEvents={false}
      />,
    );

    await waitFor(() => {
      const tool = latestResult?.listItems.find(isToolItem);
      expect(tool?.title).toBe("pwd");
    });
  });

  it("falls back to a full rebuild when an appended event targets a turn outside the cached groups", async () => {
    const askUserQuestionAnswers = new Map<string, AskUserQuestionAnswerState>();
    const emptyToolsByTurnId: Record<string, SessionTurnTool[]> = {};
    const singleTurn = [
      {
        turn_id: "turn-1",
        session_id: "session-1",
        run_id: null,
        user_message_id: "message-1",
        status: "running",
        start_seq: 1,
        end_seq: 2,
        started_at: "2025-12-15T00:00:00.000Z",
        updated_at: "2025-12-15T00:00:01.000Z",
        assistant_partial: "",
        thought_partial: "",
        metrics_json: null,
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
    ] as SessionTurn[];
    const singleMessages = [
      {
        id: "message-1",
        session_id: "session-1",
        task_id: "task-1",
        turn_id: "turn-1",
        turn_sequence: 1,
        role: "user",
        content: "Only turn",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:00.000Z",
        order_seq: 1,
      },
    ] as unknown as Message[];
    const turnsStamp = buildTurnsStamp(singleTurn);
    const messagesStamp = buildMessagesStamp(singleMessages);
    const { rerender } = renderController({
      turns: singleTurn,
      messages: singleMessages,
      events: [],
      eventsStamp: "0:0",
      turnsStamp,
      messagesStamp,
      toolsByTurnId: emptyToolsByTurnId,
      askUserQuestionAnswers,
    });

    await waitFor(() => {
      expect(expectGroup("turn-turn-1").header?.content).toBe("Only turn");
    });

    const firstGroupBefore = expectGroup("turn-turn-1");

    rerender(
      <Harness
        sessionId="session-1"
        turnsStamp={turnsStamp}
        messagesStamp={messagesStamp}
        eventsStamp="1:1"
        verbosity="default"
        turns={singleTurn}
        messages={singleMessages}
        events={[
          {
            seq: 1,
            id: "event-foreign-turn",
            session_id: "session-1",
            run_id: "run-1",
            turn_id: "turn-2",
            event_type: "tool_call",
            payload_json: {
              tool_call_id: "tool-2",
              title: "echo hi",
              order_seq: 2,
            },
            created_at: "2025-12-15T00:00:01.500Z",
          },
        ]}
        toolsByTurnId={emptyToolsByTurnId}
        toolSummariesReady
        askUserQuestionAnswers={askUserQuestionAnswers}
        enableDebugEvents={false}
      />,
    );

    await waitFor(() => {
      expect(expectGroup("turn-turn-1")).not.toBe(firstGroupBefore);
    });
  });

  it("fully rebuilds when turnsStamp changes for a same-length in-place turn update", async () => {
    const askUserQuestionAnswers = new Map<string, AskUserQuestionAnswerState>();
    const emptyToolsByTurnId: Record<string, SessionTurnTool[]> = {};
    const multiTurns = [
      {
        turn_id: "turn-1",
        session_id: "session-1",
        run_id: null,
        user_message_id: "message-1",
        status: "running",
        start_seq: 1,
        end_seq: 2,
        started_at: "2025-12-15T00:00:00.000Z",
        updated_at: "2025-12-15T00:00:01.000Z",
        assistant_partial: "",
        thought_partial: "",
        metrics_json: null,
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
      {
        turn_id: "turn-2",
        session_id: "session-1",
        run_id: null,
        user_message_id: "message-2",
        status: "completed",
        start_seq: 3,
        end_seq: 4,
        started_at: "2025-12-15T00:00:02.000Z",
        updated_at: "2025-12-15T00:00:03.000Z",
        assistant_partial: "",
        thought_partial: "",
        metrics_json: null,
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
    ] as SessionTurn[];
    const multiMessages = [
      {
        id: "message-1",
        session_id: "session-1",
        task_id: "task-1",
        turn_id: "turn-1",
        turn_sequence: 1,
        role: "user",
        content: "First turn",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:00.000Z",
        order_seq: 1,
      },
      {
        id: "message-2",
        session_id: "session-1",
        task_id: "task-1",
        turn_id: "turn-2",
        turn_sequence: 2,
        role: "user",
        content: "Second turn",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:02.000Z",
        order_seq: 3,
      },
    ] as unknown as Message[];
    const { rerender } = renderController({
      turns: multiTurns,
      messages: multiMessages,
      events: [],
      eventsStamp: "0:0",
      turnsStamp: buildTurnsStamp(multiTurns),
      messagesStamp: buildMessagesStamp(multiMessages),
      toolsByTurnId: emptyToolsByTurnId,
      askUserQuestionAnswers,
    });

    await waitFor(() => {
      expect(expectGroup("turn-turn-1").items.some(isTurnStatusItem)).toBe(true);
    });

    const secondGroupBefore = expectGroup("turn-turn-2");
    const updatedTurns = [
      multiTurns[0],
      {
        ...multiTurns[1],
        status: "running",
      },
    ] as SessionTurn[];

    rerender(
      <Harness
        sessionId="session-1"
        turnsStamp={buildTurnsStamp(updatedTurns, 1)}
        messagesStamp={buildMessagesStamp(multiMessages)}
        eventsStamp="0:0"
        verbosity="default"
        turns={updatedTurns}
        messages={multiMessages}
        events={[]}
        toolsByTurnId={emptyToolsByTurnId}
        toolSummariesReady
        askUserQuestionAnswers={askUserQuestionAnswers}
        enableDebugEvents={false}
      />,
    );

    await waitFor(() => {
      const secondGroup = expectGroup("turn-turn-2");
      expect(secondGroup.items.some(isTurnStatusItem)).toBe(true);
    });

    expect(expectGroup("turn-turn-2")).not.toBe(secondGroupBefore);
  });

  it("prepends older history without rebuilding retained tail groups", async () => {
    const askUserQuestionAnswers = new Map<string, AskUserQuestionAnswerState>();
    const emptyToolsByTurnId: Record<string, SessionTurnTool[]> = {};
    const loadedTurns = [
      {
        turn_id: "turn-1",
        session_id: "session-1",
        run_id: null,
        user_message_id: "message-1",
        status: "completed",
        start_seq: 3,
        end_seq: 4,
        started_at: "2025-12-15T00:00:02.000Z",
        updated_at: "2025-12-15T00:00:03.000Z",
        assistant_partial: "",
        thought_partial: "",
        metrics_json: null,
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
      {
        turn_id: "turn-2",
        session_id: "session-1",
        run_id: null,
        user_message_id: "message-2",
        status: "completed",
        start_seq: 5,
        end_seq: 6,
        started_at: "2025-12-15T00:00:04.000Z",
        updated_at: "2025-12-15T00:00:05.000Z",
        assistant_partial: "",
        thought_partial: "",
        metrics_json: null,
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
    ] as SessionTurn[];
    const loadedMessages = [
      {
        id: "message-1",
        session_id: "session-1",
        task_id: "task-1",
        turn_id: "turn-1",
        turn_sequence: 1,
        role: "user",
        content: "First loaded turn",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:02.000Z",
        order_seq: 3,
      },
      {
        id: "message-2",
        session_id: "session-1",
        task_id: "task-1",
        turn_id: "turn-2",
        turn_sequence: 2,
        role: "user",
        content: "Second loaded turn",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:04.000Z",
        order_seq: 5,
      },
    ] as unknown as Message[];
    const { rerender } = renderController({
      turns: loadedTurns,
      messages: loadedMessages,
      events: [],
      eventsStamp: "0:0",
      turnsStamp: buildTurnsStamp(loadedTurns),
      messagesStamp: buildMessagesStamp(loadedMessages),
      toolsByTurnId: emptyToolsByTurnId,
      askUserQuestionAnswers,
    });

    await waitFor(() => {
      expect(latestResult?.view.groups.map((group) => group.key)).toEqual(["turn-turn-1", "turn-turn-2"]);
    });

    const firstRetainedGroup = expectGroup("turn-turn-1");
    const secondRetainedGroup = expectGroup("turn-turn-2");
    const firstRetainedItem = latestResult?.listItems[0];

    const prependedTurns = [
      {
        turn_id: "turn-0",
        session_id: "session-1",
        run_id: null,
        user_message_id: "message-0",
        status: "completed",
        start_seq: 1,
        end_seq: 2,
        started_at: "2025-12-15T00:00:00.000Z",
        updated_at: "2025-12-15T00:00:01.000Z",
        assistant_partial: "",
        thought_partial: "",
        metrics_json: null,
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
      ...loadedTurns,
    ] as SessionTurn[];
    const prependedMessages = [
      {
        id: "message-0",
        session_id: "session-1",
        task_id: "task-1",
        turn_id: "turn-0",
        turn_sequence: 0,
        role: "user",
        content: "Older history turn",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:00.000Z",
        order_seq: 1,
      },
      ...loadedMessages,
    ] as unknown as Message[];

    rerender(
      <Harness
        sessionId="session-1"
        turnsStamp={buildTurnsStamp(prependedTurns, 1)}
        messagesStamp={buildMessagesStamp(prependedMessages, 1)}
        eventsStamp="0:0"
        verbosity="default"
        turns={prependedTurns}
        messages={prependedMessages}
        events={[]}
        toolsByTurnId={emptyToolsByTurnId}
        toolSummariesReady
        askUserQuestionAnswers={askUserQuestionAnswers}
        enableDebugEvents={false}
      />,
    );

    await waitFor(() => {
      expect(latestResult?.view.groups.map((group) => group.key)).toEqual([
        "turn-turn-0",
        "turn-turn-1",
        "turn-turn-2",
      ]);
    });

    expect(latestResult?.lastOp.kind).toBe("prepend_history");
    expect(expectGroup("turn-turn-1")).toBe(firstRetainedGroup);
    expect(expectGroup("turn-turn-2")).toBe(secondRetainedGroup);
    const prependedRange = latestResult?.groupRanges.get("turn-turn-0");
    expect(prependedRange).toBeTruthy();
    expect(latestResult?.listItems[prependedRange!.end]).toBe(firstRetainedItem);
  });

  it("falls back to reconcile when prepend history also mutates the retained tail", async () => {
    const askUserQuestionAnswers = new Map<string, AskUserQuestionAnswerState>();
    const emptyToolsByTurnId: Record<string, SessionTurnTool[]> = {};
    const loadedTurns = [
      {
        turn_id: "turn-1",
        session_id: "session-1",
        run_id: null,
        user_message_id: "message-1",
        status: "completed",
        start_seq: 3,
        end_seq: 4,
        started_at: "2025-12-15T00:00:02.000Z",
        updated_at: "2025-12-15T00:00:03.000Z",
        assistant_partial: "",
        thought_partial: "",
        metrics_json: null,
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
    ] as SessionTurn[];
    const loadedMessages = [
      {
        id: "message-1",
        session_id: "session-1",
        task_id: "task-1",
        turn_id: "turn-1",
        turn_sequence: 1,
        role: "user",
        content: "Loaded turn",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:02.000Z",
        order_seq: 3,
      },
    ] as unknown as Message[];
    const { rerender } = renderController({
      turns: loadedTurns,
      messages: loadedMessages,
      events: [],
      eventsStamp: "0:0",
      turnsStamp: buildTurnsStamp(loadedTurns),
      messagesStamp: buildMessagesStamp(loadedMessages),
      toolsByTurnId: emptyToolsByTurnId,
      askUserQuestionAnswers,
    });

    await waitFor(() => {
      expect(latestResult?.view.groups.map((group) => group.key)).toEqual(["turn-turn-1"]);
    });

    const retainedGroupBefore = expectGroup("turn-turn-1");

    const prependedTurns = [
      {
        turn_id: "turn-0",
        session_id: "session-1",
        run_id: null,
        user_message_id: "message-0",
        status: "completed",
        start_seq: 1,
        end_seq: 2,
        started_at: "2025-12-15T00:00:00.000Z",
        updated_at: "2025-12-15T00:00:01.000Z",
        assistant_partial: "",
        thought_partial: "",
        metrics_json: null,
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
      },
      ...loadedTurns.map((turn) => ({ ...turn })),
    ] as SessionTurn[];
    const mutatedTailMessages = [
      {
        id: "message-0",
        session_id: "session-1",
        task_id: "task-1",
        turn_id: "turn-0",
        turn_sequence: 0,
        role: "user",
        content: "Older history turn",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:00.000Z",
        order_seq: 1,
      },
      {
        ...loadedMessages[0],
        content: "Loaded turn changed",
      },
    ] as unknown as Message[];

    rerender(
      <Harness
        sessionId="session-1"
        turnsStamp={buildTurnsStamp(prependedTurns, 1)}
        messagesStamp={buildMessagesStamp(mutatedTailMessages, 1)}
        eventsStamp="0:0"
        verbosity="default"
        turns={prependedTurns}
        messages={mutatedTailMessages}
        events={[]}
        toolsByTurnId={emptyToolsByTurnId}
        toolSummariesReady
        askUserQuestionAnswers={askUserQuestionAnswers}
        enableDebugEvents={false}
      />,
    );

    await waitFor(() => {
      expect(latestResult?.view.groups.map((group) => group.key)).toEqual([
        "turn-turn-0",
        "turn-turn-1",
      ]);
    });

    expect(latestResult?.lastOp.kind).toBe("reconcile");
    expect(expectGroup("turn-turn-1")).not.toBe(retainedGroupBefore);
  });
});
