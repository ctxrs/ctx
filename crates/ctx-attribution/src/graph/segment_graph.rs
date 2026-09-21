//! Read-only blame queries over one checked immutable segment generation.

mod authority;
#[cfg(test)]
mod authority_order_tests;
mod blame;
#[cfg(test)]
mod commit_rewrite_tests;
mod merge;
#[cfg(test)]
mod tests;

use thiserror::Error;

use crate::git_executable::GitExecutable;
use crate::protocol::CoreMaterializationReceipt;
use crate::query::{
    BlameFactFamily, BlameFactPosition, BlameGraph, Fact, FileBlamePosition, GitBlameWindow,
    LineRange, ProductionAttribution, QueryError, QueryPage, Resource, ResourceId,
    ResourceSelector,
};

use super::GRAPH_EVIDENCE_FINGERPRINT;
use super::segment::{
    FlatOpenPolicy, FlatSegmentError, PinnedFlatGeneration, PinnedFlatGenerationError,
    SegmentStoreError,
};

/// Manifest identity for the Flat record/resource representation.
pub const SEGMENT_SCHEMA_IDENTITY: &str =
    "sha256:c2bd3af6cdc02ed58e14089b0c14d994e5ee737ec6a0605d1a751998abb076d9";
/// Exact rootless schema before conservative quoted-shell interpretation.
/// Accept only checked control for a clean rebuild, never prior facts.
/// Retire when upgrades from its writing releases are outside the supported floor.
pub const PRE_STATIC_SHELL_QUOTING_SEGMENT_SCHEMA_IDENTITY: &str =
    "sha256:dbbd6248abd9c65534ea66a4a824debd717c3c1d65c73bfb1604d071bbb556dc";
/// Exact schema before rootless facts. Accepted only as checked control
/// to rebuild, never as serving records. Remove when upgrades from this schema
/// are outside the supported release floor.
pub const PRE_OPTIONAL_ROOT_SEGMENT_SCHEMA_IDENTITY: &str =
    "sha256:1c02f56b09ca2da335301bc7efd06cf608b3aaaf37aea0b6805a173a4a7924bb";
/// Exact schema emitted by the direct-materialization predecessor. It is
/// accepted only as checked control while Core supplies a clean
/// replacement generation. Remove this decoder only after the first release
/// writing `SEGMENT_SCHEMA_IDENTITY` is the minimum supported index format.
pub const PRE_DIRECT_MATERIALIZATION_SEGMENT_SCHEMA_IDENTITY: &str =
    "sha256:23b39ef1e49e02a4b7946c37900844219c672d53da0d3a45a37541cedbff0ebb";
/// Manifest identity for exact Core evidence retained by Flat records.
pub const SEGMENT_EVIDENCE_IDENTITY: &str = GRAPH_EVIDENCE_FINGERPRINT;
/// Manifest identity for the cursor ordering implemented by [`BlameGraph`].
pub const SEGMENT_ORDERING_IDENTITY: &str =
    "sha256:a35314ca803d82a905dee94023d5a998ad0975581d7a8af30bf1e9170e3db06c";

#[doc(hidden)]
pub const SEGMENT_SCHEMA_CONTRACT: &[u8] = b"ctx-attribution-flat-fst-schema-v1\n\
bounded-shell-subset-revision=6\n\
plain-segment-file-version=1\n\
flat-publication-checked-chunk-bytes=16384\n\
flat-publication-retained-accounted-bytes-max=16777216\n\
flat-publication-index-associations-max=32768\n\
plain-reader-cache-max-bytes-per-file=2097152\n\
flat-container-version=2\n\
flat-directory-version=2\n\
fst-map-key-version=2\n\
fst-scoped-repository-key=sha256-length-delimited-v1\n\
logical-repository-id-max-bytes=65536\n\
logical-repository-id=opaque-exact-nonempty-v1\n\
repository-resource-display-index=bounded-safe-only-v1\n\
flat-record-payload-max-bytes=1048576\n\
fst-shard-max-bytes=262144\n\
fst-shard-max-keys=1024\n\
flat-prefix-page-max-fst-shards=16\n\
tombstone-page-max-bytes=65536\n\
graph-flat-directory-max-bytes=67108864\n\
manifest-version=1\n\
segment-ref-publication-generation=1\n\
event-index-version=6\n\
event-index-magic=CTXEVI06\n\
event-index-operation-key=source-id,event-id,publication-generation\n\
event-index-operation-kinds=add,replace,delete\n\
event-index-event-session-reference=u32-segment-local\n\
event-index-session-dictionary=stable-session,parent-session,optional-root-session,relationship,proof\n\
event-index-copied-origin=sparse-copied-events-only\n\
event-index-copied-origin-reference=opaque-target-may-be-absent\n\
event-index-lineage-validation=local-shape-bijection-owner-consistency\n\
event-index-output-proof=event-sequence,core-sha256,core-record-leaf-sha256,flat-record-count-u32,event-output-root-sha256\n\
event-index-non-add-proof=prior-event-state-sha256\n\
event-index-replace-physical-effect=one-event-state-and-one-generation-local-event-and-flat-tombstone-pair\n\
event-output-root-domain=ctx-pro-qualification-event-output-root-v1\n\
event-output-root-inputs=publication-generation,canonical-source,canonical-event-id,event-sequence,core-record-sha256,core-record-leaf-sha256,flat-record-count,ordered-record-id-and-full-serving-record-sha256\n\
event-index-history-chain=each-newer-operation-commits-immediately-prior-event-state-identity\n\
event-index-same-layer-duplicate-operation-key=reject\n\
event-index-coverage=packed-6-bit\n\
event-index-lookup=sorted-fixed-row\n\
event-index-fst=absent\n\
staged-page-version=7\n\
staged-prepared-unit-producer-disposition=explicit-semantic\n\
publication-semantics-digest-version=5\n\
publication-semantics-digest-domain=ctx-pro-direct-publication-semantics-v1\n\
publication-semantics-digest-inputs=prior-fold,canonical-validated-event-delta-page-output\n\
publication-semantics-resume=direct-page-fold\n\
serving-record-json-version=2\n\
serving-record-id=core-logical-fact-v4\n\
serving-record-authority-eligibility=direct-asserted-verified\n\
serving-record-producer-disposition=eligible-unique,abstain-unknown,ineligible-copied\n\
serving-record-producer-authority-families=git.commit.produced,git.commit.replaced,git.commit.ambiguous,git.commit.cherry_picked,pull_request.produced,forge.pull_request.produced,forge.create,forge.merge\n\
serving-record-id-index-term=1\n\
serving-fact-family-count=26\n\
typed-commit-operation-families=git.commit.produced,git.commit.replaced,git.commit.cherry_picked\n\
typed-commit-operation-admission=core-asserted-complete-repository-verified-mappings-only\n\
typed-commit-operation-mappings-max=32\n\
typed-commit-operation-identity=logical-repository,object-format,full-oid\n\
typed-commit-operation-attributes=operation-id,receipt-id,kind,relation-class,proof-class,state,object-format\n\
typed-commit-operation-yields=distinct-result-per-operation-event\n\
typed-commit-operation-index=validated-operation-id-exact-term-only\n\
typed-commit-replacement-direction=replaced-object-to-replacement-subject\n\
typed-commit-derivation-direction=source-object-to-derived-result-subject\n\
typed-commit-operation-authority=event-scoped-asserted-verified-without-actor-transfer\n\
typed-commit-operation-abstention=ambiguous,contradicted,unlinked,pr-merge\n\
repository-attribution-session-joins=record-local-direct-session-and-optional-root-only\n\
repository-binding-serving-emission=file-or-commit-or-pull-request-evidence-record\n\
repository-remote-alias-emission=file-or-commit-or-pull-request-evidence-record\n\
repository-live-access-emission=file-evidence-record\n\
event-owner-version=3\n\
event-owner-root-session-id=optional-provider-native-no-fallback\n\
event-tombstone-membership-version=3\n\
event-tombstone-membership-key=publication-generation,source-id,event-id\n\
newest-first-current-before-event-shadow=1\n\
same-publication-flat-chunks-one-layer=1\n\
graph-query-candidates-max=262144\n\
graph-tombstone-probes-max=262144\n\
graph-tombstone-pages-max=256\n\
graph-tombstone-page-bytes-max=16777216\n\
graph-tombstone-checked-chunks-max=512\n\
graph-tombstone-checked-bytes-max=33554432\n";

#[doc(hidden)]
pub const SEGMENT_ORDERING_CONTRACT: &[u8] = b"ctx-attribution-flat-blame-ordering-v1\n\
blame-ordering-version=1\n\
checked-publication-generation-desc=1\n\
same-publication-chunks-unordered-temporally=1\n\
commit-rank-time-desc-related-fact=1\n\
pull-request-proof-first-then-activity-time-desc=1\n\
production-produced-before-ambiguous=1\n\
citation-chronology-event-sequence-first=1\n";

const _: () = assert!(crate::query::BLAME_ORDERING_VERSION == 1);

const QUERY_BACKEND_ERROR: &str = "checked graph segment query failed";

#[derive(Debug, Error)]
pub enum SegmentGraphError {
    #[error("invalid attribution input: {0}")]
    InvalidInput(String),
    #[error("work graph requires materialization")]
    MaterializationRequired,
    #[error("query cursor is invalid")]
    InvalidCursor,
    #[error("query cursor refers to an older graph state")]
    StaleCursor,
    #[error("one query record exceeds the protocol frame bound")]
    QueryRecordTooLarge,
    #[error("authorized Git is unavailable")]
    GitAuthorityUnavailable,
    #[error("the active graph segment manifest is unavailable")]
    Unavailable,
    #[error("the active graph segment manifest has an incompatible {0} identity")]
    Identity(&'static str),
    #[error("the active graph segment set is corrupt: {0}")]
    Corrupt(&'static str),
    #[error(transparent)]
    Store(#[from] SegmentStoreError),
    #[error(transparent)]
    Flat(#[from] FlatSegmentError),
    #[error("graph segment verification failed")]
    Verification(#[source] std::io::Error),
    #[error("attribution query failed")]
    Query(#[from] QueryError),
}

/// A pinned read backend for exactly one active manifest generation.
///
/// Construction checks the manifest plus every referenced container
/// header. Each plain range is checked when read, so a bounded query
/// does not scan unrelated segment_bytes. Queries never reopen the manifest or
/// consult relational or provider history.
pub struct SegmentGraph {
    storage: PinnedFlatGeneration,
    git_executable: Option<GitExecutable>,
}

impl SegmentGraph {
    pub fn generation_id(&self) -> &str {
        self.storage.generation_id()
    }
    pub fn from_pinned(
        storage: PinnedFlatGeneration,
        git_executable: Option<GitExecutable>,
    ) -> Self {
        Self {
            storage,
            git_executable,
        }
    }

    pub const fn flat_open_policy() -> FlatOpenPolicy {
        FlatOpenPolicy::new(
            SEGMENT_SCHEMA_IDENTITY,
            SEGMENT_EVIDENCE_IDENTITY,
            SEGMENT_ORDERING_IDENTITY,
        )
    }

    #[cfg(test)]
    fn open(
        root: &std::path::Path,
        git_executable: Option<GitExecutable>,
    ) -> Result<Self, SegmentGraphError> {
        let storage =
            ctx_attribution_index::FlatStore::new(root).open_active(Self::flat_open_policy())?;
        Ok(Self::from_pinned(storage, git_executable))
    }

    pub const fn graph_generation(&self) -> u64 {
        self.storage.graph_generation()
    }

    pub const fn completed_receipt(&self) -> &CoreMaterializationReceipt {
        self.storage.completed_receipt()
    }

    fn query_error() -> QueryError {
        QueryError::Backend(QUERY_BACKEND_ERROR.to_owned())
    }

    #[must_use]
    pub const fn git_authority_available(&self) -> bool {
        self.git_executable.is_some()
    }
}

impl From<PinnedFlatGenerationError> for SegmentGraphError {
    fn from(error: PinnedFlatGenerationError) -> Self {
        match error {
            PinnedFlatGenerationError::Unavailable => Self::Unavailable,
            PinnedFlatGenerationError::Identity(identity) => Self::Identity(identity),
            PinnedFlatGenerationError::Corrupt(detail) => Self::Corrupt(detail),
            PinnedFlatGenerationError::SegmentIdentityChanged => {
                Self::Corrupt("Flat segment identity changed while opening")
            }
            PinnedFlatGenerationError::Store(error) => Self::Store(error),
            PinnedFlatGenerationError::Flat(error) => Self::Flat(error),
            PinnedFlatGenerationError::Verification(error) => Self::Verification(error),
        }
    }
}

impl BlameGraph for &SegmentGraph {
    fn resolve(
        &self,
        selector: &ResourceSelector,
        limit: usize,
    ) -> Result<Vec<Resource>, QueryError> {
        self.resolve_resources(selector, limit)
    }

    fn resolve_commits(
        &self,
        commits: &[String],
        repository: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, Resource)>, QueryError> {
        self.resolve_commit_resources(commits, repository, limit)
    }

    fn resources(&self, ids: &[ResourceId], limit: usize) -> Result<Vec<Resource>, QueryError> {
        self.load_resources(ids, limit)
    }

    fn blame_facts_page(
        &self,
        target: &ResourceId,
        family: BlameFactFamily,
        after: Option<&BlameFactPosition>,
        limit: usize,
    ) -> Result<QueryPage<(Fact, BlameFactPosition), BlameFactPosition>, QueryError> {
        self.facts_page(target, family, after, limit)
    }

    fn git_blame_window(
        &self,
        file: &ResourceId,
        requested: Option<LineRange>,
        resume: Option<&FileBlamePosition>,
    ) -> Result<GitBlameWindow, QueryError> {
        super::git::blame_with_authority(*self, file, requested, resume)
    }

    fn production_attribution(
        &self,
        commit_ids: &[ResourceId],
        limit_per_commit: usize,
    ) -> Result<Vec<ProductionAttribution>, QueryError> {
        self.attributions(commit_ids, limit_per_commit)
    }
}
