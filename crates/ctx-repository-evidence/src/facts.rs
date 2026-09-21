//! Bounded literal facts accepted by repository-evidence evaluation.
//!
//! This module deliberately owns no capture-envelope or provider-adapter
//! types. The Core adapter translates immutable Core
//! literal fields into these values without introducing inferred evidence.

use super::{
    LinkedOutcomeEvidence, LiteralOutcomeObservation, LiteralPullRequestAssociationObservation,
};
use crate::model::{
    GitObjectId, RepositoryAbstentionReason, RepositoryAlias, RepositoryFileInvocationKind,
    RepositoryFileInvocationTextRange, RepositoryFileObservationKind, RepositoryVcsObservationKind,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiteralFileObservation {
    pub path: String,
    pub prior_path: Option<String>,
    pub kind: RepositoryFileObservationKind,
}

/// Exact request-side file intent supplied from neutral record facts.
///
/// Callers must not synthesize this from generic file observations,
/// recursively discovered paths, structured JSON, or tool results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiteralRepositoryFileInvocation {
    pub operation_ordinal: u32,
    pub path: String,
    pub prior_path: Option<String>,
    pub kind: RepositoryFileInvocationKind,
    pub tool_name: Option<String>,
    pub normalized_text_range: Option<RepositoryFileInvocationTextRange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiteralVcsObservation {
    pub path: Option<String>,
    pub kind: RepositoryVcsObservationKind,
    pub object_id: Option<GitObjectId>,
    pub parent_object_ids: Vec<GitObjectId>,
    pub reference: Option<String>,
}

/// One bounded set of neutral literal facts for an immutable Core activity.
///
/// The Core adapter may carry only provider-declared cwd/workdir,
/// literal command text or tool arguments, exact native call/result linkage
/// and status, and literal file, VCS, commit, or PR strings here. Every
/// repository identity, candidate, confidence, outcome, and abstention below
/// this boundary is a derived conclusion.
#[derive(Debug, Default, Clone)]
pub struct NeutralRepositoryFacts {
    pub activity_at_unix_ms: Option<i64>,
    pub provider_native_repository_aliases: Vec<RepositoryAlias>,
    pub session_cwd: Option<String>,
    pub declared_tool_workdir: Option<String>,
    pub command: Option<String>,
    pub command_disposition: CommandLiteralDisposition,
    pub provider_native_context_ambiguous: bool,
    pub repository_file_invocation_evidence: Vec<LiteralRepositoryFileInvocation>,
    pub file_observations: Vec<LiteralFileObservation>,
    pub vcs_observations: Vec<LiteralVcsObservation>,
    pub outcome_operation_repository_path: Option<String>,
    pub outcome_output_repository_path: Option<String>,
    pub outcome_observations: Vec<LiteralOutcomeObservation>,
    pub pull_request_associations: Vec<LiteralPullRequestAssociationObservation>,
    pub outcome_abstentions: Vec<(RepositoryAbstentionReason, &'static str)>,
}

impl NeutralRepositoryFacts {
    pub fn apply_linked_outcome_evidence(&mut self, linked: LinkedOutcomeEvidence) {
        self.provider_native_repository_aliases = linked.provider_native_repository_aliases;
        self.outcome_operation_repository_path = linked.outcome_operation_repository_path;
        self.outcome_output_repository_path = linked.outcome_output_repository_path;
        self.outcome_observations = linked.outcomes;
        self.pull_request_associations = linked.pull_request_associations;
        self.outcome_abstentions = linked.abstentions;
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum CommandLiteralDisposition {
    #[default]
    Analyze,
    CommandTooLarge,
}
