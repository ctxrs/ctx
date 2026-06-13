export type ExecutionEnvironment = "host" | "sandbox";

export type Session = {
  id: string;
  task_id: string;
  workspace_id: string;
  worktree_id: string;
  parent_session_id?: string | null;
  relationship?: string | null;
  provider_id: string;
  model_id: string;
  reasoning_effort?: string | null;
  title: string;
  agent_role: string;
  status: string;
  provider_session_ref?: string | null;
  execution_environment?: ExecutionEnvironment | null;
  created_at?: string;
  updated_at?: string;
};

export type SessionMetadata = Session;

export type TerminalStatus = "running" | "exited";

export type TerminalSession = {
  id: string;
  workspace_id: string;
  task_id?: string | null;
  session_id?: string | null;
  worktree_id?: string | null;
  cwd: string;
  shell: string;
  title: string;
  status: TerminalStatus;
  exit_code?: number | null;
  stream_path: string;
  created_at: string;
  updated_at: string;
};

export type SessionSummary = {
  id: string;
  task_id: string;
  workspace_id: string;
  parent_session_id?: string | null;
  relationship?: string | null;
  provider_id: string;
  model_id: string;
  reasoning_effort?: string | null;
  title: string;
  status: string;
  execution_environment?: ExecutionEnvironment | null;
  created_at: string;
  updated_at: string;
};

export type SubagentInvocationChild = {
  invocation_id: string;
  child_session_id: string;
  run_id?: string | null;
  position: number;
  status: string;
  label?: string | null;
  harness?: string | null;
  model?: string | null;
  reasoning_effort?: string | null;
  prompt_length: number;
  created_at: string;
  updated_at: string;
};

export type SubagentInvocation = {
  id: string;
  tool_call_id: string;
  parent_session_id: string;
  parent_turn_id?: string | null;
  requested_count: number;
  request_json?: Record<string, unknown> | null;
  status: string;
  created_at: string;
  updated_at: string;
  children: SubagentInvocationChild[];
};
