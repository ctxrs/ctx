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
