use std::{collections::HashSet, path::Path};

use ctx_history_core::{
    GitObjectFormat, GitObjectId, RepositoryAbstentionReason, RepositoryAlias, RepositoryAliasKind,
    RepositoryObjectReplacement, RepositoryOutcomeKind, RepositoryOutcomeLinkage,
    RepositoryOutcomeObservation, RepositoryPullRequestIdentity,
    CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
};
use serde_json::{json, Map, Value};
use url::Url;

use crate::{
    provider::codex::events::CodexToolCallContext,
    repository_attribution::{
        bounded_outcome_plan, BoundedOutcomeOperation, BoundedOutcomePlan,
        BoundedOutcomePlanDisposition,
    },
    OutputOutcome, OutputOutcomeMetadata,
};

const MAX_EXACT_OUTCOME_OBJECTS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexRepositoryResultEvidence {
    pub(crate) command: String,
    pub(crate) declared_workdir: Option<String>,
    pub(crate) outcome_operation_repository_path: Option<String>,
    pub(crate) outcome_output_repository_path: Option<String>,
    pub(crate) structured_content: Value,
    pub(crate) provider_native_repository_aliases: Vec<RepositoryAlias>,
    pub(crate) outcomes: Vec<RepositoryOutcomeObservation>,
    pub(crate) abstentions: Vec<(RepositoryAbstentionReason, &'static str)>,
}

pub(crate) fn repository_result_evidence(
    payload: &Value,
    context: &CodexToolCallContext,
    result_call_id: &str,
    result_record_sha256: [u8; 32],
    observed_at_unix_ms: i64,
    result_outcome: &OutputOutcomeMetadata,
) -> Option<CodexRepositoryResultEvidence> {
    let command = context.exact_command.as_deref()?;
    let base = context
        .declared_workdir
        .as_deref()
        .or(context.session_cwd.as_deref())
        .and_then(|value| crate::repository_attribution::lexical_absolute(value, None));
    let plan = match base.as_deref() {
        Some(base) => bounded_outcome_plan(command, base),
        None => match bounded_outcome_plan(command, Path::new("/")) {
            BoundedOutcomePlanDisposition::Planned(_) => {
                return Some(abstained_result(
                    command,
                    context,
                    result_call_id,
                    result_record_sha256,
                    (None, None),
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
            return Some(abstained_result(
                command,
                context,
                result_call_id,
                result_record_sha256,
                (operation_path, output_path),
                reason,
                detail,
            ));
        }
        BoundedOutcomePlanDisposition::Planned(plan) => plan,
    };
    let (operation_repository_path, output_repository_path) = plan_paths(&plan);

    if result_outcome.outcome != OutputOutcome::Success {
        return Some(abstained_result(
            command,
            context,
            result_call_id,
            result_record_sha256,
            (operation_repository_path, output_repository_path),
            RepositoryAbstentionReason::OutcomeResultInadmissible,
            "recognized_outcome_command_did_not_succeed",
        ));
    }

    if context.continuation_cell_id.is_some() && !super::terminal_continuation_result(payload) {
        return Some(abstained_result(
            command,
            context,
            result_call_id,
            result_record_sha256,
            (operation_repository_path, output_repository_path),
            RepositoryAbstentionReason::OutcomeResultInadmissible,
            "continuation_result_has_no_exact_terminal_control",
        ));
    }
    let Some(origin_call_id) = context.origin_call_id.as_deref() else {
        return Some(abstained_result(
            command,
            context,
            result_call_id,
            result_record_sha256,
            (operation_repository_path, output_repository_path),
            RepositoryAbstentionReason::ProviderOutputUnjoined,
            "outcome_result_has_no_exact_origin_call",
        ));
    };
    let Some(origin_event_sequence) = context.origin_event_sequence else {
        return Some(abstained_result(
            command,
            context,
            result_call_id,
            result_record_sha256,
            (operation_repository_path, output_repository_path),
            RepositoryAbstentionReason::ProviderOutputUnjoined,
            "outcome_result_has_no_exact_origin_event",
        ));
    };
    if context.continuation_capacity_exceeded
        || context.continuation_call_id_sha256.len()
            > crate::provider::codex::nativepath::MAX_CODEX_TOOL_CONTEXTS
        || context
            .continuation_call_id_sha256
            .iter()
            .collect::<HashSet<_>>()
            .len()
            != context.continuation_call_id_sha256.len()
    {
        return Some(abstained_result(
            command,
            context,
            result_call_id,
            result_record_sha256,
            (operation_repository_path, output_repository_path),
            RepositoryAbstentionReason::LinkageCapacityExceeded,
            "outcome_linkage_capacity_or_uniqueness_failed",
        ));
    }
    if context.correlation_ambiguous {
        return Some(abstained_result(
            command,
            context,
            result_call_id,
            result_record_sha256,
            (operation_repository_path, output_repository_path),
            RepositoryAbstentionReason::ProviderOutputUnjoined,
            "outcome_call_result_correlation_is_ambiguous",
        ));
    }
    let linkage = RepositoryOutcomeLinkage {
        provider: "codex".to_owned(),
        origin_call_id: origin_call_id.to_owned(),
        result_call_id: result_call_id.to_owned(),
        origin_event_sequence,
        continuation_call_id_sha256: context.continuation_call_id_sha256.clone(),
        result_record_sha256,
    };
    let Some(output) = super::repository_result_output(payload) else {
        return Some(abstained_result(
            command,
            context,
            result_call_id,
            result_record_sha256,
            (operation_repository_path, output_repository_path),
            RepositoryAbstentionReason::OutcomeResultInadmissible,
            "linked_outcome_result_has_no_exact_output_field",
        ));
    };

    let parsed = parse_operation_result(output, &plan, observed_at_unix_ms, linkage);
    let (outcomes, aliases, abstentions) = match parsed {
        OperationResult::Exact { outcome, aliases } => (vec![*outcome], aliases, Vec::new()),
        OperationResult::RewriteUnlinked => {
            return Some(abstained_result(
                command,
                context,
                result_call_id,
                result_record_sha256,
                (operation_repository_path, output_repository_path),
                RepositoryAbstentionReason::HistoryRewriteUnlinked,
                "rewrite_result_has_no_exact_nonbranching_replacement_lineage",
            ));
        }
        OperationResult::Inadmissible => {
            return Some(abstained_result(
                command,
                context,
                result_call_id,
                result_record_sha256,
                (operation_repository_path, output_repository_path),
                RepositoryAbstentionReason::OutcomeResultInadmissible,
                "linked_result_has_no_exact_operation_specific_outcome",
            ));
        }
    };

    Some(CodexRepositoryResultEvidence {
        command: command.to_owned(),
        declared_workdir: context.declared_workdir.clone(),
        outcome_operation_repository_path: operation_repository_path,
        outcome_output_repository_path: output_repository_path,
        structured_content: result_summary(
            context,
            result_call_id,
            result_record_sha256,
            outcomes.len(),
        ),
        provider_native_repository_aliases: aliases,
        outcomes,
        abstentions,
    })
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
    plan: &BoundedOutcomePlan,
    observed_at_unix_ms: i64,
    linkage: RepositoryOutcomeLinkage,
) -> OperationResult {
    match plan.operation {
        BoundedOutcomeOperation::Commit {
            rewrites_history,
            exact_oid_output,
        } => {
            let parsed = exact_commit_result(output, exact_oid_output);
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
    for start in replaced.iter() {
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

fn abstained_result(
    command: &str,
    context: &CodexToolCallContext,
    result_call_id: &str,
    result_record_sha256: [u8; 32],
    routes: (Option<String>, Option<String>),
    reason: RepositoryAbstentionReason,
    detail: &'static str,
) -> CodexRepositoryResultEvidence {
    let (outcome_operation_repository_path, outcome_output_repository_path) = routes;
    CodexRepositoryResultEvidence {
        command: command.to_owned(),
        declared_workdir: context.declared_workdir.clone(),
        outcome_operation_repository_path,
        outcome_output_repository_path,
        structured_content: result_summary(context, result_call_id, result_record_sha256, 0),
        provider_native_repository_aliases: Vec::new(),
        outcomes: Vec::new(),
        abstentions: vec![(reason, detail)],
    }
}

fn result_summary(
    context: &CodexToolCallContext,
    result_call_id: &str,
    result_record_sha256: [u8; 32],
    captured_outcomes: usize,
) -> Value {
    json!({
        "provider_native_tool_result": {
            "provider": "codex",
            "origin_call_id": context.origin_call_id,
            "result_call_id": result_call_id,
            "origin_event_sequence": context.origin_event_sequence,
            "continuation_call_id_sha256": context.continuation_call_id_sha256
                .iter()
                .map(hex_digest)
                .collect::<Vec<_>>(),
            "result_record_sha256": hex_digest(&result_record_sha256),
            "outcome_capture_revision": CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
            "captured_outcomes": captured_outcomes,
            "raw_output_retained": false,
        }
    })
}

fn hex_digest(value: &[u8; 32]) -> String {
    value.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(command: &str) -> CodexToolCallContext {
        CodexToolCallContext {
            exact_command: Some(command.to_owned()),
            session_cwd: Some("/repo".to_owned()),
            declared_workdir: Some("/repo".to_owned()),
            origin_call_id: Some("call-origin".to_owned()),
            origin_event_sequence: Some(7),
            ..CodexToolCallContext::default()
        }
    }

    fn success() -> OutputOutcomeMetadata {
        OutputOutcomeMetadata {
            outcome: OutputOutcome::Success,
            exit_code: Some(0),
            duration_ms: Some(1),
        }
    }

    fn capture(command: &str, output: Value) -> CodexRepositoryResultEvidence {
        repository_result_evidence(
            &json!({"output": output}),
            &context(command),
            "call-origin",
            [9; 32],
            10,
            &success(),
        )
        .unwrap()
    }

    #[test]
    fn exact_commit_output_is_last_unique_full_oid_line_only() {
        let oid = "0123456789abcdef0123456789abcdef01234567";
        let captured = capture(
            "git commit -m exact && git rev-parse --verify HEAD",
            Value::String(format!(
                "Process exited with code 0\nFinal output:\n[main abc1234] exact\n{oid}\n"
            )),
        );
        assert_eq!(captured.outcomes[0].produced_object_ids[0].hex, oid);

        for inadmissible in [
            format!("[main abc1234] exact\n{oid}\ntrailing prose\n"),
            format!("{oid}\n{oid}\n"),
            format!("commit completed near diagnostic token {oid}"),
        ] {
            let captured = capture(
                "git commit -m exact && git rev-parse HEAD",
                Value::String(inadmissible),
            );
            assert!(captured.outcomes.is_empty());
            assert_eq!(
                captured.abstentions[0].0,
                RepositoryAbstentionReason::OutcomeResultInadmissible
            );
        }
    }

    #[test]
    fn operation_and_output_routes_must_be_the_same() {
        let oid = "0123456789abcdef0123456789abcdef01234567";
        let mismatched = capture(
            "git -C /repo/A commit -m exact && git rev-parse HEAD",
            Value::String(oid.to_owned()),
        );
        assert!(mismatched.outcomes.is_empty());
        assert_eq!(
            mismatched.abstentions[0].0,
            RepositoryAbstentionReason::ConflictingIdentity
        );
        let matched = capture(
            "git -C /repo/A commit -m exact && git -C /repo/A rev-parse HEAD",
            Value::String(oid.to_owned()),
        );
        assert_eq!(
            matched.outcome_operation_repository_path.as_deref(),
            Some("/repo/A")
        );
        assert_eq!(matched.outcomes.len(), 1);
    }

    #[test]
    fn rewrite_requires_exact_nonbranching_cycle_free_lineage() {
        let old = "1111111111111111111111111111111111111111";
        let new = "2222222222222222222222222222222222222222";
        let explicit = capture(
            "git commit --amend --no-edit",
            json!({"old_oid": old, "new_oid": new}),
        );
        assert_eq!(explicit.outcomes[0].replacement_lineage.len(), 1);
        assert!(explicit.abstentions.is_empty());

        for output in [
            json!({"new_oid": new}),
            json!({"replacements": [
                {"old_oid": old, "new_oid": new},
                {"old_oid": old, "new_oid": "3333333333333333333333333333333333333333"}
            ]}),
            json!({"replacements": [
                {"old_oid": old, "new_oid": new},
                {"old_oid": new, "new_oid": old}
            ]}),
        ] {
            let captured = capture("git rebase main", output);
            assert!(captured.outcomes.is_empty());
            assert_eq!(
                captured.abstentions[0].0,
                RepositoryAbstentionReason::HistoryRewriteUnlinked
            );
        }
    }

    #[test]
    fn pr_schemas_are_exact_and_merge_command_selectors_reconcile() {
        let url = "https://github.com/acme/repo/pull/42";
        let created = capture("gh pr create", Value::String(url.to_owned()));
        assert_eq!(
            created.outcomes[0].kind,
            RepositoryOutcomeKind::PullRequestCreated
        );

        let merge_oid = "abcdefabcdefabcdefabcdefabcdefabcdefabcd";
        let merged = capture(
            "gh pr merge 42 --repo acme/repo",
            json!({"url": url, "number": 42, "id": "PR_42", "merge_commit_oid": merge_oid}),
        );
        assert_eq!(
            merged.outcomes[0].kind,
            RepositoryOutcomeKind::PullRequestMerged
        );
        assert_eq!(merged.outcomes[0].produced_object_ids[0].hex, merge_oid);

        let reordered = capture(
            "gh pr merge --repo acme/repo --admin 42",
            json!({"url": url, "number": 42, "merge_commit_oid": merge_oid}),
        );
        assert_eq!(reordered.outcomes.len(), 1);

        for command in [
            "gh pr merge 41 --repo acme/repo",
            "gh pr merge 42 --repo other/repo",
        ] {
            let captured = capture(
                command,
                json!({"url": url, "number": 42, "merge_commit_oid": merge_oid}),
            );
            assert!(captured.outcomes.is_empty());
        }
        let incidental = capture(
            "gh pr create",
            json!({"message": {"url": url}, "number": 42}),
        );
        assert!(incidental.outcomes.is_empty());
    }

    #[test]
    fn failure_wrapper_dynamic_alias_and_linkage_capacity_fail_closed() {
        let oid = "0123456789abcdef0123456789abcdef01234567";
        let failed = repository_result_evidence(
            &json!({"output": {"commit_oid": oid}}),
            &context("git commit -m failed"),
            "call-origin",
            [9; 32],
            10,
            &OutputOutcomeMetadata {
                outcome: OutputOutcome::Failure,
                exit_code: Some(1),
                duration_ms: None,
            },
        )
        .unwrap();
        assert!(failed.outcomes.is_empty());
        assert_eq!(
            failed.abstentions[0].0,
            RepositoryAbstentionReason::OutcomeResultInadmissible
        );

        let wrapped = capture("A=1 git commit -m hidden", json!({"commit_oid": oid}));
        assert!(wrapped.outcomes.is_empty());
        assert_eq!(
            wrapped.abstentions[0].0,
            RepositoryAbstentionReason::UnknownWrapper
        );
        let dynamic = capture(
            "git commit -m dynamic && git rev-parse $HEAD",
            Value::String(oid.to_owned()),
        );
        assert!(dynamic.outcomes.is_empty());
        assert_eq!(
            dynamic.abstentions[0].0,
            RepositoryAbstentionReason::UnsupportedShell
        );
        assert!(repository_result_evidence(
            &json!({"output": {"commit_oid": oid}}),
            &context("git ci -m alias"),
            "call-origin",
            [9; 32],
            10,
            &success(),
        )
        .is_none());

        let mut overflow = context("git commit -m exact");
        overflow.continuation_capacity_exceeded = true;
        let overflow = repository_result_evidence(
            &json!({"output": {"commit_oid": oid}}),
            &overflow,
            "call-origin",
            [9; 32],
            10,
            &success(),
        )
        .unwrap();
        assert!(overflow.outcomes.is_empty());
        assert_eq!(
            overflow.abstentions[0].0,
            RepositoryAbstentionReason::LinkageCapacityExceeded
        );
    }

    #[test]
    fn incidental_nested_commit_oid_is_not_an_outcome_schema() {
        let oid = "0123456789abcdef0123456789abcdef01234567";
        let captured = capture(
            "git commit -m exact",
            json!({"message": {"commit_oid": oid}}),
        );
        assert!(captured.outcomes.is_empty());
        assert_eq!(
            captured.abstentions[0].0,
            RepositoryAbstentionReason::OutcomeResultInadmissible
        );

        let ambiguous_top_level = repository_result_evidence(
            &json!({
                "output": {"commit_oid": oid},
                "result": {"commit_oid": oid}
            }),
            &context("git commit -m exact"),
            "call-origin",
            [9; 32],
            10,
            &success(),
        )
        .unwrap();
        assert!(ambiguous_top_level.outcomes.is_empty());
        assert_eq!(
            ambiguous_top_level.abstentions[0].0,
            RepositoryAbstentionReason::OutcomeResultInadmissible
        );
    }
}
