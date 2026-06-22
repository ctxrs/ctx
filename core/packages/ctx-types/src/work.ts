import type { RecordFidelity, RecordSource, RecordTrust } from "./agentWork";

export type WorkLifecycle =
  | "active"
  | "waiting"
  | "blocked"
  | "ready_for_review"
  | "merged"
  | "abandoned";

export type WorkTrustVerdict =
  | "verified"
  | "stale"
  | "missing_evidence"
  | "partial"
  | "untrusted_local_capture"
  | "failed";

export type WorkSummaryFreshness = "missing" | "fresh" | "stale" | "partial" | "locked";
export type WorkEvidenceFreshness = "fresh" | "stale" | "partial" | "unknown";
export type WorkEvidenceStatus = "observed_pass" | "observed_fail" | "skipped" | "unknown" | "stale";
export type WorkEvidenceKind =
  | "command"
  | "test"
  | "lint"
  | "format"
  | "typecheck"
  | "build"
  | "screenshot"
  | "recording"
  | "log"
  | "manual_review"
  | "agent_review"
  | "ci_result"
  | "artifact_inspection";
export type WorkLinkTargetKind =
  | "task"
  | "session"
  | "run"
  | "change_set"
  | "contribution"
  | "pull_request"
  | "commit"
  | "branch"
  | "worktree"
  | "artifact"
  | "evidence"
  | "summary"
  | "file"
  | "external";
export type WorkLinkRole = "source" | "result" | "evidence" | "context" | "parent" | "child" | "related";
export type WorkEventType =
  | "session"
  | "user_message"
  | "assistant_message"
  | "tool_call_start"
  | "tool_call_end"
  | "tool_output"
  | "command_capture"
  | "artifact_created"
  | "change_set_updated"
  | "pull_request_linked"
  | "commit_linked"
  | "evidence_observed"
  | "summary_generated"
  | "import"
  | "export"
  | "note"
  | "other";
export type WorkActorKind = "human" | "agent" | "subagent" | "system" | "plugin";
export type WorkRedactionClass = "public" | "local_redacted" | "local_private" | "sensitive";
export type WorkSummaryKind =
  | "live_summary"
  | "context_summary"
  | "report_summary"
  | "decision_log"
  | "evidence_summary";
export type WorkSummaryAudience = "agent" | "human" | "reviewer";
export type WorkSummaryGenerationMethod = "deterministic" | "agent_submitted" | "provider_llm" | "manual";
export type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };

export type WorkspaceWorkRecord = {
  work_id: string;
  workspace_id: string;
  title?: string | null;
  objective?: string | null;
  lifecycle: WorkLifecycle;
  primary_branch?: string | null;
  base_commit?: string | null;
  head_commit?: string | null;
  trust_verdict: WorkTrustVerdict;
  summary_freshness: WorkSummaryFreshness;
  created_at: string;
  updated_at: string;
  schema_version: number;
};

export type WorkspaceWorkLink = {
  link_id: string;
  work_id: string;
  workspace_id: string;
  target_kind: WorkLinkTargetKind;
  target_id?: string | null;
  target_json?: JsonValue | null;
  role: WorkLinkRole;
  source: RecordSource;
  fidelity: RecordFidelity;
  trust: RecordTrust;
  created_at: string;
  updated_at: string;
  schema_version: number;
};

export type WorkspaceWorkEvent = {
  event_id: string;
  work_id: string;
  workspace_id: string;
  sequence: number;
  source_kind?: string | null;
  source_id?: string | null;
  event_type: WorkEventType;
  event_time: string;
  actor_kind: WorkActorKind;
  provider?: string | null;
  harness?: string | null;
  model?: string | null;
  redaction_class: WorkRedactionClass;
  source: RecordSource;
  fidelity: RecordFidelity;
  trust: RecordTrust;
  redacted_text?: string | null;
  created_at: string;
  schema_version: number;
};

export type WorkspaceWorkEvidence = {
  evidence_id: string;
  work_id: string;
  workspace_id: string;
  kind: WorkEvidenceKind;
  status: WorkEvidenceStatus;
  freshness: WorkEvidenceFreshness;
  claim?: string | null;
  command?: string | null;
  argv: string[];
  cwd?: string | null;
  exit_code?: number | null;
  head_sha?: string | null;
  branch?: string | null;
  output_ref?: JsonValue | null;
  artifact_ref?: JsonValue | null;
  source: RecordSource;
  fidelity: RecordFidelity;
  trust: RecordTrust;
  started_at: string;
  finished_at: string;
  created_at: string;
  updated_at: string;
  schema_version: number;
};

export type WorkspaceWorkSummary = {
  summary_id: string;
  work_id: string;
  workspace_id: string;
  kind: WorkSummaryKind;
  audience: WorkSummaryAudience;
  text: string;
  structured_json?: JsonValue | null;
  generation_method: WorkSummaryGenerationMethod;
  provider?: string | null;
  model?: string | null;
  template?: string | null;
  source_material_left_machine: boolean;
  freshness: WorkSummaryFreshness;
  source_revision_key?: string | null;
  generated_at: string;
  created_at: string;
  updated_at: string;
  schema_version: number;
};

export type WorkspaceWorkSummaryClaim = {
  claim_id: string;
  summary_id: string;
  work_id: string;
  workspace_id: string;
  claim_text: string;
  claim_kind?: string | null;
  source_kind: string;
  source_id: string;
  record_hash?: string | null;
  freshness: WorkSummaryFreshness;
  redaction_class: WorkRedactionClass;
  created_at: string;
  schema_version: number;
};

export type WorkspaceWorkDuplicateStrongLink = {
  target_kind: WorkLinkTargetKind;
  target_id: string;
  work_ids: string[];
};

export type WorkspaceWorkTrustSummary = {
  verdict: WorkTrustVerdict;
  reason: string;
  recommended_next_action: string;
  open_risks: string[];
};

export type WorkspaceWorkEvidenceSummary = {
  total: number;
  passing: number;
  failing: number;
  stale: number;
  missing: number;
};

export type WorkspaceWorkChangeSummary = {
  change_sets: number;
  contributions: number;
  pull_requests: JsonValue[];
  commits: string[];
};

export type WorkspaceWorkSafeJson = {
  value: JsonValue;
  redacted: boolean;
  redaction_notes: string[];
};

export type WorkspaceWorkInspectorTranscriptItem = {
  event_id: string;
  id?: string | null;
  sequence: number;
  event_type: WorkEventType;
  event_time: string;
  actor_kind: WorkActorKind;
  provider?: string | null;
  harness?: string | null;
  model?: string | null;
  redaction_class: WorkRedactionClass;
  text_preview?: string | null;
};

export type WorkspaceWorkInspectorCommand = {
  id: string;
  evidence_id: string;
  command?: string | null;
  argv: string[];
  cwd?: string | null;
  cwd_label?: string | null;
  exit_code?: number | null;
  status: WorkEvidenceStatus;
  freshness: WorkEvidenceFreshness;
  stdout_preview?: string | null;
  stderr_preview?: string | null;
  output_truncated: boolean;
  preview_limit_bytes?: number | null;
  stdout_size_bytes?: number | null;
  stderr_size_bytes?: number | null;
  stdout_sha256?: string | null;
  stderr_sha256?: string | null;
  stdout_truncated?: boolean;
  stderr_truncated?: boolean;
  started_at?: string | null;
  finished_at?: string | null;
  output_ref?: JsonValue | null;
};

export type WorkspaceWorkInspectorArtifact = {
  id: string;
  artifact_id?: string | null;
  source_kind?: string | null;
  source_id?: string | null;
  kind?: string | null;
  label?: string | null;
  display_name?: string | null;
  mime_type?: string | null;
  bytes?: number | null;
  missing: boolean;
  unavailable_reason?: string | null;
  render_kind: "raster_image" | "text" | "download_only" | "unavailable";
  download_url?: string | null;
  open_url?: string | null;
  thumbnail_url?: string | null;
  preview_url?: string | null;
  created_at?: string | null;
};

export type WorkspaceWorkInspectorRawRedactedJson = {
  safe_json: JsonValue;
};

export type WorkspaceWorkInspectorOverview = {
  title?: string | null;
  objective?: string | null;
  lifecycle: WorkLifecycle;
  primary_branch?: string | null;
  base_commit?: string | null;
  head_commit?: string | null;
  created_at: string;
  updated_at: string;
};

export type WorkspaceWorkInspectorTimelineItem = {
  sequence: number;
  event_time: string;
  kind: string;
  title: string;
  detail?: string | null;
  source_event_id?: string | null;
  source_evidence_id?: string | null;
};

export type WorkspaceWorkInspectorSubagent = {
  id: string;
  child_session_id?: string | null;
  run_id?: string | null;
  label?: string | null;
  summary?: string | null;
  status?: string | null;
  role?: string | null;
  provider?: string | null;
  harness?: string | null;
  model?: string | null;
  prompt_length?: number | null;
  event_count: number;
  latest_event_time?: string | null;
  transcript_preview: WorkspaceWorkInspectorTranscriptItem[];
};

export type WorkspaceWorkArtifactSummary = {
  total: number;
  refs: WorkspaceWorkSafeJson[];
};

export type WorkspaceWorkInspector = {
  work: WorkspaceWorkRecord;
  links: WorkspaceWorkLink[];
  overview: WorkspaceWorkInspectorOverview;
  trust: WorkspaceWorkTrustSummary;
  context: WorkspaceWorkSafeJson;
  safe_json: WorkspaceWorkSafeJson;
  raw_redacted_json: WorkspaceWorkSafeJson;
  evidence_summary: WorkspaceWorkEvidenceSummary;
  change_summary: WorkspaceWorkChangeSummary;
  artifact_summary: WorkspaceWorkArtifactSummary;
  transcript: WorkspaceWorkInspectorTranscriptItem[];
  commands: WorkspaceWorkInspectorCommand[];
  artifacts: WorkspaceWorkInspectorArtifact[];
  evidence: WorkspaceWorkEvidence[];
  change_sets: JsonValue[];
  contributions: JsonValue[];
  summaries: WorkspaceWorkSummary[];
  summary_claims: WorkspaceWorkSummaryClaim[];
  timeline: WorkspaceWorkEvent[];
  timeline_items: WorkspaceWorkInspectorTimelineItem[];
  subagents: WorkspaceWorkInspectorSubagent[];
  duplicate_strong_links: WorkspaceWorkDuplicateStrongLink[];
  raw_transcript_available: boolean;
  raw_transcript_included: boolean;
};

export type WorkspaceWorkReport = {
  work: WorkspaceWorkRecord;
  links: WorkspaceWorkLink[];
  trust: WorkspaceWorkTrustSummary;
  evidence_summary: WorkspaceWorkEvidenceSummary;
  evidence: WorkspaceWorkEvidence[];
  change_summary: WorkspaceWorkChangeSummary;
  change_sets: JsonValue[];
  contributions: JsonValue[];
  summaries: WorkspaceWorkSummary[];
  summary_claims: WorkspaceWorkSummaryClaim[];
  timeline: WorkspaceWorkEvent[];
  duplicate_strong_links: WorkspaceWorkDuplicateStrongLink[];
  raw_transcript_available: boolean;
  raw_transcript_included: boolean;
};

export type WorkspaceWorkListResponse = {
  work: WorkspaceWorkRecord[];
};

export type WorkspaceWorkDetailResponse = {
  work: WorkspaceWorkRecord;
  links: WorkspaceWorkLink[];
  evidence: WorkspaceWorkEvidence[];
  summaries: WorkspaceWorkSummary[];
  summary_claims: WorkspaceWorkSummaryClaim[];
  duplicate_strong_links: WorkspaceWorkDuplicateStrongLink[];
  raw_detail_included: boolean;
};

export type WorkspaceWorkContextResponse = {
  work_id: string;
  budget_tokens: number;
  title?: string | null;
  state: string;
  trust_verdict: WorkTrustVerdict;
  summary_freshness: WorkSummaryFreshness;
  context: JsonValue;
  raw_transcript_available: boolean;
  raw_transcript_included: boolean;
};

export type WorkspaceWorkTimelineResponse = {
  work_id: string;
  events: WorkspaceWorkEvent[];
  raw_transcript_included: boolean;
};

export type WorkspaceWorkEvidenceResponse = {
  work_id: string;
  evidence: WorkspaceWorkEvidence[];
};
