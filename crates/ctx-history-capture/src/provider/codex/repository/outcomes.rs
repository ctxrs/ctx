use ctx_history_core::{
    RepositoryAbstentionReason, RepositoryAlias, CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
    CORE_REPOSITORY_PULL_REQUEST_ASSOCIATION_CAPTURE_REVISION,
};
use serde_json::{json, Value};

use crate::{
    provider::codex::events::CodexToolCallContext,
    repository_attribution::{
        bounded_outcome_evidence_relevant, bounded_pull_request_association_query,
        lexical_absolute, linked_outcome_evidence, LinkedOutcomeEvidence, LinkedOutcomeInput,
        UnscopedOutcomeObservation, UnscopedPullRequestAssociationObservation,
    },
    OutputOutcome, OutputOutcomeMetadata,
};

const MAX_CODEX_EXEC_RESULT_ENVELOPE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexRepositoryResultEvidence {
    pub(crate) origin_occurred_at_unix_ms: Option<i64>,
    pub(crate) command: Option<String>,
    pub(crate) command_too_large: bool,
    pub(crate) declared_workdir: Option<String>,
    pub(crate) outcome_operation_repository_path: Option<String>,
    pub(crate) outcome_output_repository_path: Option<String>,
    pub(crate) structured_content: Value,
    pub(crate) provider_native_repository_aliases: Vec<RepositoryAlias>,
    pub(crate) outcomes: Vec<UnscopedOutcomeObservation>,
    pub(crate) pull_request_associations: Vec<UnscopedPullRequestAssociationObservation>,
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
    let command = context.exact_command.as_deref();
    if command.is_none() {
        return context
            .command_too_large
            .then(|| CodexRepositoryResultEvidence {
                origin_occurred_at_unix_ms: context.origin_occurred_at_unix_ms,
                command: None,
                command_too_large: true,
                declared_workdir: context.declared_workdir.clone(),
                outcome_operation_repository_path: None,
                outcome_output_repository_path: None,
                structured_content: result_summary(
                    context,
                    result_call_id,
                    result_record_sha256,
                    0,
                    0,
                ),
                provider_native_repository_aliases: Vec::new(),
                outcomes: Vec::new(),
                pull_request_associations: Vec::new(),
                abstentions: Vec::new(),
            });
    }
    let command = command?;
    let missing_output = super::repository_result_output(payload).is_none();
    let null_output = Value::Null;
    let provider_output = super::repository_result_output(payload).unwrap_or(&null_output);
    let exact_output;
    let unwrap_repository_evidence = context.tool_name == "exec_command"
        && (bounded_outcome_evidence_relevant(command)
            || context
                .declared_workdir
                .as_deref()
                .and_then(|workdir| lexical_absolute(workdir, None))
                .and_then(|base| bounded_pull_request_association_query(command, &base))
                .is_some());
    let output = if unwrap_repository_evidence {
        match provider_output.as_str().map(exact_codex_exec_result_body) {
            Some(Ok(Some(body))) => {
                exact_output = Value::String(body.to_owned());
                &exact_output
            }
            Some(Err(())) => &null_output,
            Some(Ok(None)) | None => provider_output,
        }
    } else {
        provider_output
    };
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
    let captured_associations = linked.pull_request_associations.len();
    Some(CodexRepositoryResultEvidence {
        origin_occurred_at_unix_ms: context.origin_occurred_at_unix_ms,
        command: Some(command.to_owned()),
        command_too_large: false,
        declared_workdir: context.declared_workdir.clone(),
        outcome_operation_repository_path: linked.outcome_operation_repository_path,
        outcome_output_repository_path: linked.outcome_output_repository_path,
        structured_content: result_summary(
            context,
            result_call_id,
            result_record_sha256,
            captured_outcomes,
            captured_associations,
        ),
        provider_native_repository_aliases: linked.provider_native_repository_aliases,
        outcomes: linked.outcomes,
        pull_request_associations: linked.pull_request_associations,
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
    linked.pull_request_associations.clear();
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
    captured_associations: usize,
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
            "pull_request_association_capture_revision": CORE_REPOSITORY_PULL_REQUEST_ASSOCIATION_CAPTURE_REVISION,
            "captured_pull_request_associations": captured_associations,
            "raw_output_retained": false,
        }
    })
}

fn exact_codex_exec_result_body(output: &str) -> Result<Option<&str>, ()> {
    if !output.starts_with("Chunk ID: ") {
        return if output
            .lines()
            .any(|line| line.trim().starts_with("Chunk ID: "))
        {
            Err(())
        } else {
            Ok(None)
        };
    }
    if output.is_empty()
        || output.len() > MAX_CODEX_EXEC_RESULT_ENVELOPE_BYTES
        || output.contains('\0')
    {
        return Err(());
    }
    let (chunk_id, remainder) = output
        .strip_prefix("Chunk ID: ")
        .and_then(|value| value.split_once('\n'))
        .ok_or(())?;
    if chunk_id.len() != 6
        || !chunk_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(());
    }
    let (wall_time, remainder) = remainder
        .strip_prefix("Wall time: ")
        .and_then(|value| value.split_once(" seconds\n"))
        .ok_or(())?;
    if wall_time.is_empty() || wall_time.len() > 32 {
        return Err(());
    }
    let mut wall_time_components = wall_time.split('.');
    let whole = wall_time_components.next().ok_or(())?;
    let fractional = wall_time_components.next();
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || fractional.is_some_and(|value| {
            value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit())
        })
        || wall_time_components.next().is_some()
        || wall_time
            .parse::<f64>()
            .ok()
            .is_none_or(|seconds| !seconds.is_finite())
    {
        return Err(());
    }
    let remainder = remainder
        .strip_prefix("Process exited with code 0\n")
        .ok_or(())?;
    let body = if let Some(remainder) = remainder.strip_prefix("Original token count: ") {
        let (token_count, remainder) = remainder.split_once('\n').ok_or(())?;
        if token_count.is_empty()
            || token_count.len() > 20
            || !token_count.bytes().all(|byte| byte.is_ascii_digit())
            || token_count.parse::<u64>().is_err()
        {
            return Err(());
        }
        remainder.strip_prefix("Output:\n").ok_or(())?
    } else {
        remainder.strip_prefix("Final output:\n").ok_or(())?
    };
    if body.is_empty()
        || body.len() > MAX_CODEX_EXEC_RESULT_ENVELOPE_BYTES
        || body.lines().any(|line| {
            let line = line.trim();
            line.starts_with("Chunk ID: ")
                || line.starts_with("Wall time: ")
                || line.starts_with("Process exited with code ")
                || line.starts_with("Original token count: ")
                || line == "Output:"
                || line == "Final output:"
                || line.starts_with("Warning: truncated output (original token count: ")
                || line.starts_with("Warning: truncated output (original char count: ")
        })
    {
        return Err(());
    }
    Ok(Some(body))
}

fn hex_digest(value: &[u8; 32]) -> String {
    value.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_history_core::RepositoryOutcomeKind;

    fn exact_outcome(
        value: &UnscopedOutcomeObservation,
    ) -> &ctx_history_core::RepositoryOutcomeObservation {
        match value {
            UnscopedOutcomeObservation::Exact(outcome) => outcome,
            UnscopedOutcomeObservation::DeferredCommit(_) => panic!("expected exact outcome"),
        }
    }

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

    fn real_context(command: &str, workdir: &str, call_id: &str) -> CodexToolCallContext {
        CodexToolCallContext {
            tool_name: "exec_command".to_owned(),
            exact_command: Some(command.to_owned()),
            session_cwd: Some("/workspace/synthetic-project".to_owned()),
            declared_workdir: Some(workdir.to_owned()),
            origin_call_id: Some(call_id.to_owned()),
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
        assert_eq!(
            exact_outcome(&captured.outcomes[0]).kind,
            RepositoryOutcomeKind::Commit
        );
        assert_eq!(
            exact_outcome(&captured.outcomes[0]).produced_object_ids[0].hex,
            oid
        );
        assert_eq!(
            captured.structured_content["provider_native_tool_result"]["captured_outcomes"],
            1
        );
        assert!(!serde_json::to_string(&captured.structured_content)
            .unwrap()
            .contains(oid));
    }

    #[test]
    fn codex_captures_bounded_multiline_pull_request_association() {
        let command = "gh pr view 203 --json state,mergedAt,mergeCommit,url\n\
                       git fetch origin main\n\
                       git log -1 --oneline origin/main";
        let output = concat!(
            "{\"mergeCommit\":{\"oid\":\"103c0105645cc02c730f98eba2831fba854d3569\"},",
            "\"mergedAt\":\"2026-07-29T17:11:30Z\",\"state\":\"MERGED\",",
            "\"url\":\"https://github.com/ctxrs/ctx/pull/203\"}\n",
            "From github.com:ctxrs/ctx\n",
            "103c0105 merge\n",
        );
        let captured = repository_result_evidence(
            &json!({"output": output}),
            &context(command),
            "call-result",
            [8; 32],
            10,
            &success(),
        )
        .unwrap();
        assert!(captured.outcomes.is_empty());
        assert_eq!(captured.pull_request_associations.len(), 1);
        assert_eq!(
            captured.pull_request_associations[0].merged_as.hex,
            "103c0105645cc02c730f98eba2831fba854d3569"
        );
    }

    #[test]
    fn codex_unwraps_the_exact_exec_result_envelope() {
        let command = "gh pr view 203 --json state,mergedAt,mergeCommit,url\n\
                       git fetch origin main\n\
                       git log -1 --oneline origin/main";
        let call_id = "call_5eCij3bzNa2V5SUxyUIR9INA";
        let output = concat!(
            "Chunk ID: 57a5b6\n",
            "Wall time: 0.3549 seconds\n",
            "Process exited with code 0\n",
            "Original token count: 95\n",
            "Output:\n",
            "{\"mergeCommit\":{\"oid\":\"103c0105645cc02c730f98eba2831fba854d3569\"},",
            "\"mergedAt\":\"2026-07-28T01:19:34Z\",\"state\":\"MERGED\",",
            "\"url\":\"https://github.com/ctxrs/ctx/pull/203\"}\n",
            "From https://github.com/ctxrs/ctx\n",
            "103c01056 merge\n",
        );
        let captured = repository_result_evidence(
            &json!({"output": output}),
            &real_context(command, "/repo", call_id),
            call_id,
            [0x50; 32],
            10,
            &success(),
        )
        .unwrap();
        let [association] = captured.pull_request_associations.as_slice() else {
            panic!("expected exact pull request association");
        };
        assert_eq!(association.pull_request.number, 203);
        assert_eq!(
            association.merged_as.hex,
            "103c0105645cc02c730f98eba2831fba854d3569"
        );
    }

    #[test]
    fn codex_retains_real_multiline_commit_and_pull_request_outcomes() {
        let commit_command = concat!(
            "git add crates/ctx-cli/src/pro/commercial_lifecycle.rs ",
            "crates/ctx-cli/src/pro/commercial_lifecycle/tests.rs\n",
            "git commit -m \"fix(pro): refresh auth while Checkout polls\""
        );
        let commit_output = concat!(
            "Chunk ID: 042609\n",
            "Wall time: 0.0000 seconds\n",
            "Process exited with code 0\n",
            "Original token count: 36\n",
            "Output:\n",
            "[codex/v026-checkout-token-refresh 0c1ce19ed] ",
            "fix(pro): refresh auth while Checkout polls\n",
            " 2 files changed, 290 insertions(+), 34 deletions(-)"
        );
        let captured = repository_result_evidence(
            &json!({"output": commit_output}),
            &real_context(commit_command, "/repo", "call-commit"),
            "call-commit",
            [0x42; 32],
            10,
            &success(),
        )
        .unwrap();
        let [UnscopedOutcomeObservation::DeferredCommit(deferred)] = captured.outcomes.as_slice()
        else {
            panic!("expected one deferred commit: {captured:#?}");
        };
        assert_eq!(deferred.oid_prefix, "0c1ce19ed");

        let pull_request_command = concat!(
            "git push -u origin codex/v026-checkout-token-refresh\n",
            "gh pr create --base main --head codex/v026-checkout-token-refresh ",
            "--title \"fix(pro): refresh auth while Checkout polls\" --body '",
            "## Summary\n",
            "- refresh a rejected WorkOS access token noninteractively during long Checkout polling\n",
            "- keep account and subject identity pinned across refresh\n",
            "- zeroize superseded access tokens and reuse the refreshed credential for entitlement issuance\n",
            "- bound transient and fatal refresh behavior with regression coverage\n\n",
            "## Production finding\n",
            "A real unreleased-CLI Checkout dogfood remained open beyond the short WorkOS token lifetime and failed at approximately six minutes with HTTP 401. This patch keeps the supported 30-minute Checkout wait viable.\n\n",
            "## Validation\n",
            "- `bazel test //crates/ctx-cli:unit_tests --config=ci`\n",
            "- focused Checkout polling tests\n",
            "- `git diff --check`\n",
            "'"
        );
        let pull_request_output = concat!(
            "Chunk ID: 6c59d6\n",
            "Wall time: 2.2410 seconds\n",
            "Process exited with code 0\n",
            "Original token count: 122\n",
            "Output:\n",
            "remote: \n",
            "remote: Create a pull request for 'codex/v026-checkout-token-refresh' on GitHub by visiting:        \n",
            "remote:      https://github.com/ctxrs/ctx/pull/new/codex/v026-checkout-token-refresh        \n",
            "remote: \n",
            "To https://github.com/ctxrs/ctx.git\n",
            " * [new branch]          codex/v026-checkout-token-refresh -> codex/v026-checkout-token-refresh\n",
            "branch 'codex/v026-checkout-token-refresh' set up to track 'origin/codex/v026-checkout-token-refresh'.\n",
            "https://github.com/ctxrs/ctx/pull/203\n"
        );
        let captured = repository_result_evidence(
            &json!({"output": pull_request_output}),
            &real_context(pull_request_command, "/repo", "call-pr"),
            "call-pr",
            [0x43; 32],
            10,
            &success(),
        )
        .unwrap();
        let [outcome] = captured.outcomes.as_slice() else {
            panic!("expected one pull request creation: {captured:#?}");
        };
        assert_eq!(
            exact_outcome(outcome).kind,
            RepositoryOutcomeKind::PullRequestCreated
        );

        let ambiguous = format!("{pull_request_output}\nhttps://github.com/ctxrs/ctx/pull/204");
        let captured = repository_result_evidence(
            &json!({"output": ambiguous}),
            &real_context(pull_request_command, "/repo", "call-pr-ambiguous"),
            "call-pr-ambiguous",
            [0x45; 32],
            10,
            &success(),
        )
        .unwrap();
        assert!(captured.outcomes.is_empty());
        assert_eq!(
            captured.abstentions[0].0,
            RepositoryAbstentionReason::OutcomeResultInadmissible
        );

        let truncated = format!(
            "{pull_request_output}\n…42 tokens truncated…\nhttps://github.com/ctxrs/ctx/pull/204"
        );
        let captured = repository_result_evidence(
            &json!({"output": truncated}),
            &real_context(pull_request_command, "/repo", "call-pr-truncated"),
            "call-pr-truncated",
            [0x46; 32],
            10,
            &success(),
        )
        .unwrap();
        assert!(captured.outcomes.is_empty());
        assert_eq!(
            captured.abstentions[0].0,
            RepositoryAbstentionReason::OutcomeResultInadmissible
        );

        let omitted = format!(
            "{pull_request_output}[omitted 2 text items ...]\nhttps://github.com/ctxrs/ctx/pull/204\n"
        );
        let captured = repository_result_evidence(
            &json!({"output": omitted}),
            &real_context(pull_request_command, "/repo", "call-pr-omitted"),
            "call-pr-omitted",
            [0x47; 32],
            11,
            &success(),
        )
        .unwrap();
        assert!(captured.outcomes.is_empty());
        assert_eq!(
            captured.abstentions[0].0,
            RepositoryAbstentionReason::OutcomeResultInadmissible
        );
    }

    #[test]
    fn codex_retains_amended_commit_identity_without_claiming_replacement_lineage() {
        let oid = "68e96ac54a807931ef5629b1ef0fc0416e85d729";
        let command = concat!(
            "git status --short && git add crates/ctx-history-capture/src/provider/providers/openhands/tests.rs && ",
            "git commit --amend --no-edit && git rev-parse HEAD && git status --short"
        );
        let output = format!(
            "Chunk ID: d05827\nWall time: 0.0000 seconds\nProcess exited with code 0\nOriginal token count: 147\nOutput:\n M crates/ctx-history-capture/src/provider/providers/openhands/tests.rs\n[ctx/v026-pro-output-redesign-20260723 68e96ac54] feat(pro): materialize provider outputs outside core ingestion\n 190 files changed, 22435 insertions(+), 4456 deletions(-)\n{oid}"
        );
        let captured = repository_result_evidence(
            &json!({"output": output}),
            &real_context(command, "/repo", "call-amend"),
            "call-amend",
            [0x44; 32],
            10,
            &success(),
        )
        .unwrap();
        assert_eq!(
            exact_outcome(&captured.outcomes[0]).produced_object_ids[0].hex,
            oid
        );
        assert!(exact_outcome(&captured.outcomes[0])
            .replacement_lineage
            .is_empty());
        assert_eq!(
            captured.abstentions[0].0,
            RepositoryAbstentionReason::HistoryRewriteUnlinked
        );
    }

    #[test]
    fn codex_exec_result_envelope_is_fail_closed() {
        let command = "gh pr view 203 --json state,mergedAt,mergeCommit,url";
        let body = concat!(
            "{\"mergeCommit\":{\"oid\":\"103c0105645cc02c730f98eba2831fba854d3569\"},",
            "\"mergedAt\":\"2026-07-28T01:19:34Z\",\"state\":\"MERGED\",",
            "\"url\":\"https://github.com/ctxrs/ctx/pull/203\"}\n",
        );
        let exact = format!(
            "Chunk ID: 57a5b6\nWall time: 0.3549 seconds\nProcess exited with code 0\nOriginal token count: 95\nOutput:\n{body}"
        );
        for output in [
            exact.replacen("code 0", "code 1", 1),
            exact.replacen("0.3549 seconds", "3e-1 seconds", 1),
            exact.replacen("Original token count: 95\n", "", 1),
            exact.replacen("Output:\n", "Output:\nOutput:\n", 1),
            format!("{exact}{exact}"),
        ] {
            let captured = repository_result_evidence(
                &json!({"output": output}),
                &real_context(command, "/repo", "call-result"),
                "call-result",
                [0x58; 32],
                10,
                &success(),
            )
            .unwrap();
            assert!(captured.pull_request_associations.is_empty());
        }
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
