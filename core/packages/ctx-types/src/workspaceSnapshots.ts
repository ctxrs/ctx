import type { Task } from "./workspace";
import type { Session, SessionMetadata, SessionSummary } from "./session";
import type { Artifact, Message, SessionActivityState, SessionEvent, SessionTurn, SessionTurnToolSummary } from "./sessionTranscript";

export type WorkspaceTaskSummary = {
  task: Task;
  provider_ids?: string[];
  sessions?: SessionSummary[];
  sort_at: string;
};

export type WorkspaceIndexCursor = {
  sort_at: string;
  task_id: string;
};

export type WorkspaceIndexPage = {
  workspace_id: string;
  snapshot_rev: number;
  tasks: WorkspaceTaskSummary[];
  next_cursor?: WorkspaceIndexCursor | null;
  total_active: number;
  total_archived: number;
};

export type WorkspaceArchivedPage = {
  workspace_id: string;
  archived_rev?: number;
  tasks: WorkspaceTaskSummary[];
  next_cursor?: WorkspaceIndexCursor | null;
  total_archived: number;
};

export type WorkspaceIndexEvent =
  | {
      type: "ready";
      workspace_id: string;
      snapshot_rev: number;
      archived_rev: number;
    }
  | {
      type: "task_upsert";
      workspace_id: string;
      snapshot_rev: number;
      task: WorkspaceTaskSummary;
    }
  | {
      type: "task_delete";
      workspace_id: string;
      snapshot_rev: number;
      task_id: string;
    };

export type SessionSnapshotSummary = {
  session: SessionMetadata;
  last_message_at?: string | null;
  last_message_preview?: string | null;
  last_event_seq?: number | null;
  projection_rev?: number;
  state_rev?: number;
  activity?: SessionActivityState;
  unread?: boolean;
};

export type SessionHeadSnapshot = {
  session: SessionMetadata;
  turns: SessionTurn[];
  tool_summaries?: SessionTurnToolSummary[];
  events?: SessionEvent[];
  messages: Message[];
  last_event_seq: number;
  projection_rev?: number;
  state_rev?: number;
  activity?: SessionActivityState;
  has_more_turns: boolean;
  history_cursor?: number | null;
  has_more_history: boolean;
  summary_checkpoint?: SessionSummaryCheckpoint | null;
  head_window?: SessionHeadWindow;
};

export type SessionSnapshot = {
  summary: SessionSnapshotSummary;
  head?: SessionHeadSnapshot | null;
  state?: SessionState | null;
};

export type SessionGitStatusSummary = {
  summary_line: string;
  branch?: string | null;
  upstream?: string | null;
  ahead: number;
  behind: number;
  detached: boolean;
  staged: number;
  unstaged: number;
  untracked: number;
};

export type SessionState = {
  artifacts: Artifact[];
  git_status?: SessionGitStatusSummary | null;
};

export type WorktreeVcsComputeState = "computing" | "ready" | "error";
export type WorktreeVcsFreshness = "unknown" | "refreshing" | "fresh" | "stale" | "error";
export type DiffUnavailableReason = "no_repo" | "no_target_branch";

export type WorktreeVcsBaseResolutionKind = "explicit_base" | "merge_base" | "worktree_base";

export type WorktreeVcsTargetSource = "explicit" | "primary_branch_config";

export type WorktreeVcsBaseResolution = {
  kind: WorktreeVcsBaseResolutionKind;
  target_source?: WorktreeVcsTargetSource | null;
  error?: string | null;
};

export type WorktreeVcsSummary = {
  file_count?: number | null;
  line_additions?: number | null;
  line_deletions?: number | null;
  line_count?: number | null;
};

export type WorktreeVcsTouchedFile = {
  path: string;
  orig_path?: string | null;
  index_status?: string | null;
  worktree_status?: string | null;
};

export type WorktreeVcsTouchedFiles = {
  items?: WorktreeVcsTouchedFile[];
  truncated?: boolean;
  total_count?: number | null;
};

export type WorktreeVcsTouchedFilesState =
  | "not_loaded"
  | "loading"
  | "ready"
  | "stale"
  | "error";

export type WorktreeVcsGitStatusSummary = {
  raw?: string;
  summary_line?: string;
  branch?: string | null;
  upstream?: string | null;
  ahead: number;
  behind: number;
  detached: boolean;
  staged: number;
  unstaged: number;
  untracked: number;
  entries?: WorktreeVcsTouchedFile[];
};

export type WorktreeVcsSnapshot = {
  worktree_id: string;
  rev: number;
  emitted_at_ms: number;
  base_commit_sha: string;
  head_commit_sha: string;
  target_branch?: string | null;
  target_branch_commit_sha?: string | null;
  base_resolution: WorktreeVcsBaseResolution;
  compute_state: WorktreeVcsComputeState;
  summary: WorktreeVcsSummary;
  git_status: WorktreeVcsGitStatusSummary;
  touched_files: WorktreeVcsTouchedFiles;
  touched_files_state?: WorktreeVcsTouchedFilesState;
  freshness: WorktreeVcsFreshness;
  available?: boolean;
  unavailable_reason?: DiffUnavailableReason | null;
  schema_version: number;
};

export type WorkspaceActiveTaskSummary = {
  task: Task;
  primary_session: SessionSnapshotSummary;
  primary_session_head?: SessionHeadSnapshot | null;
  sessions: SessionSnapshotSummary[];
  sort_at: string;
};

export type WorkspaceActivePage = {
  tasks: WorkspaceActiveTaskSummary[];
  total_count: number;
};

export type WorkspaceActiveSnapshot = {
  workspace_id: string;
  snapshot_rev: number;
  archived_rev?: number;
  active: WorkspaceActivePage;
};

export type WorkspaceActiveHeadBatch = {
  workspace_id: string;
  snapshot_rev: number;
  heads: SessionHeadSnapshot[];
};

export type SessionSummaryCheckpoint = {
  session_id: string;
  checkpoint_id: string;
  summary: string;
  last_turn_id?: string | null;
  last_event_seq?: number | null;
  created_at: string;
  updated_at: string;
};

export type SessionHeadWindow = {
  turn_limit: number;
  message_limit: number;
  event_limit: number;
  byte_limit: number;
  turn_count: number;
  message_count: number;
  event_count: number;
  bytes: number;
  truncated?: boolean;
};

export type SessionHead = {
  session: Session;
  turns: SessionTurn[];
  tool_summaries?: SessionTurnToolSummary[];
  events?: SessionEvent[];
  messages: Message[];
  last_event_seq: number;
  projection_rev?: number;
  activity?: SessionActivityState;
  has_more_turns: boolean;
  summary_checkpoint?: SessionSummaryCheckpoint | null;
  head_window?: SessionHeadWindow;
};

export type SessionHeadDelta = {
  session_id: string;
  last_event_seq: number;
  projection_rev?: number;
  state_rev?: number;
  emitted_at_ms?: number | null;
  session?: Session | null;
  activity?: SessionActivityState | null;
  event?: SessionEvent | null;
  turn?: SessionTurn | null;
  message?: Message | null;
  tool_summaries?: SessionTurnToolSummary[];
};

export type SessionHistoryPage = {
  session_id: string;
  turns: SessionTurn[];
  messages: Message[];
  next_cursor?: number | null;
  has_more: boolean;
};

export type SessionEventsPage = {
  session_id: string;
  events: SessionEvent[];
  next_cursor?: number | null;
  has_more: boolean;
};

export type WorktreeBootstrapNotice = {
  worktree_id: string;
  worktree_root: string;
  status: "success" | "failed" | "timeout";
  started_at: string;
  finished_at: string;
  exit_code?: number | null;
  timeout_sec?: number | null;
  config_path?: string | null;
  config_key?: string | null;
  command?: string | null;
  script_path?: string | null;
  log_path?: string | null;
  log_truncated?: boolean | null;
  error?: string | null;
};

export type WorkspaceActiveSnapshotTaskDeltaKind = "updated" | "archived" | "unarchived";

export type WorkspaceActiveSnapshotTaskDelta = {
  kind: WorkspaceActiveSnapshotTaskDeltaKind;
  task: Task;
};

export type WorkspaceActiveSnapshotTaskDeltaEvent = {
  type: "task_delta";
  workspace_id: string;
  snapshot_rev: number;
  delta: WorkspaceActiveSnapshotTaskDelta;
};

export type WorkspaceActiveSnapshotSessionSummaryDelta = {
  session_id: string;
  task_id: string;
  activity?: SessionActivityState | null;
  last_message_at?: string | null;
  last_message_preview?: string | null;
  last_event_seq?: number | null;
  projection_rev?: number | null;
  state_rev?: number | null;
  emitted_at_ms?: number | null;
};

export type WorkspaceActiveSnapshotSessionSummaryDeltaEvent = {
  type: "session_summary_delta";
  workspace_id: string;
  snapshot_rev: number;
  delta: WorkspaceActiveSnapshotSessionSummaryDelta;
};

export type WorkspaceActiveSnapshotEvent =
  | {
      type: "ready";
      workspace_id: string;
      snapshot_rev: number;
      archived_rev?: number;
    }
  | WorkspaceActiveSnapshotTaskDeltaEvent
  | {
      type: "active_task_upsert";
      workspace_id: string;
      snapshot_rev: number;
      task: WorkspaceActiveTaskSummary;
    }
  | {
      type: "active_task_delete";
      workspace_id: string;
      snapshot_rev: number;
      task_id: string;
    }
  | WorkspaceActiveSnapshotSessionSummaryDeltaEvent
  | {
      type: "session_summary";
      workspace_id: string;
      snapshot_rev: number;
      summary: SessionSnapshotSummary;
    }
  | {
      type: "session_head_delta";
      workspace_id: string;
      snapshot_rev: number;
      delta: SessionHeadDelta;
    }
  | {
      type: "session_head_seed";
      workspace_id: string;
      snapshot_rev: number;
      head: SessionHeadSnapshot;
    }
  | {
      type: "session_gap";
      workspace_id: string;
      snapshot_rev: number;
      session_id: string;
      after_seq: number;
      reason?: string | null;
      seed_follows?: boolean;
    }
  | {
      type: "worktree_bootstrap";
      workspace_id: string;
      snapshot_rev: number;
      notice: WorktreeBootstrapNotice;
    }
  | {
      type: "archived_task_upsert";
      workspace_id: string;
      snapshot_rev?: number;
      archived_rev: number;
      task: WorkspaceTaskSummary;
    }
  | {
      type: "archived_task_delete";
      workspace_id: string;
      snapshot_rev?: number;
      archived_rev: number;
      task_id: string;
    };

export type WorkspaceActiveSnapshotSessionSubscription = {
  session_id: string;
  intent?: WorkspaceActiveSnapshotSessionIntent;
  replay: WorkspaceActiveSnapshotSessionReplay;
};

export type WorkspaceActiveSnapshotSessionIntent = "head" | "replay";

export type WorkspaceActiveSnapshotSessionReplay =
  | {
      mode: "auto";
    }
  | {
      mode: "reset";
    }
  | {
      mode: "resume";
      after_seq: number;
      after_projection_rev?: number;
    };

export type WorkspaceActiveSnapshotSubscribeScope = "active";

export type WorkspaceActiveSnapshotClientMessage =
  | {
      type: "subscribe";
      session_ids?: (string)[];
      sessions?: WorkspaceActiveSnapshotSessionSubscription[];
      task_ids?: (string)[];
      foreground_session_id?: string;
      scope?: WorkspaceActiveSnapshotSubscribeScope | null;
      include_active_heads?: boolean;
    };

export type WorktreeVcsStreamTier = "summary" | "details";

export type WorktreeVcsStreamClientMessage =
  | {
      type: "replace_subscription";
      summary_worktree_ids?: string[];
      detail_worktree_ids?: string[];
    }
  | {
      type: "refresh";
      worktree_ids?: string[];
      tier: WorktreeVcsStreamTier;
    };

export type WorktreeVcsStreamMessage =
  | {
      type: "ready";
      workspace_id: string;
      vcs_generation: number;
    }
  | {
      type: "subscribed";
      workspace_id: string;
      demand_generation: number;
      summary_worktree_ids: string[];
      detail_worktree_ids: string[];
    }
  | {
      type: "summary_snapshot";
      workspace_id: string;
      worktree_id: string;
      demand_generation: number;
      snapshot: WorktreeVcsSnapshot;
    }
  | {
      type: "details_snapshot";
      workspace_id: string;
      worktree_id: string;
      demand_generation: number;
      snapshot: WorktreeVcsSnapshot;
    }
  | {
      type: "unavailable_snapshot";
      workspace_id: string;
      worktree_id: string;
      demand_generation: number;
      snapshot: WorktreeVcsSnapshot;
    }
  | {
      type: "reset_required";
      workspace_id: string;
      vcs_generation: number;
    };
