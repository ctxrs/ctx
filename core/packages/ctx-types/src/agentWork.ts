export type RecordSource =
  | "unknown"
  | "worktree"
  | "session"
  | "merge_queue"
  | "pull_request"
  | "manual"
  | "external";

export type RecordOrigin = "unknown" | "user" | "agent" | "system" | "imported";

export type RecordFidelity =
  | "unknown"
  | "declared"
  | "summary"
  | "diff"
  | "commit"
  | "exact";

export type RecordTrust = "unknown" | "low" | "medium" | "high" | "verified";

export type PullRequestProvider = string;

export type Sha256DigestValue = string;

export type AgentWorkSourceRecord = {
  schema_version?: number;
  record_id: string;
  previous_hash?: Sha256DigestValue | null;
  payload_hash: Sha256DigestValue;
  record_hash: Sha256DigestValue;
  created_at: string;
};

export type GitFingerprint = {
  repo_root?: string | null;
  head_sha?: string | null;
  branch?: string | null;
  patch_sha256: Sha256DigestValue;
  status_sha256: Sha256DigestValue;
  untracked_sha256: Sha256DigestValue;
  changed_paths_sha256: Sha256DigestValue;
  dirty: boolean;
};

export type PullRequestRef = {
  provider: PullRequestProvider;
  owner: string;
  repo: string;
  number: number;
  id?: string | null;
  url?: string | null;
  title?: string | null;
};

export type PullRequestLinkKind = "source" | "target" | "result" | "related";

export type PullRequestLink = {
  kind?: PullRequestLinkKind;
  pull_request: PullRequestRef;
  url?: string | null;
  title?: string | null;
  state?: string | null;
};

export type ContributionRole =
  | "authored"
  | "validated"
  | "reviewed"
  | "context"
  | "result"
  | "related";

export type ChangeSetContributionEndpoint =
  | {
      kind: "change_set" | "change-set";
      change_set_id: string;
      id?: string | null;
    }
  | {
      kind: "change_set" | "change-set";
      id: string;
      change_set_id?: string | null;
    };

export type CheckContributionEndpoint =
  | {
      kind: "check";
      check_id: string;
      id?: string | null;
    }
  | {
      kind: "check";
      id: string;
      check_id?: string | null;
    };

export type TaskContributionEndpoint =
  | {
      kind: "task";
      task_id: string;
      id?: string | null;
    }
  | {
      kind: "task";
      id: string;
      task_id?: string | null;
    };

export type SessionContributionEndpoint =
  | {
      kind: "session";
      session_id: string;
      provider?: string | null;
      id?: string | null;
      turn_id?: string | null;
      run_id?: string | null;
    }
  | {
      kind: "session";
      id: string;
      provider?: string | null;
      session_id?: string | null;
      turn_id?: string | null;
      run_id?: string | null;
    };

export type RunContributionEndpoint =
  | {
      kind: "run";
      run_id: string;
      id?: string | null;
      session_id?: string | null;
    }
  | {
      kind: "run";
      id: string;
      run_id?: string | null;
      session_id?: string | null;
    };

export type WorktreeContributionEndpoint =
  | {
      kind: "worktree";
      worktree_id: string;
      id?: string | null;
    }
  | {
      kind: "worktree";
      id: string;
      worktree_id?: string | null;
    };

export type ArtifactContributionEndpoint =
  | {
      kind: "artifact";
      artifact_id: string;
      id?: string | null;
      digest?: string | null;
      relative_path?: string | null;
    }
  | {
      kind: "artifact";
      id: string;
      artifact_id?: string | null;
      digest?: string | null;
      relative_path?: string | null;
    }
  | {
      kind: "artifact";
      digest: string;
      artifact_id?: string | null;
      id?: string | null;
      relative_path?: string | null;
    }
  | {
      kind: "artifact";
      relative_path: string;
      artifact_id?: string | null;
      id?: string | null;
      digest?: string | null;
    };

export type ContributionEndpoint =
  | {
      kind: "account";
      account_id: string;
    }
  | {
      kind: "workspace";
      workspace_id: string;
    }
  | TaskContributionEndpoint
  | SessionContributionEndpoint
  | RunContributionEndpoint
  | {
      kind: "agent";
      session_id?: string | null;
      run_id?: string | null;
      label?: string | null;
    }
  | {
      kind: "system";
      label?: string | null;
    }
  | WorktreeContributionEndpoint
  | ChangeSetContributionEndpoint
  | {
      kind: "pull_request" | "pull-request";
      pull_request: PullRequestRef;
    }
  | ArtifactContributionEndpoint
  | CheckContributionEndpoint
  | {
      kind: "evidence";
      id: string;
    }
  | {
      kind: "review_attestation" | "review-attestation";
      id: string;
    }
  | {
      kind: "commit";
      sha: string;
    }
  | {
      kind: "branch";
      name: string;
    }
  | {
      kind: "file";
      /** Workspace-relative path. Shareable exports should not include host-local absolute paths. */
      path: string;
      worktree_id?: string | null;
    }
  | {
      kind: "external";
      source: string;
      identifier?: string | null;
      url?: string | null;
    };

export type ContributionSubject = ContributionEndpoint;
export type ContributionTarget = ContributionEndpoint;

export type ChangeSet = {
  id: string;
  workspace_id: string;
  source_worktree_id?: string | null;
  source?: RecordSource;
  origin?: RecordOrigin;
  fidelity?: RecordFidelity;
  trust?: RecordTrust;
  title?: string | null;
  summary?: string | null;
  description?: string | null;
  fingerprint?: GitFingerprint | null;
  base_revision?: string | null;
  head_revision?: string | null;
  target_branch?: string | null;
  pull_requests?: PullRequestLink[];
  source_records?: AgentWorkSourceRecord[];
  issuer?: string | null;
  created_at?: string | null;
  updated_at?: string | null;
  schema_version?: number;
};

export type Contribution = {
  id: string;
  workspace_id: string;
  change_set_id?: string | null;
  subject: ContributionSubject;
  target: ContributionTarget;
  role?: ContributionRole;
  source?: RecordSource;
  origin?: RecordOrigin;
  fidelity?: RecordFidelity;
  trust?: RecordTrust;
  summary?: string | null;
  fingerprint?: GitFingerprint | null;
  issuer?: string | null;
  metadata_json?: unknown;
  source_records?: AgentWorkSourceRecord[];
  created_at?: string | null;
  updated_at?: string | null;
  schema_version?: number;
};

export type AgentWork = {
  change_sets: ChangeSet[];
  contributions: Contribution[];
};

export type WorkspaceAgentWork = AgentWork;
