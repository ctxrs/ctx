import type { MessageAttachment, SessionEvent, SessionTurnStatus } from "@ctx/types";

export type ThreadMessageItem = {
  kind: "message";
  id: string;
  role: "user" | "assistant" | "system";
  content: string;
  attachments: MessageAttachment[];
  created_at: string;
};

export type ThreadSpacerItem = {
  kind: "spacer";
  id: string;
  created_at: string;
};

export type ThreadAssistantItem = {
  kind: "assistant";
  id: string;
  turn_id: string;
  created_at: string;
  content: string;
  thought: string;
  is_complete: boolean;
  thought_seconds?: number;
};

export type ThreadThoughtItem = {
  kind: "thought";
  id: string;
  turn_id: string;
  created_at: string;
  content: string;
};

export type ThreadTurnStatusItem = {
  kind: "turn_status";
  id: string;
  turn_id: string;
  created_at: string;
  status: SessionTurnStatus;
  started_at: string;
  updated_at: string;
  custom_status?: string | null;
  assistant_messages_content?: string;
};

export type ThreadToolLocation = {
  path?: string;
  range?: unknown;
};

export type ThreadToolItem = {
  kind: "tool";
  id: string;
  created_at: string;
  updated_at: string;
  tool_call_id: string;
  tool_kind: string;
  provider_tool_name?: string;
  title: string;
  subtitle?: string;
  status: string;
  locations: ThreadToolLocation[];
  input: unknown;
  output_text: string;
  raw: unknown;
  updates_seen: number;
  has_details?: boolean;
};

export type ThreadToolGroupItem = {
  kind: "tool_group";
  id: string;
  turn_id: string;
  created_at: string;
  updated_at: string;
  tool_total: number;
  tool_pending: number;
  tool_running: number;
  tool_completed: number;
  tool_failed: number;
  tools: ThreadToolItem[];
  thought: string;
};

export type AskUserQuestionThreadItem = {
  kind: "ask_user_question";
  id: string;
  turn_id: string;
  created_at: string;
  tool_call_id: string;
  input: unknown;
  answers?: Record<string, string>;
  outcome?: "submitted" | "cancelled";
  answered: boolean;
};

export type ThreadItem =
  | ThreadMessageItem
  | ThreadSpacerItem
  | ThreadAssistantItem
  | ThreadThoughtItem
  | ThreadTurnStatusItem
  | ThreadToolGroupItem
  | ThreadToolItem
  | AskUserQuestionThreadItem;

export type AskUserQuestionAnswerState = {
  outcome: "submitted" | "cancelled";
  answers: Record<string, string>;
};

export type WorkbenchTurnHeader = {
  id: string;
  content: string;
  plain_text?: string;
  content_revision?: string;
  attachments: MessageAttachment[];
  created_at: string;
};

export type WorkbenchListItem =
  | ThreadItem
  | {
      kind: "turn_header";
      id: string;
      header: WorkbenchTurnHeader;
    };

export type ScrollbarDragState = {
  pointerId: number;
  startY: number;
  startScrollTop: number;
  trackHeight: number;
  thumbHeight: number;
  scrollHeight: number;
  clientHeight: number;
};

export type WorkbenchThreadView = {
  groups: Array<{
    key: string;
    header: WorkbenchTurnHeader | null;
    items: ThreadItem[];
  }>;
  debugEvents: SessionEvent[];
};
