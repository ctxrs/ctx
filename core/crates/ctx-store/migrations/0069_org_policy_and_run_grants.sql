CREATE TABLE IF NOT EXISTS org_policy_snapshots (
  id TEXT PRIMARY KEY NOT NULL,
  org_id TEXT NOT NULL,
  policy_version TEXT NOT NULL,
  issued_at TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  grace_expires_at TEXT NOT NULL,
  allowed_providers_json TEXT,
  allowed_models_json TEXT NOT NULL DEFAULT '{}',
  required_execution_environment TEXT,
  allowed_network_profiles_json TEXT NOT NULL,
  route_policy_json TEXT NOT NULL,
  archive_policy_json TEXT NOT NULL,
  features_json TEXT NOT NULL DEFAULT '{}',
  signature TEXT NOT NULL,
  cached_at TEXT NOT NULL,
  UNIQUE(org_id, policy_version)
);

CREATE INDEX IF NOT EXISTS idx_org_policy_snapshots_org_issued
  ON org_policy_snapshots(org_id, issued_at DESC);

CREATE TABLE IF NOT EXISTS daemon_enrollments (
  id TEXT PRIMARY KEY NOT NULL,
  account_id TEXT NOT NULL,
  org_id TEXT NOT NULL,
  org_membership_id TEXT NOT NULL,
  membership_role TEXT NOT NULL,
  plan_type TEXT NOT NULL,
  status TEXT NOT NULL,
  policy_signature_algorithm TEXT NOT NULL,
  policy_signing_key TEXT NOT NULL,
  active_policy_snapshot_id TEXT,
  enrolled_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  revoked_at TEXT
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_daemon_enrollments_org_id
  ON daemon_enrollments(org_id);

CREATE TABLE IF NOT EXISTS workspace_policy_overlays (
  workspace_id TEXT PRIMARY KEY NOT NULL,
  org_id TEXT NOT NULL,
  allowed_providers_json TEXT,
  allowed_models_json TEXT NOT NULL DEFAULT '{}',
  required_execution_environment TEXT,
  allowed_network_profiles_json TEXT,
  allowed_route_types_json TEXT,
  features_json TEXT NOT NULL DEFAULT '{}',
  updated_at TEXT NOT NULL,
  FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_workspace_policy_overlays_org_id
  ON workspace_policy_overlays(org_id);

CREATE TABLE IF NOT EXISTS run_grants (
  id TEXT PRIMARY KEY NOT NULL,
  run_id TEXT NOT NULL UNIQUE,
  session_id TEXT NOT NULL,
  workspace_id TEXT NOT NULL,
  account_id TEXT NOT NULL,
  org_id TEXT NOT NULL,
  membership_role TEXT,
  policy_version TEXT NOT NULL,
  provider_id TEXT NOT NULL,
  model_id TEXT NOT NULL,
  execution_environment TEXT NOT NULL,
  network_profile TEXT NOT NULL,
  route_type TEXT,
  archive_mode TEXT NOT NULL,
  issued_at TEXT NOT NULL,
  expires_at TEXT,
  decision_source TEXT NOT NULL,
  FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE,
  FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_run_grants_session_id
  ON run_grants(session_id);

CREATE INDEX IF NOT EXISTS idx_run_grants_org_issued
  ON run_grants(org_id, issued_at DESC);

CREATE TABLE IF NOT EXISTS policy_decision_events (
  id TEXT PRIMARY KEY NOT NULL,
  run_grant_id TEXT,
  run_id TEXT,
  session_id TEXT,
  workspace_id TEXT,
  account_id TEXT,
  org_id TEXT,
  policy_snapshot_id TEXT,
  policy_version TEXT,
  decision_source TEXT NOT NULL,
  outcome TEXT NOT NULL,
  deny_reason TEXT,
  requested_provider_id TEXT,
  requested_model_id TEXT,
  requested_execution_environment TEXT,
  requested_network_profile TEXT,
  requested_route_type TEXT,
  detail TEXT,
  created_at TEXT NOT NULL,
  FOREIGN KEY (run_grant_id) REFERENCES run_grants(id) ON DELETE SET NULL,
  FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE SET NULL,
  FOREIGN KEY (workspace_id) REFERENCES workspaces(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_policy_decision_events_run_id
  ON policy_decision_events(run_id, created_at ASC);

CREATE INDEX IF NOT EXISTS idx_policy_decision_events_session_id
  ON policy_decision_events(session_id, created_at ASC);
