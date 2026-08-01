use std::{cmp::Ordering, collections::HashSet};

use serde::{Deserialize, Serialize};

use super::{
    validation::{
        validate_count, validate_optional_text, validate_repository_alias_component,
        validate_repository_relative_path, validate_text,
    },
    CoreRecordError, CoreRecordResult, CORE_BOUNDED_SHELL_SUBSET_REVISION,
    CORE_MISSING_ACTIVITY_TIME_UNIX_MS,
    CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_REVISION,
    CORE_REPOSITORY_OBSERVATION_REVISION, CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
    MAX_GIT_REF_BYTES, MAX_OUTCOME_LINKAGE_ITEMS, MAX_REPOSITORY_ALIASES, MAX_REPOSITORY_EVIDENCE,
    MAX_REPOSITORY_ITEMS, MAX_REPOSITORY_NAMESPACE_PARTS, MAX_REPOSITORY_RELATIVE_PATH_BYTES,
    MAX_TEXT_METADATA_BYTES,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
        if self.evidence.is_empty() || self.association_policy_revision == 0 {
            return Err(CoreRecordError::EmptyField {
                field: "repository_evidence",
            });
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
}

/// Credential-free logical forge or configured-remote identity.
///
/// The structured shape intentionally has no URL, userinfo, token, or
/// credential-bearing field.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    pub outcome_capture_revision: u32,
    pub session_cwd: Option<String>,
    pub declared_tool_workdir: Option<String>,
    pub derived_effective_cwd: Option<String>,
    pub command_specific_repository_path: Option<String>,
    pub outcome_operation_repository_path: Option<String>,
    pub outcome_output_repository_path: Option<String>,
}

impl Default for RepositoryCandidateEvidence {
    fn default() -> Self {
        Self {
            repository_observation_revision: CORE_REPOSITORY_OBSERVATION_REVISION,
            bounded_shell_subset_revision: CORE_BOUNDED_SHELL_SUBSET_REVISION,
            outcome_capture_revision: CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
            session_cwd: None,
            declared_tool_workdir: None,
            derived_effective_cwd: None,
            command_specific_repository_path: None,
            outcome_operation_repository_path: None,
            outcome_output_repository_path: None,
        }
    }
}

impl RepositoryCandidateEvidence {
    pub fn validate_contract(&self) -> CoreRecordResult<()> {
        if self.repository_observation_revision != CORE_REPOSITORY_OBSERVATION_REVISION
            || self.bounded_shell_subset_revision != CORE_BOUNDED_SHELL_SUBSET_REVISION
            || self.outcome_capture_revision != CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION
        {
            return Err(CoreRecordError::InvalidRepositoryRevisions);
        }
        validate_optional_text(
            "repository_session_cwd",
            self.session_cwd.as_deref(),
            MAX_REPOSITORY_RELATIVE_PATH_BYTES,
        )?;
        validate_optional_text(
            "repository_declared_tool_workdir",
            self.declared_tool_workdir.as_deref(),
            MAX_REPOSITORY_RELATIVE_PATH_BYTES,
        )?;
        validate_optional_text(
            "repository_derived_effective_cwd",
            self.derived_effective_cwd.as_deref(),
            MAX_REPOSITORY_RELATIVE_PATH_BYTES,
        )?;
        validate_optional_text(
            "repository_command_specific_path",
            self.command_specific_repository_path.as_deref(),
            MAX_REPOSITORY_RELATIVE_PATH_BYTES,
        )?;
        validate_optional_text(
            "repository_outcome_operation_path",
            self.outcome_operation_repository_path.as_deref(),
            MAX_REPOSITORY_RELATIVE_PATH_BYTES,
        )?;
        validate_optional_text(
            "repository_outcome_output_path",
            self.outcome_output_repository_path.as_deref(),
            MAX_REPOSITORY_RELATIVE_PATH_BYTES,
        )
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
        if self.association_policy_revision == 0 {
            return Err(CoreRecordError::EmptyField {
                field: "association_policy_revision",
            });
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
        if let RepositoryVcsObservationKind::Outcome(outcome) = &self.kind {
            if self.object_id.is_some()
                || !self.parent_object_ids.is_empty()
                || self.reference.is_some()
                || self.relative_path.is_some()
            {
                return Err(CoreRecordError::InvalidRepositoryOutcome);
            }
            outcome.validate_contract()?;
        }
        Ok(())
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
    pub replacement_lineage: Vec<RepositoryObjectReplacement>,
    pub pull_request: Option<RepositoryPullRequestIdentity>,
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
        validate_count(
            "repository_outcome_replacement_lineage",
            self.replacement_lineage.len(),
            MAX_REPOSITORY_ITEMS,
        )?;
        let mut produced = HashSet::new();
        for object_id in &self.produced_object_ids {
            object_id.validate_contract()?;
            if !produced.insert((object_id.format, object_id.hex.as_str())) {
                return Err(CoreRecordError::InvalidRepositoryOutcome);
            }
        }
        let mut replacements = HashSet::new();
        let mut replaced_ids = HashSet::new();
        let mut replacement_ids = HashSet::new();
        for replacement in &self.replacement_lineage {
            replacement.validate_contract()?;
            if !produced.contains(&(
                replacement.replacement.format,
                replacement.replacement.hex.as_str(),
            )) || !replaced_ids.insert(replacement.replaced.clone())
                || !replacement_ids.insert(replacement.replacement.clone())
                || !replacements.insert((
                    replacement.replaced.clone(),
                    replacement.replacement.clone(),
                ))
            {
                return Err(CoreRecordError::InvalidRepositoryOutcome);
            }
        }
        for start in &replaced_ids {
            let mut visited = HashSet::new();
            let mut current = start;
            while let Some(next) = self
                .replacement_lineage
                .iter()
                .find(|replacement| &replacement.replaced == current)
                .map(|replacement| &replacement.replacement)
            {
                if !visited.insert(current) || next == start {
                    return Err(CoreRecordError::InvalidRepositoryOutcome);
                }
                current = next;
            }
        }
        if let Some(pull_request) = &self.pull_request {
            pull_request.validate_contract()?;
        }
        match self.kind {
            RepositoryOutcomeKind::Commit
                if !self.produced_object_ids.is_empty() && self.pull_request.is_none() => {}
            RepositoryOutcomeKind::PullRequestCreated
                if self.produced_object_ids.is_empty()
                    && self.replacement_lineage.is_empty()
                    && self.pull_request.is_some() => {}
            RepositoryOutcomeKind::PullRequestMerged
                if self.produced_object_ids.len() == 1
                    && self.replacement_lineage.is_empty()
                    && self.pull_request.is_some() => {}
            _ => return Err(CoreRecordError::InvalidRepositoryOutcome),
        }
        self.linkage.validate_contract()
    }

    pub fn object_ids(&self) -> impl Iterator<Item = &GitObjectId> {
        self.produced_object_ids.iter().chain(
            self.replacement_lineage
                .iter()
                .flat_map(|replacement| [&replacement.replaced, &replacement.replacement]),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryOutcomeKind {
    Commit,
    PullRequestCreated,
    PullRequestMerged,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryObjectReplacement {
    pub replaced: GitObjectId,
    pub replacement: GitObjectId,
}

impl RepositoryObjectReplacement {
    fn validate_contract(&self) -> CoreRecordResult<()> {
        self.replaced.validate_contract()?;
        self.replacement.validate_contract()?;
        if self.replaced == self.replacement || self.replaced.format != self.replacement.format {
            return Err(CoreRecordError::InvalidRepositoryOutcome);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryPullRequestIdentity {
    pub forge_repository: RepositoryAlias,
    pub number: u64,
    pub provider_id: Option<String>,
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

/// Bounded native linkage proving which structured result belongs to which
/// command. Output bodies are represented only by the exact record digest.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
