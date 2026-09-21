use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::envelope::{Confidence, Fact, FactState, Observation, ResourceRef};
use crate::protocol::{
    CORE_REPOSITORY_CONTRACT_REVISION, CoreRecord, MAX_REPOSITORY_COMMIT_OPERATION_MAPPINGS,
    ResourceKind, canonical_logical_repository_id,
};
use ctx_repository_evidence::{
    GitObjectFormat, GitObjectId, RepositoryAliasKind, RepositoryBinding,
    RepositoryCommitMappingCompleteness, RepositoryCommitOperationEvent,
    RepositoryCommitOperationKind, RepositoryCommitOperationProof, RepositoryCommitOperationState,
    RepositoryEvaluation, RepositoryFileObservationKind, RepositoryOutcomeKind,
    RepositoryOutcomeLinkage, RepositoryOutcomeObservation,
    RepositoryPullRequestAssociationObservation, RepositoryPullRequestIdentity,
    RepositoryVcsObservationKind,
};

use super::ProducerAuthorityDisposition;

#[derive(Default)]
pub(super) struct ProjectedRepositories {
    pub(super) facts: Vec<Fact>,
    pub(super) logical_binding: bool,
    pub(super) live_access: bool,
    pub(super) file_evidence: bool,
    pub(super) commit_evidence: bool,
    pub(super) pull_request_evidence: bool,
}

impl ProjectedRepositories {
    fn apply_authority_disposition(&mut self, disposition: ProducerAuthorityDisposition) {
        self.facts.retain(|fact| {
            disposition.permits_positive_authority()
                || !ctx_attribution_index::fact_type_may_confer_producer_authority(&fact.fact_type)
        });
        self.file_evidence = self
            .facts
            .iter()
            .any(|fact| fact_mentions_kind(fact, ResourceKind::File));
        self.commit_evidence = self
            .facts
            .iter()
            .any(|fact| fact_mentions_kind(fact, ResourceKind::Commit));
        self.pull_request_evidence = self
            .facts
            .iter()
            .any(|fact| fact_mentions_kind(fact, ResourceKind::PullRequest));
    }
}

fn fact_mentions_kind(fact: &Fact, kind: ResourceKind) -> bool {
    fact.subject.kind == kind
        || fact
            .object
            .as_ref()
            .is_some_and(|object| object.kind == kind)
}

pub(super) fn project_repositories(
    record: &CoreRecord,
    repository: &RepositoryEvaluation,
    observation: &Observation,
    disposition: ProducerAuthorityDisposition,
) -> ProjectedRepositories {
    let mut projected = ProjectedRepositories::default();
    let mut bindings = BTreeMap::new();
    for binding in &repository.repository_bindings {
        projected.logical_binding = true;
        bindings.insert(binding.binding_id.as_str(), binding);
    }
    for file in &repository.repository_file_observations {
        let Some(binding) = bindings.get(file.repository_binding_id.as_str()) else {
            continue;
        };
        projected.file_evidence = true;
        let subject = scoped_resource(ResourceKind::File, &file.relative_path, binding, None);
        let mut attributes = BTreeMap::from([(
            "observation_kind".to_owned(),
            file_observation_kind(file.kind).to_owned(),
        )]);
        if let Some(prior) = &file.prior_relative_path {
            attributes.insert("prior_relative_path".to_owned(), prior.clone());
        }
        projected.facts.push(core_fact(
            record,
            observation,
            "file.touched",
            subject,
            "observed_in_core",
            Some(session_resource(record)),
            attributes,
        ));
    }
    for vcs in &repository.repository_vcs_observations {
        let Some(binding) = bindings.get(vcs.repository_binding_id.as_str()) else {
            continue;
        };
        match &vcs.kind {
            RepositoryVcsObservationKind::Outcome(outcome) => {
                project_outcome(record, observation, binding, outcome, &mut projected);
                continue;
            }
            RepositoryVcsObservationKind::PullRequestAssociation(association) => {
                project_pull_request_association(
                    record,
                    observation,
                    binding,
                    association,
                    &mut projected,
                );
                continue;
            }
            _ => {}
        }
        // Generic VCS refs remain repository metadata. They never create
        // Commit or pull-request authority, regardless of how plausible their
        // object IDs or prose may look.
        if let Some(path) = &vcs.relative_path {
            projected.file_evidence = true;
            projected.facts.push(core_fact(
                record,
                observation,
                "file.touched",
                scoped_resource(ResourceKind::File, path, binding, None),
                "observed_in_core_vcs",
                Some(session_resource(record)),
                BTreeMap::from([(
                    "observation_kind".to_owned(),
                    vcs_observation_kind(&vcs.kind).to_owned(),
                )]),
            ));
        }
    }
    // Coverage and binding-side serving rows are derived only after facts that
    // could confer producer authority have been filtered by typed Core origin.
    // This prevents copied or unknown positive outcomes from advertising a
    // blame operation that has no queryable evidence.
    projected.apply_authority_disposition(disposition);

    // Repository bindings are repeated on ordinary Core records so every
    // record is independently attributable. Persisting their identical alias
    // and live-root facts on every event multiplies serving storage without
    // making a blame answer possible. Keep the complete event in the event
    // state index, but emit serving metadata only beside direct file or
    // production evidence that can actually reach a blame query.
    if projected.file_evidence || projected.commit_evidence || projected.pull_request_evidence {
        for binding in bindings.values() {
            project_binding(
                record,
                observation,
                binding,
                projected.file_evidence,
                &mut projected,
            );
        }
    }
    projected
}

fn project_binding(
    record: &CoreRecord,
    observation: &Observation,
    binding: &RepositoryBinding,
    include_live_access: bool,
    projected: &mut ProjectedRepositories,
) {
    let logical_repository_id = canonical_logical_repository_id(&binding.logical_repository_id);
    let repository = scoped_resource(
        ResourceKind::Repository,
        logical_repository_id.as_ref(),
        binding,
        None,
    );
    let worktree = binding.worktree_id.as_deref().map(|worktree_id| {
        scoped_resource(
            ResourceKind::Worktree,
            worktree_id,
            binding,
            Some(worktree_id),
        )
    });
    for alias in &binding.aliases {
        let canonical = format!(
            "https://{}/{}/{}",
            alias.host.to_ascii_lowercase(),
            alias.namespace.join("/"),
            alias.name
        );
        let mut attributes = BTreeMap::from([(
            "alias_kind".to_owned(),
            match alias.kind {
                RepositoryAliasKind::Forge => "forge",
                RepositoryAliasKind::Remote => "remote",
            }
            .to_owned(),
        )]);
        if let Some(remote_name) = &alias.remote_name {
            attributes.insert("remote_name".to_owned(), remote_name.clone());
        }
        projected.facts.push(core_fact(
            record,
            observation,
            "git.remote.alias",
            scoped_resource(ResourceKind::Remote, &canonical, binding, None),
            "aliases",
            Some(repository.clone()),
            attributes,
        ));
    }
    if include_live_access
        && let (Some(worktree), Some(access)) = (&worktree, &binding.local_root_authorization)
    {
        projected.live_access = true;
        projected.facts.push(core_fact(
            record,
            observation,
            "_ctx.repository.live_access",
            worktree.clone(),
            "authorized_by_core",
            Some(repository),
            BTreeMap::from([
                ("local_root".to_owned(), access.local_root.clone()),
                (
                    "locator_fingerprint".to_owned(),
                    hex::encode(access.local_root_authorization_fingerprint),
                ),
                (
                    "locator_fingerprint_revision".to_owned(),
                    access
                        .local_root_authorization_fingerprint_revision
                        .to_string(),
                ),
                (
                    "observed_at_unix_ms".to_owned(),
                    access.observed_at_unix_ms.to_string(),
                ),
            ]),
        ));
    }
}

fn project_outcome(
    record: &CoreRecord,
    observation: &Observation,
    binding: &RepositoryBinding,
    outcome: &RepositoryOutcomeObservation,
    projected: &mut ProjectedRepositories,
) {
    let Some(object_format) = binding.git_object_format else {
        return;
    };
    if outcome.validate_contract().is_err()
        || outcome
            .object_ids()
            .any(|object_id| object_id.format != object_format)
    {
        return;
    }

    let attributes = outcome_attributes(outcome);
    let operation_identity_sha256 = source_operation_identity(record, &outcome.linkage);
    match outcome.kind {
        RepositoryOutcomeKind::Commit => {
            let facts_before = projected.facts.len();
            if let Some(operation) = &outcome.commit_operation {
                project_commit_operation(
                    record,
                    observation,
                    binding,
                    outcome,
                    operation,
                    &operation_identity_sha256,
                    projected,
                );
            } else {
                for object_id in &outcome.produced_object_ids {
                    projected.facts.push(core_outcome_fact(
                        record,
                        observation,
                        "git.commit.produced",
                        commit_resource(object_id, binding),
                        "produced_by",
                        Some(session_resource(record)),
                        outcome.observed_at_unix_ms,
                        &operation_identity_sha256,
                        attributes.clone(),
                    ));
                }
            }
            projected.commit_evidence |= projected.facts.len() > facts_before;
        }
        RepositoryOutcomeKind::PullRequestCreated => {
            let Some(pull_request) = outcome
                .pull_request
                .as_ref()
                .filter(|identity| binding.accepts_pull_request(identity))
            else {
                return;
            };
            projected.facts.push(core_outcome_fact(
                record,
                observation,
                "forge.create",
                pull_request_resource(pull_request, binding),
                "created_by",
                Some(session_resource(record)),
                outcome.observed_at_unix_ms,
                &operation_identity_sha256,
                attributes,
            ));
            projected.pull_request_evidence = true;
        }
        RepositoryOutcomeKind::PullRequestMerged => {
            let Some(pull_request) = outcome
                .pull_request
                .as_ref()
                .filter(|identity| binding.accepts_pull_request(identity))
            else {
                return;
            };
            let Some(merge_object_id) = outcome.pull_request_merge_commit.as_ref() else {
                return;
            };
            let pull_request_resource = pull_request_resource(pull_request, binding);
            let commit_resource = commit_resource(merge_object_id, binding);
            projected.facts.push(core_outcome_fact(
                record,
                observation,
                "forge.merge",
                pull_request_resource.clone(),
                "merged_by",
                Some(session_resource(record)),
                outcome.observed_at_unix_ms,
                &operation_identity_sha256,
                attributes.clone(),
            ));
            projected.facts.push(core_outcome_fact(
                record,
                observation,
                "forge.pull_request.merged_as",
                pull_request_resource,
                "merged_as",
                Some(commit_resource),
                outcome.observed_at_unix_ms,
                &operation_identity_sha256,
                attributes,
            ));
            projected.commit_evidence = true;
            projected.pull_request_evidence = true;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn project_commit_operation(
    record: &CoreRecord,
    observation: &Observation,
    binding: &RepositoryBinding,
    outcome: &RepositoryOutcomeObservation,
    operation: &RepositoryCommitOperationEvent,
    operation_identity_sha256: &str,
    projected: &mut ProjectedRepositories,
) {
    let Some(mut attributes) = admitted_commit_operation_attributes(outcome, operation) else {
        return;
    };

    let (operation_kind, relation_family, relation_predicate, relation_class) = match operation.kind
    {
        RepositoryCommitOperationKind::Amend => {
            ("amend", "git.commit.replaced", "replaces", "replacement")
        }
        RepositoryCommitOperationKind::Rebase => {
            ("rebase", "git.commit.replaced", "replaces", "replacement")
        }
        RepositoryCommitOperationKind::CherryPick => (
            "cherry_pick",
            "git.commit.cherry_picked",
            "cherry_picked_from",
            "derivation",
        ),
    };
    attributes.extend([
        ("operation_kind".to_owned(), operation_kind.to_owned()),
        ("relation_class".to_owned(), relation_class.to_owned()),
    ]);
    let mut yielded_results = BTreeMap::new();
    for mapping in &operation.mappings {
        projected.facts.push(core_outcome_fact(
            record,
            observation,
            relation_family,
            commit_resource(&mapping.result, binding),
            relation_predicate,
            Some(commit_resource(&mapping.source, binding)),
            outcome.observed_at_unix_ms,
            operation_identity_sha256,
            attributes.clone(),
        ));
        yielded_results.insert(
            (mapping.result.format, mapping.result.hex.as_str()),
            &mapping.result,
        );
    }
    for result in yielded_results.into_values() {
        projected.facts.push(core_outcome_fact(
            record,
            observation,
            "git.commit.produced",
            commit_resource(result, binding),
            "produced_by",
            Some(session_resource(record)),
            outcome.observed_at_unix_ms,
            operation_identity_sha256,
            attributes.clone(),
        ));
    }
}

fn project_pull_request_association(
    record: &CoreRecord,
    observation: &Observation,
    binding: &RepositoryBinding,
    association: &RepositoryPullRequestAssociationObservation,
    projected: &mut ProjectedRepositories,
) {
    let Some(object_format) = binding.git_object_format else {
        return;
    };
    if association.validate_contract().is_err()
        || !binding.accepts_pull_request(&association.pull_request)
        || association
            .object_ids()
            .any(|object_id| object_id.format != object_format)
    {
        return;
    }

    let operation_identity_sha256 = source_operation_identity(record, &association.linkage);
    let pull_request = pull_request_resource(&association.pull_request, binding);
    projected.facts.push(
        core_fact(
            record,
            observation,
            "forge.pull_request.merged_as",
            pull_request.clone(),
            "merged_as",
            Some(commit_resource(&association.merged_as, binding)),
            pull_request_association_attributes(association, "linked_forge_result"),
        )
        .with_operation_identity_sha256(&operation_identity_sha256),
    );
    let contains_commit_attributes = pull_request_association_attributes(
        association,
        "linked_forge_result_and_source_certified_git_topology",
    );
    for object_id in &association.contains_commits {
        projected.facts.push(
            core_fact(
                record,
                observation,
                "forge.pull_request.contains_commit",
                pull_request.clone(),
                "contains_commit",
                Some(commit_resource(object_id, binding)),
                contains_commit_attributes.clone(),
            )
            .with_operation_identity_sha256(&operation_identity_sha256),
        );
    }
    projected.commit_evidence = true;
    projected.pull_request_evidence = true;
}

fn pull_request_association_attributes(
    association: &RepositoryPullRequestAssociationObservation,
    source_authority: &'static str,
) -> BTreeMap<String, String> {
    let mut attributes = BTreeMap::from([
        (
            "projection_authority".to_owned(),
            "repository_pull_request_association".to_owned(),
        ),
        ("source_authority".to_owned(), source_authority.to_owned()),
        (
            "association_capture_revision".to_owned(),
            association.association_capture_revision.to_string(),
        ),
        (
            "origin_event_sequence".to_owned(),
            association.linkage.origin_event_sequence.to_string(),
        ),
        (
            "result_record_sha256".to_owned(),
            hex::encode(association.linkage.result_record_sha256),
        ),
    ]);
    if let Some(provider_id) = &association.pull_request.provider_id {
        attributes.insert("pull_request_provider_id".to_owned(), provider_id.clone());
    }
    attributes
}

fn outcome_attributes(outcome: &RepositoryOutcomeObservation) -> BTreeMap<String, String> {
    let kind = match outcome.kind {
        RepositoryOutcomeKind::Commit => "commit",
        RepositoryOutcomeKind::PullRequestCreated => "pull_request_created",
        RepositoryOutcomeKind::PullRequestMerged => "pull_request_merged",
    };
    let mut attributes = BTreeMap::from([
        ("outcome_kind".to_owned(), kind.to_owned()),
        (
            "outcome_capture_revision".to_owned(),
            outcome.outcome_capture_revision.to_string(),
        ),
        (
            "origin_event_sequence".to_owned(),
            outcome.linkage.origin_event_sequence.to_string(),
        ),
        (
            "result_record_sha256".to_owned(),
            hex::encode(outcome.linkage.result_record_sha256),
        ),
    ]);
    if let Some(provider_id) = outcome
        .pull_request
        .as_ref()
        .and_then(|pull_request| pull_request.provider_id.as_ref())
    {
        attributes.insert("pull_request_provider_id".to_owned(), provider_id.clone());
    }
    attributes
}

fn admitted_commit_operation_attributes(
    outcome: &RepositoryOutcomeObservation,
    operation: &RepositoryCommitOperationEvent,
) -> Option<BTreeMap<String, String>> {
    let object_format = operation.mappings.first()?.source.format;
    if operation.mappings.len() > MAX_REPOSITORY_COMMIT_OPERATION_MAPPINGS
        || operation.state != RepositoryCommitOperationState::Asserted
        || operation.mapping_completeness != RepositoryCommitMappingCompleteness::Complete
        || !operation.unlinked_sources.is_empty()
        || !operation.unlinked_results.is_empty()
        || !matches!(
            &operation.proof,
            RepositoryCommitOperationProof::RepositoryVerifiedYield(_)
        )
        || operation.repository_verified_yields().count() != operation.mappings.len()
        || operation.mappings.iter().any(|mapping| {
            mapping.source == mapping.result
                || !exact_object_id_in_format(&mapping.source, object_format)
                || !exact_object_id_in_format(&mapping.result, object_format)
        })
    {
        return None;
    }
    let mut attributes = outcome_attributes(outcome);
    attributes.extend([
        ("operation_id".to_owned(), hex::encode(operation.event_id)),
        ("receipt_id".to_owned(), hex::encode(operation.receipt_id)),
        ("proof_class".to_owned(), "repository_verified".to_owned()),
        ("operation_state".to_owned(), "asserted".to_owned()),
        (
            "object_format".to_owned(),
            match object_format {
                GitObjectFormat::Sha1 => "sha1",
                GitObjectFormat::Sha256 => "sha256",
            }
            .to_owned(),
        ),
    ]);
    Some(attributes)
}

fn exact_object_id_in_format(object_id: &GitObjectId, format: GitObjectFormat) -> bool {
    let expected_len = match format {
        GitObjectFormat::Sha1 => 40,
        GitObjectFormat::Sha256 => 64,
    };
    object_id.format == format
        && object_id.hex.len() == expected_len
        && object_id
            .hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn source_operation_identity(record: &CoreRecord, linkage: &RepositoryOutcomeLinkage) -> String {
    let mut identity = Sha256::new();
    identity.update(b"ctx.pro.source-operation.v1\0");
    operation_identity_field(&mut identity, record.source.provider().as_bytes());
    operation_identity_field(&mut identity, &record.source.identity().digest());
    operation_identity_field(&mut identity, &record.session_id.digest());
    operation_identity_optional_field(
        &mut identity,
        record.provider_session_id.as_deref().map(str::as_bytes),
    );
    operation_identity_field(&mut identity, linkage.provider.as_bytes());
    operation_identity_field(&mut identity, linkage.origin_call_id.as_bytes());
    operation_identity_field(&mut identity, linkage.result_call_id.as_bytes());
    identity.update(linkage.origin_event_sequence.to_be_bytes());
    identity.update((linkage.continuation_call_id_sha256.len() as u128).to_be_bytes());
    for continuation in &linkage.continuation_call_id_sha256 {
        operation_identity_field(&mut identity, continuation);
    }
    operation_identity_field(&mut identity, &linkage.result_record_sha256);
    hex::encode(identity.finalize())
}

fn operation_identity_optional_field(identity: &mut Sha256, value: Option<&[u8]>) {
    identity.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        operation_identity_field(identity, value);
    }
}

fn operation_identity_field(identity: &mut Sha256, value: &[u8]) {
    identity.update((value.len() as u128).to_be_bytes());
    identity.update(value);
}

fn commit_resource(object_id: &GitObjectId, binding: &RepositoryBinding) -> ResourceRef {
    scoped_resource(ResourceKind::Commit, &object_id.hex, binding, None)
}

fn pull_request_resource(
    pull_request: &RepositoryPullRequestIdentity,
    binding: &RepositoryBinding,
) -> ResourceRef {
    let alias = &pull_request.forge_repository;
    let mut path = alias.namespace.clone();
    path.push(alias.name.clone());
    scoped_resource(
        ResourceKind::PullRequest,
        &format!(
            "{}/{}/pull_request/{}",
            alias.host.to_ascii_lowercase(),
            path.join("/"),
            pull_request.number
        ),
        binding,
        None,
    )
}

fn core_fact(
    record: &CoreRecord,
    observation: &Observation,
    fact_type: &str,
    subject: ResourceRef,
    predicate: &str,
    object: Option<ResourceRef>,
    mut attributes: BTreeMap<String, String>,
) -> Fact {
    attributes.insert("observation_origin".to_owned(), "direct".to_owned());
    core_fact_at(
        record,
        observation,
        fact_type,
        subject,
        predicate,
        object,
        record.occurred_at_unix_ms,
        attributes,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "Project the existing fact fields and operation identity without changing their typed call contract."
)]
fn core_outcome_fact(
    record: &CoreRecord,
    observation: &Observation,
    fact_type: &str,
    subject: ResourceRef,
    predicate: &str,
    object: Option<ResourceRef>,
    occurred_at_unix_ms: i64,
    operation_identity_sha256: &str,
    attributes: BTreeMap<String, String>,
) -> Fact {
    core_fact_at(
        record,
        observation,
        fact_type,
        subject,
        predicate,
        object,
        Some(occurred_at_unix_ms),
        attributes,
    )
    .with_operation_identity_sha256(operation_identity_sha256)
}

#[allow(clippy::too_many_arguments)]
fn core_fact_at(
    record: &CoreRecord,
    observation: &Observation,
    fact_type: &str,
    subject: ResourceRef,
    predicate: &str,
    object: Option<ResourceRef>,
    occurred_at_unix_ms: Option<i64>,
    attributes: BTreeMap<String, String>,
) -> Fact {
    Fact::create(
        fact_type,
        subject,
        predicate,
        object,
        occurred_at_unix_ms.map(|value| value.to_string()),
        Confidence::Verified,
        FactState::Asserted,
        "core.repository",
        CORE_REPOSITORY_CONTRACT_REVISION.to_string(),
        record.session_id.to_string(),
        record.root_session_id.map(|root| root.to_string()),
        vec![crate::envelope::EvidenceRef::from_observation(
            observation,
            "core_typed_repository_observation",
        )],
        attributes,
    )
}

fn scoped_resource(
    kind: ResourceKind,
    id: &str,
    binding: &RepositoryBinding,
    worktree_id: Option<&str>,
) -> ResourceRef {
    let logical_repository_id = canonical_logical_repository_id(&binding.logical_repository_id);
    worktree_id.map_or_else(
        || ResourceRef::in_repository(kind, id, logical_repository_id.as_ref()),
        |worktree_id| {
            ResourceRef::in_worktree(kind, id, logical_repository_id.as_ref(), worktree_id)
        },
    )
}

fn session_resource(record: &CoreRecord) -> ResourceRef {
    ResourceRef::new(ResourceKind::Session, record.session_id.to_string())
}

pub(super) fn has_repository_candidate(repository: &RepositoryEvaluation) -> bool {
    let candidate = &repository.repository_candidate_evidence;
    !repository.repository_bindings.is_empty()
        || !repository.repository_abstentions.is_empty()
        || !candidate.candidates.is_empty()
}

const fn file_observation_kind(kind: RepositoryFileObservationKind) -> &'static str {
    match kind {
        RepositoryFileObservationKind::Read => "read",
        RepositoryFileObservationKind::Created => "created",
        RepositoryFileObservationKind::Modified => "modified",
        RepositoryFileObservationKind::Deleted => "deleted",
        RepositoryFileObservationKind::Renamed => "renamed",
        RepositoryFileObservationKind::Unknown => "unknown",
    }
}

const fn vcs_observation_kind(kind: &RepositoryVcsObservationKind) -> &'static str {
    match kind {
        RepositoryVcsObservationKind::Head => "head",
        RepositoryVcsObservationKind::Commit => "commit",
        RepositoryVcsObservationKind::Branch => "branch",
        RepositoryVcsObservationKind::Worktree => "worktree",
        RepositoryVcsObservationKind::Change => "change",
        RepositoryVcsObservationKind::Reference => "reference",
        RepositoryVcsObservationKind::Outcome(_) => "outcome",
        RepositoryVcsObservationKind::PullRequestAssociation(_) => "pull_request_association",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn operation_outcome(
        operation: RepositoryCommitOperationEvent,
    ) -> RepositoryOutcomeObservation {
        RepositoryOutcomeObservation {
            kind: RepositoryOutcomeKind::Commit,
            produced_object_ids: Vec::new(),
            commit_operation: Some(operation),
            pull_request: None,
            pull_request_merge_commit: None,
            observed_at_unix_ms: 1,
            linkage: RepositoryOutcomeLinkage {
                provider: "fixture".to_owned(),
                origin_call_id: "origin".to_owned(),
                result_call_id: "result".to_owned(),
                origin_event_sequence: 1,
                continuation_call_id_sha256: Vec::new(),
                result_record_sha256: [3; 32],
            },
            outcome_capture_revision: crate::protocol::CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
        }
    }

    #[test]
    fn empty_commit_operation_mapping_abstains_without_panicking() {
        let operation = RepositoryCommitOperationEvent {
            event_id: [1; 32],
            receipt_id: [2; 32],
            kind: RepositoryCommitOperationKind::CherryPick,
            mappings: Vec::new(),
            unlinked_sources: Vec::new(),
            unlinked_results: Vec::new(),
            mapping_completeness: RepositoryCommitMappingCompleteness::Complete,
            state: RepositoryCommitOperationState::Asserted,
            proof: RepositoryCommitOperationProof::RecordExact,
        };
        let outcome = operation_outcome(operation.clone());

        assert!(admitted_commit_operation_attributes(&outcome, &operation).is_none());
    }

    #[test]
    fn thirty_three_commit_operation_mappings_abstain() {
        let mappings = (1_u64..=33)
            .map(|value| crate::protocol::RepositoryCommitMapping {
                source: GitObjectId {
                    format: GitObjectFormat::Sha1,
                    hex: format!("{value:040x}"),
                },
                result: GitObjectId {
                    format: GitObjectFormat::Sha1,
                    hex: format!("{:040x}", value + 100),
                },
            })
            .collect::<Vec<_>>();
        let proof = crate::protocol::RepositoryVerifiedYieldProof {
            command_pre_head: Some(mappings[0].source.clone()),
            sequencer_pre_head: Some(mappings[0].source.clone()),
            exact_source_oids: mappings
                .iter()
                .map(|mapping| mapping.source.clone())
                .collect(),
            command_post_head: mappings[0].result.clone(),
            repository_geometry_before_sha256: [4; 32],
            repository_geometry_after_sha256: [4; 32],
            exact_result_map_sha256: [5; 32],
            drift_excluded: true,
            mutation_excluded: true,
        };
        let operation = RepositoryCommitOperationEvent {
            event_id: [1; 32],
            receipt_id: [2; 32],
            kind: RepositoryCommitOperationKind::Rebase,
            mappings,
            unlinked_sources: Vec::new(),
            unlinked_results: Vec::new(),
            mapping_completeness: RepositoryCommitMappingCompleteness::Complete,
            state: RepositoryCommitOperationState::Asserted,
            proof: RepositoryCommitOperationProof::RepositoryVerifiedYield(Box::new(proof)),
        };
        let outcome = operation_outcome(operation.clone());

        assert_eq!(operation.repository_verified_yields().count(), 33);
        assert!(admitted_commit_operation_attributes(&outcome, &operation).is_none());
    }
}
