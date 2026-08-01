use ctx_history_core::{
    RepositoryAbstentionReason, RepositoryAlias, RepositoryOutcomeObservation,
    CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
};
use serde_json::{json, Value};

use crate::{
    provider::codex::events::CodexToolCallContext,
    repository_attribution::{linked_outcome_evidence, LinkedOutcomeEvidence, LinkedOutcomeInput},
    OutputOutcome, OutputOutcomeMetadata,
};

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
    let missing_output = super::repository_result_output(payload).is_none();
    let null_output = Value::Null;
    let output = super::repository_result_output(payload).unwrap_or(&null_output);
    let mut linked = linked_outcome_evidence(LinkedOutcomeInput {
        provider: "codex",
        command,
        session_cwd: context.session_cwd.as_deref(),
        declared_workdir: context.declared_workdir.as_deref(),
        origin_call_id: context.origin_call_id.as_deref().unwrap_or_default(),
        result_call_id,
        origin_event_sequence: context.origin_event_sequence.unwrap_or_default(),
        continuation_call_id_sha256: &context.continuation_call_id_sha256,
        result_record_sha256,
        observed_at_unix_ms,
        result_outcome: result_outcome.outcome,
        result_output: output,
        structured_commit_oid: None,
        output_repository_path: None,
    })?;

    if result_outcome.outcome == OutputOutcome::Success {
        if context.continuation_cell_id.is_some() && !super::terminal_continuation_result(payload) {
            replace_with_abstention(
                &mut linked,
                RepositoryAbstentionReason::OutcomeResultInadmissible,
                "continuation_result_has_no_exact_terminal_control",
            );
        } else if context.origin_call_id.is_none() || context.origin_event_sequence.is_none() {
            replace_with_abstention(
                &mut linked,
                RepositoryAbstentionReason::ProviderOutputUnjoined,
                "outcome_result_has_no_exact_origin",
            );
        } else if context.continuation_capacity_exceeded
            || context.continuation_call_id_sha256.len()
                > crate::provider::codex::nativepath::MAX_CODEX_TOOL_CONTEXTS
            || has_duplicate_digests(&context.continuation_call_id_sha256)
        {
            replace_with_abstention(
                &mut linked,
                RepositoryAbstentionReason::LinkageCapacityExceeded,
                "outcome_linkage_capacity_or_uniqueness_failed",
            );
        } else if context.correlation_ambiguous {
            replace_with_abstention(
                &mut linked,
                RepositoryAbstentionReason::ProviderOutputUnjoined,
                "outcome_call_result_correlation_is_ambiguous",
            );
        } else if missing_output {
            replace_with_abstention(
                &mut linked,
                RepositoryAbstentionReason::OutcomeResultInadmissible,
                "linked_outcome_result_has_no_exact_output_field",
            );
        }
    }

    let captured_outcomes = linked.outcomes.len();
    Some(CodexRepositoryResultEvidence {
        command: command.to_owned(),
        declared_workdir: context.declared_workdir.clone(),
        outcome_operation_repository_path: linked.outcome_operation_repository_path,
        outcome_output_repository_path: linked.outcome_output_repository_path,
        structured_content: result_summary(
            context,
            result_call_id,
            result_record_sha256,
            captured_outcomes,
        ),
        provider_native_repository_aliases: linked.provider_native_repository_aliases,
        outcomes: linked.outcomes,
        abstentions: linked.abstentions,
    })
}

fn replace_with_abstention(
    linked: &mut LinkedOutcomeEvidence,
    reason: RepositoryAbstentionReason,
    detail: &'static str,
) {
    linked.provider_native_repository_aliases.clear();
    linked.outcomes.clear();
    linked.abstentions = vec![(reason, detail)];
}

fn has_duplicate_digests(digests: &[[u8; 32]]) -> bool {
    let mut unique = std::collections::HashSet::with_capacity(digests.len());
    digests.iter().any(|digest| !unique.insert(digest))
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
    use ctx_history_core::RepositoryOutcomeKind;

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

    #[test]
    fn codex_uses_provider_neutral_exact_outcome_parser_and_redacted_summary() {
        let oid = "0123456789abcdef0123456789abcdef01234567";
        let captured = repository_result_evidence(
            &json!({"output": oid}),
            &context("git commit -m exact && git rev-parse --verify HEAD"),
            "call-origin",
            [9; 32],
            10,
            &success(),
        )
        .unwrap();
        assert_eq!(captured.outcomes[0].kind, RepositoryOutcomeKind::Commit);
        assert_eq!(captured.outcomes[0].produced_object_ids[0].hex, oid);
        assert_eq!(
            captured.structured_content["provider_native_tool_result"]["captured_outcomes"],
            1
        );
        assert!(!serde_json::to_string(&captured.structured_content)
            .unwrap()
            .contains(oid));
    }

    #[test]
    fn codex_specific_continuation_and_linkage_gates_fail_closed() {
        let oid = "0123456789abcdef0123456789abcdef01234567";
        let command = "git commit -m exact && git rev-parse HEAD";

        let mut continuation = context(command);
        continuation.continuation_cell_id = Some("cell-1".to_owned());
        let nonterminal = repository_result_evidence(
            &json!({"output": oid}),
            &continuation,
            "call-result",
            [1; 32],
            10,
            &success(),
        )
        .unwrap();
        assert!(nonterminal.outcomes.is_empty());
        assert_eq!(
            nonterminal.abstentions[0].0,
            RepositoryAbstentionReason::OutcomeResultInadmissible
        );

        let mut missing_origin = context(command);
        missing_origin.origin_event_sequence = None;
        let unjoined = repository_result_evidence(
            &json!({"output": oid}),
            &missing_origin,
            "call-result",
            [2; 32],
            10,
            &success(),
        )
        .unwrap();
        assert!(unjoined.outcomes.is_empty());
        assert_eq!(
            unjoined.abstentions[0].0,
            RepositoryAbstentionReason::ProviderOutputUnjoined
        );

        let mut duplicate = context(command);
        duplicate.continuation_call_id_sha256 = vec![[3; 32], [3; 32]];
        let overflow = repository_result_evidence(
            &json!({"output": oid}),
            &duplicate,
            "call-result",
            [4; 32],
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
    fn ambiguous_or_missing_top_level_output_is_not_outcome_evidence() {
        for payload in [
            json!({"output": "value", "result": "value"}),
            json!({"unrelated": "value"}),
        ] {
            let captured = repository_result_evidence(
                &payload,
                &context("git commit -m exact && git rev-parse HEAD"),
                "call-result",
                [5; 32],
                10,
                &success(),
            )
            .unwrap();
            assert!(captured.outcomes.is_empty());
            assert_eq!(
                captured.abstentions[0].0,
                RepositoryAbstentionReason::OutcomeResultInadmissible
            );
        }
    }
}
