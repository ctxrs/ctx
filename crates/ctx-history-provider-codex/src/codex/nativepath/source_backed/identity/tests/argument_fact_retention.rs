use super::*;
use ctx_history_core::{ActivityJsonCapture, LiteralFactKind};

use crate::codex::nativepath::{
    record::{parse_session_meta, CodexDecodedRecord, CodexRetainedKind},
    rows::build_source_backed_event_row,
};

fn argument_draft(bytes: usize) -> (CodexSessionRow, CodexCoreRecordDraft) {
    let owner = parse_session_meta(br#"{"type":"session_meta","payload":{"id":"019fb100-0000-7000-8000-000000000099","timestamp":"2026-09-14T00:00:00Z","cwd":"/workspace"}}"#).unwrap();
    let arguments = serde_json::json!({"cmd": "x".repeat(bytes)}).to_string();
    let encoded = serde_json::to_string(&arguments).unwrap();
    // Synthetic, with old facts on both sides of the optional argument facts.
    let raw = format!(
        r#"{{"type":"response_item","payload":{{"type":"function_call","id":"item","call_id":"call","name":"exec_command","path":"before.rs","arguments":{encoded},"url":"https://example.com/after"}}}}"#
    );
    assert!(raw.len() < crate::codex::nativepath::reader::MAX_CODEX_RECORD_BYTES);
    let retained = CodexDecodedRecord {
        occurred_at: owner.started_at,
        payload: serde_json::from_str::<serde_json::Value>(&raw).unwrap()["payload"].clone(),
    };
    let mut row = build_source_backed_event_row(
        0,
        CodexRetainedKind::ToolCall,
        &owner.native_session_id,
        &retained,
        raw.as_bytes(),
    )
    .unwrap()
    .unwrap()
    .row;
    row.session_cwd.clone_from(&owner.cwd);
    assert!(row.lexical_body.len() < ctx_history_core::MAX_CORE_CONTENT_BYTES);
    (owner, row)
}

fn finalize(owner: &CodexSessionRow, row: CodexCoreRecordDraft) -> Option<CoreRecord> {
    let source = codex_source_key_in_root(None, &owner.native_session_id).unwrap();
    let session = codex_session_identity(&source, &owner.native_session_id).unwrap();
    codex_core_record(
        &source,
        session,
        None,
        owner,
        row,
        &mut CodexEventIdentityStateV0::default(),
    )
    .unwrap()
}

fn assert_large_argument_retention(bytes: usize, arguments_present: bool) {
    let (owner, row) = argument_draft(bytes);
    let lexical_body = row.lexical_body.clone();
    // Before argument decoding this record had only the two outer facts.
    // Keep every other input identical to exercise the original size policy.
    let mut before_decoding = row.clone();
    before_decoding.decoded_argument_facts = None;
    before_decoding
        .activity
        .as_mut()
        .unwrap()
        .facts
        .retain(|fact| fact.kind != LiteralFactKind::Command);
    let baseline =
        finalize(&owner, before_decoding).expect("original size policy retains the event");
    let baseline_arguments = &baseline
        .content
        .activity
        .as_ref()
        .unwrap()
        .invocation
        .as_ref()
        .unwrap()
        .arguments;
    assert_eq!(
        matches!(baseline_arguments, ActivityJsonCapture::Present { .. }),
        arguments_present
    );
    let actual = finalize(&owner, row);
    assert!(
        actual.is_some(),
        "optional decoded facts must not reject a retained event"
    );
    let actual = actual.unwrap();
    actual.validate_contract().unwrap();
    assert!(actual.content.normalized_body.as_deref() == Some(lexical_body.as_str()));
    assert_eq!(actual.event_id, baseline.event_id);
    assert_eq!(actual.native_event_id, baseline.native_event_id);
    assert_eq!(actual.session_id, baseline.session_id);
    let activity = actual.content.activity.as_ref().unwrap();
    assert!(
        &activity.invocation.as_ref().unwrap().arguments == baseline_arguments,
        "optional facts must not displace previously retained arguments"
    );
    assert_eq!(
        activity.facts,
        baseline.content.activity.as_ref().unwrap().facts
    );
    assert_eq!(
        activity
            .facts
            .iter()
            .map(|fact| fact.value.as_str())
            .collect::<Vec<_>>(),
        ["/workspace", "before.rs", "https://example.com/after"]
    );
}

#[test]
fn nine_mib_decoded_literal_does_not_reject_final_core_record() {
    assert_large_argument_retention(9 * 1024 * 1024, false);
}

#[test]
fn six_mib_decoded_literal_does_not_displace_retained_arguments() {
    assert_large_argument_retention(6 * 1024 * 1024, true);
}

#[test]
fn fifteen_mib_decoded_literal_fits_the_draft_page_before_finalization() {
    let (owner, row) = argument_draft(15 * 1024 * 1024);
    assert!(
        row.estimated_owned_bytes().unwrap()
            < crate::codex::nativepath::reader::MAX_CODEX_SOURCE_BACKED_SINGLE_ROW_PAGE_BYTES
                - 4096,
        "optional facts must not exhaust the draft-page budget before Core finalization"
    );
    assert!(finalize(&owner, row).is_some());
}

#[test]
fn small_decoded_literal_survives_core_finalization_in_order() {
    let (owner, row) = argument_draft(16);
    let expected_arguments = row
        .activity
        .as_ref()
        .unwrap()
        .invocation
        .as_ref()
        .unwrap()
        .arguments
        .clone();
    let record = finalize(&owner, row).unwrap();
    record.validate_contract().unwrap();
    let activity = record.content.activity.unwrap();
    assert_eq!(activity.invocation.unwrap().arguments, expected_arguments);
    assert_eq!(
        activity
            .facts
            .iter()
            .map(|fact| fact.value.as_str())
            .collect::<Vec<_>>(),
        [
            "/workspace",
            "before.rs",
            "xxxxxxxxxxxxxxxx",
            "https://example.com/after"
        ]
    );
}

#[test]
fn session_facts_participate_in_the_exact_final_byte_budget() {
    use ctx_history_core::{ProviderDeclaredFact, MAX_CORE_CONTENT_BYTES};

    for extra in [0, 1] {
        let (owner, mut row) = argument_draft(1024);
        let mut expected_activity = row.activity.clone().unwrap();
        expected_activity.facts.insert(
            0,
            ProviderDeclaredFact {
                kind: LiteralFactKind::SessionCwd,
                value: "/workspace".to_owned(),
            },
        );
        let overhead = serde_json::to_vec(row.structured_content.as_ref().unwrap())
            .unwrap()
            .len()
            + serde_json::to_vec(&expected_activity).unwrap().len();
        // A synthetic draft places total final content exactly at/one byte over
        // the real budget, including the session fact added by finalization.
        row.lexical_body = "b".repeat(MAX_CORE_CONTENT_BYTES - overhead + extra);
        let body_bytes = row.lexical_body.len();
        let record = finalize(&owner, row).unwrap();
        record.validate_contract().unwrap();
        assert_eq!(
            record.content.normalized_body.as_ref().unwrap().len(),
            body_bytes
        );
        assert!(record.content.structured_content.is_some());
        let activity = record.content.activity.unwrap();
        if extra != 0 {
            expected_activity.facts.remove(2);
        }
        assert_eq!(activity, expected_activity);
    }
}

#[test]
fn session_fact_count_pressure_preserves_existing_literal_facts() {
    use ctx_history_core::{ProviderDeclaredFact, MAX_PROVIDER_DECLARED_FACTS};

    let (owner, mut row) = argument_draft(16);
    let activity = row.activity.as_mut().unwrap();
    activity.facts.extend(vec![
        ProviderDeclaredFact {
            kind: LiteralFactKind::File,
            value: "existing.rs".to_owned(),
        };
        MAX_PROVIDER_DECLARED_FACTS - 3
    ]);
    let mut expected = activity.facts.clone();
    expected.remove(1); // Only the decoded command, not either outer fact.
    expected.insert(
        0,
        ProviderDeclaredFact {
            kind: LiteralFactKind::SessionCwd,
            value: "/workspace".to_owned(),
        },
    );
    let record = finalize(&owner, row).unwrap();
    record.validate_contract().unwrap();
    assert_eq!(record.content.activity.unwrap().facts, expected);
}

#[test]
fn omitting_only_decoded_facts_does_not_leave_an_invalid_empty_activity() {
    let (owner, mut row) = argument_draft(1024);
    let activity = row.activity.as_mut().unwrap();
    let command = activity.facts.remove(1);
    activity.facts = vec![command];
    activity.provider_call_id = None;
    activity.invocation = None;
    row.decoded_argument_facts = Some(0..1);
    row.session_cwd = None;
    row.structured_content = None;
    row.lexical_body = "b".repeat(ctx_history_core::MAX_CORE_CONTENT_BYTES - 512);
    let record = finalize(&owner, row).unwrap();
    record.validate_contract().unwrap();
    assert!(record.content.activity.is_none());
}
