//! Storage-independent work-graph query values and bounds.

use std::collections::BTreeSet;
use std::fmt;

use crate::protocol::{
    BlameDiagnosticCandidate, ContinuationReason, EvidenceCitation, GitSnapshot,
    MAX_BLAME_DIAGNOSTIC_CANDIDATES, ProductionRelationship, PullRequestAction,
    PullRequestCommitRelationship, ResolvedBlameTarget,
};
pub use crate::protocol::{LineRange, ResourceKind};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceSelector {
    pub kind: ResourceKind,
    pub value: String,
    pub repository: Option<String>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct ResourceId(pub String);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Resource {
    pub id: ResourceId,
    pub kind: ResourceKind,
    pub display: String,
    /// Stable graph resource ID for the certified logical repository that owns
    /// this resource. Repository resources themselves leave this empty.
    pub logical_repository: Option<ResourceId>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Confidence {
    Verified,
    High,
    Medium,
    Ambiguous,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum FactState {
    Asserted,
    Ambiguous,
    Contradicted,
    Superseded,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct Citation(pub EvidenceCitation);

impl Citation {
    /// Accepts only a stable, generation-bound Core evidence identity.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidCitation`] when the citation is not usable
    /// under the Core identity contract.
    pub fn new(citation: EvidenceCitation) -> Result<Self, QueryError> {
        if !citation.is_usable() {
            return Err(QueryError::InvalidCitation);
        }
        Ok(Self(citation))
    }

    #[must_use]
    pub fn is_exact(&self) -> bool {
        self.0.is_usable()
    }

    #[must_use]
    pub const fn evidence(&self) -> &EvidenceCitation {
        &self.0
    }
}

/// Internal graph fact primitive. Only target-specific projections cross the protocol.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fact {
    pub id: String,
    pub fact_type: String,
    pub subject: ResourceId,
    pub predicate: String,
    pub object: Option<ResourceId>,
    pub value: Option<String>,
    pub occurred_at_ms: Option<i64>,
    pub confidence: Confidence,
    pub state: FactState,
    pub detector_version: String,
    pub root_run: Option<ResourceId>,
    pub direct_actor: Option<ResourceId>,
    pub citations: Vec<Citation>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryPage<T, C> {
    pub items: Vec<T>,
    pub next_cursor: Option<C>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitLineObservation {
    pub file: ResourceId,
    pub lines: LineRange,
    pub commit_selector: String,
    pub citation: Citation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitBlameWindow {
    pub head_oid: String,
    pub worktree_status: crate::protocol::WorktreeStatus,
    pub window_start: u32,
    pub window_end: u32,
    pub observations: Vec<GitLineObservation>,
    pub more_committed_lines: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductionAttribution {
    pub fact_id: String,
    pub commit: ResourceId,
    pub producing_session: ResourceId,
    pub parent_session: Option<ResourceId>,
    pub root_run: Option<ResourceId>,
    pub direct_actor: Option<ResourceId>,
    pub fact_occurred_at_ms: Option<i64>,
    pub relationship: ProductionRelationship,
    pub confidence: Confidence,
    pub state: FactState,
    pub citations: Vec<Citation>,
}

/// Maximum number of producer candidates retained for one blame item.
///
/// This semantic disclosure bound is intentionally narrower than the hard
/// protocol and graph-work bounds. The graph still fails closed when those
/// larger bounds are exceeded before choosing the deterministic candidates.
pub const MAX_ATTRIBUTION_CANDIDATES: usize = 5;
const MAX_PUBLIC_DIAGNOSTIC_CANDIDATE_BYTES: usize = 160;

/// Safe, bounded public identities retained when exact resolution is ambiguous.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AmbiguityCandidates {
    pub candidates: Vec<BlameDiagnosticCandidate>,
    pub candidates_truncated: bool,
}

impl AmbiguityCandidates {
    #[must_use]
    pub fn undisclosed() -> Self {
        Self::bounded(std::iter::empty())
    }

    #[must_use]
    pub fn repositories(selectors: impl IntoIterator<Item = String>) -> Self {
        Self::bounded(selectors.into_iter().filter_map(|selector| {
            safe_logical_repository(&selector)
                .then_some(BlameDiagnosticCandidate::Repository { selector })
        }))
    }

    #[must_use]
    pub fn commits(repository: &str, oids: impl IntoIterator<Item = String>) -> Self {
        if !safe_logical_repository(repository) {
            return Self::bounded(std::iter::empty());
        }
        Self::bounded(oids.into_iter().filter_map(|oid| {
            let oid = oid.to_ascii_lowercase();
            safe_commit_oid(&oid).then(|| BlameDiagnosticCandidate::Commit {
                repository: repository.to_owned(),
                oid,
            })
        }))
    }

    fn bounded(candidates: impl IntoIterator<Item = BlameDiagnosticCandidate>) -> Self {
        let mut candidates = candidates.into_iter().collect::<BTreeSet<_>>();
        // Disclosing one surviving public identity would reveal relative
        // uniqueness; candidate-free ambiguity preserves the diagnosis safely.
        if candidates.len() == 1 {
            candidates.clear();
        }
        let candidates_truncated = candidates.len() > MAX_BLAME_DIAGNOSTIC_CANDIDATES;
        Self {
            candidates: candidates
                .into_iter()
                .take(MAX_BLAME_DIAGNOSTIC_CANDIDATES)
                .collect(),
            candidates_truncated,
        }
    }
}

fn safe_logical_repository(value: &str) -> bool {
    let Some((host, path)) = value
        .strip_prefix("forge:")
        .and_then(|identity| identity.split_once('/'))
    else {
        return false;
    };
    value.len() <= MAX_PUBLIC_DIAGNOSTIC_CANDIDATE_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
        && safe_public_forge_host(host)
        && safe_public_forge_path(path)
}

fn safe_public_forge_host(host: &str) -> bool {
    let reserved = matches!(
        host,
        "localhost" | "local" | "private" | "workspace" | "internal"
    ) || [
        ".localhost",
        ".local",
        ".private",
        ".workspace",
        ".internal",
    ]
    .iter()
    .any(|suffix| host.ends_with(suffix));
    !host.is_empty()
        && host.contains('.')
        && host.bytes().any(|byte| byte.is_ascii_alphabetic())
        && !host.bytes().any(|byte| byte.is_ascii_uppercase())
        && !reserved
        && host.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

fn safe_public_forge_path(path: &str) -> bool {
    !path.is_empty()
        && path.split('/').all(|component| {
            !component.is_empty()
                && !matches!(component, "." | "..")
                && component.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
                })
        })
}

fn safe_commit_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttributionOutcome {
    Proven,
    Possible,
    Conflicting,
    None,
}

/// Classifies already-authorized producer evidence without selecting a winner.
///
/// Incompatible asserted producers are useful conflicting evidence, not query
/// ambiguity. Ambiguity while resolving the target, repository, or commit is
/// represented separately by [`QueryError`].
#[must_use]
pub fn attribution_outcome(attributions: &[ProductionAttribution]) -> AttributionOutcome {
    let asserted = attributions
        .iter()
        .filter(|attribution| {
            attribution.relationship == ProductionRelationship::ProducedBy
                && attribution.state == FactState::Asserted
        })
        .map(|attribution| &attribution.producing_session)
        .collect::<BTreeSet<_>>();
    match asserted.len() {
        2.. => AttributionOutcome::Conflicting,
        1 => AttributionOutcome::Proven,
        _ if attributions.iter().any(|attribution| {
            attribution.relationship == ProductionRelationship::PossiblyProducedBy
                && attribution.state == FactState::Ambiguous
        }) =>
        {
            AttributionOutcome::Possible
        }
        _ => AttributionOutcome::None,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlameFactPosition {
    pub class: u8,
    pub rank: u8,
    pub occurred_at_ms: Option<i64>,
    pub resource_id: String,
    pub fact_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileBlamePosition {
    pub head_oid: String,
    pub requested_start: u32,
    pub requested_end: Option<u32>,
    pub window_start: u32,
    pub window_end: u32,
    pub next_line: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BlamePosition {
    File(FileBlamePosition),
    Commit(BlameFactPosition),
    PullRequest(BlameFactPosition),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileBlameEntry {
    pub id: String,
    pub lines: LineRange,
    pub commit: Resource,
    pub line_citations: Vec<Citation>,
    pub production: Vec<ProductionAttribution>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitBlameEntry {
    pub fact: Fact,
    pub attribution: AttributionOutcome,
    pub subject: Resource,
    pub object: Option<Resource>,
    pub parent_session: Option<Resource>,
    pub direct_actor: Option<Resource>,
    pub owning_root: Option<Resource>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PullRequestActivityEntry {
    pub pull_request: Resource,
    pub fact: Fact,
    pub action: PullRequestAction,
    pub session: Resource,
    pub direct_actor: Option<Resource>,
    pub owning_root: Option<Resource>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PullRequestCommitEntry {
    pub pull_request: Resource,
    pub fact: Fact,
    pub relationship: PullRequestCommitRelationship,
    pub commit: Resource,
    pub production: Vec<ProductionAttribution>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BlameEntry {
    File(FileBlameEntry),
    Commit(CommitBlameEntry),
    PullRequestActivity(PullRequestActivityEntry),
    PullRequestCommit(PullRequestCommitEntry),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlamePage {
    pub target: ResolvedBlameTarget,
    pub git_snapshot: Option<GitSnapshot>,
    pub entries: Vec<BlameEntry>,
    /// Exact continuation position after each corresponding entry.
    pub positions: Vec<BlamePosition>,
    /// Supporting session/root/actor resources needed by nested attributions.
    pub resources: Vec<Resource>,
    pub has_more: bool,
    pub continuation_reason: ContinuationReason,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QueryBounds {
    pub max_matches: usize,
    pub max_attributions_per_match: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueryOperation {
    FileBlame,
    CommitBlame,
    PullRequestBlame,
}

impl Default for QueryBounds {
    fn default() -> Self {
        Self {
            max_matches: 100,
            max_attributions_per_match: 100,
        }
    }
}

impl QueryBounds {
    pub fn validate(self) -> Result<Self, QueryError> {
        if self.max_matches == 0
            || self.max_matches > crate::protocol::MAX_BLAME_RESULTS as usize
            || self.max_attributions_per_match == 0
            || self.max_attributions_per_match > crate::protocol::MAX_BLAME_ATTRIBUTIONS_PER_MATCH
        {
            return Err(QueryError::InvalidBounds);
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QueryError {
    InvalidBounds,
    InvalidRequest(String),
    TargetNotFound(ResourceKind),
    RepositorySelectorNotFound,
    Backend(String),
    RepositoryNotBound,
    RepositoryUnavailable,
    GitUnavailable,
    AmbiguousRepositoryCandidates(AmbiguityCandidates),
    AmbiguousTarget(AmbiguityCandidates),
    AmbiguousCommitRewrite(AmbiguityCandidates),
    OperationUnavailable(QueryOperation),
    AttributionLimitExceeded(ResourceId),
    LineOutOfRange,
    StaleSnapshot,
    UncitedFact(String),
    InvalidCitation,
}

impl fmt::Display for QueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for QueryError {}
