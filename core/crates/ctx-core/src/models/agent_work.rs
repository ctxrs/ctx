use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ids::*;

pub const AGENT_WORK_EXPORT_SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RecordSource {
    #[default]
    Unknown,
    Worktree,
    Session,
    MergeQueue,
    PullRequest,
    Manual,
    External,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RecordOrigin {
    #[default]
    Unknown,
    User,
    Agent,
    System,
    Imported,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RecordFidelity {
    #[default]
    Unknown,
    Declared,
    Summary,
    Diff,
    Commit,
    Exact,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RecordTrust {
    #[default]
    Unknown,
    Low,
    Medium,
    High,
    Verified,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Sha256DigestValue(pub String);

impl Sha256DigestValue {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        Self(hex::encode(hasher.finalize()))
    }

    pub fn from_serializable<T: Serialize>(value: &T) -> serde_json::Result<Self> {
        let bytes = serde_json::to_vec(value)?;
        Ok(Self::from_bytes(&bytes))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Serialize)]
struct AgentWorkSourceRecordForHash<'a> {
    schema_version: i64,
    record_id: &'a AgentWorkSourceRecordId,
    previous_hash: &'a Option<Sha256DigestValue>,
    payload_hash: &'a Sha256DigestValue,
    created_at: &'a DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentWorkSourceRecord {
    #[serde(default = "default_agent_work_schema_version")]
    pub schema_version: i64,
    pub record_id: AgentWorkSourceRecordId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_hash: Option<Sha256DigestValue>,
    pub payload_hash: Sha256DigestValue,
    pub record_hash: Sha256DigestValue,
    pub created_at: DateTime<Utc>,
}

impl AgentWorkSourceRecord {
    pub fn from_payload<T: Serialize>(
        schema_version: i64,
        record_id: AgentWorkSourceRecordId,
        previous_hash: Option<Sha256DigestValue>,
        payload: &T,
        created_at: DateTime<Utc>,
    ) -> serde_json::Result<Self> {
        let payload_hash = Sha256DigestValue::from_serializable(payload)?;
        Self::from_payload_hash(
            schema_version,
            record_id,
            previous_hash,
            payload_hash,
            created_at,
        )
    }

    pub fn from_payload_hash(
        schema_version: i64,
        record_id: AgentWorkSourceRecordId,
        previous_hash: Option<Sha256DigestValue>,
        payload_hash: Sha256DigestValue,
        created_at: DateTime<Utc>,
    ) -> serde_json::Result<Self> {
        let for_hash = AgentWorkSourceRecordForHash {
            schema_version,
            record_id: &record_id,
            previous_hash: &previous_hash,
            payload_hash: &payload_hash,
            created_at: &created_at,
        };
        let record_hash = Sha256DigestValue::from_serializable(&for_hash)?;
        Ok(Self {
            schema_version,
            record_id,
            previous_hash,
            payload_hash,
            record_hash,
            created_at,
        })
    }

    pub fn verify_payload<T: Serialize>(&self, payload: &T) -> serde_json::Result<bool> {
        let payload_hash = Sha256DigestValue::from_serializable(payload)?;
        Ok(payload_hash == self.payload_hash && self.verify_record_hash()?)
    }

    pub fn verify_record_hash(&self) -> serde_json::Result<bool> {
        let for_hash = AgentWorkSourceRecordForHash {
            schema_version: self.schema_version,
            record_id: &self.record_id,
            previous_hash: &self.previous_hash,
            payload_hash: &self.payload_hash,
            created_at: &self.created_at,
        };
        Ok(Sha256DigestValue::from_serializable(&for_hash)? == self.record_hash)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitFingerprint {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub patch_sha256: Sha256DigestValue,
    pub status_sha256: Sha256DigestValue,
    pub untracked_sha256: Sha256DigestValue,
    pub changed_paths_sha256: Sha256DigestValue,
    pub dirty: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PullRequestRef {
    pub provider: String,
    pub owner: String,
    pub repo: String,
    pub number: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestLinkKind {
    Source,
    Target,
    Result,
    #[default]
    Related,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ContributionRole {
    Authored,
    Validated,
    Reviewed,
    Context,
    Result,
    #[default]
    Related,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PullRequestLink {
    #[serde(default, skip_serializing_if = "is_default_pull_request_link_kind")]
    pub kind: PullRequestLinkKind,
    pub pull_request: PullRequestRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContributionEndpoint {
    Account {
        account_id: AccountId,
    },
    Workspace {
        workspace_id: WorkspaceId,
    },
    Task {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<TaskId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    Session {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<SessionId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_id: Option<TurnId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
    },
    Run {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<SessionId>,
    },
    Agent {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<SessionId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    System {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    Worktree {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worktree_id: Option<WorktreeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    #[serde(alias = "change-set")]
    ChangeSet {
        #[serde(alias = "id")]
        change_set_id: ChangeSetId,
    },
    #[serde(alias = "pull-request")]
    PullRequest {
        pull_request: PullRequestRef,
    },
    Artifact {
        #[serde(default, alias = "id", skip_serializing_if = "Option::is_none")]
        artifact_id: Option<ArtifactId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        digest: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        relative_path: Option<String>,
    },
    Check {
        #[serde(alias = "id")]
        check_id: String,
    },
    Evidence {
        id: String,
    },
    #[serde(alias = "review-attestation")]
    ReviewAttestation {
        id: String,
    },
    Commit {
        sha: String,
    },
    Branch {
        name: String,
    },
    File {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worktree_id: Option<WorktreeId>,
    },
    External {
        source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        identifier: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
}

pub type ContributionSubject = ContributionEndpoint;
pub type ContributionTarget = ContributionEndpoint;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChangeSet {
    pub id: ChangeSetId,
    pub workspace_id: WorkspaceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_worktree_id: Option<WorktreeId>,
    #[serde(default, skip_serializing_if = "is_default_record_source")]
    pub source: RecordSource,
    #[serde(default, skip_serializing_if = "is_default_record_origin")]
    pub origin: RecordOrigin,
    #[serde(default, skip_serializing_if = "is_default_record_fidelity")]
    pub fidelity: RecordFidelity,
    #[serde(default, skip_serializing_if = "is_default_record_trust")]
    pub trust: RecordTrust,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<GitFingerprint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pull_requests: Vec<PullRequestLink>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_records: Vec<AgentWorkSourceRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
    #[serde(default = "default_agent_work_schema_version")]
    pub schema_version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Contribution {
    pub id: ContributionId,
    pub workspace_id: WorkspaceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_set_id: Option<ChangeSetId>,
    pub subject: ContributionSubject,
    pub target: ContributionTarget,
    #[serde(default, skip_serializing_if = "is_default_contribution_role")]
    pub role: ContributionRole,
    #[serde(default, skip_serializing_if = "is_default_record_source")]
    pub source: RecordSource,
    #[serde(default, skip_serializing_if = "is_default_record_origin")]
    pub origin: RecordOrigin,
    #[serde(default, skip_serializing_if = "is_default_record_fidelity")]
    pub fidelity: RecordFidelity,
    #[serde(default, skip_serializing_if = "is_default_record_trust")]
    pub trust: RecordTrust,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<GitFingerprint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_json: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_records: Vec<AgentWorkSourceRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
    #[serde(default = "default_agent_work_schema_version")]
    pub schema_version: i64,
}

fn is_default_record_source(v: &RecordSource) -> bool {
    *v == RecordSource::default()
}

fn is_default_record_origin(v: &RecordOrigin) -> bool {
    *v == RecordOrigin::default()
}

fn is_default_record_fidelity(v: &RecordFidelity) -> bool {
    *v == RecordFidelity::default()
}

fn is_default_record_trust(v: &RecordTrust) -> bool {
    *v == RecordTrust::default()
}

fn is_default_pull_request_link_kind(v: &PullRequestLinkKind) -> bool {
    *v == PullRequestLinkKind::default()
}

fn is_default_contribution_role(v: &ContributionRole) -> bool {
    *v == ContributionRole::default()
}

fn default_agent_work_schema_version() -> i64 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn record_enums_serialize_snake_case() {
        assert_eq!(
            serde_json::to_string(&RecordSource::PullRequest).unwrap(),
            "\"pull_request\""
        );
        assert_eq!(
            serde_json::to_string(&RecordOrigin::Imported).unwrap(),
            "\"imported\""
        );
        assert_eq!(
            serde_json::to_string(&RecordFidelity::Commit).unwrap(),
            "\"commit\""
        );
        assert_eq!(
            serde_json::to_string(&RecordTrust::Verified).unwrap(),
            "\"verified\""
        );
    }

    #[test]
    fn changeset_serializes_public_shape() {
        let pull_request = PullRequestRef {
            provider: "github".into(),
            owner: "ctxrs".into(),
            repo: "ctx".into(),
            number: 42,
            id: Some("PR_kwDOExample".into()),
            url: Some("https://github.com/ctxrs/ctx/pull/42".into()),
            title: Some("Model scaffolding".into()),
        };
        let changeset = ChangeSet {
            id: ChangeSetId::new(),
            workspace_id: WorkspaceId::new(),
            source_worktree_id: Some(WorktreeId::new()),
            source: RecordSource::Worktree,
            origin: RecordOrigin::Agent,
            fidelity: RecordFidelity::Diff,
            trust: RecordTrust::Verified,
            title: Some("Model scaffolding".into()),
            summary: Some("Adds graph model scaffolding".into()),
            description: None,
            fingerprint: None,
            base_revision: Some("base-sha".into()),
            head_revision: Some("head-sha".into()),
            target_branch: Some("main".into()),
            pull_requests: vec![PullRequestLink {
                kind: PullRequestLinkKind::Result,
                pull_request,
                url: Some("https://github.com/ctxrs/ctx/pull/42".into()),
                title: None,
                state: Some("open".into()),
            }],
            source_records: Vec::new(),
            issuer: Some("local".into()),
            created_at: None,
            updated_at: None,
            schema_version: 1,
        };

        let value = serde_json::to_value(&changeset).unwrap();

        assert_eq!(value.get("source"), Some(&json!("worktree")));
        assert_eq!(value.get("origin"), Some(&json!("agent")));
        assert_eq!(value.get("fidelity"), Some(&json!("diff")));
        assert_eq!(value.get("trust"), Some(&json!("verified")));
        assert_eq!(
            value.pointer("/pull_requests/0/kind"),
            Some(&json!("result"))
        );
        assert_eq!(
            value.pointer("/pull_requests/0/pull_request/provider"),
            Some(&json!("github"))
        );

        let round_trip: ChangeSet = serde_json::from_value(value).unwrap();
        assert_eq!(round_trip.source, RecordSource::Worktree);
        assert_eq!(
            round_trip.pull_requests[0].kind,
            PullRequestLinkKind::Result
        );
    }

    #[test]
    fn contribution_subject_and_target_are_tagged() {
        let contribution = Contribution {
            id: ContributionId::new(),
            workspace_id: WorkspaceId::new(),
            change_set_id: Some(ChangeSetId::new()),
            subject: ContributionSubject::Session {
                session_id: Some(SessionId::new()),
                provider: None,
                id: None,
                turn_id: Some(TurnId::new()),
                run_id: None,
            },
            target: ContributionTarget::File {
                path: "core/crates/ctx-core/src/models/agent_work.rs".into(),
                worktree_id: Some(WorktreeId::new()),
            },
            role: ContributionRole::Authored,
            source: RecordSource::Session,
            origin: RecordOrigin::Agent,
            fidelity: RecordFidelity::Exact,
            trust: RecordTrust::High,
            summary: Some("Added public model scaffolding".into()),
            fingerprint: None,
            issuer: Some("local".into()),
            metadata_json: Some(json!({ "files": 1 })),
            source_records: Vec::new(),
            created_at: None,
            updated_at: None,
            schema_version: 1,
        };

        let value = serde_json::to_value(&contribution).unwrap();

        assert_eq!(value.pointer("/subject/kind"), Some(&json!("session")));
        assert_eq!(value.pointer("/target/kind"), Some(&json!("file")));
        assert_eq!(
            value.pointer("/target/path"),
            Some(&json!("core/crates/ctx-core/src/models/agent_work.rs"))
        );

        let round_trip: Contribution = serde_json::from_value(value).unwrap();
        assert!(matches!(
            round_trip.subject,
            ContributionSubject::Session { .. }
        ));
        assert!(matches!(round_trip.target, ContributionTarget::File { .. }));
    }

    #[test]
    fn contribution_can_link_task_to_pull_request() {
        let contribution = Contribution {
            id: ContributionId::new(),
            workspace_id: WorkspaceId::new(),
            change_set_id: None,
            subject: ContributionSubject::Task {
                task_id: Some(TaskId::new()),
                id: None,
            },
            target: ContributionTarget::PullRequest {
                pull_request: PullRequestRef {
                    provider: "github".into(),
                    owner: "ctxrs".into(),
                    repo: "ctx".into(),
                    number: 108,
                    id: None,
                    url: None,
                    title: None,
                },
            },
            role: ContributionRole::Related,
            source: RecordSource::Manual,
            origin: RecordOrigin::User,
            fidelity: RecordFidelity::Declared,
            trust: RecordTrust::Medium,
            summary: Some("Task contributes to PR review scope".into()),
            fingerprint: None,
            issuer: None,
            metadata_json: None,
            source_records: Vec::new(),
            created_at: None,
            updated_at: None,
            schema_version: 1,
        };

        let value = serde_json::to_value(&contribution).unwrap();

        assert_eq!(value.pointer("/subject/kind"), Some(&json!("task")));
        assert_eq!(value.pointer("/target/kind"), Some(&json!("pull_request")));
        assert_eq!(
            value.pointer("/target/pull_request/provider"),
            Some(&json!("github"))
        );

        let round_trip: Contribution = serde_json::from_value(value).unwrap();
        assert!(matches!(
            round_trip.subject,
            ContributionSubject::Task { .. }
        ));
        assert!(matches!(
            round_trip.target,
            ContributionTarget::PullRequest { .. }
        ));
    }

    #[test]
    fn contribution_endpoint_accepts_public_kebab_case_aliases() {
        let change_set_id = ChangeSetId::new();
        let change_set: ContributionEndpoint = serde_json::from_value(json!({
            "kind": "change-set",
            "id": change_set_id,
        }))
        .unwrap();
        assert_eq!(
            change_set,
            ContributionEndpoint::ChangeSet { change_set_id }
        );

        let pull_request: ContributionEndpoint = serde_json::from_value(json!({
            "kind": "pull-request",
            "pull_request": {
                "provider": "github",
                "owner": "ctxrs",
                "repo": "ctx",
                "number": 108
            }
        }))
        .unwrap();
        assert!(matches!(
            pull_request,
            ContributionEndpoint::PullRequest { .. }
        ));
    }

    #[test]
    fn contribution_endpoint_accepts_external_endpoint_ids() {
        let task: ContributionEndpoint = serde_json::from_value(json!({
            "kind": "task",
            "id": "task-123"
        }))
        .unwrap();
        assert_eq!(
            task,
            ContributionEndpoint::Task {
                task_id: None,
                id: Some("task-123".to_string())
            }
        );

        let session: ContributionEndpoint = serde_json::from_value(json!({
            "kind": "session",
            "provider": "codex",
            "id": "thr_external"
        }))
        .unwrap();
        assert_eq!(
            session,
            ContributionEndpoint::Session {
                session_id: None,
                provider: Some("codex".to_string()),
                id: Some("thr_external".to_string()),
                turn_id: None,
                run_id: None
            }
        );

        let run: ContributionEndpoint = serde_json::from_value(json!({
            "kind": "run",
            "id": "run_external"
        }))
        .unwrap();
        assert_eq!(
            run,
            ContributionEndpoint::Run {
                run_id: None,
                id: Some("run_external".to_string()),
                session_id: None
            }
        );

        let worktree: ContributionEndpoint = serde_json::from_value(json!({
            "kind": "worktree",
            "id": "wtr_external"
        }))
        .unwrap();
        assert_eq!(
            worktree,
            ContributionEndpoint::Worktree {
                worktree_id: None,
                id: Some("wtr_external".to_string())
            }
        );
    }

    #[test]
    fn contribution_endpoint_accepts_external_check_ids() {
        let endpoint: ContributionEndpoint = serde_json::from_value(json!({
            "kind": "check",
            "id": "github:ctxrs/ctx/actions/runs/123456/jobs/987654"
        }))
        .unwrap();

        assert_eq!(
            endpoint,
            ContributionEndpoint::Check {
                check_id: "github:ctxrs/ctx/actions/runs/123456/jobs/987654".to_string()
            }
        );
    }

    #[test]
    fn agent_work_schema_version_defaults_to_public_v1() {
        let change_set = serde_json::from_value::<ChangeSet>(json!({
            "id": ChangeSetId::new(),
            "workspace_id": WorkspaceId::new()
        }))
        .unwrap();
        let contribution = serde_json::from_value::<Contribution>(json!({
            "id": ContributionId::new(),
            "workspace_id": WorkspaceId::new(),
            "subject": {
                "kind": "system"
            },
            "target": {
                "kind": "external",
                "source": "test"
            }
        }))
        .unwrap();

        assert_eq!(change_set.schema_version, 1);
        assert_eq!(contribution.schema_version, 1);
    }

    #[test]
    fn source_record_verifies_payload_and_record_hashes() {
        let payload = json!({
            "record_type": "change_set",
            "id": "chg_source",
            "title": "imported source"
        });
        let source_record = AgentWorkSourceRecord::from_payload(
            AGENT_WORK_EXPORT_SCHEMA_VERSION,
            AgentWorkSourceRecordId::from_id("rec_source"),
            None,
            &payload,
            Utc::now(),
        )
        .unwrap();

        assert!(source_record.verify_record_hash().unwrap());
        assert!(source_record.verify_payload(&payload).unwrap());
        assert!(!source_record
            .verify_payload(&json!({ "record_type": "change_set", "id": "tampered" }))
            .unwrap());
    }

    #[test]
    fn changeset_serializes_source_records() {
        let source_record = AgentWorkSourceRecord::from_payload(
            AGENT_WORK_EXPORT_SCHEMA_VERSION,
            AgentWorkSourceRecordId::from_id("rec_export"),
            None,
            &json!({ "id": "chg_export" }),
            Utc::now(),
        )
        .unwrap();
        let value = serde_json::to_value(ChangeSet {
            id: ChangeSetId::from_id("chg_export"),
            workspace_id: WorkspaceId::new(),
            source_worktree_id: None,
            source: RecordSource::External,
            origin: RecordOrigin::Imported,
            fidelity: RecordFidelity::Exact,
            trust: RecordTrust::Verified,
            title: None,
            summary: None,
            description: None,
            fingerprint: None,
            base_revision: None,
            head_revision: None,
            target_branch: None,
            pull_requests: Vec::new(),
            source_records: vec![source_record.clone()],
            issuer: Some("import".into()),
            created_at: None,
            updated_at: None,
            schema_version: 1,
        })
        .unwrap();

        assert_eq!(
            value.pointer("/source_records/0/record_id"),
            Some(&json!("rec_export"))
        );
        let round_trip: ChangeSet = serde_json::from_value(value).unwrap();
        assert_eq!(round_trip.source_records, vec![source_record]);
    }
}
