//! Repository interpretation and certification evidence.
//!
//! These derived values are separate from immutable Core records.

use std::{cmp::Ordering, collections::HashSet};

pub use ctx_attribution_model::{GitObjectFormat, RepositoryFileInvocationKind};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MISSING_ACTIVITY_TIME_UNIX_MS: i64 = i64::MIN;
pub const REPOSITORY_OBSERVATION_REVISION: u32 = 5;
// Revision 5 admits only `ctx-build-governor exec -- <literal command>` as
// transparent transport for a bounded outcome plan. All other governor forms
// and dynamic forwarded arguments remain non-authoritative.
pub const BOUNDED_SHELL_SUBSET_REVISION: u32 = 6;
pub const REPOSITORY_ASSOCIATION_POLICY_REVISION: u32 = 6;
pub const PULL_REQUEST_ASSOCIATION_CAPTURE_REVISION: u32 = 3;
pub const OUTCOME_CAPTURE_REVISION: u32 = 5;
pub const LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_REVISION: u32 = 1;
pub const LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_DOMAIN: &[u8] =
    b"ctx.repository.local-root-authorization.v1\0";
pub const MAX_COMMIT_OPERATION_MAPPINGS: usize = 32;

// Established aliases for consumers of the repository evidence contract.
pub const CORE_REPOSITORY_OBSERVATION_REVISION: u32 = REPOSITORY_OBSERVATION_REVISION;
pub const CORE_BOUNDED_SHELL_SUBSET_REVISION: u32 = BOUNDED_SHELL_SUBSET_REVISION;
pub const CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION: u32 = REPOSITORY_ASSOCIATION_POLICY_REVISION;
pub const CORE_REPOSITORY_PULL_REQUEST_ASSOCIATION_CAPTURE_REVISION: u32 =
    PULL_REQUEST_ASSOCIATION_CAPTURE_REVISION;
pub const CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_REVISION: u32 =
    LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_REVISION;
pub const CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_DOMAIN: &[u8] =
    LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_DOMAIN;
pub const CORE_MISSING_ACTIVITY_TIME_UNIX_MS: i64 = MISSING_ACTIVITY_TIME_UNIX_MS;

/// The result of interpreting one bounded collection of neutral facts.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryEvaluation {
    pub repository_candidate_evidence: RepositoryCandidateEvidence,
    pub repository_bindings: Vec<RepositoryBinding>,
    pub repository_abstentions: Vec<RepositoryAbstention>,
    pub repository_file_invocation_evidence: Vec<RepositoryFileInvocationEvidence>,
    pub repository_file_observations: Vec<RepositoryFileObservation>,
    pub repository_vcs_observations: Vec<RepositoryVcsObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GitObjectId {
    pub format: GitObjectFormat,
    pub hex: String,
}

impl GitObjectId {
    pub fn validate_contract(&self) -> Result<(), RepositoryModelError> {
        let length = match self.format {
            GitObjectFormat::Sha1 => 40,
            GitObjectFormat::Sha256 => 64,
        };
        (self.hex.len() == length
            && self
                .hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        .then_some(())
        .ok_or(RepositoryModelError::InvalidGitObjectId)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RepositoryAlias {
    pub kind: RepositoryAliasKind,
    pub host: String,
    pub namespace: Vec<String>,
    pub name: String,
    pub remote_name: Option<String>,
}

impl RepositoryAlias {
    pub fn validate_contract(&self) -> Result<(), RepositoryModelError> {
        (!self.host.is_empty()
            && !self.namespace.is_empty()
            && !self.name.is_empty()
            && self
                .namespace
                .iter()
                .chain(std::iter::once(&self.name))
                .all(|part| {
                    !part.is_empty()
                        && !matches!(part.as_str(), "." | "..")
                        && !part
                            .bytes()
                            .any(|byte| byte.is_ascii_control() || matches!(byte, b'/' | b'\\'))
                }))
        .then_some(())
        .ok_or(RepositoryModelError::InvalidRepositoryAlias)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryAliasKind {
    Forge,
    Remote,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryLocalRootAuthorization {
    pub local_root: String,
    pub local_root_authorization_fingerprint_revision: u32,
    pub local_root_authorization_fingerprint: [u8; 32],
    pub observed_at_unix_ms: i64,
}

impl RepositoryLocalRootAuthorization {
    pub fn provider_activity_order(&self, other: &Self) -> Option<Ordering> {
        (self.observed_at_unix_ms != MISSING_ACTIVITY_TIME_UNIX_MS
            && other.observed_at_unix_ms != MISSING_ACTIVITY_TIME_UNIX_MS
            && self.observed_at_unix_ms != other.observed_at_unix_ms)
            .then(|| self.observed_at_unix_ms.cmp(&other.observed_at_unix_ms))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    pub fn validate_contract(&self) -> Result<(), RepositoryModelError> {
        (!self.binding_id.is_empty()
            && !self.logical_repository_id.is_empty()
            && !self.evidence.is_empty()
            && self.association_policy_revision == REPOSITORY_ASSOCIATION_POLICY_REVISION
            && self
                .aliases
                .iter()
                .all(|alias| alias.validate_contract().is_ok()))
        .then_some(())
        .ok_or(RepositoryModelError::InvalidRepositoryBinding)
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryEvidence {
    pub kind: RepositoryEvidenceKind,
    pub confidence: RepositoryEvidenceConfidence,
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryEvidenceConfidence {
    Explicit,
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
            repository_observation_revision: REPOSITORY_OBSERVATION_REVISION,
            bounded_shell_subset_revision: BOUNDED_SHELL_SUBSET_REVISION,
            association_policy_revision: REPOSITORY_ASSOCIATION_POLICY_REVISION,
            outcome_capture_revision: OUTCOME_CAPTURE_REVISION,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryAbstention {
    pub evidence_kind: RepositoryEvidenceKind,
    pub reason: RepositoryAbstentionReason,
    pub detail: Option<String>,
    pub association_policy_revision: u32,
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RepositoryFileInvocationEvidence {
    pub operation_ordinal: u32,
    pub repository_binding_id: String,
    pub relative_path: String,
    pub prior_relative_path: Option<String>,
    pub kind: RepositoryFileInvocationKind,
    pub tool_name: Option<String>,
    pub normalized_text_range: Option<RepositoryFileInvocationTextRange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RepositoryFileInvocationTextRange {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryFileObservation {
    pub repository_binding_id: String,
    pub relative_path: String,
    pub kind: RepositoryFileObservationKind,
    pub prior_relative_path: Option<String>,
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
pub struct RepositoryVcsObservation {
    pub repository_binding_id: String,
    pub kind: RepositoryVcsObservationKind,
    pub object_id: Option<GitObjectId>,
    pub parent_object_ids: Vec<GitObjectId>,
    pub reference: Option<String>,
    pub relative_path: Option<String>,
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

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RepositoryOutcomeObservation {
    pub kind: RepositoryOutcomeKind,
    pub produced_object_ids: Vec<GitObjectId>,
    pub commit_operation: Option<RepositoryCommitOperationEvent>,
    pub pull_request: Option<RepositoryPullRequestIdentity>,
    pub pull_request_merge_commit: Option<GitObjectId>,
    pub observed_at_unix_ms: i64,
    pub linkage: RepositoryOutcomeLinkage,
    pub outcome_capture_revision: u32,
}

impl RepositoryOutcomeObservation {
    pub fn validate_contract(&self) -> Result<(), RepositoryModelError> {
        if self.outcome_capture_revision != OUTCOME_CAPTURE_REVISION
            || self.produced_object_ids.len() > 256
            || !canonical_object_set(&self.produced_object_ids)
            || self
                .commit_operation
                .as_ref()
                .is_some_and(|operation| operation.validate_contract(&self.linkage).is_err())
            || self
                .pull_request
                .as_ref()
                .is_some_and(|pull_request| pull_request.validate_contract().is_err())
            || self
                .pull_request_merge_commit
                .as_ref()
                .is_some_and(|object_id| object_id.validate_contract().is_err())
            || self.linkage.validate_contract().is_err()
        {
            return Err(RepositoryModelError::InvalidRepositoryOutcome);
        }
        match self.kind {
            RepositoryOutcomeKind::Commit
                if self.pull_request.is_none()
                    && self.pull_request_merge_commit.is_none()
                    && ((!self.produced_object_ids.is_empty()
                        && self.commit_operation.is_none())
                        || (self.produced_object_ids.is_empty()
                            && self.commit_operation.is_some())) =>
            {
                Ok(())
            }
            RepositoryOutcomeKind::PullRequestCreated
                if self.produced_object_ids.is_empty()
                    && self.commit_operation.is_none()
                    && self.pull_request.is_some()
                    && self.pull_request_merge_commit.is_none() =>
            {
                Ok(())
            }
            RepositoryOutcomeKind::PullRequestMerged
                if self.produced_object_ids.is_empty()
                    && self.commit_operation.is_none()
                    && self.pull_request.is_some()
                    && self.pull_request_merge_commit.is_some() =>
            {
                Ok(())
            }
            _ => Err(RepositoryModelError::InvalidRepositoryOutcome),
        }
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
pub struct RepositoryCommitMapping {
    pub source: GitObjectId,
    pub result: GitObjectId,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    #[allow(clippy::too_many_arguments)]
    pub fn repository_verified_yield(
        linkage: &RepositoryOutcomeLinkage,
        kind: RepositoryCommitOperationKind,
        mut mappings: Vec<RepositoryCommitMapping>,
        command_pre_head: Option<GitObjectId>,
        sequencer_pre_head: Option<GitObjectId>,
        command_post_head: GitObjectId,
        repository_object_domain_sha256: [u8; 32],
    ) -> Result<Self, RepositoryModelError> {
        mappings.sort();
        if mappings.is_empty()
            || mappings.len() > MAX_COMMIT_OPERATION_MAPPINGS
            || mappings.iter().any(|mapping| {
                mapping.source.validate_contract().is_err()
                    || mapping.result.validate_contract().is_err()
                    || mapping.source == mapping.result
                    || mapping.source.format != mapping.result.format
            })
            || mappings.windows(2).any(|pair| pair[0] == pair[1])
            || repository_object_domain_sha256 == [0; 32]
            || command_post_head.validate_contract().is_err()
            || !mappings
                .iter()
                .any(|mapping| mapping.result == command_post_head)
            || !verified_yield_shape_is_exact(
                kind,
                &mappings,
                command_pre_head.as_ref(),
                sequencer_pre_head.as_ref(),
                &command_post_head,
            )
        {
            return Err(RepositoryModelError::InvalidRepositoryOutcome);
        }
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
        Ok(Self {
            event_id: unscoped_operation_id(linkage, kind),
            receipt_id: repository_outcome_receipt_id(linkage),
            kind,
            mappings,
            unlinked_sources: Vec::new(),
            unlinked_results: Vec::new(),
            mapping_completeness: RepositoryCommitMappingCompleteness::Complete,
            state: RepositoryCommitOperationState::Asserted,
            proof: RepositoryCommitOperationProof::RepositoryVerifiedYield(Box::new(proof)),
        })
    }
    pub fn record_exact_unlinked(
        linkage: &RepositoryOutcomeLinkage,
        kind: RepositoryCommitOperationKind,
        mut unlinked_sources: Vec<GitObjectId>,
        mut unlinked_results: Vec<GitObjectId>,
        state: RepositoryCommitOperationState,
    ) -> Result<Self, RepositoryModelError> {
        unlinked_sources.sort();
        unlinked_sources.dedup();
        unlinked_results.sort();
        unlinked_results.dedup();
        if state == RepositoryCommitOperationState::Asserted
            || (unlinked_sources.is_empty() && unlinked_results.is_empty())
            || unlinked_sources
                .iter()
                .chain(&unlinked_results)
                .any(|object_id| object_id.validate_contract().is_err())
        {
            return Err(RepositoryModelError::InvalidRepositoryOutcome);
        }
        Ok(Self {
            event_id: unscoped_operation_id(linkage, kind),
            receipt_id: repository_outcome_receipt_id(linkage),
            kind,
            mappings: Vec::new(),
            unlinked_sources,
            unlinked_results,
            mapping_completeness: RepositoryCommitMappingCompleteness::None,
            state,
            proof: RepositoryCommitOperationProof::RecordExact,
        })
    }
    pub fn object_ids(&self) -> impl Iterator<Item = &GitObjectId> {
        self.mappings
            .iter()
            .flat_map(|mapping| [&mapping.source, &mapping.result])
            .chain(self.unlinked_sources.iter())
            .chain(self.unlinked_results.iter())
    }
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

    fn validate_contract(
        &self,
        linkage: &RepositoryOutcomeLinkage,
    ) -> Result<(), RepositoryModelError> {
        if self.mappings.len() > MAX_COMMIT_OPERATION_MAPPINGS
            || self.unlinked_sources.len() > 256
            || self.unlinked_results.len() > 256
            || self.event_id == [0; 32]
            || self.receipt_id != repository_outcome_receipt_id(linkage)
            || self.mappings.windows(2).any(|pair| pair[0] >= pair[1])
            || !canonical_object_set(&self.unlinked_sources)
            || !canonical_object_set(&self.unlinked_results)
        {
            return Err(RepositoryModelError::InvalidRepositoryOutcome);
        }
        let mut sources = HashSet::new();
        let mut results = HashSet::new();
        for mapping in &self.mappings {
            if mapping.source.validate_contract().is_err()
                || mapping.result.validate_contract().is_err()
                || mapping.source == mapping.result
                || mapping.source.format != mapping.result.format
                || !sources.insert(&mapping.source)
                || !results.insert(&mapping.result)
            {
                return Err(RepositoryModelError::InvalidRepositoryOutcome);
            }
        }
        if self
            .unlinked_sources
            .iter()
            .any(|value| sources.contains(value))
            || self
                .unlinked_results
                .iter()
                .any(|value| results.contains(value))
        {
            return Err(RepositoryModelError::InvalidRepositoryOutcome);
        }
        let format = self.object_ids().next().map(|value| value.format);
        if format.is_none() || self.object_ids().any(|value| Some(value.format) != format) {
            return Err(RepositoryModelError::InvalidRepositoryOutcome);
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
            _ => return Err(RepositoryModelError::InvalidRepositoryOutcome),
        }
        self.proof.validate_contract(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryCommitOperationKind {
    Amend,
    Rebase,
    CherryPick,
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
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RepositoryCommitOperationProof {
    RecordExact,
    RepositoryVerifiedYield(Box<RepositoryVerifiedYieldProof>),
}
impl RepositoryCommitOperationProof {
    fn validate_contract(
        &self,
        event: &RepositoryCommitOperationEvent,
    ) -> Result<(), RepositoryModelError> {
        match self {
            Self::RecordExact if event.state != RepositoryCommitOperationState::Asserted => Ok(()),
            Self::RecordExact => Err(RepositoryModelError::InvalidRepositoryOutcome),
            Self::RepositoryVerifiedYield(proof) => proof.validate_contract(event),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    fn validate_contract(
        &self,
        event: &RepositoryCommitOperationEvent,
    ) -> Result<(), RepositoryModelError> {
        if self
            .command_pre_head
            .as_ref()
            .is_some_and(|value| value.validate_contract().is_err())
            || self
                .sequencer_pre_head
                .as_ref()
                .is_some_and(|value| value.validate_contract().is_err())
            || self.command_post_head.validate_contract().is_err()
            || !canonical_object_set(&self.exact_source_oids)
            || event.mappings.is_empty()
        {
            return Err(RepositoryModelError::InvalidRepositoryOutcome);
        }
        let mapped_sources = canonical_mapping_sources(&event.mappings);
        let mut mapped_results = event
            .mappings
            .iter()
            .map(|mapping| mapping.result.clone())
            .collect::<Vec<_>>();
        mapped_results.sort();
        mapped_results.dedup();
        let format = event.mappings[0].source.format;
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
                .is_some_and(|value| value.format != format)
            || self
                .sequencer_pre_head
                .as_ref()
                .is_some_and(|value| value.format != format)
            || !verified_yield_shape_is_exact(
                event.kind,
                &event.mappings,
                self.command_pre_head.as_ref(),
                self.sequencer_pre_head.as_ref(),
                &self.command_post_head,
            )
        {
            return Err(RepositoryModelError::InvalidRepositoryOutcome);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RepositoryPullRequestIdentity {
    pub forge_repository: RepositoryAlias,
    pub number: u64,
    pub provider_id: Option<String>,
}
impl RepositoryPullRequestIdentity {
    fn validate_contract(&self) -> Result<(), RepositoryModelError> {
        if self.forge_repository.validate_contract().is_err()
            || self.forge_repository.kind != RepositoryAliasKind::Forge
            || self.forge_repository.remote_name.is_some()
            || self.number == 0
        {
            return Err(RepositoryModelError::InvalidRepositoryOutcome);
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RepositoryPullRequestAssociationObservation {
    pub pull_request: RepositoryPullRequestIdentity,
    pub merged_as: GitObjectId,
    pub contains_commits: Vec<GitObjectId>,
    pub linkage: RepositoryOutcomeLinkage,
    pub association_capture_revision: u32,
}
impl RepositoryPullRequestAssociationObservation {
    pub fn validate_contract(&self) -> Result<(), RepositoryModelError> {
        if self.association_capture_revision != PULL_REQUEST_ASSOCIATION_CAPTURE_REVISION
            || self.pull_request.validate_contract().is_err()
            || self.merged_as.validate_contract().is_err()
            || self.contains_commits.len() > 256
            || !canonical_object_set(&self.contains_commits)
            || self
                .contains_commits
                .iter()
                .any(|value| value.format != self.merged_as.format || value == &self.merged_as)
            || self.linkage.validate_contract().is_err()
        {
            return Err(RepositoryModelError::InvalidRepositoryOutcome);
        }
        Ok(())
    }

    pub fn object_ids(&self) -> impl Iterator<Item = &GitObjectId> {
        std::iter::once(&self.merged_as).chain(self.contains_commits.iter())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RepositoryOutcomeLinkage {
    pub provider: String,
    pub origin_call_id: String,
    pub result_call_id: String,
    pub origin_event_sequence: u64,
    pub continuation_call_id_sha256: Vec<[u8; 32]>,
    pub result_record_sha256: [u8; 32],
}
impl RepositoryOutcomeLinkage {
    fn validate_contract(&self) -> Result<(), RepositoryModelError> {
        if self.provider.is_empty()
            || self.origin_call_id.is_empty()
            || self.result_call_id.is_empty()
            || self.continuation_call_id_sha256.len() > 256
            || self.result_record_sha256 == [0; 32]
            || self.continuation_call_id_sha256.contains(&[0; 32])
            || self
                .continuation_call_id_sha256
                .iter()
                .collect::<HashSet<_>>()
                .len()
                != self.continuation_call_id_sha256.len()
        {
            return Err(RepositoryModelError::InvalidRepositoryOutcome);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepositoryModelError {
    InvalidGitObjectId,
    InvalidRepositoryAlias,
    InvalidRepositoryBinding,
    InvalidRepositoryOutcome,
}

pub fn repository_outcome_receipt_id(linkage: &RepositoryOutcomeLinkage) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ctx.repository.outcome-receipt.v1\0");
    update_component(&mut digest, linkage.provider.as_bytes());
    update_component(&mut digest, linkage.origin_call_id.as_bytes());
    update_component(&mut digest, linkage.result_call_id.as_bytes());
    digest.update(linkage.origin_event_sequence.to_be_bytes());
    for continuation in &linkage.continuation_call_id_sha256 {
        digest.update(continuation);
    }
    digest.update(linkage.result_record_sha256);
    digest.finalize().into()
}

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
        update_component(&mut digest, mapping.source.hex.as_bytes());
        update_component(&mut digest, mapping.result.hex.as_bytes());
    }
    digest.finalize().into()
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

fn canonical_object_set(values: &[GitObjectId]) -> bool {
    !values.windows(2).any(|pair| pair[0] >= pair[1])
        && values.iter().all(|value| value.validate_contract().is_ok())
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

fn verified_yield_shape_is_exact(
    kind: RepositoryCommitOperationKind,
    mappings: &[RepositoryCommitMapping],
    command_pre_head: Option<&GitObjectId>,
    sequencer_pre_head: Option<&GitObjectId>,
    command_post_head: &GitObjectId,
) -> bool {
    match kind {
        RepositoryCommitOperationKind::Amend => {
            mappings.len() == 1
                && sequencer_pre_head.is_none()
                && command_pre_head == Some(&mappings[0].source)
                && mappings[0].result == *command_post_head
        }
        RepositoryCommitOperationKind::Rebase => {
            command_pre_head.is_some()
                && sequencer_pre_head == command_pre_head
                && mappings
                    .iter()
                    .any(|mapping| Some(&mapping.source) == command_pre_head)
        }
        RepositoryCommitOperationKind::CherryPick => {
            mappings.len() == 1
                && ((command_pre_head.is_none() && sequencer_pre_head.is_none())
                    || (command_pre_head.is_some() && sequencer_pre_head == command_pre_head))
        }
    }
}

fn unscoped_operation_id(
    linkage: &RepositoryOutcomeLinkage,
    kind: RepositoryCommitOperationKind,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ctx.repository.commit-operation-event.unscoped.v1\0");
    digest.update(repository_outcome_receipt_id(linkage));
    digest.update([match kind {
        RepositoryCommitOperationKind::Amend => 1,
        RepositoryCommitOperationKind::Rebase => 2,
        RepositoryCommitOperationKind::CherryPick => 3,
    }]);
    digest.finalize().into()
}
fn update_component(digest: &mut Sha256, value: &[u8]) {
    digest.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(value);
}
