import type {
  AttachmentMode,
  AttachmentUpdatePolicy,
  ExecutionEnvironment,
  MergeQueueEntry,
  Session,
  Task,
  WorkspaceActiveHeadBatch,
  TerminalSession,
  Workspace,
  WorkspaceActiveSnapshot,
  WorkspaceArchivedPage,
  WorkspaceIndexCursor,
  Worktree,
  WorkspaceAttachment,
  WorkspaceAttachmentKind,
} from "@ctx/types";
import { apiAny, daemonFetchRaw, idToString } from "./clientBase";
import { setBrowserStreamQueryToken } from "./browserStreamAuth";
import { getDaemonConnection, getDaemonWsUrl } from "./daemonConnection";
import {
  trackWorkspaceCreated,
  trackWorkspaceCreateFailed,
  trackWorkspaceCreateSubmitted,
  trackWorkspaceCreateSucceeded,
} from "../utils/analytics";

export const listWorkspaces = () =>
  apiAny<Workspace[]>("/api/workspaces");

export type AgentSystemPromptConfig = {
  default_append: string;
  configured_append?: string | null;
  effective_append?: string | null;
  source: "default" | "config" | "disabled";
};

export type SubagentSystemPromptConfig = {
  default_append: string;
  configured_append?: string | null;
  effective_append?: string | null;
  source: "default" | "config" | "disabled";
};

export const createWorkspace = async (
  root_path: string,
  name?: string,
  workspaceKind: "local" | "remote" = "local",
  source: "wizard" | "launcher" | "api" | "unknown" = "unknown",
  executionMode?: "host" | "sandbox",
) => {
  const analyticsProps = { workspaceKind, source, executionMode };
  trackWorkspaceCreateSubmitted(analyticsProps);
  try {
    const workspace = await apiAny<Workspace>("/api/workspaces", {
      method: "POST",
      body: JSON.stringify({ root_path, name }),
    });
    trackWorkspaceCreated({ workspaceKind, executionMode });
    trackWorkspaceCreateSucceeded(analyticsProps);
    return workspace;
  } catch (error) {
    trackWorkspaceCreateFailed({
      workspaceKind,
      source,
      executionMode,
      failureKind: classifyWorkspaceCreateFailure(error),
    });
    throw error;
  }
};

const classifyWorkspaceCreateFailure = (
  error: unknown,
): "network_error" | "request_error" | "unknown" => {
  if (error instanceof TypeError) return "network_error";
  if (error instanceof Error) return "request_error";
  return "unknown";
};

export const getWorkspace = (id: string) =>
  apiAny<Workspace>(`/api/workspaces/${id}`);

export const deleteWorkspace = (workspaceId: string) =>
  apiAny<void>(`/api/workspaces/${workspaceId}`, {
    method: "DELETE",
  });

export type UpdateMergeQueueConfigRequest = {
  enabled: boolean;
  target_branch?: string | null;
  verify_command?: string | null;
  push_on_success?: boolean | null;
  push_remote?: string | null;
  push_branch?: string | null;
};

export type WorkspaceMergeQueueConfig = {
  enabled: boolean;
  target_branch: string;
  verify_command?: string | null;
  push_on_success: boolean;
  push_remote: string;
  push_branch: string;
};

export type UpdateWorkspaceConfigResponse = {
  ok: boolean;
};

export type WorkspacePrimaryBranch = {
  primary_branch: string;
};

export type WorkspaceProviderModelPreference = {
  provider_id: string;
  preferred_model_id?: string | null;
};

export const getWorkspacePrimaryBranch = (workspaceId: string) =>
  apiAny<WorkspacePrimaryBranch>(`/api/workspaces/${workspaceId}/primary_branch`);

export const updateWorkspacePrimaryBranch = (workspaceId: string, primary_branch: string) =>
  apiAny<WorkspacePrimaryBranch>(`/api/workspaces/${workspaceId}/primary_branch`, {
    method: "POST",
    body: JSON.stringify({ primary_branch }),
  });

export const getWorkspaceProviderModelPreference = (
  workspaceId: string,
  providerId: string,
) =>
  apiAny<WorkspaceProviderModelPreference>(
    `/api/workspaces/${workspaceId}/provider_model_preferences/${providerId}`,
  );

export const updateWorkspaceProviderModelPreference = (
  workspaceId: string,
  providerId: string,
  preferred_model_id?: string | null,
) =>
  apiAny<WorkspaceProviderModelPreference>(
    `/api/workspaces/${workspaceId}/provider_model_preferences/${providerId}`,
    {
      method: "POST",
      body: JSON.stringify({ preferred_model_id: preferred_model_id ?? null }),
    },
  );

export const updateWorkspaceMergeQueueConfig = (workspaceId: string, req: UpdateMergeQueueConfigRequest) =>
  apiAny<UpdateWorkspaceConfigResponse>(`/api/workspaces/${workspaceId}/merge_queue_config`, {
    method: "POST",
    body: JSON.stringify(req),
  });

export const getWorkspaceMergeQueueConfig = (workspaceId: string) =>
  apiAny<WorkspaceMergeQueueConfig>(`/api/workspaces/${workspaceId}/merge_queue_config`);

export type UpdateExecutionConfigRequest = {
  environment: "host" | "sandbox";
  network_mode?: "llm_only" | "allowlist" | "all" | null;
  allowlist?: string[] | null;
};

export type WorkspaceExecutionConfig = {
  source: "workspace" | "daemon_default";
  environment: "host" | "sandbox";
  network_mode?: "llm_only" | "allowlist" | "all" | null;
  allowlist?: string[] | null;
};

export const getWorkspaceExecutionConfig = (workspaceId: string) =>
  apiAny<WorkspaceExecutionConfig>(`/api/workspaces/${workspaceId}/execution_config`);

export const updateWorkspaceExecutionConfig = (workspaceId: string, req: UpdateExecutionConfigRequest) =>
  apiAny<UpdateWorkspaceConfigResponse>(`/api/workspaces/${workspaceId}/execution_config`, {
    method: "POST",
    body: JSON.stringify(req),
  });

export const ensureWorkspaceHarnessContainer = (workspaceId: string) =>
  apiAny<void>(`/api/workspaces/${workspaceId}/harness_container/ensure`, {
    method: "POST",
  });

export type ExecutionLaunchPhase =
  | "artifact_download"
  | "machine_check"
  | "machine_start_or_init"
  | "image_check"
  | "image_load"
  | "container_check"
  | "container_start_or_create"
  | "runtime_network_setup"
  | "ready";

export type ExecutionLaunchLogLevel = "info" | "warn" | "error";

export type ExecutionLaunchState = "running" | "ready" | "error";

export type ExecutionLaunchLogLine = {
  seq: number;
  ts: string;
  phase: ExecutionLaunchPhase;
  level: ExecutionLaunchLogLevel;
  message: string;
};

export type ExecutionLaunchDownloadStatus = {
  artifact: string;
  downloaded_bytes: number;
  total_bytes?: number | null;
  bytes_per_sec?: number | null;
};

export type ExecutionLaunchPhaseLifecycle =
  | "pending"
  | "running"
  | "completed"
  | "error"
  | (string & {});

export type ExecutionLaunchPhaseStatus = {
  phase: ExecutionLaunchPhase;
  started_at: string;
  // Compatibility: older or mixed-shape snapshots may still use legacy completion fields.
  status?: ExecutionLaunchPhaseLifecycle | null;
  completed_at?: string | null;
  finished_at?: string | null;
  elapsed_ms?: number | null;
};

export type ExecutionLaunchSnapshot = {
  job_id: string;
  workspace_id: string;
  kind: "workspace_launch" | "startup_prewarm";
  state: ExecutionLaunchState;
  created_at: string;
  started_at: string;
  updated_at?: string | null;
  finished_at?: string | null;
  current_phase?: ExecutionLaunchPhase | null;
  current_step_label?: string | null;
  progress_pct?: number | null;
  eta_ms?: number | null;
  active_download?: ExecutionLaunchDownloadStatus | null;
  phases: ExecutionLaunchPhaseStatus[];
  logs: ExecutionLaunchLogLine[];
  error?: string | null;
};

export type ExecutionLaunchStreamEvent =
  | { type: "launch_snapshot"; snapshot: ExecutionLaunchSnapshot }
  | { type: "launch_log"; job_id: string; line: ExecutionLaunchLogLine }
  | { type: "launch_complete"; snapshot: ExecutionLaunchSnapshot }
  | { type: "launch_error"; snapshot: ExecutionLaunchSnapshot };

export type RuntimePrewarmScope = "runtime" | "launch_ready" | "builder" | "all";

export type LinuxSandboxRuntimeState =
  | "ready"
  | "download_pending"
  | "downloaded_not_activated"
  | "activating"
  | "unsupported"
  | "failed";

export type LinuxSandboxRuntimeStatus = {
  state: LinuxSandboxRuntimeState;
  supported: boolean;
  distro?: string | null;
  cache_root: string;
  staged_archive_path?: string | null;
  activation_script_path?: string | null;
  runtime_cli_path?: string | null;
  message: string;
};

export type LinuxSandboxActivationMode = "local" | "remote";

export type LinuxSandboxRuntimePrepareResult = {
  ready: boolean;
  needs_password: boolean;
  status: LinuxSandboxRuntimeStatus;
  message: string;
};

export const startExecutionLaunch = (workspaceId: string) =>
  apiAny<ExecutionLaunchSnapshot>("/api/execution/launch/start", {
    method: "POST",
    body: JSON.stringify({ workspace_id: workspaceId }),
  });

export const startRuntimePrewarm = (prewarmScope: RuntimePrewarmScope) =>
  apiAny<ExecutionLaunchSnapshot>("/api/execution/launch/start", {
    method: "POST",
    body: JSON.stringify({
      kind: "startup_prewarm",
      prewarm_scope: prewarmScope,
    }),
  });

export const startWorkspaceSetupLaunchHandoff = (workspaceId: string) =>
  startExecutionLaunch(workspaceId);

export const getExecutionLaunchStatus = (jobId: string) =>
  apiAny<ExecutionLaunchSnapshot>(
    `/api/execution/launch/status?job_id=${encodeURIComponent(jobId)}`,
  );

export const buildExecutionLaunchWsUrl = async (jobId: string): Promise<string> => {
  const qs = new URLSearchParams();
  qs.set("job_id", jobId);
  await setBrowserStreamQueryToken(qs, getDaemonConnection().authToken, {
    kind: "execution_launch",
    jobId,
  });
  return getDaemonWsUrl("/api/execution/launch/stream", qs);
};

export const getLinuxSandboxRuntimeStatus = () =>
  apiAny<LinuxSandboxRuntimeStatus>("/api/execution/linux_sandbox_runtime/status");

export const stageLinuxSandboxRuntime = () =>
  apiAny<LinuxSandboxRuntimeStatus>("/api/execution/linux_sandbox_runtime/stage", {
    method: "POST",
  });

export const prepareLinuxSandboxRuntime = (
  activationMode: LinuxSandboxActivationMode,
  sudoPassword?: string | null,
) =>
  apiAny<LinuxSandboxRuntimePrepareResult>("/api/execution/linux_sandbox_runtime/prepare", {
    method: "POST",
    body: JSON.stringify({
      activation_mode: activationMode,
      sudo_password: sudoPassword ?? null,
    }),
  });

export type UpdateWorktreeBootstrapConfigRequest = {
  setup_command?: string | null;
  timeout_sec?: number | null;
  wait_for_completion?: boolean | null;
};

export type WorkspaceWorktreeBootstrapConfig = {
  setup_command?: string | null;
  timeout_sec?: number | null;
  wait_for_completion?: boolean | null;
};

export const getWorkspaceWorktreeBootstrapConfig = (workspaceId: string) =>
  apiAny<WorkspaceWorktreeBootstrapConfig>(`/api/workspaces/${workspaceId}/worktree_bootstrap_config`);

export const updateWorkspaceWorktreeBootstrapConfig = (workspaceId: string, req: UpdateWorktreeBootstrapConfigRequest) =>
  apiAny<UpdateWorkspaceConfigResponse>(`/api/workspaces/${workspaceId}/worktree_bootstrap_config`, {
    method: "POST",
    body: JSON.stringify(req),
  });

export type CreateWorkspaceAttachmentRequest = {
  kind: WorkspaceAttachmentKind;
  name: string;
  source: string;
  revision?: string | null;
  subpath?: string | null;
  mount_relpath?: string | null;
  mode?: AttachmentMode | null;
  update_policy?: AttachmentUpdatePolicy | null;
};

export type DeleteWorkspaceAttachmentRequest = {
  kind: WorkspaceAttachmentKind;
  name: string;
};

export const listWorkspaceAttachments = (workspaceId: string) =>
  apiAny<WorkspaceAttachment[]>(`/api/workspaces/${workspaceId}/attachments`);

export const syncWorkspaceAttachments = (workspaceId: string, refresh?: boolean) =>
  apiAny<WorkspaceAttachment[]>(`/api/workspaces/${workspaceId}/attachments/sync`, {
    method: "POST",
    body: JSON.stringify({ refresh: Boolean(refresh) }),
  });

export const createWorkspaceAttachment = (workspaceId: string, req: CreateWorkspaceAttachmentRequest) =>
  apiAny<WorkspaceAttachment[]>(`/api/workspaces/${workspaceId}/attachments`, {
    method: "POST",
    body: JSON.stringify(req),
  });

export const deleteWorkspaceAttachment = (workspaceId: string, req: DeleteWorkspaceAttachmentRequest) =>
  apiAny<WorkspaceAttachment[]>(`/api/workspaces/${workspaceId}/attachments`, {
    method: "DELETE",
    body: JSON.stringify(req),
  });

export const getAgentSystemPrompt = (workspaceId: string) =>
  apiAny<AgentSystemPromptConfig>(`/api/workspaces/${workspaceId}/agent_system_prompt`);

export const updateAgentSystemPrompt = (workspaceId: string, req: { system_prompt_append?: string | null }) =>
  apiAny<AgentSystemPromptConfig>(`/api/workspaces/${workspaceId}/agent_system_prompt`, {
    method: "POST",
    body: JSON.stringify(req),
  });

export const getSubagentSystemPrompt = (workspaceId: string) =>
  apiAny<SubagentSystemPromptConfig>(`/api/workspaces/${workspaceId}/subagent_system_prompt`);

export const updateSubagentSystemPrompt = (workspaceId: string, req: { system_prompt_append?: string | null }) =>
  apiAny<SubagentSystemPromptConfig>(`/api/workspaces/${workspaceId}/subagent_system_prompt`, {
    method: "POST",
    body: JSON.stringify(req),
  });

export type CreateTerminalRequest = {
  task_id?: string | null;
  session_id?: string | null;
  worktree_id?: string | null;
  cwd?: string | null;
  shell?: string | null;
};

export const listWorkspaceTerminals = (workspaceId: string) =>
  apiAny<TerminalSession[]>(`/api/workspaces/${workspaceId}/terminals`);

export const createWorkspaceTerminal = (workspaceId: string, req: CreateTerminalRequest) =>
  apiAny<TerminalSession>(`/api/workspaces/${workspaceId}/terminals`, {
    method: "POST",
    body: JSON.stringify(req),
  });

export const deleteTerminal = (terminalId: string) =>
  apiAny<void>(`/api/terminals/${terminalId}`, { method: "DELETE" });

export type TerminalStreamConnectInfo = {
  stream_path: string;
  expires_at: string;
};

export const mintTerminalStreamPath = (terminalId: string) =>
  apiAny<TerminalStreamConnectInfo>(`/api/terminals/${terminalId}/stream_token`, {
    method: "POST",
  });

export type WorkspaceActiveSnapshotParams = {
  limit?: number;
};

export const getWorkspaceActiveSnapshot = (workspaceId: string, params?: WorkspaceActiveSnapshotParams) => {
  const search = new URLSearchParams();
  if (params?.limit) search.set("limit", String(params.limit));
  const qs = search.toString();
  const suffix = qs ? `?${qs}` : "";
  return apiAny<WorkspaceActiveSnapshot>(`/api/workspaces/${workspaceId}/active_snapshot${suffix}`);
};

export const getWorkspaceActiveHeads = (workspaceId: string) => {
  return apiAny<WorkspaceActiveHeadBatch>(`/api/workspaces/${workspaceId}/active_heads`);
};

export const listWorkspaceTasks = (workspaceId: string) =>
  apiAny<Task[]>(`/api/workspaces/${workspaceId}/tasks`);

export type WorkspaceArchivedPageParams = {
  limit?: number;
  cursor?: WorkspaceIndexCursor | null;
};

export const listWorkspaceArchivedTaskSummaries = (
  workspaceId: string,
  params?: WorkspaceArchivedPageParams,
) => {
  const search = new URLSearchParams();
  if (params?.limit) search.set("limit", String(params.limit));
  if (params?.cursor) {
    const cursorSortAt = String(params.cursor.sort_at ?? "").trim();
    const cursorTaskId = idToString(params.cursor.task_id);
    if (cursorSortAt) search.set("cursor_sort_at", cursorSortAt);
    if (cursorTaskId) search.set("cursor_task_id", cursorTaskId);
  }
  const qs = search.toString();
  const suffix = qs ? `?${qs}` : "";
  return apiAny<WorkspaceArchivedPage>(`/api/workspaces/${workspaceId}/archived_task_summaries${suffix}`);
};

export const listTaskSessions = (taskId: string) =>
  apiAny<Session[]>(`/api/tasks/${taskId}/sessions`);

export const createTask = (
  workspaceId: string,
  title: string,
  description?: string,
  opts?: {
    id?: string;
    default_session?: {
      id?: string;
      provider_id: string;
      model_id: string;
      reasoning_effort?: string | null;
      remember_model_preference?: boolean;
      execution_environment?: ExecutionEnvironment;
      worktree_id?: string;
      initial_prompt?: string;
      initial_message_id?: string;
      initial_turn_id?: string;
    };
  },
) =>
  apiAny<Task>(`/api/workspaces/${workspaceId}/tasks`, {
    method: "POST",
    body: JSON.stringify({
      ...(opts?.id ? { id: opts.id } : {}),
      title,
      description,
      ...(opts?.default_session ? { default_session: opts.default_session } : {}),
    }),
  });

export const updateTaskTitle = (taskId: string, title: string) =>
  apiAny<Task>(`/api/tasks/${taskId}/title`, { method: "POST", body: JSON.stringify({ title }) });

export const deleteTask = (taskId: string) =>
  apiAny<void>(`/api/tasks/${taskId}`, { method: "DELETE" });

export type ArchiveTaskResponse = Task & { cleanup_failed?: boolean };

export const archiveTask = (taskId: string) =>
  apiAny<ArchiveTaskResponse>(`/api/tasks/${taskId}/archive`, { method: "POST" });

export const unarchiveTask = (taskId: string) =>
  apiAny<Task>(`/api/tasks/${taskId}/unarchive`, { method: "POST" });

export const markTaskRead = (taskId: string) =>
  apiAny<Task>(`/api/tasks/${taskId}/mark_read`, { method: "POST" });

export const markTaskUnread = (taskId: string) =>
  apiAny<Task>(`/api/tasks/${taskId}/mark_unread`, { method: "POST" });

export const getWorktree = (worktreeId: string) =>
  apiAny<Worktree>(`/api/worktrees/${worktreeId}`);

export const getWorktreeBootstrapLogs = async (worktreeId: string): Promise<string> => {
  const resp = await daemonFetchRaw(`/api/worktrees/${worktreeId}/bootstrap/logs`);
  if (resp.status >= 400) {
    const msg = String(resp.body || "").trim();
    throw new Error(msg || `Failed to download logs (${resp.status}).`);
  }
  return resp.body ?? "";
};

export const listMergeQueueEntries = (workspaceId: string, opts?: { limit?: number }) => {
  const qs = new URLSearchParams({ workspace_id: workspaceId });
  if (typeof opts?.limit === "number") qs.set("limit", String(opts.limit));
  const suffix = qs.toString() ? `?${qs.toString()}` : "";
  return apiAny<MergeQueueEntry[]>(`/api/merge-queue/entries${suffix}`);
};

export const submitMergeQueueEntry = (payload: {
  session_id?: string;
  worktree_id?: string;
  target_branch?: string;
  message?: string;
}) =>
  apiAny<MergeQueueEntry>("/api/merge-queue/entries", {
    method: "POST",
    body: JSON.stringify(payload),
  });

export const cancelMergeQueueEntry = (workspaceId: string, entryId: string) =>
  apiAny<MergeQueueEntry>(`/api/workspaces/${workspaceId}/merge_queue/entries/${entryId}/cancel`, {
    method: "POST",
  });

export const retryMergeQueueEntry = (workspaceId: string, entryId: string) =>
  apiAny<MergeQueueEntry>(`/api/workspaces/${workspaceId}/merge_queue/entries/${entryId}/retry`, {
    method: "POST",
  });

export const getMergeQueueEntryLogs = async (workspaceId: string, entryId: string): Promise<string> => {
  const resp = await daemonFetchRaw(`/api/workspaces/${workspaceId}/merge_queue/entries/${entryId}/logs`);
  if (resp.status >= 400) {
    const msg = String(resp.body || "").trim();
    throw new Error(msg || `Failed to download logs (${resp.status}).`);
  }
  return resp.body ?? "";
};
