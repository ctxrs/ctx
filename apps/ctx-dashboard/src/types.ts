export type TagCount = {
  tag: string;
  count: number;
};

export type DashboardSummary = {
  record_count: number;
  evidence_count: number;
  linked_pr_count: number;
  tags: TagCount[];
};

export type PrivacySummary = {
  default_redacted: boolean;
  raw_transcripts_withheld: number;
  redacted_previews: number;
  withheld_links: number;
  local_paths_redacted: boolean;
};

export type DashboardRecord = {
  id: string;
  title: string;
  body: string;
  tags: string[];
  kind: string;
  workspace?: string | null;
  pr_url?: string | null;
  created_at: string;
  updated_at: string;
};

export type EvidenceCommand = {
  id: string;
  record_id?: string | null;
  command: string;
  exit_code: number;
  duration_ms: number;
  started_at: string;
  output_preview?: string | null;
};

export type PullRequest = {
  url: string;
  title?: string | null;
  state?: string | null;
  head_ref?: string | null;
  base_ref?: string | null;
};

export type DashboardSession = {
  id: string;
  work_record_id?: string | null;
  parent_session_id?: string | null;
  root_session_id?: string | null;
  is_primary?: boolean | null;
  provider?: string | null;
  support_status?: string | null;
  capture_path?: string | null;
  privacy_note?: string | null;
  external_session_id?: string | null;
  external_agent_id?: string | null;
  agent_type?: string | null;
  role_hint?: string | null;
  status?: string | null;
  fidelity?: string | null;
  transcript_blob_id?: string | null;
  started_at?: string | null;
  ended_at?: string | null;
  [key: string]: unknown;
};

export type DashboardRun = {
  id: string;
  work_record_id?: string | null;
  session_id?: string | null;
  run_type?: string | null;
  status?: string | null;
  command_preview?: string | null;
  exit_code?: number | null;
  started_at?: string | null;
  ended_at?: string | null;
  [key: string]: unknown;
};

export type DashboardEvent = {
  id: string;
  seq?: number | null;
  work_record_id?: string | null;
  session_id?: string | null;
  run_id?: string | null;
  event_type?: string | null;
  role?: string | null;
  preview?: string | null;
  payload_blob_id?: string | null;
  redaction_state?: string | null;
  fidelity?: string | null;
  occurred_at?: string | null;
  [key: string]: unknown;
};

export type DashboardArtifact = {
  id: string;
  kind?: string | null;
  byte_size?: number | null;
  media_type?: string | null;
  redaction_state?: string | null;
  preview?: string | null;
  [key: string]: unknown;
};

export type DashboardEvidenceMetadata = {
  id: string;
  work_record_id?: string | null;
  kind?: string | null;
  status?: string | null;
  freshness?: string | null;
  stale_reason?: string | null;
  [key: string]: unknown;
};

export type DashboardSummaryItem = {
  id: string;
  work_record_id?: string | null;
  session_id?: string | null;
  kind?: string | null;
  model_or_source?: string | null;
  text?: string | null;
  [key: string]: unknown;
};

export type DashboardData = {
  schema_version: number;
  product: string;
  share_safe: boolean;
  summary: DashboardSummary;
  privacy: PrivacySummary;
  views: string[];
  records: DashboardRecord[];
  commands: EvidenceCommand[];
  sessions: DashboardSession[];
  runs: DashboardRun[];
  events: DashboardEvent[];
  vcs_workspaces: Record<string, unknown>[];
  vcs_changes: Record<string, unknown>[];
  pull_requests: PullRequest[];
  artifacts: DashboardArtifact[];
  evidence_metadata: DashboardEvidenceMetadata[];
  files_touched: Record<string, unknown>[];
  summaries: DashboardSummaryItem[];
  status: {
    export_mode: string;
    local_only: boolean;
    javascript_app: string;
    data_contract: string;
    search_command: string;
  };
};
