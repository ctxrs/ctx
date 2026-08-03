mod association;
mod attributor;
mod engine;
mod git;
mod identity;
mod outcome;
mod scoping;
mod shell;

use ctx_history_core::{
    CoreRecordAnnotation, GitObjectId, RepositoryAbstentionReason, RepositoryAlias,
    RepositoryFileInvocationKind, RepositoryFileInvocationTextRange, RepositoryFileObservationKind,
    RepositoryVcsObservationKind,
};
use serde_json::Value;

pub(crate) use association::{
    exact_pull_request_association, UnscopedPullRequestAssociationObservation,
};
pub(crate) use attributor::RepositoryAttributor;
#[cfg(test)]
pub(crate) use engine::attribute;
pub(crate) use engine::MAX_REPOSITORY_CANDIDATES;
use engine::{ScopedFileInput, ScopedRepositoryFileInvocationEvidence, ScopedVcsInput};
use git::CertifiedCandidate;
use identity::push_abstention;
pub(crate) use outcome::{
    linked_outcome_evidence, LinkedOutcomeEvidence, LinkedOutcomeInput, UnscopedOutcomeObservation,
};
pub(crate) use shell::{
    bounded_outcome_evidence_relevant, bounded_outcome_plan,
    bounded_pull_request_association_query, lexical_absolute, BoundedCommitProducer,
    BoundedOutcomeOperation, BoundedOutcomePlan, BoundedOutcomePlanDisposition, MAX_COMMAND_BYTES,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnscopedFileObservation {
    pub(crate) path: String,
    pub(crate) prior_path: Option<String>,
    pub(crate) kind: RepositoryFileObservationKind,
}

/// Exact request-side file intent supplied by a provider adapter.
///
/// Callers must not synthesize this from generic file observations,
/// recursively discovered paths, structured JSON, or tool results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnscopedRepositoryFileInvocationEvidence {
    pub(crate) operation_ordinal: u32,
    pub(crate) path: String,
    pub(crate) prior_path: Option<String>,
    pub(crate) kind: RepositoryFileInvocationKind,
    pub(crate) tool_name: Option<String>,
    pub(crate) normalized_text_range: Option<RepositoryFileInvocationTextRange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnscopedVcsObservation {
    pub(crate) path: Option<String>,
    pub(crate) kind: RepositoryVcsObservationKind,
    pub(crate) object_id: Option<GitObjectId>,
    pub(crate) parent_object_ids: Vec<GitObjectId>,
    pub(crate) reference: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct AttributionInput {
    pub(crate) activity_at_unix_ms: Option<i64>,
    pub(crate) provider_native_repository_aliases: Vec<RepositoryAlias>,
    pub(crate) session_cwd: Option<String>,
    pub(crate) declared_tool_workdir: Option<String>,
    pub(crate) command: Option<String>,
    pub(crate) command_disposition: CommandEvidenceDisposition,
    pub(crate) provider_native_context_ambiguous: bool,
    pub(crate) structured_content: Option<Value>,
    pub(crate) repository_file_invocation_evidence: Vec<UnscopedRepositoryFileInvocationEvidence>,
    pub(crate) file_observations: Vec<UnscopedFileObservation>,
    pub(crate) vcs_observations: Vec<UnscopedVcsObservation>,
    pub(crate) outcome_operation_repository_path: Option<String>,
    pub(crate) outcome_output_repository_path: Option<String>,
    pub(crate) outcome_observations: Vec<UnscopedOutcomeObservation>,
    pub(crate) pull_request_associations: Vec<UnscopedPullRequestAssociationObservation>,
    pub(crate) outcome_abstentions: Vec<(RepositoryAbstentionReason, &'static str)>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandEvidenceDisposition {
    #[default]
    Analyze,
    CommandTooLarge,
}

pub(crate) fn apply_annotation(
    record: &mut ctx_history_core::CoreRecord,
    annotation: CoreRecordAnnotation,
) {
    record.content.structured_content = annotation.structured_content;
    record.metadata = annotation.metadata;
    record.repository_candidate_evidence = annotation.repository_candidate_evidence;
    record.repository_bindings = annotation.repository_bindings;
    record.repository_abstentions = annotation.repository_abstentions;
    record.repository_file_invocation_evidence = annotation.repository_file_invocation_evidence;
    record.repository_file_observations = annotation.repository_file_observations;
    record.repository_vcs_observations = annotation.repository_vcs_observations;
}

#[cfg(test)]
mod tests;
