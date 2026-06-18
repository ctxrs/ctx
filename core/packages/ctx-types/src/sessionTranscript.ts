export type Message = {
  id: string;
  session_id: string;
  task_id: string;
  run_id?: string | null;
  turn_id?: string | null;
  turn_sequence?: number | null;
  order_seq?: number | null;
  role: "user" | "assistant" | "system";
  content: string;
  attachments?: MessageAttachment[];
  delivery: "immediate" | "queued";
  delivered_at?: string | null;
  created_at: string;
};

export type Artifact = {
  id: string;
  session_id: string;
  task_id: string;
  workspace_id: string;
  worktree_id: string;
  name?: string | null;
  absolute_path?: string | null;
  relative_path?: string | null;
  mime_type: string;
  bytes: number;
  created_at: string;
  missing?: boolean | null;
};

export type MessageAttachment =
  | {
      kind: "image";
      mime_type: string;
      data_base64: string;
      name?: string | null;
    }
  | {
      kind: "image_ref";
      blob_id: string;
      mime_type: string;
      name?: string | null;
    };

export type SessionEventType =
  | "init"
  | "user_message"
  | "input_queued"
  | "turn_queued"
  | "turn_started"
  | "context_window_update"
  | "turn_finished"
  | "message_queue_added"
  | "message_queue_updated"
  | "message_queue_removed"
  | "message_queue_promoted"
  | "auth_required"
  | "notice"
  | "assistant_chunk"
  | "thought_chunk"
  | "assistant_complete"
  | "assistant_message_inserted"
  | "tool_call"
  | "tool_call_update"
  | "tool_result"
  | "plan"
  | "artifacts_set"
  | "done"
  | "interrupt_requested"
  | "turn_interrupted"
  | "error";

export type TurnLifecycleEventPayload = {
  message_id?: string;
  queue_position?: number | null;
  status?: SessionTurnStatus;
  reason?: string;
  provider_cancelled?: boolean;
};

export type MessageQueueEventPayload = {
  message_id: string;
  queue_position?: number | null;
  previous_position?: number | null;
  reason?: string;
};

export type SessionEvent = {
  seq: number | null;
  id: string;
  session_id: string;
  run_id?: string | null;
  turn_id?: string | null;
  event_type: SessionEventType;
  payload_json: Record<string, unknown> | null;
  transient?: boolean;
  created_at: string;
};

export type SessionTurnStatus =
  | "queued"
  | "starting"
  | "running"
  | "completed"
  | "interrupted"
  | "failed";

export type SessionActivityState = {
  is_working: boolean;
  last_turn_status?: SessionTurnStatus | null;
};

export type SessionTurnFailure = {
  message?: string | null;
  details?: unknown | null;
  kind?: string | null;
  reason?: string | null;
  provider?: string | null;
  provider_id?: string | null;
};

export type SessionTurn = {
  turn_id: string;
  session_id: string;
  run_id?: string | null;
  user_message_id?: string | null;
  status: SessionTurnStatus;
  start_seq?: number | null;
  end_seq?: number | null;
  started_at: string;
  updated_at: string;
  assistant_partial?: string | null;
  thought_partial?: string | null;
  metrics_json?: Record<string, unknown> | null;
  failure?: SessionTurnFailure | null;
  tool_total: number;
  tool_pending: number;
  tool_running: number;
  tool_completed: number;
  tool_failed: number;
};

export type SessionTurnTool = {
  session_id: string;
  tool_call_id: string;
  turn_id: string;
  tool_kind?: string | null;
  provider_tool_name?: string | null;
  title?: string | null;
  subtitle?: string | null;
  status?: string | null;
  input_json?: Record<string, unknown> | null;
  output_text?: string | null;
  order_seq: number;
  input_truncated?: boolean | null;
  input_original_bytes?: number | null;
  output_truncated?: boolean | null;
  output_original_bytes?: number | null;
  first_event_seq?: number | null;
  created_at: string;
  updated_at: string;
};

export type SessionTurnToolSummary = {
  session_id: string;
  tool_call_id: string;
  turn_id: string;
  tool_kind?: string | null;
  provider_tool_name?: string | null;
  title?: string | null;
  subtitle?: string | null;
  status?: string | null;
  input_preview?: Record<string, unknown> | null;
  output_preview?: string | null;
  order_seq: number;
  input_truncated?: boolean | null;
  input_original_bytes?: number | null;
  output_truncated?: boolean | null;
  output_original_bytes?: number | null;
  first_event_seq?: number | null;
  created_at: string;
  updated_at: string;
};

export type EventEnvelope = SessionEvent;

export type ToolCallRecord = SessionTurnTool;

export type TranscriptRecord =
  | {
      record_type: "message";
      message: Message;
    }
  | {
      record_type: "event";
      event: EventEnvelope;
    }
  | {
      record_type: "tool_call";
      tool_call: ToolCallRecord;
    }
  | {
      record_type: "artifact";
      artifact: Artifact;
    };
