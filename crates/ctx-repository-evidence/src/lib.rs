//! Bounded repository-evidence evaluation for ctx.
//!
//! Evaluation interprets retained Core activity and certifies local Git evidence.
//! It preserves typed conclusions and abstentions without modifying repositories,
//! importing provider history, or accessing the network.

mod association;
mod core_adapter;
mod evaluation;
pub mod facts;
mod git;
mod identity;
mod model;
mod outcome;
mod resolver;
mod scoping;
mod shell;

pub use association::{LiteralPullRequestAssociationObservation, exact_pull_request_association};
pub use core_adapter::CoreRepositoryEvidenceAdapter;
pub use evaluation::{MAX_REPOSITORY_CANDIDATES, evaluate};
use evaluation::{ScopedFileInput, ScopedRepositoryFileInvocationEvidence, ScopedVcsInput};
pub use facts::{
    CommandLiteralDisposition, LiteralFileObservation, LiteralRepositoryFileInvocation,
    LiteralVcsObservation, NeutralRepositoryFacts,
};
use git::CertifiedCandidate;
use identity::push_abstention;
pub use model::{
    BOUNDED_SHELL_SUBSET_REVISION, CORE_BOUNDED_SHELL_SUBSET_REVISION,
    CORE_MISSING_ACTIVITY_TIME_UNIX_MS, CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
    CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_DOMAIN,
    CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_REVISION,
    CORE_REPOSITORY_OBSERVATION_REVISION,
    CORE_REPOSITORY_PULL_REQUEST_ASSOCIATION_CAPTURE_REVISION, GitObjectFormat, GitObjectId,
    LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_DOMAIN, LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_REVISION,
    MAX_COMMIT_OPERATION_MAPPINGS, MISSING_ACTIVITY_TIME_UNIX_MS, OUTCOME_CAPTURE_REVISION,
    PULL_REQUEST_ASSOCIATION_CAPTURE_REVISION, REPOSITORY_ASSOCIATION_POLICY_REVISION,
    REPOSITORY_OBSERVATION_REVISION, RepositoryAbstention, RepositoryAbstentionReason,
    RepositoryAlias, RepositoryAliasKind, RepositoryBinding, RepositoryCandidate,
    RepositoryCandidateEvidence, RepositoryCandidateKind, RepositoryCommitMapping,
    RepositoryCommitMappingCompleteness, RepositoryCommitOperationEvent,
    RepositoryCommitOperationKind, RepositoryCommitOperationProof, RepositoryCommitOperationState,
    RepositoryEvaluation, RepositoryEvidence, RepositoryEvidenceConfidence, RepositoryEvidenceKind,
    RepositoryFileInvocationEvidence, RepositoryFileInvocationKind,
    RepositoryFileInvocationTextRange, RepositoryFileObservation, RepositoryFileObservationKind,
    RepositoryLocalRootAuthorization, RepositoryModelError, RepositoryOutcomeKind,
    RepositoryOutcomeLinkage, RepositoryOutcomeObservation,
    RepositoryPullRequestAssociationObservation, RepositoryPullRequestIdentity,
    RepositoryVcsObservation, RepositoryVcsObservationKind, RepositoryVerifiedYieldProof,
};
pub use outcome::{
    LinkedOutcomeEvidence, LinkedOutcomeInput, LiteralOutcomeObservation, linked_outcome_evidence,
};
pub use resolver::RepositoryEvidenceResolver;
pub use shell::{
    BoundedCommitProducer, BoundedOutcomeOperation, BoundedOutcomePlan,
    BoundedOutcomePlanDisposition, MAX_COMMAND_BYTES, bounded_outcome_plan,
    bounded_pull_request_association_query, lexical_absolute,
};

#[cfg(test)]
mod tests;
