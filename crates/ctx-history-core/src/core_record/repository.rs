use std::{cmp::Ordering, collections::HashSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{SourceKey, StableEntityId, StableEntityKind};

use super::{
    validation::{
        validate_count, validate_optional_text, validate_owned_identity,
        validate_repository_alias_component, validate_repository_relative_path, validate_text,
    },
    CoreRecordError, CoreRecordResult, CORE_BOUNDED_SHELL_SUBSET_REVISION,
    CORE_MISSING_ACTIVITY_TIME_UNIX_MS, CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
    CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_REVISION,
    CORE_REPOSITORY_OBSERVATION_REVISION, CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
    CORE_REPOSITORY_PULL_REQUEST_ASSOCIATION_CAPTURE_REVISION, MAX_GIT_REF_BYTES,
    MAX_OUTCOME_LINKAGE_ITEMS, MAX_REPOSITORY_ALIASES, MAX_REPOSITORY_COMMIT_OPERATION_MAPPINGS,
    MAX_REPOSITORY_EVIDENCE, MAX_REPOSITORY_ITEMS, MAX_REPOSITORY_NAMESPACE_PARTS,
    MAX_REPOSITORY_RELATIVE_PATH_BYTES, MAX_TEXT_METADATA_BYTES,
};

const MAX_REPOSITORY_TOOL_NAME_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitObjectFormat {
    Sha1,
    Sha256,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryBinding {
    pub binding_id: String,
    pub logical_repository_id: String,
    pub checkout_id: Option<String>,
    pub worktree_id: Option<String>,
    pub aliases: Vec<RepositoryAlias>,
    pub git_object_format: Option<GitObjectFormat>,
    pub local_root_authorization: Option<RepositoryLocalRootAuthorization>,
    pub evidence: Vec<RepositoryEvidence>,
    pub association_policy_revision: u32,
}

impl RepositoryBinding {
    pub fn validate_contract(&self) -> CoreRecordResult<()> {
        validate_text("binding_id", &self.binding_id, MAX_TEXT_METADATA_BYTES)?;
        validate_text(
            "logical_repository_id",
            &self.logical_repository_id,
            MAX_TEXT_METADATA_BYTES,
        )?;
        validate_optional_text(
            "checkout_id",
            self.checkout_id.as_deref(),
            MAX_TEXT_METADATA_BYTES,
        )?;
        validate_optional_text(
            "worktree_id",
            self.worktree_id.as_deref(),
            MAX_TEXT_METADATA_BYTES,
        )?;
        if self.worktree_id.is_some() && self.checkout_id.is_none() {
            return Err(CoreRecordError::InvalidIdentityRelationship);
        }
        validate_count(
            "repository_aliases",
            self.aliases.len(),
            MAX_REPOSITORY_ALIASES,
        )?;
        validate_count(
            "repository_evidence",
            self.evidence.len(),
            MAX_REPOSITORY_EVIDENCE,
        )?;
        if self.evidence.is_empty() {
            return Err(CoreRecordError::EmptyField {
                field: "repository_evidence",
            });
        }
        if self.association_policy_revision != CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION {
            return Err(CoreRecordError::InvalidRepositoryRevisions);
        }
        let mut aliases = HashSet::new();
        for alias in &self.aliases {
            alias.validate_contract()?;
            if !aliases.insert(alias) {
                return Err(CoreRecordError::DuplicateRepositoryIdentity {
                    field: "repository_alias",
                    value: format!("{}:{}", alias.host, alias.name),
                });
            }
        }
        if let Some(local_root) = &self.local_root_authorization {
            if self.checkout_id.is_none() || self.worktree_id.is_none() {
                return Err(CoreRecordError::InvalidIdentityRelationship);
            }
            local_root.validate_contract()?;
        }
        Ok(())
    }

    #[must_use]
    pub fn accepts_pull_request(&self, pull_request: &RepositoryPullRequestIdentity) -> bool {
        let logical_forge_matches = self
            .logical_repository_id
            .strip_prefix("forge:")
            .map(|logical| forge_logical_identity_matches(logical, &pull_request.forge_repository));
        if logical_forge_matches == Some(false) {
            return false;
        }
        let aliases_match = self
            .aliases
            .iter()
            .any(|alias| repository_alias_identity_matches(alias, &pull_request.forge_repository));
        logical_forge_matches == Some(true) || (logical_forge_matches.is_none() && aliases_match)
    }
}

/// Credential-free logical forge or configured-remote identity.
///
/// The structured shape intentionally has no URL, userinfo, token, or
/// credential-bearing field.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryAlias {
    pub kind: RepositoryAliasKind,
    pub host: String,
    pub namespace: Vec<String>,
    pub name: String,
    pub remote_name: Option<String>,
}

impl RepositoryAlias {
    fn validate_contract(&self) -> CoreRecordResult<()> {
        if self.host.is_empty()
            || self.host.len() > MAX_TEXT_METADATA_BYTES
            || self
                .host
                .bytes()
                .any(|byte| byte.is_ascii_control() || matches!(byte, b'@' | b'/' | b'\\'))
        {
            return Err(CoreRecordError::InvalidRepositoryAlias);
        }
        validate_count(
            "repository_alias_namespace",
            self.namespace.len(),
            MAX_REPOSITORY_NAMESPACE_PARTS,
        )?;
        if self.namespace.is_empty() {
            return Err(CoreRecordError::InvalidRepositoryAlias);
        }
        for component in self.namespace.iter().chain(std::iter::once(&self.name)) {
            validate_repository_alias_component(component)?;
        }
        validate_optional_text(
            "repository_remote_name",
            self.remote_name.as_deref(),
            MAX_TEXT_METADATA_BYTES,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryAliasKind {
    Forge,
    Remote,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryLocalRootAuthorization {
    pub local_root: String,
    pub local_root_authorization_fingerprint_revision: u32,
    pub local_root_authorization_fingerprint: [u8; 32],
    pub observed_at_unix_ms: i64,
}

impl RepositoryLocalRootAuthorization {
    /// Orders two local-root observations only when provider activity time is
    /// present and strictly different. `None` is intentionally ambiguous:
    /// callers must not break equal/missing-time ties by ingestion order.
    pub fn provider_activity_order(&self, other: &Self) -> Option<Ordering> {
        (self.observed_at_unix_ms != CORE_MISSING_ACTIVITY_TIME_UNIX_MS
            && other.observed_at_unix_ms != CORE_MISSING_ACTIVITY_TIME_UNIX_MS
            && self.observed_at_unix_ms != other.observed_at_unix_ms)
            .then(|| self.observed_at_unix_ms.cmp(&other.observed_at_unix_ms))
    }

    fn validate_contract(&self) -> CoreRecordResult<()> {
        validate_text(
            "repository_local_root",
            &self.local_root,
            MAX_REPOSITORY_RELATIVE_PATH_BYTES,
        )?;
        let bytes = self.local_root.as_bytes();
        let is_posix_absolute = self.local_root.starts_with('/');
        let is_windows_drive_absolute = bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\');
        let is_windows_unc = self.local_root.starts_with("\\\\");
        if !is_posix_absolute && !is_windows_drive_absolute && !is_windows_unc {
            return Err(CoreRecordError::InvalidIdentityRelationship);
        }
        if self.local_root_authorization_fingerprint_revision
            != CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_REVISION
        {
            return Err(CoreRecordError::InvalidRepositoryRevisions);
        }
        if self.local_root_authorization_fingerprint == [0; 32] {
            return Err(CoreRecordError::EmptyField {
                field: "repository_local_root_authorization_fingerprint",
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryEvidence {
    pub kind: RepositoryEvidenceKind,
    pub confidence: RepositoryEvidenceConfidence,
}

/// Structured activity that justified a repository candidate or binding.
/// Declared tool workdirs and effective shell cwd observations are distinct;
/// P0 deliberately assigns no command-parsing or cwd-resolution semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryEvidenceKind {
    ProviderNativeProject,
    ProviderNativeResult,
    DeclaredToolWorkdir,
    DerivedEffectiveCwd,
    CommandSpecificRepositoryPath,
    FileActivity,
    VcsActivity,
    SessionCwd,
}

/// One independent path candidate observed before repository certification.
///
/// Candidate identity is the pair of its evidence kind and path. The complete
/// collection is a canonical set, so repeated evidence is deduplicated and
/// provider traversal order cannot change stored Core bytes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryCandidate {
    pub kind: RepositoryCandidateKind,
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryCandidateKind {
    SessionCwd,
    DeclaredToolWorkdir,
    DerivedEffectiveCwd,
    CommandSpecificRepositoryPath,
    FileActivityPath,
    VcsActivityPath,
    OutcomeOperationRepositoryPath,
    OutcomeOutputRepositoryPath,
}

/// Independent structured repository candidate evidence retained before
/// bounded certification. These values are not repository authority.
///
/// In particular, declared workdir, derived effective cwd, and a
/// command-specific repository path must never be collapsed into one cwd.
/// P0 defines only the storage contract and does not parse or resolve commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryCandidateEvidence {
    pub repository_observation_revision: u32,
    pub bounded_shell_subset_revision: u32,
    pub association_policy_revision: u32,
    pub outcome_capture_revision: u32,
    pub candidates: Vec<RepositoryCandidate>,
}

impl Default for RepositoryCandidateEvidence {
    fn default() -> Self {
        Self {
            repository_observation_revision: CORE_REPOSITORY_OBSERVATION_REVISION,
            bounded_shell_subset_revision: CORE_BOUNDED_SHELL_SUBSET_REVISION,
            association_policy_revision: CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
            outcome_capture_revision: CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
            candidates: Vec::new(),
        }
    }
}

impl RepositoryCandidateEvidence {
    pub fn insert(&mut self, kind: RepositoryCandidateKind, path: String) {
        let candidate = RepositoryCandidate { kind, path };
        if let Err(index) = self.candidates.binary_search(&candidate) {
            self.candidates.insert(index, candidate);
        }
    }

    pub fn paths(&self, kind: RepositoryCandidateKind) -> impl Iterator<Item = &str> {
        self.candidates
            .iter()
            .filter(move |candidate| candidate.kind == kind)
            .map(|candidate| candidate.path.as_str())
    }

    pub fn validate_contract(&self) -> CoreRecordResult<()> {
        if self.repository_observation_revision != CORE_REPOSITORY_OBSERVATION_REVISION
            || self.bounded_shell_subset_revision != CORE_BOUNDED_SHELL_SUBSET_REVISION
            || self.association_policy_revision != CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION
            || self.outcome_capture_revision != CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION
        {
            return Err(CoreRecordError::InvalidRepositoryRevisions);
        }
        validate_count(
            "repository_candidate_evidence",
            self.candidates.len(),
            MAX_REPOSITORY_ITEMS,
        )?;
        if self.candidates.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(CoreRecordError::NonCanonicalRepositoryCandidateEvidence);
        }
        for candidate in &self.candidates {
            validate_text(
                "repository_candidate_path",
                &candidate.path,
                MAX_REPOSITORY_RELATIVE_PATH_BYTES,
            )?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryEvidenceConfidence {
    Explicit,
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryAbstention {
    pub evidence_kind: RepositoryEvidenceKind,
    pub reason: RepositoryAbstentionReason,
    pub detail: Option<String>,
    pub association_policy_revision: u32,
}

impl RepositoryAbstention {
    pub fn validate_contract(&self) -> CoreRecordResult<()> {
        validate_optional_text(
            "repository_abstention_detail",
            self.detail.as_deref(),
            MAX_TEXT_METADATA_BYTES,
        )?;
        if self.association_policy_revision != CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION {
            return Err(CoreRecordError::InvalidRepositoryRevisions);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryAbstentionReason {
    NoCandidate,
    Unavailable,
    Ambiguous,
    Unsafe,
    Unsupported,
    ConflictingIdentity,
    DynamicPath,
    UnknownWrapper,
    ProfileDependent,
    UnsupportedShell,
    CommandTooLarge,
    CandidateLimitExceeded,
    CandidateMissingBeforeCertification,
    UnsafePath,
    UnscopedFileActivity,
    AmbiguousCandidates,
    AmbiguousRemote,
    GitProbeFailed,
    ProbeBudgetExceeded,
    ProviderOutputUnjoined,
    LinkageCapacityExceeded,
    OutcomeResultInadmissible,
    HistoryRewriteUnlinked,
    OutcomeRepositoryUnbound,
    ConcurrentDrift,
    PlatformUnsupported,
}

/// Exact provider-native request intent for one repository file operation.
///
/// This is not an effect-success assertion. The optional text range selects
/// bytes in the enclosing record's normalized body and never stores copied
/// body or preview text.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryFileInvocationEvidence {
    pub operation_ordinal: u32,
    pub repository_binding_id: String,
    pub relative_path: String,
    pub prior_relative_path: Option<String>,
    pub kind: RepositoryFileInvocationKind,
    pub tool_name: Option<String>,
    pub normalized_text_range: Option<RepositoryFileInvocationTextRange>,
}

impl RepositoryFileInvocationEvidence {
    pub fn validate_contract(&self, normalized_body: Option<&str>) -> CoreRecordResult<()> {
        validate_text(
            "repository_binding_id",
            &self.repository_binding_id,
            MAX_TEXT_METADATA_BYTES,
        )?;
        validate_repository_relative_path(&self.relative_path)?;
        if let Some(prior) = &self.prior_relative_path {
            validate_repository_relative_path(prior)?;
        }
        if matches!(self.kind, RepositoryFileInvocationKind::Rename)
            != self.prior_relative_path.is_some()
        {
            return Err(CoreRecordError::InvalidRepositoryFileInvocationEvidence);
        }
        validate_optional_text(
            "repository_file_invocation_tool_name",
            self.tool_name.as_deref(),
            MAX_REPOSITORY_TOOL_NAME_BYTES,
        )?;
        if let Some(range) = &self.normalized_text_range {
            range.validate_contract(normalized_body)?;
        }
        Ok(())
    }
}

/// Closed request-intent actions; uncertain actions must be omitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryFileInvocationKind {
    Read,
    Create,
    Modify,
    Delete,
    Rename,
    Write,
}

/// Half-open UTF-8 byte range into `CoreContent.normalized_body`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryFileInvocationTextRange {
    /// Inclusive UTF-8 byte offset.
    pub start: u32,
    /// Exclusive UTF-8 byte offset.
    pub end: u32,
}

impl RepositoryFileInvocationTextRange {
    fn validate_contract(&self, normalized_body: Option<&str>) -> CoreRecordResult<()> {
        let Some(body) = normalized_body else {
            return Err(CoreRecordError::InvalidRepositoryFileInvocationEvidence);
        };
        let start = usize::try_from(self.start)
            .map_err(|_| CoreRecordError::InvalidRepositoryFileInvocationEvidence)?;
        let end = usize::try_from(self.end)
            .map_err(|_| CoreRecordError::InvalidRepositoryFileInvocationEvidence)?;
        if start >= end
            || end > body.len()
            || !body.is_char_boundary(start)
            || !body.is_char_boundary(end)
        {
            return Err(CoreRecordError::InvalidRepositoryFileInvocationEvidence);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryFileObservation {
    pub repository_binding_id: String,
    pub relative_path: String,
    pub kind: RepositoryFileObservationKind,
    pub prior_relative_path: Option<String>,
}

impl RepositoryFileObservation {
    pub fn validate_contract(&self) -> CoreRecordResult<()> {
        validate_text(
            "repository_binding_id",
            &self.repository_binding_id,
            MAX_TEXT_METADATA_BYTES,
        )?;
        validate_repository_relative_path(&self.relative_path)?;
        if let Some(prior) = &self.prior_relative_path {
            validate_repository_relative_path(prior)?;
        }
        if matches!(self.kind, RepositoryFileObservationKind::Renamed)
            != self.prior_relative_path.is_some()
        {
            return Err(CoreRecordError::InvalidRepositoryRelativePath(
                self.relative_path.clone(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryFileObservationKind {
    Read,
    Created,
    Modified,
    Deleted,
    Renamed,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryVcsObservation {
    pub repository_binding_id: String,
    pub kind: RepositoryVcsObservationKind,
    pub object_id: Option<GitObjectId>,
    pub parent_object_ids: Vec<GitObjectId>,
    pub reference: Option<String>,
    pub relative_path: Option<String>,
}

impl RepositoryVcsObservation {
    pub fn validate_contract(&self) -> CoreRecordResult<()> {
        validate_text(
            "repository_binding_id",
            &self.repository_binding_id,
            MAX_TEXT_METADATA_BYTES,
        )?;
        validate_count(
            "parent_object_ids",
            self.parent_object_ids.len(),
            MAX_REPOSITORY_ITEMS,
        )?;
        if let Some(object_id) = &self.object_id {
            object_id.validate_contract()?;
        }
        for object_id in &self.parent_object_ids {
            object_id.validate_contract()?;
        }
        validate_optional_text(
            "repository_vcs_reference",
            self.reference.as_deref(),
            MAX_GIT_REF_BYTES,
        )?;
        if let Some(path) = &self.relative_path {
            validate_repository_relative_path(path)?;
        }
        match &self.kind {
            RepositoryVcsObservationKind::Outcome(outcome) => {
                if self.has_outer_object_fields() {
                    return Err(CoreRecordError::InvalidRepositoryOutcome);
                }
                outcome.validate_contract()?;
            }
            RepositoryVcsObservationKind::PullRequestAssociation(association) => {
                if self.has_outer_object_fields() {
                    return Err(CoreRecordError::InvalidRepositoryPullRequestAssociation);
                }
                association.validate_contract()?;
            }
            _ => {}
        }
        Ok(())
    }

    fn has_outer_object_fields(&self) -> bool {
        self.object_id.is_some()
            || !self.parent_object_ids.is_empty()
            || self.reference.is_some()
            || self.relative_path.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryVcsObservationKind {
    Head,
    Commit,
    Branch,
    Worktree,
    Change,
    Reference,
    Outcome(Box<RepositoryOutcomeObservation>),
    PullRequestAssociation(Box<RepositoryPullRequestAssociationObservation>),
}

/// One exact repository outcome observed prospectively at provider event time.
///
/// The observation is nested in a repository-scoped VCS observation so it can
/// never exist without a certified `repository_binding_id`. It intentionally
/// models commit and pull-request operations in one lifecycle-neutral shape.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryOutcomeObservation {
    pub kind: RepositoryOutcomeKind,
    pub produced_object_ids: Vec<GitObjectId>,
    pub commit_operation: Option<RepositoryCommitOperationEvent>,
    pub pull_request: Option<RepositoryPullRequestIdentity>,
    /// Exact forge-reported merge association. This is never a commit yield.
    pub pull_request_merge_commit: Option<GitObjectId>,
    pub observed_at_unix_ms: i64,
    pub linkage: RepositoryOutcomeLinkage,
    pub outcome_capture_revision: u32,
}

impl RepositoryOutcomeObservation {
    pub fn validate_contract(&self) -> CoreRecordResult<()> {
        if self.outcome_capture_revision != CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION {
            return Err(CoreRecordError::InvalidRepositoryRevisions);
        }
        validate_count(
            "repository_outcome_object_ids",
            self.produced_object_ids.len(),
            MAX_REPOSITORY_ITEMS,
        )?;
        let mut produced = HashSet::new();
        for object_id in &self.produced_object_ids {
            object_id.validate_contract()?;
            if !produced.insert((object_id.format, object_id.hex.as_str())) {
                return Err(CoreRecordError::InvalidRepositoryOutcome);
            }
        }
        if let Some(operation) = &self.commit_operation {
            operation.validate_contract(&self.linkage)?;
        }
        if let Some(pull_request) = &self.pull_request {
            pull_request.validate_contract()?;
        }
        if let Some(merge_commit) = &self.pull_request_merge_commit {
            merge_commit.validate_contract()?;
        }
        match self.kind {
            RepositoryOutcomeKind::Commit
                if self.pull_request.is_none()
                    && self.pull_request_merge_commit.is_none()
                    && ((!self.produced_object_ids.is_empty()
                        && self.commit_operation.is_none())
                        || (self.produced_object_ids.is_empty()
                            && self.commit_operation.is_some())) => {}
            RepositoryOutcomeKind::PullRequestCreated
                if self.produced_object_ids.is_empty()
                    && self.commit_operation.is_none()
                    && self.pull_request.is_some()
                    && self.pull_request_merge_commit.is_none() => {}
            RepositoryOutcomeKind::PullRequestMerged
                if self.produced_object_ids.is_empty()
                    && self.commit_operation.is_none()
                    && self.pull_request.is_some()
                    && self.pull_request_merge_commit.is_some() => {}
            _ => return Err(CoreRecordError::InvalidRepositoryOutcome),
        }
        self.linkage.validate_contract()
    }

    pub fn object_ids(&self) -> impl Iterator<Item = &GitObjectId> {
        self.produced_object_ids
            .iter()
            .chain(
                self.commit_operation
                    .iter()
                    .flat_map(RepositoryCommitOperationEvent::object_ids),
            )
            .chain(self.pull_request_merge_commit.iter())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryOutcomeKind {
    Commit,
    PullRequestCreated,
    PullRequestMerged,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryCommitMapping {
    pub source: GitObjectId,
    pub result: GitObjectId,
}

impl RepositoryCommitMapping {
    fn validate_contract(&self) -> CoreRecordResult<()> {
        self.source.validate_contract()?;
        self.result.validate_contract()?;
        if self.source == self.result || self.source.format != self.result.format {
            return Err(CoreRecordError::InvalidRepositoryOutcome);
        }
        Ok(())
    }
}

/// One operation-scoped causal observation. Object identity remains the
/// enclosing logical repository plus object format plus full OID; checkout,
/// worktree, and ref values remain observation context on the binding.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryCommitOperationEvent {
    pub event_id: [u8; 32],
    pub receipt_id: [u8; 32],
    pub kind: RepositoryCommitOperationKind,
    pub mappings: Vec<RepositoryCommitMapping>,
    pub unlinked_sources: Vec<GitObjectId>,
    pub unlinked_results: Vec<GitObjectId>,
    pub mapping_completeness: RepositoryCommitMappingCompleteness,
    pub state: RepositoryCommitOperationState,
    pub proof: RepositoryCommitOperationProof,
}

impl RepositoryCommitOperationEvent {
    /// Admits one exact yield only after the caller has supplied the closed
    /// operation proof gathered from a linked command/result record and one
    /// drift-free repository/object-domain verification pass.
    #[allow(clippy::too_many_arguments)]
    pub fn repository_verified_yield(
        source: &SourceKey,
        core_event_id: StableEntityId,
        core_session_id: StableEntityId,
        repository: &RepositoryBinding,
        linkage: &RepositoryOutcomeLinkage,
        kind: RepositoryCommitOperationKind,
        mut mappings: Vec<RepositoryCommitMapping>,
        command_pre_head: Option<GitObjectId>,
        sequencer_pre_head: Option<GitObjectId>,
        command_post_head: GitObjectId,
        repository_object_domain_sha256: [u8; 32],
    ) -> CoreRecordResult<Self> {
        mappings.sort();
        let object_format = mappings
            .first()
            .map(|mapping| mapping.source.format)
            .ok_or(CoreRecordError::InvalidRepositoryOutcome)?;
        let exact_source_oids = canonical_mapping_sources(&mappings);
        let proof = RepositoryVerifiedYieldProof {
            command_pre_head,
            sequencer_pre_head,
            exact_source_oids,
            command_post_head,
            repository_geometry_before_sha256: repository_object_domain_sha256,
            repository_geometry_after_sha256: repository_object_domain_sha256,
            exact_result_map_sha256: repository_result_map_sha256(&mappings),
            drift_excluded: true,
            mutation_excluded: true,
        };
        let event = Self {
            event_id: repository_commit_operation_event_id(
                source,
                core_event_id,
                core_session_id,
                repository,
                object_format,
                &mappings,
                linkage,
                kind,
            ),
            receipt_id: repository_outcome_receipt_id(linkage),
            kind,
            mappings,
            unlinked_sources: Vec::new(),
            unlinked_results: Vec::new(),
            mapping_completeness: RepositoryCommitMappingCompleteness::Complete,
            state: RepositoryCommitOperationState::Asserted,
            proof: RepositoryCommitOperationProof::RepositoryVerifiedYield(proof),
        };
        event.validate_contract(linkage)?;
        event.validate_scoped_identity(
            source,
            core_event_id,
            core_session_id,
            repository,
            linkage,
        )?;
        Ok(event)
    }

    /// Retains an exact provider record without granting causal-edge or yield
    /// authority. Unlinked objects remain observations only.
    #[allow(clippy::too_many_arguments)]
    pub fn record_exact_unlinked(
        source: &SourceKey,
        core_event_id: StableEntityId,
        core_session_id: StableEntityId,
        repository: &RepositoryBinding,
        linkage: &RepositoryOutcomeLinkage,
        kind: RepositoryCommitOperationKind,
        mut unlinked_sources: Vec<GitObjectId>,
        mut unlinked_results: Vec<GitObjectId>,
        state: RepositoryCommitOperationState,
    ) -> CoreRecordResult<Self> {
        unlinked_sources.sort();
        unlinked_sources.dedup();
        unlinked_results.sort();
        unlinked_results.dedup();
        let object_format = unlinked_sources
            .first()
            .or_else(|| unlinked_results.first())
            .map(|object_id| object_id.format)
            .ok_or(CoreRecordError::InvalidRepositoryOutcome)?;
        let event = Self {
            event_id: repository_commit_operation_event_id(
                source,
                core_event_id,
                core_session_id,
                repository,
                object_format,
                &[],
                linkage,
                kind,
            ),
            receipt_id: repository_outcome_receipt_id(linkage),
            kind,
            mappings: Vec::new(),
            unlinked_sources,
            unlinked_results,
            mapping_completeness: RepositoryCommitMappingCompleteness::None,
            state,
            proof: RepositoryCommitOperationProof::RecordExact,
        };
        event.validate_contract(linkage)?;
        event.validate_scoped_identity(
            source,
            core_event_id,
            core_session_id,
            repository,
            linkage,
        )?;
        Ok(event)
    }

    fn validate_contract(&self, linkage: &RepositoryOutcomeLinkage) -> CoreRecordResult<()> {
        validate_count(
            "repository_commit_operation_mappings",
            self.mappings.len(),
            MAX_REPOSITORY_COMMIT_OPERATION_MAPPINGS,
        )?;
        validate_count(
            "repository_commit_operation_unlinked_sources",
            self.unlinked_sources.len(),
            MAX_REPOSITORY_ITEMS,
        )?;
        validate_count(
            "repository_commit_operation_unlinked_results",
            self.unlinked_results.len(),
            MAX_REPOSITORY_ITEMS,
        )?;
        if self.event_id == [0; 32] || self.receipt_id != repository_outcome_receipt_id(linkage) {
            return Err(CoreRecordError::InvalidRepositoryOutcome);
        }

        let mut mappings = HashSet::new();
        if self.mappings.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(CoreRecordError::InvalidRepositoryOutcome);
        }
        let mut mapped_sources = HashSet::new();
        let mut mapped_results = HashSet::new();
        for mapping in &self.mappings {
            mapping.validate_contract()?;
            if !mappings.insert(mapping) {
                return Err(CoreRecordError::InvalidRepositoryOutcome);
            }
            mapped_sources.insert(&mapping.source);
            mapped_results.insert(&mapping.result);
        }
        validate_canonical_object_set(&self.unlinked_sources)?;
        validate_canonical_object_set(&self.unlinked_results)?;
        if self
            .unlinked_sources
            .iter()
            .any(|source| mapped_sources.contains(source))
            || self
                .unlinked_results
                .iter()
                .any(|result| mapped_results.contains(result))
        {
            return Err(CoreRecordError::InvalidRepositoryOutcome);
        }
        let format = self.object_ids().next().map(|object_id| object_id.format);
        if format.is_none()
            || self
                .object_ids()
                .any(|object_id| Some(object_id.format) != format)
        {
            return Err(CoreRecordError::InvalidRepositoryOutcome);
        }
        match self.mapping_completeness {
            RepositoryCommitMappingCompleteness::Complete
                if !self.mappings.is_empty()
                    && self.unlinked_sources.is_empty()
                    && self.unlinked_results.is_empty() => {}
            RepositoryCommitMappingCompleteness::Partial
                if !self.mappings.is_empty()
                    && (!self.unlinked_sources.is_empty() || !self.unlinked_results.is_empty()) => {
            }
            RepositoryCommitMappingCompleteness::None
                if self.mappings.is_empty()
                    && (!self.unlinked_sources.is_empty() || !self.unlinked_results.is_empty()) => {
            }
            _ => return Err(CoreRecordError::InvalidRepositoryOutcome),
        }
        self.proof.validate_contract(self)
    }

    pub(super) fn validate_scoped_identity(
        &self,
        source: &SourceKey,
        core_event_id: StableEntityId,
        core_session_id: StableEntityId,
        repository: &RepositoryBinding,
        linkage: &RepositoryOutcomeLinkage,
    ) -> CoreRecordResult<()> {
        source
            .validate_contract()
            .map_err(|_| CoreRecordError::InvalidIdentityRelationship)?;
        validate_owned_identity(core_event_id, StableEntityKind::Event, source)?;
        validate_owned_identity(core_session_id, StableEntityKind::Session, source)?;
        repository.validate_contract()?;
        let object_format = self
            .object_ids()
            .next()
            .map(|object_id| object_id.format)
            .ok_or(CoreRecordError::InvalidRepositoryOutcome)?;
        if repository
            .git_object_format
            .is_some_and(|format| format != object_format)
            || self.event_id
                != repository_commit_operation_event_id(
                    source,
                    core_event_id,
                    core_session_id,
                    repository,
                    object_format,
                    &self.mappings,
                    linkage,
                    self.kind,
                )
        {
            return Err(CoreRecordError::InvalidRepositoryOutcome);
        }
        Ok(())
    }

    pub fn object_ids(&self) -> impl Iterator<Item = &GitObjectId> {
        self.mappings
            .iter()
            .flat_map(|mapping| [&mapping.source, &mapping.result])
            .chain(self.unlinked_sources.iter())
            .chain(self.unlinked_results.iter())
    }

    #[must_use]
    pub fn operation_class(&self) -> RepositoryCommitOperationClass {
        self.kind.operation_class()
    }

    /// Results admitted by Core as exact operation yields. Unlinked results
    /// and non-asserted/non-verified observations intentionally return none.
    pub fn repository_verified_yields(&self) -> impl Iterator<Item = &GitObjectId> {
        let admitted = self.state == RepositoryCommitOperationState::Asserted
            && matches!(
                self.proof,
                RepositoryCommitOperationProof::RepositoryVerifiedYield(_)
            );
        self.mappings
            .iter()
            .filter(move |_| admitted)
            .map(|mapping| &mapping.result)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryCommitOperationKind {
    Amend,
    Rebase,
    CherryPick,
}

impl RepositoryCommitOperationKind {
    #[must_use]
    pub const fn operation_class(self) -> RepositoryCommitOperationClass {
        match self {
            Self::Amend | Self::Rebase => RepositoryCommitOperationClass::Replacement,
            Self::CherryPick => RepositoryCommitOperationClass::Derivation,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryCommitOperationClass {
    Replacement,
    Derivation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryCommitMappingCompleteness {
    Complete,
    Partial,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryCommitOperationState {
    Asserted,
    Ambiguous,
    Contradicted,
}

/// Proof is deliberately orthogonal to operation kind, mapping completeness,
/// and merged state. Only `repository_verified_yield` admits direct yields.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RepositoryCommitOperationProof {
    RecordExact,
    RepositoryVerifiedYield(RepositoryVerifiedYieldProof),
}

impl RepositoryCommitOperationProof {
    fn validate_contract(&self, event: &RepositoryCommitOperationEvent) -> CoreRecordResult<()> {
        match self {
            Self::RecordExact if event.state != RepositoryCommitOperationState::Asserted => Ok(()),
            Self::RecordExact => Err(CoreRecordError::InvalidRepositoryOutcome),
            Self::RepositoryVerifiedYield(proof) => proof.validate_contract(event),
        }
    }
}

/// Closed proof predicates required before Core admits an asserted yield.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryVerifiedYieldProof {
    pub command_pre_head: Option<GitObjectId>,
    pub sequencer_pre_head: Option<GitObjectId>,
    pub exact_source_oids: Vec<GitObjectId>,
    pub command_post_head: GitObjectId,
    pub repository_geometry_before_sha256: [u8; 32],
    pub repository_geometry_after_sha256: [u8; 32],
    pub exact_result_map_sha256: [u8; 32],
    pub drift_excluded: bool,
    pub mutation_excluded: bool,
}

impl RepositoryVerifiedYieldProof {
    fn validate_contract(&self, event: &RepositoryCommitOperationEvent) -> CoreRecordResult<()> {
        if let Some(command_pre_head) = &self.command_pre_head {
            command_pre_head.validate_contract()?;
        }
        self.command_post_head.validate_contract()?;
        if let Some(sequencer_pre_head) = &self.sequencer_pre_head {
            sequencer_pre_head.validate_contract()?;
        }
        validate_canonical_object_set(&self.exact_source_oids)?;
        let mapped_sources = canonical_mapping_sources(&event.mappings);
        let mapped_results = canonical_mapping_results(&event.mappings);
        let object_format = event.mappings[0].source.format;
        if event.state != RepositoryCommitOperationState::Asserted
            || event.mapping_completeness != RepositoryCommitMappingCompleteness::Complete
            || !event.unlinked_sources.is_empty()
            || !event.unlinked_results.is_empty()
            || self.exact_source_oids != mapped_sources
            || !mapped_results.contains(&self.command_post_head)
            || self.exact_result_map_sha256 != repository_result_map_sha256(&event.mappings)
            || self.repository_geometry_before_sha256 == [0; 32]
            || self.repository_geometry_before_sha256 != self.repository_geometry_after_sha256
            || !self.drift_excluded
            || !self.mutation_excluded
            || self
                .command_pre_head
                .as_ref()
                .is_some_and(|object_id| object_id.format != object_format)
            || self
                .sequencer_pre_head
                .as_ref()
                .is_some_and(|object_id| object_id.format != object_format)
        {
            return Err(CoreRecordError::InvalidRepositoryOutcome);
        }
        match event.kind {
            RepositoryCommitOperationKind::Amend
                if event.mappings.len() == 1
                    && self.sequencer_pre_head.is_none()
                    && self.command_pre_head.as_ref() == Some(&event.mappings[0].source)
                    && event.mappings[0].result == self.command_post_head =>
            {
                Ok(())
            }
            RepositoryCommitOperationKind::Rebase
                if self.command_pre_head.is_some()
                    && self.sequencer_pre_head == self.command_pre_head
                    && self
                        .command_pre_head
                        .as_ref()
                        .is_some_and(|pre_head| mapped_sources.contains(pre_head)) =>
            {
                Ok(())
            }
            RepositoryCommitOperationKind::CherryPick
                if event.mappings.len() == 1
                    && ((self.command_pre_head.is_none() && self.sequencer_pre_head.is_none())
                        || (self.command_pre_head.is_some()
                            && self.sequencer_pre_head == self.command_pre_head)) =>
            {
                Ok(())
            }
            _ => Err(CoreRecordError::InvalidRepositoryOutcome),
        }
    }
}

fn validate_canonical_object_set(values: &[GitObjectId]) -> CoreRecordResult<()> {
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(CoreRecordError::InvalidRepositoryOutcome);
    }
    values.iter().try_for_each(GitObjectId::validate_contract)
}

fn canonical_mapping_sources(mappings: &[RepositoryCommitMapping]) -> Vec<GitObjectId> {
    let mut values = mappings
        .iter()
        .map(|mapping| mapping.source.clone())
        .collect::<Vec<_>>();
    values.sort();
    values.dedup();
    values
}

fn canonical_mapping_results(mappings: &[RepositoryCommitMapping]) -> Vec<GitObjectId> {
    let mut values = mappings
        .iter()
        .map(|mapping| mapping.result.clone())
        .collect::<Vec<_>>();
    values.sort();
    values.dedup();
    values
}

#[must_use]
pub fn repository_outcome_receipt_id(linkage: &RepositoryOutcomeLinkage) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ctx.repository.outcome-receipt.v1\0");
    update_identity_component(&mut digest, linkage.provider.as_bytes());
    update_identity_component(&mut digest, linkage.origin_call_id.as_bytes());
    update_identity_component(&mut digest, linkage.result_call_id.as_bytes());
    digest.update(linkage.origin_event_sequence.to_be_bytes());
    for continuation in &linkage.continuation_call_id_sha256 {
        digest.update(continuation);
    }
    digest.update(linkage.result_record_sha256);
    digest.finalize().into()
}

/// Derives one identity for one plural commit operation.
///
/// The v2 domain binds the stable Core source/event/session identities, the
/// canonical logical repository identity, object format, digest of the
/// complete canonically sorted mapping set, operation kind, and exact provider
/// linkage receipt. Checkout, worktree, and binding coordinates are
/// deliberately excluded.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn repository_commit_operation_event_id(
    source: &SourceKey,
    core_event_id: StableEntityId,
    core_session_id: StableEntityId,
    repository: &RepositoryBinding,
    object_format: GitObjectFormat,
    mappings: &[RepositoryCommitMapping],
    linkage: &RepositoryOutcomeLinkage,
    kind: RepositoryCommitOperationKind,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ctx.repository.commit-operation-event.v2\0");
    update_stable_identity_component(&mut digest, source.identity());
    update_stable_identity_component(&mut digest, core_event_id);
    update_stable_identity_component(&mut digest, core_session_id);
    update_identity_component(&mut digest, repository.logical_repository_id.as_bytes());
    digest.update([match object_format {
        GitObjectFormat::Sha1 => 1,
        GitObjectFormat::Sha256 => 2,
    }]);
    digest.update(repository_result_map_sha256(mappings));
    digest.update([match kind {
        RepositoryCommitOperationKind::Amend => 1,
        RepositoryCommitOperationKind::Rebase => 2,
        RepositoryCommitOperationKind::CherryPick => 3,
    }]);
    digest.update(repository_outcome_receipt_id(linkage));
    digest.finalize().into()
}

/// Digests the complete mapping set in canonical order, independent of input
/// order. Operation admission separately rejects duplicate or invalid maps.
#[must_use]
pub fn repository_result_map_sha256(mappings: &[RepositoryCommitMapping]) -> [u8; 32] {
    let mut mappings = mappings.to_vec();
    mappings.sort();
    let mut digest = Sha256::new();
    digest.update(b"ctx.repository.commit-result-map.v1\0");
    digest.update(
        u64::try_from(mappings.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    for mapping in mappings {
        digest.update([match mapping.source.format {
            GitObjectFormat::Sha1 => 1,
            GitObjectFormat::Sha256 => 2,
        }]);
        update_identity_component(&mut digest, mapping.source.hex.as_bytes());
        update_identity_component(&mut digest, mapping.result.hex.as_bytes());
    }
    digest.finalize().into()
}

fn update_identity_component(digest: &mut Sha256, value: &[u8]) {
    digest.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(value);
}

fn update_stable_identity_component(digest: &mut Sha256, identity: StableEntityId) {
    digest.update(identity.contract_version().to_be_bytes());
    digest.update([identity.entity_kind() as u8]);
    digest.update(identity.digest());
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryPullRequestIdentity {
    pub forge_repository: RepositoryAlias,
    pub number: u64,
    pub provider_id: Option<String>,
}

/// An inspected pull request's exact merge and membership association.
///
/// This observation never asserts that the enclosing session produced a
/// commit or performed the merge. Its contained object IDs are certified from
/// the exact two-parent merge object's `merge^1..merge^2` Git-DAG range.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryPullRequestAssociationObservation {
    pub pull_request: RepositoryPullRequestIdentity,
    pub merged_as: GitObjectId,
    pub contains_commits: Vec<GitObjectId>,
    pub linkage: RepositoryOutcomeLinkage,
    pub association_capture_revision: u32,
}

impl RepositoryPullRequestAssociationObservation {
    pub fn validate_contract(&self) -> CoreRecordResult<()> {
        if self.association_capture_revision
            != CORE_REPOSITORY_PULL_REQUEST_ASSOCIATION_CAPTURE_REVISION
        {
            return Err(CoreRecordError::InvalidRepositoryRevisions);
        }
        self.pull_request
            .validate_contract()
            .map_err(|_| CoreRecordError::InvalidRepositoryPullRequestAssociation)?;
        self.merged_as
            .validate_contract()
            .map_err(|_| CoreRecordError::InvalidRepositoryPullRequestAssociation)?;
        validate_count(
            "repository_pull_request_contains_commits",
            self.contains_commits.len(),
            MAX_REPOSITORY_ITEMS,
        )?;
        if self
            .contains_commits
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        {
            return Err(CoreRecordError::InvalidRepositoryPullRequestAssociation);
        }
        for object_id in &self.contains_commits {
            object_id
                .validate_contract()
                .map_err(|_| CoreRecordError::InvalidRepositoryPullRequestAssociation)?;
            if object_id.format != self.merged_as.format || object_id == &self.merged_as {
                return Err(CoreRecordError::InvalidRepositoryPullRequestAssociation);
            }
        }
        self.linkage
            .validate_contract()
            .map_err(|_| CoreRecordError::InvalidRepositoryPullRequestAssociation)
    }

    pub fn object_ids(&self) -> impl Iterator<Item = &GitObjectId> {
        std::iter::once(&self.merged_as).chain(self.contains_commits.iter())
    }
}

impl RepositoryPullRequestIdentity {
    fn validate_contract(&self) -> CoreRecordResult<()> {
        self.forge_repository.validate_contract()?;
        if self.forge_repository.kind != RepositoryAliasKind::Forge
            || self.forge_repository.remote_name.is_some()
            || self.number == 0
        {
            return Err(CoreRecordError::InvalidRepositoryOutcome);
        }
        validate_optional_text(
            "repository_pull_request_provider_id",
            self.provider_id.as_deref(),
            MAX_TEXT_METADATA_BYTES,
        )
    }
}

fn repository_alias_identity_matches(left: &RepositoryAlias, right: &RepositoryAlias) -> bool {
    left.host.eq_ignore_ascii_case(&right.host)
        && left.namespace == right.namespace
        && left.name == right.name
}

fn forge_logical_identity_matches(logical: &str, repository: &RepositoryAlias) -> bool {
    let Some((host, path)) = logical.split_once('/') else {
        return false;
    };
    let mut expected_path = repository.namespace.join("/");
    expected_path.push('/');
    expected_path.push_str(&repository.name);
    host.eq_ignore_ascii_case(&repository.host) && path == expected_path
}

/// Bounded native linkage proving which structured result belongs to which
/// command. Output bodies are represented only by the exact record digest.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryOutcomeLinkage {
    pub provider: String,
    pub origin_call_id: String,
    pub result_call_id: String,
    pub origin_event_sequence: u64,
    pub continuation_call_id_sha256: Vec<[u8; 32]>,
    pub result_record_sha256: [u8; 32],
}

impl RepositoryOutcomeLinkage {
    fn validate_contract(&self) -> CoreRecordResult<()> {
        validate_text(
            "repository_outcome_provider",
            &self.provider,
            MAX_TEXT_METADATA_BYTES,
        )?;
        validate_text(
            "repository_outcome_origin_call_id",
            &self.origin_call_id,
            MAX_TEXT_METADATA_BYTES,
        )?;
        validate_text(
            "repository_outcome_result_call_id",
            &self.result_call_id,
            MAX_TEXT_METADATA_BYTES,
        )?;
        validate_count(
            "repository_outcome_continuation_ids",
            self.continuation_call_id_sha256.len(),
            MAX_OUTCOME_LINKAGE_ITEMS,
        )?;
        if self.result_record_sha256 == [0; 32]
            || self.continuation_call_id_sha256.contains(&[0; 32])
            || self
                .continuation_call_id_sha256
                .iter()
                .collect::<HashSet<_>>()
                .len()
                != self.continuation_call_id_sha256.len()
        {
            return Err(CoreRecordError::InvalidRepositoryOutcome);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitObjectId {
    pub format: GitObjectFormat,
    pub hex: String,
}

impl GitObjectId {
    pub fn validate_contract(&self) -> CoreRecordResult<()> {
        let expected = match self.format {
            GitObjectFormat::Sha1 => 40,
            GitObjectFormat::Sha256 => 64,
        };
        if self.hex.len() != expected
            || !self
                .hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(CoreRecordError::InvalidGitObjectId);
        }
        Ok(())
    }
}
