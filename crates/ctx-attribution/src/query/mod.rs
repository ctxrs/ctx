//! Bounded, citation-preserving assembly for file, commit, and pull-request blame.
//!
//! Generic graph facts remain internal; only target-specific projections cross the
//! private/public protocol boundary.

#[doc(hidden)]
pub mod commit_lineage;
mod model;
pub mod ordering;
mod service;
mod service_resolution;

pub const BLAME_ORDERING_VERSION: u8 = 1;

pub use model::{
    AmbiguityCandidates, AttributionOutcome, BlameEntry, BlameFactPosition, BlamePage,
    BlamePosition, Citation, CommitBlameEntry, Confidence, Fact, FactState, FileBlameEntry,
    FileBlamePosition, GitBlameWindow, GitLineObservation, LineRange, MAX_ATTRIBUTION_CANDIDATES,
    ProductionAttribution, PullRequestActivityEntry, PullRequestCommitEntry, QueryBounds,
    QueryError, QueryOperation, QueryPage, Resource, ResourceId, ResourceKind, ResourceSelector,
    attribution_outcome,
};
pub use service::{BlameFactFamily, BlameGraph, BlameService};
