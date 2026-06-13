export type Workspace = {
  id: string;
  name: string;
  root_path: string;
  created_at: string;
  vcs_kind?: VcsKind | null;
};

export type VcsKind = "git" | "jj" | "hg" | "svn" | "p4" | "other";

export type WorkspaceAttachmentKind = "reference_repo" | "doc_mirror";

export type AttachmentMode = "ro" | "rw";

export type AttachmentUpdatePolicy = "manual" | "on_open" | "scheduled";

export type WorkspaceAttachmentStatus = "pending" | "syncing" | "ready" | "error";

export type WorkspaceAttachment = {
  id: string;
  workspace_id: string;
  kind: WorkspaceAttachmentKind;
  name: string;
  source: string;
  revision?: string | null;
  subpath?: string | null;
  mount_relpath: string;
  mode: AttachmentMode;
  update_policy: AttachmentUpdatePolicy;
  status: WorkspaceAttachmentStatus;
  last_sync_at?: string | null;
  error_message?: string | null;
  created_at: string;
  updated_at: string;
};

export type Task = {
  id: string;
  workspace_id: string;
  title: string;
  description?: string | null;
  status: string;
  primary_session_id?: string | null;
  primary_worktree_id?: string | null;
  created_at: string;
  updated_at: string;
  archived_at?: string | null;
  assistant_seen_at?: string | null;
  last_activity_at?: string | null;
  last_assistant_message_at?: string | null;
  has_active_session?: boolean;
};

export type Worktree = {
  id: string;
  workspace_id: string;
  root_path: string;
  base_commit_sha: string;
  git_branch?: string | null;
  vcs_kind?: VcsKind | null;
  base_revision?: string | null;
  vcs_ref?: string | null;
  created_at: string;
  bootstrap_status?: "success" | "failed" | "timeout";
  bootstrap_started_at?: string | null;
  bootstrap_finished_at?: string | null;
  bootstrap_exit_code?: number | null;
  bootstrap_timeout_sec?: number | null;
  bootstrap_error?: string | null;
  bootstrap_log_path?: string | null;
  bootstrap_log_truncated?: boolean | null;
  bootstrap_config_path?: string | null;
  bootstrap_config_key?: string | null;
  bootstrap_command?: string | null;
  bootstrap_script_path?: string | null;
};

export type MergeQueueEntryStatus =
  | "queued"
  | "running"
  | "passed"
  | "failed"
  | "conflict"
  | "cancelled";

export type MergeQueuePatchSource = "generated" | "provided";

export type MergeQueueEntry = {
  id: string;
  workspace_id: string;
  worktree_id?: string | null;
  session_id?: string | null;
  target_branch: string;
  message?: string | null;
  patch_source: MergeQueuePatchSource;
  base_commit_sha?: string | null;
  head_commit_sha?: string | null;
  patch_path: string;
  patch_size: number;
  status: MergeQueueEntryStatus;
  result_commit_sha?: string | null;
  error_message?: string | null;
  created_at: string;
  updated_at: string;
};

export type MergeQueueRunStatus =
  | "running"
  | "passed"
  | "failed"
  | "conflict"
  | "cancelled";

export type MergeQueueRun = {
  id: string;
  entry_id: string;
  status: MergeQueueRunStatus;
  started_at: string;
  finished_at?: string | null;
  exit_code?: number | null;
  log_path?: string | null;
  error_message?: string | null;
  result_commit_sha?: string | null;
};
