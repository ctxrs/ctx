import type {
  Message,
  Session,
  SessionEvent,
  SessionHeadSnapshot,
  SessionSnapshotSummary,
  SessionTurn,
  SessionTurnTool,
  SessionTurnToolSummary,
  Task,
  WorkspaceActiveSnapshot,
  WorkspaceActiveTaskSummary,
} from "@ctx/types";

export const FIXTURE_TIMES = {
  created: "2026-03-09T00:00:00.000Z",
  tool: "2026-03-09T00:00:01.000Z",
  assistant: "2026-03-09T00:00:02.000Z",
  updated: "2026-03-09T00:00:03.000Z",
} as const;

export type ProjectionEntityIds = {
  workspaceId: string;
  taskId: string;
  sessionId: string;
  turnId: string;
  userMessageId: string;
  assistantMessageId: string;
  userEventId: string;
  toolCallEventId: string;
  toolResultEventId: string;
  assistantEventId: string;
  partialEventId: string;
};

export type ProjectionToolSpec = {
  toolCallId: string;
  toolTitle: string;
  toolKind: string;
  toolInput: Record<string, unknown>;
  toolOutput: string;
  orderSeqs: {
    user: number;
    tool: number;
    assistant: number;
  };
};

export type ProjectionMessageSpec = ProjectionToolSpec & {
  userContent: string;
  assistantContent: string;
};

export const buildTask = (taskId: string, workspaceId: string): Task => ({
  id: taskId,
  workspace_id: workspaceId,
  title: "Projection fixture task",
  status: "running",
  created_at: FIXTURE_TIMES.created,
  updated_at: FIXTURE_TIMES.updated,
});

export const buildSession = (sessionId: string, taskId: string, workspaceId: string): Session => ({
  id: sessionId,
  task_id: taskId,
  workspace_id: workspaceId,
  worktree_id: "wt-projection",
  provider_id: "fake",
  model_id: "fake-model",
  title: "Projection fixture session",
  agent_role: "assistant",
  status: "active",
  created_at: FIXTURE_TIMES.created,
  updated_at: FIXTURE_TIMES.updated,
});

export const buildTurn = (
  sessionId: string,
  turnId: string,
  userMessageId: string,
  lastEventSeq: number,
  toolTotal: number,
): SessionTurn => ({
  turn_id: turnId,
  session_id: sessionId,
  run_id: null,
  user_message_id: userMessageId,
  status: "completed",
  start_seq: 1,
  end_seq: lastEventSeq,
  started_at: FIXTURE_TIMES.created,
  updated_at: FIXTURE_TIMES.updated,
  assistant_partial: null,
  thought_partial: null,
  metrics_json: null,
  tool_total: toolTotal,
  tool_pending: 0,
  tool_running: 0,
  tool_completed: toolTotal,
  tool_failed: 0,
});

export const buildUserMessage = (
  session: Session,
  turnId: string,
  messageId: string,
  content: string,
  orderSeq: number,
): Message => ({
  id: messageId,
  session_id: session.id,
  task_id: session.task_id,
  turn_id: turnId,
  turn_sequence: orderSeq,
  role: "user",
  content,
  attachments: [],
  delivery: "immediate",
  created_at: FIXTURE_TIMES.created,
});

export const buildAssistantMessage = (
  session: Session,
  turnId: string,
  messageId: string,
  content: string,
  orderSeq: number,
): Message => ({
  id: messageId,
  session_id: session.id,
  task_id: session.task_id,
  turn_id: turnId,
  turn_sequence: orderSeq,
  role: "assistant",
  content,
  attachments: [],
  delivery: "immediate",
  created_at: FIXTURE_TIMES.assistant,
});

export const buildToolSummary = (
  sessionId: string,
  turnId: string,
  spec: ProjectionToolSpec,
): SessionTurnToolSummary => ({
  session_id: sessionId,
  tool_call_id: spec.toolCallId,
  turn_id: turnId,
  tool_kind: spec.toolKind,
  title: spec.toolTitle,
  status: "completed",
  input_preview: spec.toolInput,
  output_preview: spec.toolOutput,
  order_seq: spec.orderSeqs.tool,
  input_truncated: null,
  input_original_bytes: null,
  output_truncated: null,
  output_original_bytes: null,
  created_at: FIXTURE_TIMES.tool,
  updated_at: FIXTURE_TIMES.updated,
});

export const buildToolByTurnId = (
  sessionId: string,
  turnId: string,
  spec: ProjectionToolSpec,
): Record<string, SessionTurnTool[]> => ({
  [turnId]: [
    {
      session_id: sessionId,
      tool_call_id: spec.toolCallId,
      turn_id: turnId,
      tool_kind: spec.toolKind,
      title: spec.toolTitle,
      status: "completed",
      input_json: spec.toolInput,
      output_text: spec.toolOutput,
      order_seq: spec.orderSeqs.tool,
      input_truncated: null,
      input_original_bytes: null,
      output_truncated: null,
      output_original_bytes: null,
      created_at: FIXTURE_TIMES.tool,
      updated_at: FIXTURE_TIMES.updated,
    },
  ],
});

export const buildStableEvents = (
  sessionId: string,
  turnId: string,
  ids: ProjectionEntityIds,
  spec: ProjectionMessageSpec,
): SessionEvent[] => [
  {
    seq: 1,
    id: ids.userEventId,
    session_id: sessionId,
    run_id: null,
    turn_id: turnId,
    event_type: "user_message",
    payload_json: {
      message_id: ids.userMessageId,
      content: spec.userContent,
      attachments: [],
      order_seq: spec.orderSeqs.user,
    },
    created_at: FIXTURE_TIMES.created,
  },
  {
    seq: 2,
    id: ids.toolCallEventId,
    session_id: sessionId,
    run_id: null,
    turn_id: turnId,
    event_type: "tool_call",
    payload_json: {
      tool_call_id: spec.toolCallId,
      title: spec.toolTitle,
      kind: spec.toolKind,
      input: spec.toolInput,
      order_seq: spec.orderSeqs.tool,
    },
    created_at: FIXTURE_TIMES.tool,
  },
  {
    seq: 3,
    id: ids.toolResultEventId,
    session_id: sessionId,
    run_id: null,
    turn_id: turnId,
    event_type: "tool_result",
    payload_json: {
      tool_call_id: spec.toolCallId,
      title: spec.toolTitle,
      kind: spec.toolKind,
      outputText: spec.toolOutput,
      order_seq: spec.orderSeqs.tool,
    },
    created_at: FIXTURE_TIMES.assistant,
  },
  {
    seq: 4,
    id: ids.assistantEventId,
    session_id: sessionId,
    run_id: null,
    turn_id: turnId,
    event_type: "assistant_complete",
    payload_json: {
      message_id: ids.assistantMessageId,
      content: spec.assistantContent,
      full_content: spec.assistantContent,
      order_seq: spec.orderSeqs.assistant,
    },
    created_at: FIXTURE_TIMES.updated,
  },
];

export const buildSummary = (
  session: Session,
  assistantContent: string,
  lastEventSeq: number,
): SessionSnapshotSummary => ({
  session,
  last_message_at: FIXTURE_TIMES.assistant,
  last_message_preview: assistantContent,
  last_event_seq: lastEventSeq,
  state_rev: lastEventSeq,
  activity: { is_working: false, last_turn_status: "completed" },
  unread: false,
});

export const buildActiveSnapshot = (
  workspaceId: string,
  activeTask: WorkspaceActiveTaskSummary,
): WorkspaceActiveSnapshot => ({
  workspace_id: workspaceId,
  snapshot_rev: 4,
  archived_rev: 0,
  active: {
    total_count: 1,
    tasks: [activeTask],
  },
});

export const buildHead = (
  session: Session,
  turn: SessionTurn,
  messages: Message[],
  events: SessionEvent[],
  toolSummaries: SessionTurnToolSummary[],
  lastEventSeq: number,
): SessionHeadSnapshot => ({
  session,
  turns: [turn],
  tool_summaries: toolSummaries,
  events,
  messages,
  last_event_seq: lastEventSeq,
  state_rev: lastEventSeq,
  activity: { is_working: false, last_turn_status: "completed" },
  has_more_turns: false,
  history_cursor: null,
  has_more_history: false,
  summary_checkpoint: null,
  head_window: {
    turn_limit: 5,
    message_limit: 200,
    event_limit: 200,
    byte_limit: 1500000,
    turn_count: 1,
    message_count: messages.length,
    event_count: events.length,
    bytes: 1024,
    truncated: false,
  },
});

export const idsFor = (prefix: string): ProjectionEntityIds => ({
  workspaceId: `ws-${prefix}`,
  taskId: `task-${prefix}`,
  sessionId: `session-${prefix}`,
  turnId: `turn-${prefix}`,
  userMessageId: `msg-${prefix}-user`,
  assistantMessageId: `msg-${prefix}-assistant`,
  userEventId: `event-${prefix}-user`,
  toolCallEventId: `event-${prefix}-tool-call`,
  toolResultEventId: `event-${prefix}-tool-result`,
  assistantEventId: `event-${prefix}-assistant`,
  partialEventId: `event-${prefix}-partial`,
});
