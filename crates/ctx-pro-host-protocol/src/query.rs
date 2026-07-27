use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize};

use super::{
    ErrorClass, EvidenceCitation, JournalCheckpoint, ProtocolError, ResourceKind, ResourceRef,
    MAX_BLAME_ATTRIBUTIONS_PER_MATCH, MAX_BLAME_CURSOR_BYTES, MAX_BLAME_EVIDENCE,
    MAX_BLAME_RESULTS, MAX_BLAME_TARGET_BYTES, MAX_CITATIONS_PER_FACT,
};

/// Exact journal checkpoint that a cited blame request requires the derived graph to match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuerySnapshotExpectation {
    pub checkpoint: JournalCheckpoint,
    pub projection_pending: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LineRange {
    pub start: u32,
    pub end: u32,
}

impl LineRange {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.start == 0 || self.end < self.start {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "line range must be positive and inclusive",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BlameTarget {
    File {
        path: String,
        repository: Option<String>,
        lines: Option<LineRange>,
    },
    Commit {
        oid: String,
        repository: Option<String>,
    },
    PullRequest {
        selector: String,
        repository: Option<String>,
    },
}

impl BlameTarget {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        let (value, repository) = match self {
            Self::File {
                path,
                repository,
                lines,
            } => {
                if let Some(lines) = lines {
                    lines.validate()?;
                }
                (path, repository)
            }
            Self::Commit { oid, repository } => (oid, repository),
            Self::PullRequest {
                selector,
                repository,
            } => {
                match pull_request_selector_kind(selector) {
                    Some(PullRequestSelectorKind::Number) if repository.is_none() => {
                        return Err(ProtocolError::new(
                            ErrorClass::InvalidRequest,
                            "pull request number requires a repository selector",
                        ));
                    }
                    Some(
                        PullRequestSelectorKind::Number | PullRequestSelectorKind::CanonicalUrl,
                    ) => {}
                    None => {
                        return Err(ProtocolError::new(
                            ErrorClass::InvalidRequest,
                            "pull request selector must be a positive decimal number or canonical supported PR URL",
                        ));
                    }
                }
                (selector, repository)
            }
        };
        validate_bounded_text(value, "blame target")?;
        if let Some(repository) = repository {
            validate_bounded_text(repository, "repository selector")?;
        }
        Ok(())
    }

    #[must_use]
    pub const fn requires_git_read(&self) -> bool {
        matches!(self, Self::File { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlameRequest {
    pub target: BlameTarget,
    pub limit: u32,
    pub cursor: Option<String>,
    pub expected_snapshot: QuerySnapshotExpectation,
}

impl BlameRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.expected_snapshot.checkpoint.validate()?;
        if self.limit == 0 || self.limit > MAX_BLAME_RESULTS {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                format!("blame limit must be between 1 and {MAX_BLAME_RESULTS}"),
            ));
        }
        validate_cursor(self.cursor.as_deref())?;
        self.target.validate()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationReason {
    MoreMatches,
    MoreCommittedLines,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlameContinuation {
    pub cursor: String,
    pub reason: ContinuationReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResolvedBlameTarget {
    File {
        path: String,
        repository: ResourceRef,
        requested_lines: Option<LineRange>,
    },
    Commit {
        commit: ResourceRef,
        repository: ResourceRef,
    },
    PullRequest {
        selector: String,
        pull_request: ResourceRef,
        repository: ResourceRef,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeStatus {
    Clean,
    Differs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitSnapshot {
    pub head_oid: String,
    pub worktree_status: WorktreeStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactConfidence {
    Explicit,
    High,
    Medium,
    Low,
    Ambiguous,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactState {
    Asserted,
    Ambiguous,
    Contradicted,
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProductionRelationship {
    ProducedBy,
    PossiblyProducedBy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentAttribution {
    pub id: String,
    pub relationship: ProductionRelationship,
    pub producing_session: ResourceRef,
    pub direct_actor: Option<ResourceRef>,
    pub owning_root: Option<ResourceRef>,
    pub confidence: FactConfidence,
    pub state: FactState,
    pub evidence_numbers: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileBlameMatch {
    pub id: String,
    pub lines: LineRange,
    pub commit: ResourceRef,
    pub line_evidence_numbers: Vec<u32>,
    pub production: Vec<AgentAttribution>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommitFactType {
    #[serde(rename = "git.commit.produced")]
    Produced,
    #[serde(rename = "git.commit.amended")]
    Amended,
    #[serde(rename = "git.commit.cherry_picked")]
    CherryPicked,
    #[serde(rename = "git.commit.reverted")]
    Reverted,
    #[serde(rename = "git.commit.pushed")]
    Pushed,
    #[serde(rename = "git.commit.inspected")]
    Inspected,
    #[serde(rename = "git.commit.referenced")]
    Referenced,
    #[serde(rename = "git.commit.ambiguous")]
    Ambiguous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitPredicate {
    ProducedBy,
    PossiblyProducedBy,
    AmendedBy,
    CherryPickedFrom,
    Reverts,
    PushedBy,
    InspectedBy,
    ReferencedBy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitBlameMatch {
    pub fact_id: String,
    pub fact_type: CommitFactType,
    pub predicate: CommitPredicate,
    pub subject: ResourceRef,
    pub object: Option<ResourceRef>,
    pub fact_occurred_at_ms: Option<i64>,
    pub confidence: FactConfidence,
    pub state: FactState,
    pub direct_actor: Option<ResourceRef>,
    pub owning_root: Option<ResourceRef>,
    pub evidence_numbers: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestAction {
    Referenced,
    Created,
    Reviewed,
    Commented,
    Merged,
    Edited,
    Closed,
    Reopened,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PullRequestActivity {
    pub fact_id: String,
    pub action: PullRequestAction,
    pub session: ResourceRef,
    pub direct_actor: Option<ResourceRef>,
    pub owning_root: Option<ResourceRef>,
    pub fact_occurred_at_ms: Option<i64>,
    pub confidence: FactConfidence,
    pub state: FactState,
    pub evidence_numbers: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestCommitRelationship {
    ContainsCommit,
    MergedAs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PullRequestCommit {
    pub fact_id: String,
    pub relationship: PullRequestCommitRelationship,
    pub commit: ResourceRef,
    pub production: Vec<AgentAttribution>,
    pub evidence_numbers: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum PullRequestBlameRelationship {
    Activity(PullRequestActivity),
    Commit(PullRequestCommit),
}

/// One complete top-level PR activity or commit-membership relationship.
///
/// Keeping each relationship as its own match makes the request limit exact while
/// preserving the PR -> commit -> producing-session proof boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PullRequestBlameMatch {
    pub pull_request: ResourceRef,
    pub relationship: PullRequestBlameRelationship,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum BlameMatch {
    File(FileBlameMatch),
    Commit(CommitBlameMatch),
    PullRequest(PullRequestBlameMatch),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NumberedEvidence {
    pub number: u32,
    pub citation: EvidenceCitation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlameResult {
    pub target: ResolvedBlameTarget,
    pub git_snapshot: Option<GitSnapshot>,
    pub matches: Vec<BlameMatch>,
    pub evidence: Vec<NumberedEvidence>,
    pub next: Option<BlameContinuation>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BlameResultWire {
    target: ResolvedBlameTarget,
    git_snapshot: Option<GitSnapshot>,
    matches: Vec<BlameMatch>,
    evidence: Vec<NumberedEvidence>,
    next: Option<BlameContinuation>,
}

impl<'de> Deserialize<'de> for BlameResult {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = BlameResultWire::deserialize(deserializer)?;
        let result = Self {
            target: wire.target,
            git_snapshot: wire.git_snapshot,
            matches: wire.matches,
            evidence: wire.evidence,
            next: wire.next,
        };
        result
            .validate()
            .map_err(|error| serde::de::Error::custom(error.message))?;
        Ok(result)
    }
}

impl BlameResult {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.matches.len() > MAX_BLAME_RESULTS as usize {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "blame result exceeds its match bound",
            ));
        }
        if self.evidence.len() > MAX_BLAME_EVIDENCE {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "blame result exceeds its evidence bound",
            ));
        }
        match (&self.target, &self.git_snapshot) {
            (ResolvedBlameTarget::File { .. }, Some(snapshot)) => {
                validate_bounded_text(&snapshot.head_oid, "Git HEAD object ID")?;
            }
            (ResolvedBlameTarget::File { .. }, None) => {
                return Err(ProtocolError::new(
                    ErrorClass::Corrupt,
                    "file blame result is missing its Git snapshot",
                ));
            }
            (
                ResolvedBlameTarget::Commit { .. } | ResolvedBlameTarget::PullRequest { .. },
                Some(_),
            ) => {
                return Err(ProtocolError::new(
                    ErrorClass::Corrupt,
                    "non-file blame result unexpectedly contains a Git snapshot",
                ));
            }
            (
                ResolvedBlameTarget::Commit { .. } | ResolvedBlameTarget::PullRequest { .. },
                None,
            ) => {}
        }
        self.target.validate()?;
        if let Some(next) = &self.next {
            validate_cursor(Some(&next.cursor))?;
        }

        let mut available = BTreeSet::new();
        for (index, evidence) in self.evidence.iter().enumerate() {
            let expected = u32::try_from(index + 1).map_err(|_| {
                ProtocolError::new(ErrorClass::Bounds, "evidence number exceeds u32")
            })?;
            if evidence.number != expected || !evidence.citation.is_usable() {
                return Err(ProtocolError::new(
                    ErrorClass::Corrupt,
                    "blame evidence must be usable and numbered contiguously from one",
                ));
            }
            available.insert(evidence.number);
        }

        let mut referenced = BTreeSet::new();
        for blame_match in &self.matches {
            blame_match.validate(&self.target, &available, &mut referenced)?;
        }
        if referenced != available {
            return Err(ProtocolError::new(
                ErrorClass::Corrupt,
                "blame result contains unreferenced evidence",
            ));
        }
        Ok(())
    }

    pub fn validate_for_request(&self, request: &BlameRequest) -> Result<(), ProtocolError> {
        request.validate()?;
        self.validate()?;
        if self.matches.len() > request.limit as usize {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "blame result exceeds the requested match limit",
            ));
        }

        match (&request.target, &self.target) {
            (
                BlameTarget::File {
                    path: requested_path,
                    repository: requested_repository,
                    lines: requested_lines,
                },
                ResolvedBlameTarget::File {
                    path: resolved_path,
                    repository: resolved_repository,
                    requested_lines: resolved_lines,
                },
            ) => {
                if requested_path != resolved_path
                    || !repository_selector_matches(
                        requested_repository.as_deref(),
                        resolved_repository,
                    )
                    || requested_lines != resolved_lines
                {
                    return Err(ProtocolError::new(
                        ErrorClass::Corrupt,
                        "resolved file target does not match the requested path, repository, and line range",
                    ));
                }
                if let Some(requested_lines) = requested_lines {
                    for blame_match in &self.matches {
                        let BlameMatch::File(file) = blame_match else {
                            return Err(ProtocolError::new(
                                ErrorClass::Corrupt,
                                "file blame request returned a non-file match",
                            ));
                        };
                        if !line_range_contains(requested_lines, &file.lines) {
                            return Err(ProtocolError::new(
                                ErrorClass::Corrupt,
                                "file blame match exceeds the requested line range",
                            ));
                        }
                    }
                }
            }
            (
                BlameTarget::Commit {
                    oid,
                    repository: requested_repository,
                },
                ResolvedBlameTarget::Commit {
                    commit: resolved_commit,
                    repository: resolved_repository,
                },
            ) => {
                if !commit_selector_matches(oid, &resolved_commit.display)
                    || !repository_selector_matches(
                        requested_repository.as_deref(),
                        resolved_repository,
                    )
                {
                    return Err(ProtocolError::new(
                        ErrorClass::Corrupt,
                        "resolved commit target does not match the requested object ID and repository",
                    ));
                }
                for blame_match in &self.matches {
                    let BlameMatch::Commit(commit) = blame_match else {
                        return Err(ProtocolError::new(
                            ErrorClass::Corrupt,
                            "commit blame request returned a non-commit match",
                        ));
                    };
                    if !same_resource_identity(&commit.subject, resolved_commit)
                        && !commit
                            .object
                            .as_ref()
                            .is_some_and(|object| same_resource_identity(object, resolved_commit))
                    {
                        return Err(ProtocolError::new(
                            ErrorClass::Corrupt,
                            "commit blame match does not involve the resolved commit",
                        ));
                    }
                }
            }
            (
                BlameTarget::PullRequest {
                    selector,
                    repository: requested_repository,
                },
                ResolvedBlameTarget::PullRequest {
                    selector: resolved_selector,
                    pull_request: resolved_pull_request,
                    repository: resolved_repository,
                },
            ) => {
                if selector != resolved_selector
                    || !repository_selector_matches(
                        requested_repository.as_deref(),
                        resolved_repository,
                    )
                {
                    return Err(ProtocolError::new(
                        ErrorClass::Corrupt,
                        "resolved pull request target does not match the requested selector and repository",
                    ));
                }
                for blame_match in &self.matches {
                    let BlameMatch::PullRequest(pull_request) = blame_match else {
                        return Err(ProtocolError::new(
                            ErrorClass::Corrupt,
                            "pull request blame request returned a non-pull-request match",
                        ));
                    };
                    if !same_resource_identity(&pull_request.pull_request, resolved_pull_request) {
                        return Err(ProtocolError::new(
                            ErrorClass::Corrupt,
                            "pull request blame match does not reference the resolved pull request",
                        ));
                    }
                }
            }
            _ => {
                return Err(ProtocolError::new(
                    ErrorClass::Corrupt,
                    "resolved blame target kind does not match the request target",
                ));
            }
        }
        Ok(())
    }
}

impl ResolvedBlameTarget {
    fn validate(&self) -> Result<(), ProtocolError> {
        match self {
            Self::File {
                path,
                repository,
                requested_lines,
            } => {
                validate_bounded_text(path, "resolved file path")?;
                validate_resource_kind(repository, ResourceKind::Repository)?;
                if let Some(lines) = requested_lines {
                    lines.validate()?;
                }
            }
            Self::Commit { commit, repository } => {
                validate_resource_kind(commit, ResourceKind::Commit)?;
                validate_resource_kind(repository, ResourceKind::Repository)?;
            }
            Self::PullRequest {
                selector,
                pull_request,
                repository,
            } => {
                if pull_request_selector_kind(selector).is_none() {
                    return Err(ProtocolError::new(
                        ErrorClass::Corrupt,
                        "resolved pull request selector is not canonical",
                    ));
                }
                validate_resource_kind(pull_request, ResourceKind::PullRequest)?;
                validate_resource_kind(repository, ResourceKind::Repository)?;
            }
        }
        Ok(())
    }
}

impl BlameMatch {
    fn validate(
        &self,
        target: &ResolvedBlameTarget,
        available: &BTreeSet<u32>,
        referenced: &mut BTreeSet<u32>,
    ) -> Result<(), ProtocolError> {
        match (self, target) {
            (Self::File(value), ResolvedBlameTarget::File { .. }) => {
                validate_bounded_text(&value.id, "file blame match ID")?;
                value.lines.validate()?;
                validate_resource_kind(&value.commit, ResourceKind::Commit)?;
                validate_evidence_numbers(&value.line_evidence_numbers, available, referenced)?;
                if value.production.len() > MAX_BLAME_ATTRIBUTIONS_PER_MATCH {
                    return Err(ProtocolError::new(
                        ErrorClass::Bounds,
                        "file blame match exceeds its attribution bound",
                    ));
                }
                for attribution in &value.production {
                    attribution.validate(available, referenced)?;
                }
            }
            (Self::Commit(value), ResolvedBlameTarget::Commit { .. }) => {
                value.validate(available, referenced)?;
            }
            (Self::PullRequest(value), ResolvedBlameTarget::PullRequest { .. }) => {
                value.validate(available, referenced)?;
            }
            _ => {
                return Err(ProtocolError::new(
                    ErrorClass::Corrupt,
                    "blame match kind does not match its resolved target",
                ));
            }
        }
        Ok(())
    }
}

impl AgentAttribution {
    fn validate(
        &self,
        available: &BTreeSet<u32>,
        referenced: &mut BTreeSet<u32>,
    ) -> Result<(), ProtocolError> {
        validate_bounded_text(&self.id, "agent attribution ID")?;
        validate_resource_kind(&self.producing_session, ResourceKind::Session)?;
        if let Some(resource) = &self.direct_actor {
            resource.validate()?;
        }
        if let Some(resource) = &self.owning_root {
            resource.validate()?;
        }
        match self.relationship {
            ProductionRelationship::ProducedBy
                if self.state != FactState::Asserted
                    || self.confidence == FactConfidence::Ambiguous =>
            {
                return Err(ProtocolError::new(
                    ErrorClass::Corrupt,
                    "asserted production has inconsistent state or confidence",
                ));
            }
            ProductionRelationship::PossiblyProducedBy
                if self.state != FactState::Ambiguous
                    || self.confidence != FactConfidence::Ambiguous =>
            {
                return Err(ProtocolError::new(
                    ErrorClass::Corrupt,
                    "possible production must preserve ambiguous state and confidence",
                ));
            }
            _ => {}
        }
        validate_evidence_numbers(&self.evidence_numbers, available, referenced)
    }
}

impl CommitBlameMatch {
    fn validate(
        &self,
        available: &BTreeSet<u32>,
        referenced: &mut BTreeSet<u32>,
    ) -> Result<(), ProtocolError> {
        validate_bounded_text(&self.fact_id, "commit fact ID")?;
        validate_resource_kind(&self.subject, ResourceKind::Commit)?;
        if let Some(object) = &self.object {
            object.validate()?;
        }
        if let Some(resource) = &self.direct_actor {
            resource.validate()?;
        }
        if let Some(resource) = &self.owning_root {
            resource.validate()?;
        }
        let expected = match self.fact_type {
            CommitFactType::Produced => CommitPredicate::ProducedBy,
            CommitFactType::Ambiguous => CommitPredicate::PossiblyProducedBy,
            CommitFactType::Amended => CommitPredicate::AmendedBy,
            CommitFactType::CherryPicked => CommitPredicate::CherryPickedFrom,
            CommitFactType::Reverted => CommitPredicate::Reverts,
            CommitFactType::Pushed => CommitPredicate::PushedBy,
            CommitFactType::Inspected => CommitPredicate::InspectedBy,
            CommitFactType::Referenced => CommitPredicate::ReferencedBy,
        };
        if self.predicate != expected {
            return Err(ProtocolError::new(
                ErrorClass::Corrupt,
                "commit fact type and predicate disagree",
            ));
        }
        if self.object.is_none()
            && !(matches!(
                self.fact_type,
                CommitFactType::CherryPicked | CommitFactType::Reverted
            ) && self.state == FactState::Ambiguous)
        {
            return Err(ProtocolError::new(
                ErrorClass::Corrupt,
                "commit fact is missing a required object",
            ));
        }
        validate_evidence_numbers(&self.evidence_numbers, available, referenced)
    }
}

impl PullRequestBlameMatch {
    fn validate(
        &self,
        available: &BTreeSet<u32>,
        referenced: &mut BTreeSet<u32>,
    ) -> Result<(), ProtocolError> {
        validate_resource_kind(&self.pull_request, ResourceKind::PullRequest)?;
        match &self.relationship {
            PullRequestBlameRelationship::Activity(activity) => {
                validate_bounded_text(&activity.fact_id, "pull request activity fact ID")?;
                validate_resource_kind(&activity.session, ResourceKind::Session)?;
                if let Some(resource) = &activity.direct_actor {
                    resource.validate()?;
                }
                if let Some(resource) = &activity.owning_root {
                    resource.validate()?;
                }
                validate_evidence_numbers(&activity.evidence_numbers, available, referenced)
            }
            PullRequestBlameRelationship::Commit(commit) => {
                validate_bounded_text(&commit.fact_id, "pull request commit fact ID")?;
                validate_resource_kind(&commit.commit, ResourceKind::Commit)?;
                if commit.production.len() > MAX_BLAME_ATTRIBUTIONS_PER_MATCH {
                    return Err(ProtocolError::new(
                        ErrorClass::Bounds,
                        "pull request commit match exceeds its attribution bound",
                    ));
                }
                validate_evidence_numbers(&commit.evidence_numbers, available, referenced)?;
                for attribution in &commit.production {
                    attribution.validate(available, referenced)?;
                }
                Ok(())
            }
        }
    }
}

fn validate_evidence_numbers(
    numbers: &[u32],
    available: &BTreeSet<u32>,
    referenced: &mut BTreeSet<u32>,
) -> Result<(), ProtocolError> {
    if numbers.is_empty() || numbers.len() > MAX_CITATIONS_PER_FACT {
        return Err(ProtocolError::new(
            ErrorClass::Bounds,
            "evidence-number list must be nonempty and within its bound",
        ));
    }
    let mut prior = 0;
    for number in numbers {
        if *number <= prior || !available.contains(number) {
            return Err(ProtocolError::new(
                ErrorClass::Corrupt,
                "evidence numbers must be unique, sorted, and present in the page",
            ));
        }
        referenced.insert(*number);
        prior = *number;
    }
    Ok(())
}

fn validate_cursor(cursor: Option<&str>) -> Result<(), ProtocolError> {
    if cursor.is_some_and(|cursor| {
        cursor.is_empty() || cursor.len() > MAX_BLAME_CURSOR_BYTES || !cursor.is_ascii()
    }) {
        return Err(ProtocolError::new(
            ErrorClass::Bounds,
            format!("blame cursor must contain 1 to {MAX_BLAME_CURSOR_BYTES} ASCII bytes"),
        ));
    }
    Ok(())
}

fn validate_bounded_text(value: &str, name: &str) -> Result<(), ProtocolError> {
    if value.trim().is_empty()
        || value.len() > MAX_BLAME_TARGET_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(ProtocolError::new(
            ErrorClass::Bounds,
            format!("{name} is empty, unsafe, or exceeds its byte bound"),
        ));
    }
    Ok(())
}

fn validate_resource_kind(
    resource: &ResourceRef,
    expected: ResourceKind,
) -> Result<(), ProtocolError> {
    resource.validate()?;
    if resource.kind != expected {
        return Err(ProtocolError::new(
            ErrorClass::Corrupt,
            "resource reference has an unexpected kind",
        ));
    }
    Ok(())
}

fn line_range_contains(outer: &LineRange, inner: &LineRange) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

fn same_resource_identity(left: &ResourceRef, right: &ResourceRef) -> bool {
    left.kind == right.kind && left.id == right.id
}

fn commit_selector_matches(requested: &str, resolved: &str) -> bool {
    requested.len() <= resolved.len()
        && requested.bytes().all(|byte| byte.is_ascii_hexdigit())
        && resolved.bytes().all(|byte| byte.is_ascii_hexdigit())
        && resolved
            .get(..requested.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(requested))
}

fn repository_selector_matches(requested: Option<&str>, resolved: &ResourceRef) -> bool {
    requested.is_none_or(|requested| {
        normalized_repository_selector(requested)
            == normalized_repository_selector(&resolved.display)
    })
}

fn normalized_repository_selector(value: &str) -> String {
    let value = value
        .strip_prefix("forge:")
        .or_else(|| value.strip_prefix("https://"))
        .unwrap_or(value)
        .trim_end_matches('/');
    let Some((host, path)) = value.split_once('/') else {
        return value.to_owned();
    };
    if !host.contains('.') {
        return value.to_owned();
    }
    format!("forge:{}/{path}", host.to_ascii_lowercase())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PullRequestSelectorKind {
    Number,
    CanonicalUrl,
}

fn pull_request_selector_kind(value: &str) -> Option<PullRequestSelectorKind> {
    if positive_decimal(value) {
        return Some(PullRequestSelectorKind::Number);
    }
    canonical_pull_request_url(value).then_some(PullRequestSelectorKind::CanonicalUrl)
}

fn positive_decimal(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value.len() == 1 || !value.starts_with('0'))
        && value.parse::<u64>().is_ok_and(|number| number > 0)
}

fn canonical_pull_request_url(value: &str) -> bool {
    let Some(remainder) = value.strip_prefix("https://") else {
        return false;
    };
    let mut components = remainder.split('/');
    let Some(host) = components.next() else {
        return false;
    };
    if !valid_forge_host(host) {
        return false;
    }
    let path = components.collect::<Vec<_>>();
    if path
        .iter()
        .any(|component| !valid_url_path_component(component))
    {
        return false;
    }

    if host == "github.com" {
        return path.len() == 4 && path[2] == "pull" && positive_decimal(path[3]);
    }
    if host == "codeberg.org" {
        return path.len() == 4 && path[2] == "pulls" && positive_decimal(path[3]);
    }
    if host == "bitbucket.org" {
        return false;
    }
    path.len() >= 5
        && path[path.len() - 3] == "-"
        && path[path.len() - 2] == "merge_requests"
        && positive_decimal(path[path.len() - 1])
}

fn valid_forge_host(host: &str) -> bool {
    !host.is_empty()
        && host.bytes().all(|byte| !byte.is_ascii_uppercase())
        && host.contains('.')
        && !host.starts_with('.')
        && !host.ends_with('.')
        && host.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

fn valid_url_path_component(component: &&str) -> bool {
    !component.is_empty()
        && component
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~'))
        && !matches!(*component, "." | "..")
}
