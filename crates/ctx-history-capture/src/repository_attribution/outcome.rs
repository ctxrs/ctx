use std::{collections::HashSet, path::Path};

use ctx_history_core::{
    GitObjectFormat, GitObjectId, RepositoryAbstentionReason, RepositoryAlias,
    RepositoryCommitMapping, RepositoryCommitOperationEvent, RepositoryCommitOperationKind,
    RepositoryCommitOperationState, RepositoryOutcomeKind, RepositoryOutcomeLinkage,
    RepositoryOutcomeObservation, CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
    MAX_REPOSITORY_COMMIT_OPERATION_MAPPINGS,
};
use serde_json::{Map, Value};

#[path = "outcome/pull_request.rs"]
mod pull_request;

use pull_request::{exact_pr_create_result, exact_pr_merge_result, pr_matches_plan};

use super::shell::BoundedCommitProducer;
use super::{
    bounded_outcome_plan, exact_pull_request_association, lexical_absolute,
    BoundedOutcomeOperation, BoundedOutcomePlan, BoundedOutcomePlanDisposition,
    UnscopedPullRequestAssociationObservation,
};
use crate::OutputOutcome;

const MAX_EXACT_OUTCOME_OBJECTS: usize = 256;
const MAX_LINKAGE_CALL_ID_BYTES: usize = 16 * 1024;
const MAX_EXACT_OUTCOME_OUTPUT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) enum UnscopedOutcomeObservation {
    Exact(Box<RepositoryOutcomeObservation>),
    DeferredCommit(DeferredCommitObservation),
    DeferredCommitOperation(DeferredCommitOperationObservation),
    DeferredCherryPick(DeferredCherryPickObservation),
}

impl From<RepositoryOutcomeObservation> for UnscopedOutcomeObservation {
    fn from(value: RepositoryOutcomeObservation) -> Self {
        Self::Exact(Box::new(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct DeferredCommitObservation {
    pub(crate) oid_prefix: String,
    pub(crate) subject: String,
    pub(crate) producer: BoundedCommitProducer,
    pub(crate) observed_at_unix_ms: i64,
    pub(crate) linkage: RepositoryOutcomeLinkage,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct DeferredCommitOperationObservation {
    pub(crate) kind: RepositoryCommitOperationKind,
    pub(crate) mappings: Vec<RepositoryCommitMapping>,
    pub(crate) command_pre_head: Option<GitObjectId>,
    pub(crate) sequencer_pre_head: Option<GitObjectId>,
    pub(crate) command_post_head: GitObjectId,
    pub(crate) observed_at_unix_ms: i64,
    pub(crate) linkage: RepositoryOutcomeLinkage,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct DeferredCherryPickObservation {
    pub(crate) source: GitObjectId,
    pub(crate) result_oid_prefix: String,
    pub(crate) result_subject: String,
    pub(crate) observed_at_unix_ms: i64,
    pub(crate) linkage: RepositoryOutcomeLinkage,
}

pub(crate) struct LinkedOutcomeInput<'a> {
    pub(crate) provider: &'static str,
    pub(crate) command: &'a str,
    pub(crate) session_cwd: Option<&'a str>,
    pub(crate) declared_workdir: Option<&'a str>,
    pub(crate) origin_call_id: &'a str,
    pub(crate) result_call_id: &'a str,
    pub(crate) origin_event_sequence: u64,
    pub(crate) continuation_call_id_sha256: &'a [[u8; 32]],
    pub(crate) result_record_sha256: [u8; 32],
    pub(crate) observed_at_unix_ms: i64,
    pub(crate) result_outcome: OutputOutcome,
    pub(crate) result_output: &'a Value,
    /// A provider-native commit field has first-present precedence. A present
    /// short or malformed value fails closed instead of falling back to text.
    pub(crate) structured_commit_oid: Option<&'a str>,
    pub(crate) output_repository_path: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LinkedOutcomeEvidence {
    pub(crate) provider_native_repository_aliases: Vec<RepositoryAlias>,
    pub(crate) outcome_operation_repository_path: Option<String>,
    pub(crate) outcome_output_repository_path: Option<String>,
    pub(crate) outcomes: Vec<UnscopedOutcomeObservation>,
    pub(crate) pull_request_associations: Vec<UnscopedPullRequestAssociationObservation>,
    pub(crate) abstentions: Vec<(RepositoryAbstentionReason, &'static str)>,
}

pub(crate) fn linked_outcome_evidence(
    input: LinkedOutcomeInput<'_>,
) -> Option<LinkedOutcomeEvidence> {
    let declared_base = input
        .declared_workdir
        .and_then(|value| lexical_absolute(value, None));
    let base = input
        .declared_workdir
        .or(input.session_cwd)
        .and_then(|value| lexical_absolute(value, None));
    if let Some(base) = declared_base.as_deref().filter(|base| {
        super::shell::bounded_pull_request_association_query(input.command, base).is_some()
    }) {
        let operation_path = Some(base.to_string_lossy().into_owned());
        if input.result_outcome != OutputOutcome::Success {
            return Some(abstained(
                operation_path,
                None,
                RepositoryAbstentionReason::OutcomeResultInadmissible,
                "recognized_pull_request_association_query_did_not_succeed",
            ));
        }
        let Some(linkage) = exact_result_linkage(&input) else {
            return Some(abstained(
                operation_path,
                None,
                RepositoryAbstentionReason::ProviderOutputUnjoined,
                "pull_request_association_linkage_is_missing_or_ambiguous",
            ));
        };
        let Some(association) = exact_pull_request_association(
            input.command,
            base.to_str()?,
            input.result_output,
            linkage,
        ) else {
            return Some(abstained(
                operation_path,
                None,
                RepositoryAbstentionReason::OutcomeResultInadmissible,
                "linked_result_has_no_exact_pull_request_association",
            ));
        };
        return Some(LinkedOutcomeEvidence {
            provider_native_repository_aliases: vec![association
                .pull_request
                .forge_repository
                .clone()],
            outcome_operation_repository_path: Some(association.repository_path.clone()),
            outcome_output_repository_path: None,
            outcomes: Vec::new(),
            pull_request_associations: vec![association],
            abstentions: Vec::new(),
        });
    }
    let plan = match base.as_deref() {
        Some(base) => bounded_outcome_plan(input.command, base),
        None => match bounded_outcome_plan(input.command, Path::new("/")) {
            BoundedOutcomePlanDisposition::Planned(_) => {
                return Some(abstained(
                    None,
                    None,
                    RepositoryAbstentionReason::OutcomeRepositoryUnbound,
                    "outcome_command_has_no_bounded_base",
                ));
            }
            disposition => disposition,
        },
    };
    let plan = match plan {
        BoundedOutcomePlanDisposition::Unrecognized => return None,
        BoundedOutcomePlanDisposition::Abstained {
            reason,
            detail,
            plan,
        } => {
            let (operation_path, output_path) =
                plan.as_deref().map(plan_paths).unwrap_or((None, None));
            return Some(abstained(operation_path, output_path, reason, detail));
        }
        BoundedOutcomePlanDisposition::Planned(plan) => plan,
    };
    let (operation_path, planned_output_path) = plan_paths(&plan);
    let native_output_path = input.output_repository_path.map(str::to_owned);
    if planned_output_path
        .as_deref()
        .zip(native_output_path.as_deref())
        .is_some_and(|(planned, native)| planned != native)
    {
        return Some(abstained(
            operation_path,
            native_output_path,
            RepositoryAbstentionReason::ConflictingIdentity,
            "planned_and_native_outcome_output_routes_conflict",
        ));
    }
    let output_path = native_output_path.or(planned_output_path);

    if input.result_outcome != OutputOutcome::Success {
        return Some(abstained(
            operation_path,
            output_path,
            RepositoryAbstentionReason::OutcomeResultInadmissible,
            "recognized_outcome_command_did_not_succeed",
        ));
    }
    let Some(linkage) = exact_result_linkage(&input) else {
        return Some(abstained(
            operation_path,
            output_path,
            RepositoryAbstentionReason::ProviderOutputUnjoined,
            "outcome_linkage_is_missing_oversized_or_ambiguous",
        ));
    };
    let parsed = parse_operation_result(
        input.result_output,
        input.structured_commit_oid,
        &plan,
        input.observed_at_unix_ms,
        linkage,
    );
    match parsed {
        OperationResult::Exact { outcome, aliases } => {
            let operation_unlinked = outcome.commit_operation.as_ref().is_some_and(|operation| {
                operation.state != RepositoryCommitOperationState::Asserted
            });
            Some(LinkedOutcomeEvidence {
                provider_native_repository_aliases: aliases,
                outcome_operation_repository_path: operation_path,
                outcome_output_repository_path: output_path,
                outcomes: vec![UnscopedOutcomeObservation::Exact(outcome)],
                pull_request_associations: Vec::new(),
                abstentions: operation_unlinked
                    .then_some((
                        RepositoryAbstentionReason::HistoryRewriteUnlinked,
                        "commit_operation_has_unlinked_source_or_result",
                    ))
                    .into_iter()
                    .collect(),
            })
        }
        OperationResult::Deferred(deferred) => Some(LinkedOutcomeEvidence {
            provider_native_repository_aliases: Vec::new(),
            outcome_operation_repository_path: operation_path,
            outcome_output_repository_path: output_path,
            outcomes: vec![UnscopedOutcomeObservation::DeferredCommit(deferred)],
            pull_request_associations: Vec::new(),
            abstentions: Vec::new(),
        }),
        OperationResult::DeferredOperation(deferred) => Some(LinkedOutcomeEvidence {
            provider_native_repository_aliases: Vec::new(),
            outcome_operation_repository_path: operation_path,
            outcome_output_repository_path: output_path,
            outcomes: vec![UnscopedOutcomeObservation::DeferredCommitOperation(
                deferred,
            )],
            pull_request_associations: Vec::new(),
            abstentions: Vec::new(),
        }),
        OperationResult::DeferredCherryPick(deferred) => Some(LinkedOutcomeEvidence {
            provider_native_repository_aliases: Vec::new(),
            outcome_operation_repository_path: operation_path,
            outcome_output_repository_path: output_path,
            outcomes: vec![UnscopedOutcomeObservation::DeferredCherryPick(deferred)],
            pull_request_associations: Vec::new(),
            abstentions: Vec::new(),
        }),
        OperationResult::RewriteUnlinked => Some(abstained(
            operation_path,
            output_path,
            RepositoryAbstentionReason::HistoryRewriteUnlinked,
            "commit_operation_has_no_exact_source_result_mapping",
        )),
        OperationResult::Inadmissible => Some(abstained(
            operation_path,
            output_path,
            RepositoryAbstentionReason::OutcomeResultInadmissible,
            "linked_result_has_no_exact_operation_specific_outcome",
        )),
    }
}

fn exact_result_linkage(input: &LinkedOutcomeInput<'_>) -> Option<RepositoryOutcomeLinkage> {
    if input.origin_call_id.is_empty()
        || input.result_call_id.is_empty()
        || input.origin_call_id.len() > MAX_LINKAGE_CALL_ID_BYTES
        || input.result_call_id.len() > MAX_LINKAGE_CALL_ID_BYTES
        || input.result_record_sha256 == [0; 32]
        || input.continuation_call_id_sha256.contains(&[0; 32])
        || input
            .continuation_call_id_sha256
            .iter()
            .collect::<HashSet<_>>()
            .len()
            != input.continuation_call_id_sha256.len()
    {
        return None;
    }
    Some(RepositoryOutcomeLinkage {
        provider: input.provider.to_owned(),
        origin_call_id: input.origin_call_id.to_owned(),
        result_call_id: input.result_call_id.to_owned(),
        origin_event_sequence: input.origin_event_sequence,
        continuation_call_id_sha256: input.continuation_call_id_sha256.to_vec(),
        result_record_sha256: input.result_record_sha256,
    })
}

fn abstained(
    operation_path: Option<String>,
    output_path: Option<String>,
    reason: RepositoryAbstentionReason,
    detail: &'static str,
) -> LinkedOutcomeEvidence {
    LinkedOutcomeEvidence {
        provider_native_repository_aliases: Vec::new(),
        outcome_operation_repository_path: operation_path,
        outcome_output_repository_path: output_path,
        outcomes: Vec::new(),
        pull_request_associations: Vec::new(),
        abstentions: vec![(reason, detail)],
    }
}

enum OperationResult {
    Exact {
        outcome: Box<RepositoryOutcomeObservation>,
        aliases: Vec<RepositoryAlias>,
    },
    RewriteUnlinked,
    Deferred(DeferredCommitObservation),
    DeferredOperation(DeferredCommitOperationObservation),
    DeferredCherryPick(DeferredCherryPickObservation),
    Inadmissible,
}

fn parse_operation_result(
    output: &Value,
    structured_commit_oid: Option<&str>,
    plan: &BoundedOutcomePlan,
    observed_at_unix_ms: i64,
    linkage: RepositoryOutcomeLinkage,
) -> OperationResult {
    match plan.operation {
        BoundedOutcomeOperation::Commit {
            producer,
            operation_kind,
            exact_oid_output,
            ..
        } => {
            let operation_source_oid = plan.operation_source_oid.as_deref();
            let default_result = || {
                if let Some(value) = structured_commit_oid {
                    object_id(value).map(|object_id| (vec![object_id], Vec::new()))
                } else {
                    exact_commit_result(
                        output,
                        producer,
                        exact_oid_output,
                        plan.machine_output_isolated,
                    )
                }
            };
            let (parsed, explicit_pre_head, explicit_post_head) =
                if operation_kind == Some(RepositoryCommitOperationKind::CherryPick) {
                    match exact_cherry_pick_result(output) {
                        Some((pre_head, mapping)) => (
                            Some((vec![mapping.result.clone()], vec![mapping])),
                            Some(pre_head),
                            None,
                        ),
                        None => (default_result(), None, None),
                    }
                } else if operation_kind == Some(RepositoryCommitOperationKind::Rebase) {
                    exact_rebase_result(output).map_or_else(
                        || (default_result(), None, None),
                        |(pre_head, post_head, produced, mappings)| {
                            (Some((produced, mappings)), Some(pre_head), Some(post_head))
                        },
                    )
                } else {
                    (default_result(), None, None)
                };
            let Some((produced_object_ids, mut mappings)) = parsed else {
                if operation_kind == Some(RepositoryCommitOperationKind::CherryPick) {
                    let deferred = structured_commit_oid.is_none().then(|| {
                        let source = object_id(operation_source_oid?)?;
                        let (result_oid_prefix, result_subject) =
                            deferred_commit_result(output, producer)?;
                        Some(DeferredCherryPickObservation {
                            source,
                            result_oid_prefix,
                            result_subject,
                            observed_at_unix_ms,
                            linkage,
                        })
                    });
                    return deferred.flatten().map_or(
                        OperationResult::RewriteUnlinked,
                        OperationResult::DeferredCherryPick,
                    );
                }
                if operation_kind.is_some() {
                    return OperationResult::RewriteUnlinked;
                }
                let deferred = structured_commit_oid
                    .is_none()
                    .then(|| deferred_commit_result(output, producer))
                    .flatten();
                return deferred.map_or(OperationResult::Inadmissible, |(oid_prefix, subject)| {
                    OperationResult::Deferred(DeferredCommitObservation {
                        oid_prefix,
                        subject,
                        producer,
                        observed_at_unix_ms,
                        linkage,
                    })
                });
            };
            let Some(operation_kind) = operation_kind else {
                if !mappings.is_empty() {
                    return OperationResult::Inadmissible;
                }
                return OperationResult::Exact {
                    outcome: Box::new(RepositoryOutcomeObservation {
                        kind: RepositoryOutcomeKind::Commit,
                        produced_object_ids,
                        commit_operation: None,
                        pull_request: None,
                        pull_request_merge_commit: None,
                        observed_at_unix_ms,
                        linkage,
                        outcome_capture_revision: CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
                    }),
                    aliases: Vec::new(),
                };
            };

            if operation_kind == RepositoryCommitOperationKind::CherryPick && mappings.is_empty() {
                let Some(source) = operation_source_oid.and_then(object_id) else {
                    return OperationResult::Inadmissible;
                };
                let [result] = produced_object_ids.as_slice() else {
                    return OperationResult::Inadmissible;
                };
                mappings.push(RepositoryCommitMapping {
                    source,
                    result: result.clone(),
                });
            }
            if mappings.is_empty() {
                let mut unlinked_sources = mappings
                    .iter()
                    .map(|mapping| mapping.source.clone())
                    .collect::<Vec<_>>();
                unlinked_sources.extend(operation_source_oid.and_then(object_id));
                let mut unlinked_results = mappings
                    .iter()
                    .map(|mapping| mapping.result.clone())
                    .collect::<Vec<_>>();
                unlinked_results.extend(produced_object_ids);
                let Ok(commit_operation) = RepositoryCommitOperationEvent::record_exact_unlinked(
                    &linkage,
                    operation_kind,
                    unlinked_sources,
                    unlinked_results,
                    RepositoryCommitOperationState::Ambiguous,
                ) else {
                    return OperationResult::Inadmissible;
                };
                return OperationResult::Exact {
                    outcome: Box::new(RepositoryOutcomeObservation {
                        kind: RepositoryOutcomeKind::Commit,
                        produced_object_ids: Vec::new(),
                        commit_operation: Some(commit_operation),
                        pull_request: None,
                        pull_request_merge_commit: None,
                        observed_at_unix_ms,
                        linkage,
                        outcome_capture_revision: CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
                    }),
                    aliases: Vec::new(),
                };
            }
            mappings.sort();
            if mappings.len() > MAX_REPOSITORY_COMMIT_OPERATION_MAPPINGS
                || operation_source_oid.is_some_and(|source| {
                    object_id(source).is_none_or(|source| {
                        !mappings.iter().any(|mapping| mapping.source == source)
                    })
                })
            {
                return OperationResult::Inadmissible;
            }
            let (command_pre_head, sequencer_pre_head, command_post_head) = match operation_kind {
                RepositoryCommitOperationKind::Amend if mappings.len() == 1 => (
                    Some(mappings[0].source.clone()),
                    None,
                    mappings[0].result.clone(),
                ),
                RepositoryCommitOperationKind::Rebase => {
                    let pre_head = match explicit_pre_head {
                        Some(pre_head)
                            if mappings.iter().any(|mapping| mapping.source == pre_head) =>
                        {
                            pre_head
                        }
                        None if mappings.len() == 1 => mappings[0].source.clone(),
                        _ => return OperationResult::Inadmissible,
                    };
                    let post_head = match explicit_post_head {
                        Some(post_head)
                            if mappings.iter().any(|mapping| mapping.result == post_head) =>
                        {
                            post_head
                        }
                        None if mappings.len() == 1 => mappings[0].result.clone(),
                        _ => return OperationResult::Inadmissible,
                    };
                    (Some(pre_head.clone()), Some(pre_head), post_head)
                }
                RepositoryCommitOperationKind::CherryPick if mappings.len() == 1 => (
                    explicit_pre_head.clone(),
                    explicit_pre_head,
                    mappings[0].result.clone(),
                ),
                _ => return OperationResult::Inadmissible,
            };
            OperationResult::DeferredOperation(DeferredCommitOperationObservation {
                kind: operation_kind,
                command_pre_head,
                sequencer_pre_head,
                command_post_head,
                mappings,
                observed_at_unix_ms,
                linkage,
            })
        }
        BoundedOutcomeOperation::PullRequestCreate => {
            let Some(pull_request) = exact_pr_create_result(output) else {
                return OperationResult::Inadmissible;
            };
            if !pr_matches_plan(&pull_request, plan) {
                return OperationResult::Inadmissible;
            }
            let alias = pull_request.forge_repository.clone();
            OperationResult::Exact {
                outcome: Box::new(RepositoryOutcomeObservation {
                    kind: RepositoryOutcomeKind::PullRequestCreated,
                    produced_object_ids: Vec::new(),
                    commit_operation: None,
                    pull_request: Some(pull_request),
                    pull_request_merge_commit: None,
                    observed_at_unix_ms,
                    linkage,
                    outcome_capture_revision: CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
                }),
                aliases: vec![alias],
            }
        }
        BoundedOutcomeOperation::PullRequestMerge => {
            let Some((pull_request, merge_oid)) = exact_pr_merge_result(output) else {
                return OperationResult::Inadmissible;
            };
            if !pr_matches_plan(&pull_request, plan) {
                return OperationResult::Inadmissible;
            }
            let alias = pull_request.forge_repository.clone();
            OperationResult::Exact {
                outcome: Box::new(RepositoryOutcomeObservation {
                    kind: RepositoryOutcomeKind::PullRequestMerged,
                    produced_object_ids: Vec::new(),
                    commit_operation: None,
                    pull_request: Some(pull_request),
                    pull_request_merge_commit: Some(merge_oid),
                    observed_at_unix_ms,
                    linkage,
                    outcome_capture_revision: CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
                }),
                aliases: vec![alias],
            }
        }
    }
}

fn exact_cherry_pick_result(output: &Value) -> Option<(GitObjectId, RepositoryCommitMapping)> {
    let object = exact_json_object(output)?;
    if !exact_keys(object, &["pre_head_oid", "source_oid", "result_oid"]) {
        return None;
    }
    Some((
        object_id(object.get("pre_head_oid")?.as_str()?)?,
        RepositoryCommitMapping {
            source: object_id(object.get("source_oid")?.as_str()?)?,
            result: object_id(object.get("result_oid")?.as_str()?)?,
        },
    ))
}

fn exact_rebase_result(
    output: &Value,
) -> Option<(
    GitObjectId,
    GitObjectId,
    Vec<GitObjectId>,
    Vec<RepositoryCommitMapping>,
)> {
    let object = exact_json_object(output)?;
    if !exact_keys(object, &["pre_head_oid", "post_head_oid", "replacements"]) {
        return None;
    }
    let pre_head = object_id(object.get("pre_head_oid")?.as_str()?)?;
    let post_head = object_id(object.get("post_head_oid")?.as_str()?)?;
    let mappings = exact_replacements(object.get("replacements")?)?;
    if !mappings.iter().any(|mapping| mapping.source == pre_head)
        || !mappings.iter().any(|mapping| mapping.result == post_head)
    {
        return None;
    }
    let produced = mappings
        .iter()
        .map(|mapping| mapping.result.clone())
        .collect();
    Some((pre_head, post_head, produced, mappings))
}

fn exact_commit_result(
    output: &Value,
    producer: BoundedCommitProducer,
    exact_oid_output: bool,
    machine_output_isolated: bool,
) -> Option<(Vec<GitObjectId>, Vec<RepositoryCommitMapping>)> {
    if exact_oid_output {
        if let Some(object_id) = exact_machine_oid_output(output, producer, machine_output_isolated)
        {
            return Some((vec![object_id], Vec::new()));
        }
    }
    let object = exact_json_object(output)?;
    if exact_keys(object, &["commit_oid"]) || exact_keys(object, &["new_oid"]) {
        let key = if object.contains_key("commit_oid") {
            "commit_oid"
        } else {
            "new_oid"
        };
        return Some((vec![object_id(object.get(key)?.as_str()?)?], Vec::new()));
    }
    if exact_keys(object, &["old_oid", "new_oid"]) {
        let replaced = object_id(object.get("old_oid")?.as_str()?)?;
        let replacement = object_id(object.get("new_oid")?.as_str()?)?;
        let mappings = vec![RepositoryCommitMapping {
            source: replaced,
            result: replacement.clone(),
        }];
        valid_mappings(&mappings).then_some((vec![replacement], mappings))
    } else if exact_keys(object, &["replacements"]) {
        let lineage = exact_replacements(object.get("replacements")?)?;
        let produced = lineage
            .iter()
            .map(|mapping| mapping.result.clone())
            .collect();
        Some((produced, lineage))
    } else {
        None
    }
}

fn exact_replacements(value: &Value) -> Option<Vec<RepositoryCommitMapping>> {
    let values = value.as_array()?;
    if values.is_empty() || values.len() > MAX_REPOSITORY_COMMIT_OPERATION_MAPPINGS {
        return None;
    }
    let mut mappings = Vec::with_capacity(values.len());
    for value in values {
        let pair = value.as_object()?;
        if !exact_keys(pair, &["old_oid", "new_oid"]) {
            return None;
        }
        mappings.push(RepositoryCommitMapping {
            source: object_id(pair.get("old_oid")?.as_str()?)?,
            result: object_id(pair.get("new_oid")?.as_str()?)?,
        });
    }
    mappings.sort();
    valid_mappings(&mappings).then_some(mappings)
}

fn deferred_commit_result(
    output: &Value,
    producer: BoundedCommitProducer,
) -> Option<(String, String)> {
    if producer == BoundedCommitProducer::Rebase {
        return None;
    }
    let output = exact_result_text(output)?;
    let mut candidates = output
        .lines()
        .filter_map(canonical_short_commit_summary)
        .collect::<Vec<_>>();
    if producer == BoundedCommitProducer::Merge {
        let merge_created = output.lines().any(|line| {
            let line = line.trim();
            line.starts_with("Merge made by the ") && line.ends_with(" strategy.")
        });
        if merge_created {
            candidates.extend(output.lines().filter_map(canonical_graph_head_summary));
        }
    }
    candidates.sort();
    candidates.dedup();
    match candidates.as_slice() {
        [candidate] => Some(candidate.clone()),
        _ => None,
    }
}

fn exact_result_text(output: &Value) -> Option<String> {
    match output {
        Value::String(value) if value.len() <= MAX_EXACT_OUTCOME_OUTPUT_BYTES => {
            Some(value.clone())
        }
        Value::Array(items) if !items.is_empty() && items.len() <= MAX_EXACT_OUTCOME_OBJECTS => {
            let mut combined = String::new();
            for item in items {
                let object = item.as_object()?;
                if !exact_keys(object, &["type", "text"])
                    || object.get("type")?.as_str()? != "input_text"
                {
                    return None;
                }
                let text = object.get("text")?.as_str()?;
                if combined.len().saturating_add(text.len()) > MAX_EXACT_OUTCOME_OUTPUT_BYTES {
                    return None;
                }
                combined.push_str(text);
            }
            Some(combined)
        }
        Value::Object(object) => {
            let mut candidates = ["aggregated", "stdout", "output", "text", "content"]
                .into_iter()
                .filter_map(|key| object.get(key).and_then(Value::as_str))
                .filter(|value| value.len() <= MAX_EXACT_OUTCOME_OUTPUT_BYTES);
            let selected = candidates.next()?;
            candidates.next().is_none().then(|| selected.to_owned())
        }
        _ => None,
    }
}

fn canonical_short_commit_summary(line: &str) -> Option<(String, String)> {
    let line = line.strip_prefix('[')?;
    let (identity, subject) = line.split_once("] ")?;
    let (_, oid_prefix) = identity.rsplit_once(' ')?;
    bounded_short_commit(oid_prefix, subject)
}

fn canonical_graph_head_summary(line: &str) -> Option<(String, String)> {
    let line = line.strip_prefix("*   ")?;
    let (oid_prefix, subject) = line.split_once(' ')?;
    bounded_short_commit(oid_prefix, subject)
}

fn bounded_short_commit(oid_prefix: &str, subject: &str) -> Option<(String, String)> {
    if !(7..=64).contains(&oid_prefix.len())
        || !oid_prefix.bytes().all(|byte| byte.is_ascii_hexdigit())
        || subject.is_empty()
        || subject.bytes().any(|byte| byte.is_ascii_control())
    {
        return None;
    }
    Some((oid_prefix.to_ascii_lowercase(), subject.to_owned()))
}

fn valid_mappings(mappings: &[RepositoryCommitMapping]) -> bool {
    let mut edges = HashSet::new();
    let mut sources = HashSet::new();
    let mut results = HashSet::new();
    for mapping in mappings {
        if mapping.source == mapping.result
            || mapping.source.format != mapping.result.format
            || !edges.insert((mapping.source.clone(), mapping.result.clone()))
            || !sources.insert(mapping.source.clone())
            || !results.insert(mapping.result.clone())
        {
            return false;
        }
    }
    true
}

fn exact_json_object(output: &Value) -> Option<&Map<String, Value>> {
    output.as_object()
}

fn exact_keys(object: &Map<String, Value>, keys: &[&str]) -> bool {
    object.len() == keys.len() && keys.iter().all(|key| object.contains_key(*key))
}

fn keys_are_subset(object: &Map<String, Value>, keys: &[&str]) -> bool {
    object.keys().all(|key| keys.contains(&key.as_str()))
}

fn exact_machine_oid_output(
    output: &Value,
    producer: BoundedCommitProducer,
    machine_output_isolated: bool,
) -> Option<GitObjectId> {
    let output = output.as_str()?;
    let lines = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let requested = object_id(lines.last().copied()?)?;
    let exactly_one_full_oid = lines
        .iter()
        .filter(|line| object_id(line).is_some())
        .count()
        == 1;
    let operation_demonstrated = match producer {
        BoundedCommitProducer::Commit => true,
        BoundedCommitProducer::Merge => {
            machine_output_isolated
                && lines[..lines.len().saturating_sub(1)].iter().any(|line| {
                    line.starts_with("Merge made by the ") && line.ends_with(" strategy.")
                })
        }
        BoundedCommitProducer::Rebase => false,
        BoundedCommitProducer::CherryPick => true,
    };
    (exactly_one_full_oid && operation_demonstrated).then_some(requested)
}

fn object_id(value: &str) -> Option<GitObjectId> {
    let format = match value.len() {
        40 => GitObjectFormat::Sha1,
        64 => GitObjectFormat::Sha256,
        _ => return None,
    };
    value
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit())
        .then(|| GitObjectId {
            format,
            hex: value.to_ascii_lowercase(),
        })
}

fn plan_paths(plan: &BoundedOutcomePlan) -> (Option<String>, Option<String>) {
    (
        Some(
            plan.operation_repository_path
                .to_string_lossy()
                .into_owned(),
        ),
        plan.output_repository_path
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned()),
    )
}
