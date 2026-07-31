use std::{collections::HashSet, path::Path};

use ctx_history_core::{
    GitObjectFormat, GitObjectId, RepositoryAbstentionReason, RepositoryAlias, RepositoryAliasKind,
    RepositoryObjectReplacement, RepositoryOutcomeKind, RepositoryOutcomeLinkage,
    RepositoryOutcomeObservation, RepositoryPullRequestIdentity,
    CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
};
use serde_json::{Map, Value};
use url::Url;

use super::{
    bounded_outcome_plan, lexical_absolute, BoundedOutcomeOperation, BoundedOutcomePlan,
    BoundedOutcomePlanDisposition,
};
use crate::OutputOutcome;

const MAX_EXACT_OUTCOME_OBJECTS: usize = 256;
const MAX_LINKAGE_CALL_ID_BYTES: usize = 16 * 1024;

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
    pub(crate) outcomes: Vec<RepositoryOutcomeObservation>,
    pub(crate) abstentions: Vec<(RepositoryAbstentionReason, &'static str)>,
}

pub(crate) fn linked_outcome_evidence(
    input: LinkedOutcomeInput<'_>,
) -> Option<LinkedOutcomeEvidence> {
    let base = input
        .declared_workdir
        .or(input.session_cwd)
        .and_then(|value| lexical_absolute(value, None));
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
    if input.origin_call_id.is_empty()
        || input.result_call_id.is_empty()
        || input.origin_call_id.len() > MAX_LINKAGE_CALL_ID_BYTES
        || input.result_call_id.len() > MAX_LINKAGE_CALL_ID_BYTES
        || input
            .continuation_call_id_sha256
            .iter()
            .collect::<HashSet<_>>()
            .len()
            != input.continuation_call_id_sha256.len()
    {
        return Some(abstained(
            operation_path,
            output_path,
            RepositoryAbstentionReason::ProviderOutputUnjoined,
            "outcome_linkage_is_missing_oversized_or_ambiguous",
        ));
    }
    let linkage = RepositoryOutcomeLinkage {
        provider: input.provider.to_owned(),
        origin_call_id: input.origin_call_id.to_owned(),
        result_call_id: input.result_call_id.to_owned(),
        origin_event_sequence: input.origin_event_sequence,
        continuation_call_id_sha256: input.continuation_call_id_sha256.to_vec(),
        result_record_sha256: input.result_record_sha256,
    };
    let parsed = parse_operation_result(
        input.result_output,
        input.structured_commit_oid,
        &plan,
        input.observed_at_unix_ms,
        linkage,
    );
    match parsed {
        OperationResult::Exact { outcome, aliases } => Some(LinkedOutcomeEvidence {
            provider_native_repository_aliases: aliases,
            outcome_operation_repository_path: operation_path,
            outcome_output_repository_path: output_path,
            outcomes: vec![*outcome],
            abstentions: Vec::new(),
        }),
        OperationResult::RewriteUnlinked => Some(abstained(
            operation_path,
            output_path,
            RepositoryAbstentionReason::HistoryRewriteUnlinked,
            "rewrite_result_has_no_exact_nonbranching_replacement_lineage",
        )),
        OperationResult::Inadmissible => Some(abstained(
            operation_path,
            output_path,
            RepositoryAbstentionReason::OutcomeResultInadmissible,
            "linked_result_has_no_exact_operation_specific_outcome",
        )),
    }
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
        abstentions: vec![(reason, detail)],
    }
}

enum OperationResult {
    Exact {
        outcome: Box<RepositoryOutcomeObservation>,
        aliases: Vec<RepositoryAlias>,
    },
    RewriteUnlinked,
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
            rewrites_history,
            exact_oid_output,
        } => {
            let parsed = if let Some(value) = structured_commit_oid {
                object_id(value).map(|object_id| (vec![object_id], Vec::new()))
            } else {
                exact_commit_result(output, exact_oid_output)
            };
            if rewrites_history
                && !matches!(parsed, Some((_, ref replacements)) if !replacements.is_empty())
            {
                return OperationResult::RewriteUnlinked;
            }
            let Some((produced_object_ids, replacement_lineage)) = parsed else {
                return OperationResult::Inadmissible;
            };
            if !rewrites_history && !replacement_lineage.is_empty() {
                return OperationResult::Inadmissible;
            }
            OperationResult::Exact {
                outcome: Box::new(RepositoryOutcomeObservation {
                    kind: RepositoryOutcomeKind::Commit,
                    produced_object_ids,
                    replacement_lineage,
                    pull_request: None,
                    observed_at_unix_ms,
                    linkage,
                    outcome_capture_revision: CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
                }),
                aliases: Vec::new(),
            }
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
                    replacement_lineage: Vec::new(),
                    pull_request: Some(pull_request),
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
                    produced_object_ids: vec![merge_oid],
                    replacement_lineage: Vec::new(),
                    pull_request: Some(pull_request),
                    observed_at_unix_ms,
                    linkage,
                    outcome_capture_revision: CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
                }),
                aliases: vec![alias],
            }
        }
    }
}

fn exact_commit_result(
    output: &Value,
    exact_oid_output: bool,
) -> Option<(Vec<GitObjectId>, Vec<RepositoryObjectReplacement>)> {
    if exact_oid_output {
        if let Some(object_id) = exact_machine_oid_output(output) {
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
        let lineage = vec![RepositoryObjectReplacement {
            replaced,
            replacement: replacement.clone(),
        }];
        valid_lineage(&lineage).then_some((vec![replacement], lineage))
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
            lineage.push(RepositoryObjectReplacement {
                replaced: object_id(pair.get("old_oid")?.as_str()?)?,
                replacement: object_id(pair.get("new_oid")?.as_str()?)?,
            });
        }
        if !valid_lineage(&lineage) {
            return None;
        }
        let produced = lineage
            .iter()
            .map(|replacement| replacement.replacement.clone())
            .collect();
        Some((produced, lineage))
    } else {
        None
    }
}

fn valid_lineage(lineage: &[RepositoryObjectReplacement]) -> bool {
    let mut replaced = HashSet::new();
    let mut replacements = HashSet::new();
    let mut edges = HashSet::new();
    for edge in lineage {
        if edge.replaced == edge.replacement
            || edge.replaced.format != edge.replacement.format
            || !replaced.insert(edge.replaced.clone())
            || !replacements.insert(edge.replacement.clone())
            || !edges.insert((edge.replaced.clone(), edge.replacement.clone()))
        {
            return false;
        }
    }
    for start in &replaced {
        let mut seen = HashSet::new();
        let mut current = start;
        while let Some(next) = lineage
            .iter()
            .find(|edge| &edge.replaced == current)
            .map(|edge| &edge.replacement)
        {
            if !seen.insert(current) || next == start {
                return false;
            }
            current = next;
        }
    }
    true
}

fn exact_pr_create_result(output: &Value) -> Option<RepositoryPullRequestIdentity> {
    if let Some(url) = output.as_str() {
        return pull_request_from_url(url.trim());
    }
    pull_request_from_exact_object(exact_json_object(output)?)
}

fn exact_pr_merge_result(output: &Value) -> Option<(RepositoryPullRequestIdentity, GitObjectId)> {
    let object = exact_json_object(output)?;
    let allowed = ["url", "number", "id", "node_id", "merge_commit_oid"];
    if !keys_are_subset(object, &allowed)
        || object.len() < 2
        || !object.contains_key("url")
        || !object.contains_key("merge_commit_oid")
    {
        return None;
    }
    let pull_request = pull_request_from_exact_object(object)?;
    let merge_oid = object_id(object.get("merge_commit_oid")?.as_str()?)?;
    Some((pull_request, merge_oid))
}

fn pull_request_from_exact_object(
    object: &Map<String, Value>,
) -> Option<RepositoryPullRequestIdentity> {
    let allowed = ["url", "number", "id", "node_id", "merge_commit_oid"];
    if !keys_are_subset(object, &allowed)
        || !object.contains_key("url")
        || (object.contains_key("id") && object.contains_key("node_id"))
    {
        return None;
    }
    let mut identity = pull_request_from_url(object.get("url")?.as_str()?)?;
    if let Some(number) = object.get("number") {
        if number.as_u64()? != identity.number {
            return None;
        }
    }
    identity.provider_id = match object.get("id").or_else(|| object.get("node_id")) {
        Some(value) => Some(value.as_str().filter(|value| !value.is_empty())?.to_owned()),
        None => None,
    };
    Some(identity)
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

fn exact_machine_oid_output(output: &Value) -> Option<GitObjectId> {
    let lines = output
        .as_str()?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let requested = object_id(lines.last().copied()?)?;
    (lines
        .iter()
        .filter(|line| object_id(line).is_some())
        .count()
        == 1)
        .then_some(requested)
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

fn pull_request_from_url(value: &str) -> Option<RepositoryPullRequestIdentity> {
    let url = Url::parse(value).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    let segments = url.path_segments()?.collect::<Vec<_>>();
    if segments.len() < 4 || segments[segments.len() - 2] != "pull" {
        return None;
    }
    let number = segments.last()?.parse::<u64>().ok()?;
    let name = segments.get(segments.len() - 3)?.to_string();
    let namespace = segments[..segments.len() - 3]
        .iter()
        .map(|segment| (*segment).to_owned())
        .collect::<Vec<_>>();
    if number == 0 || namespace.is_empty() || name.is_empty() {
        return None;
    }
    Some(RepositoryPullRequestIdentity {
        forge_repository: RepositoryAlias {
            kind: RepositoryAliasKind::Forge,
            host,
            namespace,
            name,
            remote_name: None,
        },
        number,
        provider_id: None,
    })
}

fn pr_matches_plan(
    pull_request: &RepositoryPullRequestIdentity,
    plan: &BoundedOutcomePlan,
) -> bool {
    if plan
        .expected_pr_number
        .is_some_and(|number| number != pull_request.number)
    {
        return false;
    }
    if let Some(expected) = &plan.expected_pr_repository_path {
        let mut actual = pull_request.forge_repository.namespace.clone();
        actual.push(pull_request.forge_repository.name.clone());
        if &actual != expected {
            return false;
        }
    }
    true
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
        assert_eq!(exact.outcomes[0].produced_object_ids[0].hex, oid);

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
    fn exact_rewrite_and_pull_request_schemas_are_supported() {
        let old = "1111111111111111111111111111111111111111";
        let new = "2222222222222222222222222222222222222222";
        let rewrite = serde_json::json!({"old_oid": old, "new_oid": new});
        let rewrite =
            linked_outcome_evidence(input("git commit --amend --no-edit", &rewrite)).unwrap();
        assert_eq!(rewrite.outcomes[0].replacement_lineage.len(), 1);

        let create = Value::String("https://github.com/acme/repo/pull/42".to_owned());
        let created =
            linked_outcome_evidence(input("gh pr create --repo acme/repo", &create)).unwrap();
        assert_eq!(
            created.outcomes[0].kind,
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
            merged.outcomes[0].kind,
            RepositoryOutcomeKind::PullRequestMerged
        );
    }
}
