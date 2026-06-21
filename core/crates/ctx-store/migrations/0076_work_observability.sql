CREATE TABLE IF NOT EXISTS work_records (
  work_id TEXT PRIMARY KEY NOT NULL,
  workspace_id TEXT NOT NULL,
  title TEXT,
  objective TEXT,
  lifecycle TEXT NOT NULL,
  primary_repo_root TEXT,
  primary_branch TEXT,
  base_commit TEXT,
  head_commit TEXT,
  current_diff_fingerprint_json TEXT,
  trust_verdict TEXT NOT NULL,
  summary_freshness TEXT NOT NULL,
  metadata_json TEXT,
  record_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  schema_version INTEGER NOT NULL,
  FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_work_records_workspace_updated
  ON work_records (workspace_id, updated_at DESC, work_id DESC);

CREATE INDEX IF NOT EXISTS idx_work_records_workspace_lifecycle
  ON work_records (workspace_id, lifecycle, updated_at DESC, work_id DESC);

CREATE INDEX IF NOT EXISTS idx_work_records_workspace_repo
  ON work_records (workspace_id, primary_repo_root, primary_branch, head_commit);

CREATE TABLE IF NOT EXISTS work_record_links (
  link_id TEXT PRIMARY KEY NOT NULL,
  work_id TEXT NOT NULL,
  workspace_id TEXT NOT NULL,
  target_kind TEXT NOT NULL,
  target_id TEXT,
  target_json TEXT,
  role TEXT NOT NULL,
  source TEXT NOT NULL,
  fidelity TEXT NOT NULL,
  trust TEXT NOT NULL,
  record_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  schema_version INTEGER NOT NULL,
  FOREIGN KEY (work_id) REFERENCES work_records(work_id) ON DELETE CASCADE,
  FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_work_record_links_work
  ON work_record_links (workspace_id, work_id, target_kind, updated_at DESC, link_id DESC);

CREATE INDEX IF NOT EXISTS idx_work_record_links_target
  ON work_record_links (workspace_id, target_kind, target_id, updated_at DESC, link_id DESC);

CREATE UNIQUE INDEX IF NOT EXISTS idx_work_record_links_unique_known_target
  ON work_record_links (workspace_id, target_kind, target_id, work_id, role)
  WHERE target_id IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS idx_work_record_links_unique_strong_target
  ON work_record_links (workspace_id, target_kind, target_id)
  WHERE target_id IS NOT NULL AND target_kind IN ('pull_request', 'commit');

CREATE TABLE IF NOT EXISTS work_events (
  event_id TEXT PRIMARY KEY NOT NULL,
  work_id TEXT NOT NULL,
  workspace_id TEXT NOT NULL,
  sequence INTEGER NOT NULL,
  source_kind TEXT,
  source_id TEXT,
  event_type TEXT NOT NULL,
  event_time TEXT NOT NULL,
  actor_kind TEXT NOT NULL,
  provider TEXT,
  harness TEXT,
  model TEXT,
  redaction_class TEXT NOT NULL,
  source TEXT NOT NULL,
  fidelity TEXT NOT NULL,
  trust TEXT NOT NULL,
  payload_json TEXT,
  redacted_text TEXT,
  artifact_ref_json TEXT,
  record_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  schema_version INTEGER NOT NULL,
  FOREIGN KEY (work_id) REFERENCES work_records(work_id) ON DELETE CASCADE,
  FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_work_events_work_sequence
  ON work_events (workspace_id, work_id, sequence);

CREATE INDEX IF NOT EXISTS idx_work_events_work_time
  ON work_events (workspace_id, work_id, event_time DESC, event_id DESC);

CREATE INDEX IF NOT EXISTS idx_work_events_source
  ON work_events (workspace_id, source_kind, source_id);

CREATE TABLE IF NOT EXISTS work_evidence (
  evidence_id TEXT PRIMARY KEY NOT NULL,
  work_id TEXT NOT NULL,
  workspace_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  status TEXT NOT NULL,
  freshness TEXT NOT NULL,
  claim TEXT,
  command TEXT,
  argv_json TEXT NOT NULL,
  cwd TEXT,
  exit_code INTEGER,
  repo_root TEXT,
  head_sha TEXT,
  branch TEXT,
  fingerprint_json TEXT,
  current_fingerprint_json TEXT,
  output_ref_json TEXT,
  artifact_ref_json TEXT,
  source TEXT NOT NULL,
  fidelity TEXT NOT NULL,
  trust TEXT NOT NULL,
  record_json TEXT NOT NULL,
  started_at TEXT NOT NULL,
  finished_at TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  schema_version INTEGER NOT NULL,
  FOREIGN KEY (work_id) REFERENCES work_records(work_id) ON DELETE CASCADE,
  FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_work_evidence_work_updated
  ON work_evidence (workspace_id, work_id, updated_at DESC, evidence_id DESC);

CREATE INDEX IF NOT EXISTS idx_work_evidence_freshness
  ON work_evidence (workspace_id, work_id, freshness, status);

CREATE TABLE IF NOT EXISTS work_summaries (
  summary_id TEXT PRIMARY KEY NOT NULL,
  work_id TEXT NOT NULL,
  workspace_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  audience TEXT NOT NULL,
  text TEXT NOT NULL,
  structured_json TEXT,
  generation_method TEXT NOT NULL,
  provider TEXT,
  model TEXT,
  template TEXT,
  source_material_left_machine INTEGER NOT NULL,
  freshness TEXT NOT NULL,
  source_revision_key TEXT,
  record_json TEXT NOT NULL,
  generated_at TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  schema_version INTEGER NOT NULL,
  FOREIGN KEY (work_id) REFERENCES work_records(work_id) ON DELETE CASCADE,
  FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_work_summaries_work_kind
  ON work_summaries (workspace_id, work_id, kind, audience, updated_at DESC, summary_id DESC);

CREATE TABLE IF NOT EXISTS work_summary_claims (
  claim_id TEXT PRIMARY KEY NOT NULL,
  summary_id TEXT NOT NULL,
  work_id TEXT NOT NULL,
  workspace_id TEXT NOT NULL,
  claim_text TEXT NOT NULL,
  claim_kind TEXT,
  source_kind TEXT NOT NULL,
  source_id TEXT NOT NULL,
  record_hash TEXT,
  freshness TEXT NOT NULL,
  redaction_class TEXT NOT NULL,
  record_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  schema_version INTEGER NOT NULL,
  FOREIGN KEY (summary_id) REFERENCES work_summaries(summary_id) ON DELETE CASCADE,
  FOREIGN KEY (work_id) REFERENCES work_records(work_id) ON DELETE CASCADE,
  FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_work_summary_claims_summary
  ON work_summary_claims (workspace_id, summary_id, claim_id);

CREATE INDEX IF NOT EXISTS idx_work_summary_claims_source
  ON work_summary_claims (workspace_id, source_kind, source_id);

CREATE TABLE IF NOT EXISTS work_search_docs (
  doc_id TEXT PRIMARY KEY NOT NULL,
  workspace_id TEXT NOT NULL,
  work_id TEXT NOT NULL,
  doc_type TEXT NOT NULL,
  source_id TEXT NOT NULL,
  source_kind TEXT NOT NULL,
  event_time TEXT NOT NULL,
  repo_root TEXT,
  path TEXT,
  branch TEXT,
  commit_sha TEXT,
  pr_owner TEXT,
  pr_repo TEXT,
  pr_number INTEGER,
  agent_provider TEXT,
  freshness TEXT NOT NULL,
  redaction_class TEXT NOT NULL,
  title TEXT,
  search_text_redacted TEXT NOT NULL,
  record_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  schema_version INTEGER NOT NULL,
  FOREIGN KEY (work_id) REFERENCES work_records(work_id) ON DELETE CASCADE,
  FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_work_search_docs_workspace_filters
  ON work_search_docs (
    workspace_id,
    repo_root,
    path,
    branch,
    commit_sha,
    pr_owner,
    pr_repo,
    pr_number,
    freshness,
    updated_at DESC
  );

CREATE INDEX IF NOT EXISTS idx_work_search_docs_workspace_pr
  ON work_search_docs (workspace_id, pr_owner, pr_repo, pr_number, updated_at DESC);

CREATE INDEX IF NOT EXISTS idx_work_search_docs_workspace_commit
  ON work_search_docs (workspace_id, commit_sha, updated_at DESC);

CREATE INDEX IF NOT EXISTS idx_work_search_docs_workspace_path
  ON work_search_docs (workspace_id, path, updated_at DESC);

CREATE VIRTUAL TABLE IF NOT EXISTS work_search_docs_fts USING fts5(
  doc_id UNINDEXED,
  workspace_id UNINDEXED,
  work_id UNINDEXED,
  doc_type UNINDEXED,
  source_id UNINDEXED,
  source_kind UNINDEXED,
  event_time UNINDEXED,
  repo_root UNINDEXED,
  path UNINDEXED,
  branch UNINDEXED,
  commit_sha UNINDEXED,
  pr_owner UNINDEXED,
  pr_repo UNINDEXED,
  pr_number UNINDEXED,
  agent_provider UNINDEXED,
  freshness UNINDEXED,
  redaction_class UNINDEXED,
  title UNINDEXED,
  search_text_redacted
);
