use std::{collections::HashSet, path::Path};

use ctx_history_core::{
    GitObjectFormat, GitObjectId, RepositoryAbstentionReason, RepositoryAlias,
    RepositoryCommitMapping, RepositoryCommitOperationEvent, RepositoryCommitOperationKind,
    RepositoryCommitOperationState, RepositoryOutcomeKind, RepositoryOutcomeLinkage,
    RepositoryOutcomeObservation, CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
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
    Exact(RepositoryOutcomeObservation),
    DeferredCommit(DeferredCommitObservation),
    DeferredCommitOperation(DeferredCommitOperationObservation),
    DeferredCherryPick(DeferredCherryPickObservation),
}

impl From<RepositoryOutcomeObservation> for UnscopedOutcomeObservation {
    fn from(value: RepositoryOutcomeObservation) -> Self {
        Self::Exact(value)
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
                outcomes: vec![(*outcome).into()],
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
            let (parsed, explicit_pre_head) =
                if operation_kind == Some(RepositoryCommitOperationKind::CherryPick) {
                    match exact_cherry_pick_result(output) {
                        Some((pre_head, mapping)) => (
                            Some((vec![mapping.result.clone()], vec![mapping])),
                            Some(pre_head),
                        ),
                        None => (
                            if let Some(value) = structured_commit_oid {
                                object_id(value).map(|object_id| (vec![object_id], Vec::new()))
                            } else {
                                exact_commit_result(
                                    output,
                                    producer,
                                    exact_oid_output,
                                    plan.machine_output_isolated,
                                )
                            },
                            None,
                        ),
                    }
                } else if let Some(value) = structured_commit_oid {
                    (
                        object_id(value).map(|object_id| (vec![object_id], Vec::new())),
                        None,
                    )
                } else {
                    (
                        exact_commit_result(
                            output,
                            producer,
                            exact_oid_output,
                            plan.machine_output_isolated,
                        ),
                        None,
                    )
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
            if mappings.len() != 1
                || operation_source_oid
                    .is_some_and(|source| object_id(source).as_ref() != Some(&mappings[0].source))
            {
                return OperationResult::Inadmissible;
            }
            OperationResult::DeferredOperation(DeferredCommitOperationObservation {
                kind: operation_kind,
                command_pre_head: match operation_kind {
                    RepositoryCommitOperationKind::Amend
                    | RepositoryCommitOperationKind::Rebase => Some(mappings[0].source.clone()),
                    RepositoryCommitOperationKind::CherryPick => explicit_pre_head.clone(),
                },
                sequencer_pre_head: match operation_kind {
                    RepositoryCommitOperationKind::Amend => None,
                    RepositoryCommitOperationKind::Rebase => Some(mappings[0].source.clone()),
                    RepositoryCommitOperationKind::CherryPick => explicit_pre_head,
                },
                command_post_head: mappings[0].result.clone(),
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
        let values = object.get("replacements")?.as_array()?;
        if values.is_empty() || values.len() > MAX_EXACT_OUTCOME_OBJECTS {
            return None;
        }
        let mut lineage = Vec::with_capacity(values.len());
        for value in values {
            let pair = value.as_object()?;
            if !exact_keys(pair, &["old_oid", "new_oid"]) {
                return None;
            }
            lineage.push(RepositoryCommitMapping {
                source: object_id(pair.get("old_oid")?.as_str()?)?,
                result: object_id(pair.get("new_oid")?.as_str()?)?,
            });
        }
        lineage.sort();
        if !valid_mappings(&lineage) {
            return None;
        }
        let produced = lineage
            .iter()
            .map(|mapping| mapping.result.clone())
            .collect();
        Some((produced, lineage))
    } else {
        None
    }
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
    for mapping in mappings {
        if mapping.source == mapping.result
            || mapping.source.format != mapping.result.format
            || !edges.insert((mapping.source.clone(), mapping.result.clone()))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn exact_outcome(value: &UnscopedOutcomeObservation) -> &RepositoryOutcomeObservation {
        match value {
            UnscopedOutcomeObservation::Exact(outcome) => outcome,
            UnscopedOutcomeObservation::DeferredCommit(_)
            | UnscopedOutcomeObservation::DeferredCommitOperation(_)
            | UnscopedOutcomeObservation::DeferredCherryPick(_) => {
                panic!("expected exact outcome")
            }
        }
    }

    fn input<'a>(command: &'a str, output: &'a Value) -> LinkedOutcomeInput<'a> {
        LinkedOutcomeInput {
            provider: "fixture",
            command,
            session_cwd: Some("/repo"),
            declared_workdir: Some("/repo"),
            origin_call_id: "call-origin",
            result_call_id: "call-result",
            origin_event_sequence: 7,
            continuation_call_id_sha256: &[],
            result_record_sha256: [9; 32],
            observed_at_unix_ms: 10,
            result_outcome: OutputOutcome::Success,
            result_output: output,
            structured_commit_oid: None,
            output_repository_path: Some("/repo"),
        }
    }

    #[test]
    fn exact_result_and_structured_oid_precedence_are_fail_closed() {
        let oid = "0123456789abcdef0123456789abcdef01234567";
        let output = Value::String(oid.to_owned());
        let exact = linked_outcome_evidence(input(
            "git commit -m exact && git rev-parse --verify HEAD",
            &output,
        ))
        .unwrap();
        assert_eq!(
            exact_outcome(&exact.outcomes[0]).produced_object_ids[0].hex,
            oid
        );

        let mut short = input("git commit -m exact", &output);
        short.structured_commit_oid = Some("0123456");
        let short = linked_outcome_evidence(short).unwrap();
        assert!(short.outcomes.is_empty());
        assert_eq!(
            short.abstentions[0].0,
            RepositoryAbstentionReason::OutcomeResultInadmissible
        );
    }

    #[test]
    fn canonical_cross_provider_short_commit_results_are_deferred_not_guessed() {
        for (command, output, expected_prefix, expected_subject) in [
            (
                "git commit -m exact",
                serde_json::json!([
                    {"type": "input_text", "text": "Script completed\nOutput:\n"},
                    {"type": "input_text", "text": "[main 9747be9] Fail closed on invalid retry headers\n 2 files changed, 24 insertions(+), 5 deletions(-)\n"},
                    {"type": "input_text", "text": "exit=0"}
                ]),
                "9747be9",
                "Fail closed on invalid retry headers",
            ),
            (
                "git commit -m exact",
                serde_json::json!({
                    "status": "completed",
                    "exitCode": 0,
                    "aggregated": "## main\n M src/audit.js\n[main ee42c90] feat: summarize normalized delivery policies\n 2 files changed, 68 insertions(+), 1 deletion(-)",
                    "cwd": "/repo"
                }),
                "ee42c90",
                "feat: summarize normalized delivery policies",
            ),
        ] {
            let evidence = linked_outcome_evidence(input(command, &output)).unwrap();
            let UnscopedOutcomeObservation::DeferredCommit(deferred) = &evidence.outcomes[0] else {
                panic!("expected deferred commit");
            };
            assert_eq!(deferred.oid_prefix, expected_prefix);
            assert_eq!(deferred.subject, expected_subject);
        }

        let amend = Value::String(
            "[main 1791cb3] Add bounded retry jitter normalization\n 2 files changed".to_owned(),
        );
        let evidence =
            linked_outcome_evidence(input("git commit --amend --no-edit", &amend)).unwrap();
        assert!(evidence.outcomes.is_empty());
        assert_eq!(
            evidence.abstentions[0].0,
            RepositoryAbstentionReason::HistoryRewriteUnlinked
        );
    }

    #[test]
    fn exact_commit_receipt_survives_unrelated_output_after_exact_head() {
        let oid = "cbbccc92da81bbe173789b873b2e579327b7c2e1";
        let output = Value::String(format!(
            "[ctx/v026-locator-sidecar-envelope-backfill cbbccc92d] fix(pro): reserve result bytes before source admission\n 2 files changed, 24 insertions(+), 5 deletions(-)\n{oid}\npub const MAX_PAGE_BYTES: usize = 64 * 1024 * 1024;\n"
        ));
        let command = concat!(
            "git commit -m 'fix(pro): reserve result bytes before source admission' && ",
            "git status --short && git rev-parse HEAD && ",
            "sed -n '12,18p' crates/ctx-pro-host-protocol/src/lib.rs"
        );
        let evidence = linked_outcome_evidence(input(command, &output)).unwrap();
        let UnscopedOutcomeObservation::DeferredCommit(deferred) = &evidence.outcomes[0] else {
            panic!("expected certified-receipt candidate");
        };
        assert_eq!(deferred.oid_prefix, "cbbccc92d");
        assert_eq!(
            deferred.subject,
            "fix(pro): reserve result bytes before source admission"
        );

        let ambiguous = Value::String(format!(
            "[main cbbccc92d] fix(pro): reserve result bytes before source admission\n{oid}\n[main 1111111] unrelated second commit\n"
        ));
        let evidence = linked_outcome_evidence(input(command, &ambiguous)).unwrap();
        assert!(evidence.outcomes.is_empty());
        assert_eq!(
            evidence.abstentions[0].0,
            RepositoryAbstentionReason::OutcomeResultInadmissible
        );
    }

    #[test]
    fn canonical_merge_graph_head_is_deferred_and_ambiguous_summaries_abstain() {
        let output = Value::String(
            "Merge made by the 'ort' strategy.\n README.md | 11 +++++++++++\n*   efdfa9e Merge retry validation documentation\n|\\  \n| * a69f7ff Document retry validation contract\n* | 9747be9 Fail closed on invalid retry headers\n"
                .to_owned(),
        );
        let evidence = linked_outcome_evidence(input("git merge --no-ff docs", &output)).unwrap();
        let UnscopedOutcomeObservation::DeferredCommit(deferred) = &evidence.outcomes[0] else {
            panic!("expected deferred merge");
        };
        assert_eq!(deferred.oid_prefix, "efdfa9e");
        assert_eq!(deferred.producer, BoundedCommitProducer::Merge);

        let ambiguous = Value::String("[main 1111111] first\n[main 2222222] second\n".to_owned());
        let evidence = linked_outcome_evidence(input("git commit -m exact", &ambiguous)).unwrap();
        assert!(evidence.outcomes.is_empty());
        assert_eq!(
            evidence.abstentions[0].0,
            RepositoryAbstentionReason::OutcomeResultInadmissible
        );
    }

    #[test]
    fn dry_run_and_intervening_head_changes_never_produce_exact_outcomes() {
        let oid = "0123456789abcdef0123456789abcdef01234567";
        let output = Value::String(oid.to_owned());
        for command in [
            "git commit --dry-run && git rev-parse HEAD",
            "git commit -m exact && git reset --hard HEAD^ && git rev-parse HEAD",
            "git commit -m exact && git checkout other && git rev-parse HEAD",
            "git commit -m exact && custom-command && git rev-parse HEAD",
        ] {
            let evidence = linked_outcome_evidence(input(command, &output)).unwrap();
            assert!(evidence.outcomes.is_empty(), "{command}");
            assert!(!evidence.abstentions.is_empty(), "{command}");
        }

        let stable = linked_outcome_evidence(input(
            "git commit -m exact && git status --short && git rev-parse HEAD",
            &output,
        ))
        .unwrap();
        assert_eq!(stable.outcomes.len(), 1);
        assert_eq!(
            exact_outcome(&stable.outcomes[0]).produced_object_ids[0].hex,
            oid
        );
    }

    #[test]
    fn exact_oids_from_inspection_commands_are_not_production_outcomes() {
        let oid = "d50d84a3e609d1ed30a435adbf2c19db35448b52";
        let output = Value::String(format!("{oid}\n"));

        for command in [
            format!("git show --no-patch --format=%H {oid}"),
            format!("git log -1 --format=%H {oid}"),
            format!("git rev-parse --verify {oid}^{{commit}}"),
            format!("git branch --contains {oid}"),
        ] {
            let evidence = linked_outcome_evidence(input(&command, &output));
            assert!(
                evidence
                    .as_ref()
                    .is_none_or(|evidence| evidence.outcomes.is_empty()),
                "inspection command emitted a production outcome: {command}"
            );
        }
    }

    #[test]
    fn merge_head_is_exact_only_when_the_output_demonstrates_merge_creation() {
        let oid = "0123456789abcdef0123456789abcdef01234567";
        let command = "git merge --no-ff feature && git rev-parse --verify HEAD";
        let no_op = Value::String(format!("Already up to date.\n{oid}\n"));
        let no_op = linked_outcome_evidence(input(command, &no_op)).unwrap();
        assert!(no_op.outcomes.is_empty());
        assert_eq!(
            no_op.abstentions[0].0,
            RepositoryAbstentionReason::OutcomeResultInadmissible
        );

        let created_output = Value::String(format!("Merge made by the 'ort' strategy.\n{oid}\n"));
        let created = linked_outcome_evidence(input(command, &created_output)).unwrap();
        assert_eq!(created.outcomes.len(), 1);
        assert_eq!(
            exact_outcome(&created.outcomes[0]).produced_object_ids[0].hex,
            oid
        );

        let polluted = linked_outcome_evidence(input(
            "git log -1 && git merge --no-ff feature && git rev-parse HEAD",
            &created_output,
        ))
        .unwrap();
        assert!(polluted.outcomes.is_empty());

        let intervening = linked_outcome_evidence(input(
            "git merge --no-ff feature && git status --short && git rev-parse HEAD",
            &created_output,
        ))
        .unwrap();
        assert!(intervening.outcomes.is_empty());

        for non_producing in [
            "git merge --no-ff --no-commit feature && git rev-parse HEAD",
            "git merge --no-ff --squash feature && git rev-parse HEAD",
        ] {
            let evidence = linked_outcome_evidence(input(non_producing, &created_output)).unwrap();
            assert!(evidence.outcomes.is_empty(), "{non_producing}");
        }
    }

    #[test]
    fn exact_rewrite_and_pull_request_schemas_are_supported() {
        let old = "1111111111111111111111111111111111111111";
        let new = "2222222222222222222222222222222222222222";
        let rewrite_output = serde_json::json!({"old_oid": old, "new_oid": new});
        let rewrite =
            linked_outcome_evidence(input("git commit --amend --no-edit", &rewrite_output))
                .unwrap();
        let UnscopedOutcomeObservation::DeferredCommitOperation(amend) = &rewrite.outcomes[0]
        else {
            panic!("expected deferred amend operation");
        };
        assert_eq!(amend.kind, RepositoryCommitOperationKind::Amend);
        assert_eq!(amend.mappings.len(), 1);
        assert_eq!(amend.command_pre_head.as_ref().unwrap().hex, old);
        assert_eq!(amend.command_post_head.hex, new);

        let rebase = linked_outcome_evidence(input("git rebase main", &rewrite_output)).unwrap();
        let UnscopedOutcomeObservation::DeferredCommitOperation(rebase) = &rebase.outcomes[0]
        else {
            panic!("expected deferred rebase operation");
        };
        assert_eq!(rebase.kind, RepositoryCommitOperationKind::Rebase);
        assert_eq!(rebase.mappings.len(), 1);
        assert_eq!(rebase.sequencer_pre_head.as_ref().unwrap().hex, old);

        let raw_rebase_oid = Value::String(new.to_owned());
        let raw_rebase = linked_outcome_evidence(input(
            "git rebase main && git rev-parse --verify HEAD",
            &raw_rebase_oid,
        ))
        .unwrap();
        assert!(raw_rebase.outcomes.is_empty());
        assert_eq!(
            raw_rebase.abstentions[0].0,
            RepositoryAbstentionReason::HistoryRewriteUnlinked
        );

        let amended = linked_outcome_evidence(input(
            "git commit --amend --no-edit && git rev-parse HEAD",
            &Value::String(new.to_owned()),
        ))
        .unwrap();
        let amended_outcome = exact_outcome(&amended.outcomes[0]);
        assert!(amended_outcome.produced_object_ids.is_empty());
        let operation = amended_outcome.commit_operation.as_ref().unwrap();
        assert_eq!(operation.kind, RepositoryCommitOperationKind::Amend);
        assert!(operation.mappings.is_empty());
        assert_eq!(operation.unlinked_results[0].hex, new);
        assert_eq!(
            amended.abstentions[0].0,
            RepositoryAbstentionReason::HistoryRewriteUnlinked
        );

        let create = Value::String("https://github.com/acme/repo/pull/42".to_owned());
        let created =
            linked_outcome_evidence(input("gh pr create --repo acme/repo", &create)).unwrap();
        assert_eq!(
            exact_outcome(&created.outcomes[0]).kind,
            RepositoryOutcomeKind::PullRequestCreated
        );

        let merged = serde_json::json!({
            "url": "https://github.com/acme/repo/pull/42",
            "number": 42,
            "id": "PR_42",
            "merge_commit_oid": "abcdefabcdefabcdefabcdefabcdefabcdefabcd"
        });
        let merged =
            linked_outcome_evidence(input("gh pr merge 42 --repo acme/repo", &merged)).unwrap();
        assert_eq!(
            exact_outcome(&merged.outcomes[0]).kind,
            RepositoryOutcomeKind::PullRequestMerged
        );
    }

    #[test]
    fn native_cherry_pick_stdout_is_deferred_but_failures_and_ambiguity_abstain() {
        let source = "0123456789abcdef0123456789abcdef01234567";
        let command = format!("git cherry-pick {source}");
        let output = Value::String(
            "[main a12bc34] Apply exact lineage\n 1 file changed, 1 insertion(+)\n".to_owned(),
        );
        let evidence = linked_outcome_evidence(input(&command, &output)).unwrap();
        let [UnscopedOutcomeObservation::DeferredCherryPick(deferred)] =
            evidence.outcomes.as_slice()
        else {
            panic!("expected deferred native cherry-pick");
        };
        assert_eq!(deferred.source.hex, source);
        assert_eq!(deferred.result_oid_prefix, "a12bc34");
        assert_eq!(deferred.result_subject, "Apply exact lineage");

        let mut failed = input(&command, &output);
        failed.result_outcome = OutputOutcome::Failure;
        let failed = linked_outcome_evidence(failed).unwrap();
        assert!(failed.outcomes.is_empty());
        assert_eq!(
            failed.abstentions[0].0,
            RepositoryAbstentionReason::OutcomeResultInadmissible
        );

        for output in [
            Value::String("error: could not apply 0123456... conflict\n".to_owned()),
            Value::String(
                "[main a12bc34] Apply exact lineage\n[main b23cd45] Another result\n".to_owned(),
            ),
        ] {
            let evidence = linked_outcome_evidence(input(&command, &output)).unwrap();
            assert!(evidence.outcomes.is_empty());
            assert_eq!(
                evidence.abstentions[0].0,
                RepositoryAbstentionReason::HistoryRewriteUnlinked
            );
        }
    }
}
