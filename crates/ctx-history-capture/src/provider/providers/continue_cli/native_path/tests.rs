use std::{
    fs,
    path::{Path, PathBuf},
};

use serde_json::{json, Value};

use crate::{test_support_paths::tempdir, MAX_PROVIDER_JSONL_LINE_BYTES};

use super::*;

fn write_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}

fn write_session(root: &Path, file_name: &str, session: &Value) -> PathBuf {
    let path = root.join(file_name);
    write_json(&path, session);
    path
}

fn message(id: &str, role: &str, content: impl Into<String>) -> Value {
    json!({
        "id": id,
        "timestamp": "2026-01-01T00:00:00Z",
        "message": {
            "role": role,
            "content": content.into(),
        }
    })
}

fn tool_call(id: &str, status: &str, output: impl Into<String>) -> Value {
    json!({
        "id": id,
        "timestamp": "2026-01-01T00:00:01Z",
        "message": {
            "role": "assistant",
            "content": "",
        },
        "toolCallStates": [{
            "toolCallId": format!("call-{id}"),
            "toolCall": {
                "id": format!("call-{id}"),
                "type": "function",
                "function": {
                    "name": "shell",
                    "arguments": "{\"command\":\"printf fixture\"}",
                }
            },
            "status": status,
            "output": [{
                "name": "Result",
                "content": output.into(),
            }]
        }]
    })
}

fn session(session_id: &str, history: Vec<Value>) -> Value {
    json!({
        "sessionId": session_id,
        "title": format!("Session {session_id}"),
        "createdAt": "2026-01-01T00:00:00Z",
        "workspaceDirectory": format!("/workspace/{session_id}"),
        "mode": "agent",
        "history": history,
        "usage": {
            "promptTokens": 12,
            "completionTokens": 8,
        }
    })
}

struct CollectedPreparation {
    sources: Vec<CollectedSource>,
    outcomes: Vec<ContinueSourceOutcome>,
    stats: ContinuePreparationStats,
}

#[derive(Debug)]
struct CollectedSource {
    observation: ContinueSourceObservation,
    session: ContinueSessionRow,
    events: Vec<ContinueEventRow>,
    authority: ContinueGenerationAuthority,
    output_exclusion: ContinueOutputExclusionStats,
}

fn collect(discovery: &ContinueDiscovery) -> Result<CollectedPreparation, ContinueNativePathError> {
    let mut stream = prepare_continue_discovery(discovery)?;
    let mut outcomes = Vec::new();
    let mut sources = Vec::new();
    let mut active: Option<CollectedSource> = None;
    for outcome in stream.by_ref() {
        match outcome? {
            ContinueSourceOutcome::Page(page) => {
                let ContinuePreparedPage {
                    source,
                    events,
                    terminal,
                    authority,
                    output_exclusion,
                    ..
                } = *page;
                if let Some(source) = source {
                    assert!(active.is_none());
                    active = Some(CollectedSource {
                        observation: source.observation,
                        session: source.session,
                        events: Vec::new(),
                        authority: ContinueGenerationAuthority {
                            completeness: ContinueSourceCompleteness::Complete,
                            observed_history_items: None,
                            retained_events: 0,
                            rejected_items: 0,
                        },
                        output_exclusion: ContinueOutputExclusionStats::default(),
                    });
                }
                let collected = active.as_mut().expect("page source must start before rows");
                collected.events.extend(events);
                if terminal {
                    collected.authority = authority.expect("terminal page authority");
                    collected.output_exclusion =
                        output_exclusion.expect("terminal page output stats");
                    sources.push(active.take().expect("terminal source"));
                }
            }
            outcome @ (ContinueSourceOutcome::Incomplete(_) | ContinueSourceOutcome::Failed(_)) => {
                assert!(active.is_none());
                outcomes.push(outcome);
            }
        }
    }
    assert!(active.is_none());
    Ok(CollectedPreparation {
        sources,
        outcomes,
        stats: stream.stats(),
    })
}

fn prepare(root: &Path) -> (ContinueDiscovery, CollectedPreparation) {
    let discovery = discover_continue_root(root).unwrap();
    let prepared = collect(&discovery).unwrap();
    (discovery, prepared)
}

fn complete_sources(prepared: &CollectedPreparation) -> Vec<&CollectedSource> {
    prepared.sources.iter().collect()
}

#[test]
fn baseline_retains_messages_and_body_free_calls_but_no_results() {
    let temp = tempdir().unwrap();
    for session_number in 0..2 {
        let session_id = format!("continue-baseline-{session_number}");
        let mut history = Vec::new();
        for ordinal in 0..12 {
            let id = format!("{session_id}-event-{ordinal}");
            if matches!(ordinal, 2 | 6 | 10) {
                history.push(tool_call(
                    &id,
                    "done",
                    format!("EXCLUDED-OUTPUT-{session_number}-{ordinal}"),
                ));
            } else {
                history.push(message(&id, "assistant", format!("retained-{id}")));
            }
        }
        write_session(
            temp.path(),
            &format!("{session_id}.json"),
            &session(&session_id, history),
        );
    }

    let (_, prepared) = prepare(temp.path());
    let sources = complete_sources(&prepared);
    let events = sources
        .iter()
        .flat_map(|source| source.events.iter())
        .collect::<Vec<_>>();
    assert_eq!(sources.len(), 2);
    assert_eq!(events.len(), 24);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == ContinueEventKind::Message)
            .count(),
        18
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == ContinueEventKind::ToolCall)
            .count(),
        6
    );
    assert_eq!(prepared.stats.output_exclusion.native_results_observed, 6);
    assert_eq!(prepared.stats.output_exclusion.result_string_allocations, 0);
    assert_eq!(prepared.stats.output_exclusion.result_body_bytes_decoded, 0);
    assert!(events.iter().all(|event| {
        !event.body_json.contains("EXCLUDED-OUTPUT")
            && !event.search_text.contains("EXCLUDED-OUTPUT")
            && !event.preview.contains("EXCLUDED-OUTPUT")
            && !event.body_json.contains("arguments")
    }));
}

#[test]
fn pro_profile_retains_results_only_in_transient_output_pages() {
    let temp = tempdir().unwrap();
    write_session(
        temp.path(),
        "pro.json",
        &session(
            "continue-pro",
            vec![
                message("one", "user", "safe request"),
                tool_call("two", "done", "TRANSIENT-OUTPUT-ONLY"),
            ],
        ),
    );

    let discovery = discover_continue_root(temp.path()).unwrap();
    let mut stream =
        prepare_continue_discovery_with_profile(&discovery, ContinueNativeProfile::CoreAndPro)
            .unwrap();
    let mut outputs = Vec::new();
    let mut core_bodies = Vec::new();
    for outcome in stream.by_ref() {
        let ContinueSourceOutcome::Page(mut page) = outcome.unwrap() else {
            panic!("complete Pro fixture must emit only pages");
        };
        core_bodies.extend(page.events.iter().map(|event| event.body_json.clone()));
        let transient = page
            .transient_output
            .take()
            .expect("Pro profile must carry an output frontier page");
        assert!(transient.failure.is_none());
        outputs.extend(transient.observations);
    }

    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].kind, crate::OutputObservationKind::Command);
    assert_eq!(outputs[0].call_id.as_deref(), Some("call-two"));
    assert_eq!(outputs[0].outcome.outcome, crate::OutputOutcome::Success);
    assert!(String::from_utf8_lossy(&outputs[0].content).contains("TRANSIENT-OUTPUT-ONLY"));
    assert!(core_bodies
        .iter()
        .all(|body| !body.contains("TRANSIENT-OUTPUT-ONLY")));
    let stats = stream.stats().output_exclusion;
    assert_eq!(stats.result_string_allocations, 1);
    assert!(stats.result_body_bytes_decoded > 0);
    assert_eq!(stats.result_hashes_created, 0);
    assert_eq!(stats.result_previews_created, 0);
    assert_eq!(stats.result_touches_created, 0);
    assert_eq!(stats.result_fts_documents_created, 0);
}

#[test]
fn output_heavy_retains_exact_message_and_tool_call_counts() {
    let temp = tempdir().unwrap();
    let mut remaining_tool_calls = 9;
    for source_ordinal in 0..2 {
        let session_id = format!("output-heavy-{source_ordinal}");
        let mut history = Vec::with_capacity(12);
        for event_ordinal in 0..12 {
            let id = format!("{session_id}-{event_ordinal}");
            let make_tool_call = remaining_tool_calls > 0
                && (event_ordinal % 2 == 0 || source_ordinal == 1 && event_ordinal == 11);
            if make_tool_call {
                history.push(tool_call(
                    &id,
                    "done",
                    format!("OUTPUT-HEAVY-EXCLUDED-{source_ordinal}-{event_ordinal}"),
                ));
                remaining_tool_calls -= 1;
            } else {
                history.push(message(&id, "assistant", format!("retained-{id}")));
            }
        }
        write_session(
            temp.path(),
            &format!("{session_id}.json"),
            &session(&session_id, history),
        );
    }
    assert_eq!(remaining_tool_calls, 0);

    let (_, prepared) = prepare(temp.path());
    let events = complete_sources(&prepared)
        .into_iter()
        .flat_map(|source| source.events.iter())
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 24);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == ContinueEventKind::Message)
            .count(),
        15
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == ContinueEventKind::ToolCall)
            .count(),
        9
    );
    assert_eq!(prepared.stats.output_exclusion.native_results_observed, 9);
    assert!(events.iter().all(|event| {
        !event.body_json.contains("OUTPUT-HEAVY-EXCLUDED")
            && !event.search_text.contains("OUTPUT-HEAVY-EXCLUDED")
            && !event.preview.contains("OUTPUT-HEAVY-EXCLUDED")
    }));
}

#[test]
fn every_result_like_shape_is_classified_before_large_secret_allocation() {
    const SECRET_BYTES: usize = 512 * 1024;

    let temp = tempdir().unwrap();
    let secret = format!("ALLOC-SENSITIVE-SECRET-{}", "x".repeat(SECRET_BYTES));
    let raw = format!(
        r#"{{
          "sessionId":"allocation-sensitive",
          "title":"safe title",
          "history":[{{
            "message":{{
              "role":"assistant",
              "content":[
                {{"text":"safe mixed text","type":"text"}},
                {{"input":"{secret}","name":"shell","id":"mixed-call","type":"tool_use"}},
                {{"content":"{secret}","type":"tool_result"}},
                {{"content":"{secret}","type":"future_payload"}},
                {{"content":"{secret}","type":7}},
                {{"content":{{"nested":"{secret}"}},"type":"text"}},
                {{"text":"{secret}","type":"text","future":{{"nested":"shape"}}}}
              ]
            }},
            "editorState":{{"result":"{secret}"}},
            "contextItems":[
              {{"type":"context","name":"safe.rs","content":"safe context"}},
              {{"name":"Result","content":"{secret}"}},
              {{"type":"tool_output","content":"{secret}"}},
              {{"type":"future_context","content":"{secret}"}},
              {{"type":9,"content":"{secret}"}},
              {{"type":"text","content":{{"nested":"{secret}"}}}}
            ],
            "conversationSummary":{{"content":"{secret}"}},
            "toolCallStates":[{{
              "toolCallId":"safe-call",
              "toolCall":{{
                "id":"safe-call",
                "type":"function",
                "function":{{"name":"shell","arguments":"{secret}"}}
              }},
              "status":"done",
              "output":{{"content":"{secret}"}},
              "stdout":"{secret}"
            }},{{
              "futureResult":{{"nested":"{secret}"}}
            }}]
          }},{{
            "message":{{"content":"{secret}","role":"tool"}}
          }}]
        }}"#
    );
    assert!(raw.len() < MAX_PROVIDER_JSONL_LINE_BYTES);
    fs::write(temp.path().join("session.json"), raw).unwrap();

    let (_, prepared) = prepare(temp.path());
    let source = complete_sources(&prepared)[0];
    let event = &source.events[0];
    let stats = source.output_exclusion;

    assert!(event.body_json.contains("safe mixed text"));
    assert!(event.body_json.contains("safe context"));
    assert!(event.body_json.contains("safe-call"));
    assert!(event.body_json.contains("mixed-call"));
    assert!(!event.body_json.contains("ALLOC-SENSITIVE-SECRET"));
    assert!(!event.search_text.contains("ALLOC-SENSITIVE-SECRET"));
    assert!(!event.preview.contains("ALLOC-SENSITIVE-SECRET"));
    assert!(!event.body_json.contains("arguments"));
    assert!(stats.native_results_observed >= 4);
    assert!(stats.unproven_payloads_skipped >= 8);
    assert!(
        stats.result_payload_bytes_skipped
            >= u64::try_from(SECRET_BYTES.saturating_mul(12)).unwrap()
    );
    assert!(
        stats.call_body_bytes_skipped >= u64::try_from(SECRET_BYTES.saturating_mul(2)).unwrap()
    );
    assert_eq!(stats.result_string_allocations, 0);
    assert_eq!(stats.result_body_bytes_decoded, 0);
    assert_eq!(stats.result_hashes_created, 0);
    assert_eq!(stats.result_previews_created, 0);
    assert_eq!(stats.result_touches_created, 0);
    assert_eq!(stats.result_fts_documents_created, 0);
    assert_eq!(stats.result_handoffs_created, 0);
    assert!(
        stats.retained_decode_string_bytes < 32 * 1024,
        "large skipped secrets must not appear in decoded retained-string accounting"
    );
}

#[test]
fn mixed_block_classification_is_independent_of_object_field_order() {
    let temp = tempdir().unwrap();
    fs::write(
        temp.path().join("session.json"),
        br#"{
          "sessionId":"field-order",
          "history":[{
            "message":{"role":"assistant","content":[
              {"text":"retained-before-tag","type":"text"},
              {"content":"FIELD-ORDER-SECRET","type":"command_output"}
            ]}
          }]
        }"#,
    )
    .unwrap();

    let (_, prepared) = prepare(temp.path());
    let event = &complete_sources(&prepared)[0].events[0];
    assert!(event.body_json.contains("retained-before-tag"));
    assert!(!event.body_json.contains("FIELD-ORDER-SECRET"));
}

#[test]
fn retained_role_and_type_admission_rejects_unknown_numeric_and_conflicting_records() {
    const SECRET_BYTES: usize = 64 * 1024;

    let temp = tempdir().unwrap();
    let secret = "s".repeat(SECRET_BYTES);
    let raw = format!(
        r#"{{
          "sessionId":"positive-admission",
          "history":[
            {{"message":{{"role":"user","content":"SAFE-USER"}}}},
            {{"message":{{"role":7,"content":"NUMERIC-{secret}"}}}},
            {{"message":{{"role":"future_output","content":"UNKNOWN-{secret}"}}}},
            {{"message":{{"role":"assistant","content":"ROLE-CONFLICT-{secret}","role":"tool"}}}},
            {{"message":{{"role":"tool","content":"ROLE-CONFLICT-REVERSE-{secret}","role":"assistant"}}}},
            {{"message":{{"role":"assistant","content":[
              {{"type":"tool_result","kind":"text","content":"TAG-CONFLICT-{secret}"}}
            ]}}}},
            {{"message":{{"role":"assistant","content":[
              {{"kind":"text","type":"tool_result","content":"TAG-CONFLICT-REVERSE-{secret}"}}
            ]}}}},
            {{"message":{{"role":"assistant","content":[
              {{"type":9,"content":"NUMERIC-TAG-{secret}"}}
            ]}}}},
            {{"message":{{"role":"assistant","content":[
              {{"type":"future_output","content":"UNKNOWN-TAG-{secret}"}}
            ]}}}},
            {{"message":{{"role":"assistant","content":[
              {{"type":"tool_call","id":"body-free-call"}}
            ]}}}}
          ]
        }}"#
    );
    fs::write(temp.path().join("session.json"), raw).unwrap();

    let (_, prepared) = prepare(temp.path());
    let source = complete_sources(&prepared)[0];
    assert_eq!(source.events.len(), 2);
    assert_eq!(source.events[0].identity.history_ordinal, 0);
    assert_eq!(source.events[0].search_text, "SAFE-USER");
    assert_eq!(source.events[1].identity.history_ordinal, 9);
    assert_eq!(source.events[1].kind, ContinueEventKind::ToolCall);
    assert_eq!(
        source.events[1].calls[0].call_id.as_deref(),
        Some("body-free-call")
    );
    for event in &source.events {
        assert!(!event.body_json.contains(&secret));
        assert!(!event.search_text.contains(&secret));
        assert!(!event.preview.contains(&secret));
    }
    assert_eq!(source.authority.observed_history_items, Some(10));
    assert_eq!(source.authority.retained_events, 2);
    assert_eq!(source.authority.rejected_items, 8);
    assert_eq!(source.output_exclusion.result_string_allocations, 0);
    assert_eq!(source.output_exclusion.result_body_bytes_decoded, 0);
    assert_eq!(source.output_exclusion.result_hashes_created, 0);
    assert_eq!(source.output_exclusion.result_previews_created, 0);
    assert!(
        source.output_exclusion.retained_decode_string_bytes < u64::try_from(SECRET_BYTES).unwrap(),
        "rejected role/type payloads must remain borrowed spans"
    );
}

#[test]
fn message_and_history_discriminators_invalidate_bodies_before_allocation() {
    const SECRET_BYTES: usize = 128 * 1024;

    let temp = tempdir().unwrap();
    let secret = format!("ENVELOPE-DISCRIMINATOR-SECRET-{}", "d".repeat(SECRET_BYTES));
    let raw = format!(
        r#"{{
          "sessionId":"envelope-discriminators",
          "history":[
            {{"message":{{"role":"assistant","type":"tool_result","content":"{secret}"}}}},
            {{"message":{{"role":"assistant","kind":"future_output","content":"{secret}"}}}},
            {{"type":"command_output","editorState":"{secret}"}},
            {{"kind":"future_payload","conversationSummary":"{secret}"}},
            {{"message":{{"role":"assistant","type":"message","content":"SAFE-MESSAGE"}}}},
            {{"type":"message","message":{{"role":"user","content":"SAFE-HISTORY"}}}}
          ]
        }}"#
    );
    fs::write(temp.path().join("session.json"), raw).unwrap();

    let (_, prepared) = prepare(temp.path());
    let source = complete_sources(&prepared)[0];
    assert_eq!(source.authority.observed_history_items, Some(6));
    assert_eq!(source.authority.retained_events, 2);
    assert_eq!(source.authority.rejected_items, 4);
    assert_eq!(
        source
            .events
            .iter()
            .map(|event| event.search_text.as_str())
            .collect::<Vec<_>>(),
        ["SAFE-MESSAGE", "SAFE-HISTORY"]
    );
    assert!(source.events.iter().all(|event| {
        !event.body_json.contains("ENVELOPE-DISCRIMINATOR-SECRET")
            && !event.search_text.contains("ENVELOPE-DISCRIMINATOR-SECRET")
            && !event.preview.contains("ENVELOPE-DISCRIMINATOR-SECRET")
    }));
    assert_eq!(source.output_exclusion.result_string_allocations, 0);
    assert_eq!(source.output_exclusion.result_body_bytes_decoded, 0);
    assert_eq!(source.output_exclusion.result_hashes_created, 0);
    assert_eq!(source.output_exclusion.result_previews_created, 0);
    assert!(
        source.output_exclusion.retained_decode_string_bytes < u64::try_from(SECRET_BYTES).unwrap(),
        "message/history discriminator rejection must happen while bodies are borrowed spans"
    );
}

#[test]
fn duplicate_message_fields_fail_closed_before_body_allocation_in_both_orders() {
    const SECRET_BYTES: usize = 128 * 1024;

    let temp = tempdir().unwrap();
    let secret = format!("DUPLICATE-MESSAGE-SECRET-{}", "m".repeat(SECRET_BYTES));
    let raw = format!(
        r#"{{
          "sessionId":"duplicate-message-fields",
          "history":[
            {{
              "message":{{"role":"tool","content":"{secret}"}},
              "message":{{"role":"assistant","content":"{secret}"}}
            }},
            {{
              "message":{{"role":"assistant","content":"{secret}"}},
              "message":{{"role":"tool","content":"{secret}"}}
            }},
            {{"message":{{"role":"assistant","content":"SAFE-AFTER-DUPLICATES"}}}}
          ]
        }}"#
    );
    fs::write(temp.path().join("session.json"), raw).unwrap();

    let (_, prepared) = prepare(temp.path());
    let source = complete_sources(&prepared)[0];
    assert_eq!(source.authority.observed_history_items, Some(3));
    assert_eq!(source.authority.retained_events, 1);
    assert_eq!(source.authority.rejected_items, 2);
    assert_eq!(source.events[0].identity.history_ordinal, 2);
    assert_eq!(source.events[0].search_text, "SAFE-AFTER-DUPLICATES");
    assert!(!source.events[0]
        .body_json
        .contains("DUPLICATE-MESSAGE-SECRET"));
    assert!(
        source.output_exclusion.retained_decode_string_bytes < u64::try_from(SECRET_BYTES).unwrap(),
        "neither duplicate-field order may decode a discarded message body"
    );
}

#[test]
fn conflicting_duplicate_session_ids_fail_with_exact_source_evidence() {
    let temp = tempdir().unwrap();
    fs::write(
        temp.path().join("conflict-a.json"),
        br#"{"sessionId":"identity-a","sessionId":"identity-b","history":[]}"#,
    )
    .unwrap();
    fs::write(
        temp.path().join("conflict-b.json"),
        br#"{"sessionId":"identity-b","sessionId":"identity-a","history":[]}"#,
    )
    .unwrap();
    fs::write(
        temp.path().join("identical.json"),
        br#"{
          "sessionId":"identity-same",
          "sessionId":"identity-same",
          "history":[{"message":{"role":"user","content":"accepted"}}]
        }"#,
    )
    .unwrap();

    let (_, prepared) = prepare(temp.path());
    let failures = prepared
        .outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            ContinueSourceOutcome::Failed(failure) => Some(failure),
            ContinueSourceOutcome::Incomplete(_) | ContinueSourceOutcome::Page(_) => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(failures.len(), 2);
    assert!(failures.iter().all(|failure| {
        failure.kind == ContinueSourceFailureKind::InvalidSessionId
            && failure.observation.as_ref().is_some_and(|observation| {
                observation.raw_bytes() > 0 && !observation.session_revision().is_empty()
            })
            && !failure.message.contains("identity-a")
            && !failure.message.contains("identity-b")
    }));
    assert_ne!(
        failures[0].observation.as_ref().unwrap().session_revision(),
        failures[1].observation.as_ref().unwrap().session_revision(),
        "field-order variants must retain distinct exact source revisions as conflict evidence"
    );
    let source = complete_sources(&prepared);
    assert_eq!(source.len(), 1);
    assert_eq!(source[0].session.identity.0, "identity-same");
    assert_eq!(source[0].events[0].search_text, "accepted");
}

#[test]
fn pure_result_records_are_omitted_but_body_free_call_requests_are_retained() {
    let temp = tempdir().unwrap();
    fs::write(
        temp.path().join("session.json"),
        br#"{
          "sessionId":"pure-results",
          "history":[
            {"output":"PURE-OUTPUT-SECRET"},
            {"message":{"role":"tool","content":"ROLE-RESULT-SECRET"}},
            {"toolCallStates":[{"toolCallId":null,"output":"STATE-RESULT-SECRET"}]},
            {"toolCallStates":[{"toolCallId":"body-free-request"}]},
            {"message":{"role":"assistant","content":"SAFE-MESSAGE"}}
          ]
        }"#,
    )
    .unwrap();

    let (_, prepared) = prepare(temp.path());
    let source = complete_sources(&prepared)[0];
    assert_eq!(source.authority.observed_history_items, Some(5));
    assert_eq!(source.authority.retained_events, 2);
    assert_eq!(source.authority.rejected_items, 3);
    assert_eq!(source.events.len(), 2);
    assert_eq!(source.events[0].identity.history_ordinal, 3);
    assert_eq!(source.events[0].kind, ContinueEventKind::ToolCall);
    assert_eq!(
        source.events[0].calls[0].call_id.as_deref(),
        Some("body-free-request")
    );
    assert_eq!(source.events[1].identity.history_ordinal, 4);
    assert_eq!(source.events[1].search_text, "SAFE-MESSAGE");
    assert!(source.events.iter().all(|event| {
        !event.body_json.contains("SECRET")
            && !event.search_text.contains("SECRET")
            && !event.preview.contains("SECRET")
    }));
}

#[test]
fn optional_index_is_typed_metadata_and_never_invalidates_session() {
    let temp = tempdir().unwrap();
    write_session(
        temp.path(),
        "session.json",
        &session("optional-index", vec![message("one", "user", "hello")]),
    );

    let (missing_discovery, missing_prepared) = prepare(temp.path());
    assert_eq!(
        missing_discovery.index().observation().state(),
        ContinueIndexState::Missing
    );
    assert_eq!(complete_sources(&missing_prepared).len(), 1);

    fs::write(temp.path().join("sessions.json"), b"[{\"broken\":").unwrap();
    let (malformed_discovery, malformed_prepared) = prepare(temp.path());
    assert_eq!(
        malformed_discovery.index().observation().state(),
        ContinueIndexState::Malformed
    );
    assert_eq!(complete_sources(&malformed_prepared).len(), 1);

    write_json(
        &temp.path().join("sessions.json"),
        &json!([{
            "sessionId": "optional-index",
            "title": "Indexed title",
            "workspaceDirectory": "/indexed",
            "messageCount": 1,
            "unknownLargePayload": "x".repeat(512 * 1024),
        }]),
    );
    let (ready_discovery, ready_prepared) = prepare(temp.path());
    assert_eq!(ready_discovery.stats().index_entries, 1);
    let metadata = complete_sources(&ready_prepared)[0]
        .session
        .index_metadata
        .as_ref()
        .unwrap();
    assert_eq!(metadata.title.as_deref(), Some("Indexed title"));
    assert_eq!(metadata.message_count, Some(1));
}

#[test]
fn optional_index_parses_once_into_a_capped_sorted_lookup_at_scale() {
    const INDEX_ENTRIES: usize = 8_192;

    let temp = tempdir().unwrap();
    let target_session_id = format!("index-{:04}", INDEX_ENTRIES - 1);
    write_session(
        temp.path(),
        "session.json",
        &session(
            &target_session_id,
            vec![message("one", "user", "indexed lookup")],
        ),
    );
    let mut index = String::from("[");
    for ordinal in 0..INDEX_ENTRIES {
        if ordinal > 0 {
            index.push(',');
        }
        index.push_str(&format!(
            r#"{{"sessionId":"index-{ordinal:04}","title":"Indexed {ordinal}"}}"#
        ));
    }
    index.push(']');
    fs::write(temp.path().join("sessions.json"), index).unwrap();

    let (discovery, prepared) = prepare(temp.path());
    let stats = discovery.stats();
    assert_eq!(stats.index_entries, INDEX_ENTRIES);
    assert_eq!(stats.index_resident_metadata_entries, INDEX_ENTRIES);
    let source = complete_sources(&prepared)[0];
    assert_eq!(
        source
            .session
            .index_metadata
            .as_ref()
            .and_then(|metadata| metadata.title.as_deref()),
        Some("Indexed 8191")
    );
}

#[test]
fn optional_index_rejects_entries_past_the_fixed_lookup_cap() {
    const INDEX_ENTRIES: usize = 8_193;

    let temp = tempdir().unwrap();
    write_session(
        temp.path(),
        "index-cap.json",
        &session("index-cap", vec![message("one", "user", "lookup")]),
    );
    let mut index = String::from("[");
    for ordinal in 0..INDEX_ENTRIES {
        if ordinal > 0 {
            index.push(',');
        }
        index.push_str(&format!(
            r#"{{"sessionId":"index-cap-{ordinal:04}","title":"Indexed {ordinal}"}}"#
        ));
    }
    index.push(']');
    fs::write(temp.path().join("sessions.json"), index).unwrap();

    let (discovery, prepared) = prepare(temp.path());
    assert_eq!(
        discovery.index().observation().state(),
        ContinueIndexState::Malformed
    );
    assert_eq!(discovery.stats().index_entries, 0);
    assert!(complete_sources(&prepared)[0]
        .session
        .index_metadata
        .is_none());
}

#[test]
fn index_and_session_metadata_mutate_without_event_identity_churn() {
    let temp = tempdir().unwrap();
    let session_path = write_session(
        temp.path(),
        "session.json",
        &session(
            "mutable-metadata",
            vec![message("event-metadata-id", "user", "stable body")],
        ),
    );
    write_json(
        &temp.path().join("sessions.json"),
        &json!([{"sessionId": "mutable-metadata", "title": "First title"}]),
    );
    let (first_discovery, first_prepared) = prepare(temp.path());
    let first = complete_sources(&first_prepared)[0];

    write_json(
        &temp.path().join("sessions.json"),
        &json!([{"sessionId": "mutable-metadata", "title": "Second title"}]),
    );
    let (second_discovery, second_prepared) = prepare(temp.path());
    let second = complete_sources(&second_prepared)[0];

    assert_eq!(
        first.observation.session_revision(),
        second.observation.session_revision()
    );
    assert_ne!(
        first_discovery.index().observation().dependency_revision(),
        second_discovery.index().observation().dependency_revision()
    );
    assert!(!first_discovery.index().revalidate());
    assert!(second_discovery.index().revalidate());
    assert_eq!(first.events, second.events);
    assert_ne!(first.session.metadata_hash, second.session.metadata_hash);
    assert_eq!(
        second
            .session
            .index_metadata
            .as_ref()
            .and_then(|metadata| metadata.title.as_deref()),
        Some("Second title")
    );
    assert!(first.observation.revalidate().unwrap());
    assert_eq!(first.observation.requested_path(), session_path);
}

#[test]
fn ordinal_identity_survives_rewrite_append_and_metadata_id_change() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("session.json");
    write_json(
        &path,
        &session(
            "stable-native-id",
            vec![
                message("metadata-a", "user", "first"),
                message("metadata-b", "assistant", "second"),
            ],
        ),
    );
    let (_, first_prepared) = prepare(temp.path());
    let first = complete_sources(&first_prepared)[0];

    write_json(
        &path,
        &session(
            "stable-native-id",
            vec![
                message("changed-metadata-id", "user", "first rewritten"),
                message("metadata-b", "assistant", "second"),
                message("metadata-c", "assistant", "appended"),
            ],
        ),
    );
    let (_, rewritten_prepared) = prepare(temp.path());
    let rewritten = complete_sources(&rewritten_prepared)[0];
    assert_eq!(first.events[0].identity, rewritten.events[0].identity);
    assert_ne!(
        first.events[0].content_hash,
        rewritten.events[0].content_hash
    );
    assert_eq!(first.events[1], rewritten.events[1]);
    assert_eq!(rewritten.events[2].identity.history_ordinal, 2);

    write_json(
        &path,
        &session(
            "stable-native-id",
            vec![
                message("only-id-changed", "user", "first rewritten"),
                message("metadata-b", "assistant", "second"),
                message("metadata-c", "assistant", "appended"),
            ],
        ),
    );
    let (_, id_only_prepared) = prepare(temp.path());
    let id_only = complete_sources(&id_only_prepared)[0];
    assert_eq!(
        rewritten.events[0].content_hash, id_only.events[0].content_hash,
        "history[].id is citation metadata, not content-hash authority"
    );
}

#[test]
fn incomplete_and_zero_row_sources_keep_distinct_authority_signals() {
    let temp = tempdir().unwrap();
    write_session(temp.path(), "empty.json", &session("empty", Vec::new()));
    fs::write(
        temp.path().join("incomplete.json"),
        br#"{"sessionId":"incomplete","history":["#,
    )
    .unwrap();
    fs::write(
        temp.path().join("malformed.json"),
        br#"{"sessionId":"malformed","history":]}"#,
    )
    .unwrap();

    let (discovery, prepared) = prepare(temp.path());
    assert!(discovery.root_authority().is_complete());
    assert_eq!(prepared.stats.complete_sources, 1);
    assert_eq!(prepared.stats.incomplete_sources, 1);
    assert_eq!(prepared.stats.failed_sources, 1);
    let empty = complete_sources(&prepared)[0];
    assert_eq!(
        empty.authority,
        ContinueGenerationAuthority {
            completeness: ContinueSourceCompleteness::Complete,
            observed_history_items: Some(0),
            retained_events: 0,
            rejected_items: 0,
        }
    );
    assert!(prepared.outcomes.iter().any(|outcome| matches!(
        outcome,
        ContinueSourceOutcome::Incomplete(source)
            if source.authority.completeness == ContinueSourceCompleteness::Incomplete
                && source.authority.observed_history_items.is_none()
    )));
    assert!(prepared.outcomes.iter().any(|outcome| matches!(
        outcome,
        ContinueSourceOutcome::Failed(failure)
            if failure.kind == ContinueSourceFailureKind::MalformedDocument
    )));
}

#[test]
fn valid_item_well_above_64k_is_preserved_without_silent_rejection() {
    let temp = tempdir().unwrap();
    let body = format!("LARGE-RETAINED-{}", "r".repeat(2 * 1024 * 1024));
    write_session(
        temp.path(),
        "session.json",
        &session(
            "large-retained",
            vec![message("large", "assistant", body.clone())],
        ),
    );

    let (_, prepared) = prepare(temp.path());
    let source = complete_sources(&prepared)[0];
    assert_eq!(source.events.len(), 1);
    assert_eq!(source.authority.rejected_items, 0);
    assert!(source.events[0].body_json.contains("LARGE-RETAINED"));
    assert_eq!(source.events[0].search_text, body);
}

#[test]
fn retained_item_above_the_8mib_page_bound_is_an_explicit_failure() {
    let temp = tempdir().unwrap();
    let retained_bytes = MAX_PROVIDER_JSONL_LINE_BYTES - (512 * 1024);
    let body = "n".repeat(retained_bytes);
    write_session(
        temp.path(),
        "session.json",
        &json!({
            "sessionId": "near-product-bound",
            "history": [{"message": {"role": "assistant", "content": body}}],
        }),
    );
    assert!(
        fs::metadata(temp.path().join("session.json"))
            .unwrap()
            .len()
            < MAX_PROVIDER_JSONL_LINE_BYTES as u64
    );

    let (_, prepared) = prepare(temp.path());
    assert!(complete_sources(&prepared).is_empty());
    assert!(prepared.outcomes.iter().any(|outcome| matches!(
        outcome,
        ContinueSourceOutcome::Failed(failure)
            if failure.kind == ContinueSourceFailureKind::RetainedItemTooLarge
                && failure.message.contains("page bound")
    )));
}

#[test]
fn source_above_the_16mib_bound_has_explicit_source_addressable_failure() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("too-large.json");
    fs::write(
        &path,
        format!(
            r#"{{"sessionId":"too-large","history":[],"padding":"{}"}}"#,
            "x".repeat(MAX_PROVIDER_JSONL_LINE_BYTES)
        ),
    )
    .unwrap();

    let discovery = discover_continue_root(temp.path()).unwrap();
    assert!(matches!(
        collect(&discovery),
        Err(ContinueNativePathError::SourceTooLarge {
            path: failed_path,
            limit: MAX_PROVIDER_JSONL_LINE_BYTES,
            ..
        }) if failed_path == path
    ));
}

#[test]
fn duplicate_native_ids_stream_store_resolvable_alias_and_conflict_evidence() {
    let temp = tempdir().unwrap();
    let source = session(
        "global-live-id",
        vec![message("event", "user", "same body")],
    );
    write_session(temp.path(), "a.json", &source);
    write_session(temp.path(), "b.json", &source);

    let discovery = discover_continue_root(temp.path()).unwrap();
    let prepared = collect(&discovery).unwrap();
    assert_eq!(prepared.stats.complete_sources, 2);
    assert_eq!(prepared.stats.observation_only_sources, 0);
    assert_eq!(prepared.stats.identity_entries_peak, 0);
    let sources = complete_sources(&prepared);
    assert_eq!(sources.len(), 2);
    assert_eq!(sources[0].session.identity, sources[1].session.identity);
    assert_eq!(
        sources[0].observation.session_revision(),
        sources[1].observation.session_revision()
    );

    write_session(
        temp.path(),
        "b.json",
        &session(
            "global-live-id",
            vec![message("event", "user", "conflicting body")],
        ),
    );
    let (_, conflicting) = prepare(temp.path());
    let conflicting = complete_sources(&conflicting);
    assert_eq!(conflicting.len(), 2);
    assert_eq!(
        conflicting[0].session.identity,
        conflicting[1].session.identity
    );
    assert_ne!(
        conflicting[0].observation.session_revision(),
        conflicting[1].observation.session_revision(),
        "the Store rollback boundary receives exact conflict evidence"
    );
}

#[test]
fn session_id_is_global_identity_and_missing_id_fails_closed() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("session.json");
    write_json(
        &path,
        &session(
            "global-before-replacement",
            vec![message("event", "user", "body")],
        ),
    );
    let (_, first_prepared) = prepare(temp.path());
    let first = complete_sources(&first_prepared)[0];

    write_json(
        &path,
        &session(
            "global-after-replacement",
            vec![message("event", "user", "body")],
        ),
    );
    let (_, replacement_prepared) = prepare(temp.path());
    let replacement = complete_sources(&replacement_prepared)[0];
    assert_ne!(first.session.identity, replacement.session.identity);
    assert_ne!(
        first.events[0].identity.session,
        replacement.events[0].identity.session
    );

    write_json(
        &path,
        &json!({"history": [message("event", "user", "no filename fallback")]}),
    );
    let (_, missing_id_prepared) = prepare(temp.path());
    assert!(missing_id_prepared.outcomes.iter().any(|outcome| matches!(
        outcome,
        ContinueSourceOutcome::Failed(failure)
            if failure.kind == ContinueSourceFailureKind::MissingSessionId
    )));
}

#[test]
fn exact_root_authority_binds_inventory_and_rejects_remove_restore_races() {
    let temp = tempdir().unwrap();
    let session_path = write_session(
        temp.path(),
        "session.json",
        &session(
            "authority",
            vec![message("event", "user", "unchanged bytes")],
        ),
    );
    fs::create_dir(temp.path().join("nested")).unwrap();
    fs::write(temp.path().join("nested").join("note.txt"), b"inventory").unwrap();

    let discovery = discover_continue_root(temp.path()).unwrap();
    let authority = discovery.root_authority().clone();
    assert_eq!(authority.discovered_sources(), 1);
    assert_eq!(authority.before_token(), authority.after_token());
    assert!(!authority.inventory_digest().is_empty());
    assert!(authority.revalidate().unwrap().authoritative);

    let held = temp.path().join("held.json");
    fs::rename(&session_path, &held).unwrap();
    fs::copy(&held, &session_path).unwrap();
    let replacement = authority.revalidate().unwrap();
    assert!(!replacement.authoritative);
    assert_ne!(replacement.inventory_digest, authority.inventory_digest());

    fs::remove_file(&session_path).unwrap();
    fs::rename(&held, &session_path).unwrap();
    assert!(
        !authority.revalidate().unwrap().authoritative,
        "restoring the original inode must not revive a stale root proof"
    );
}

#[cfg(windows)]
#[test]
fn windows_root_authority_binds_stable_directory_identity() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir(&root).unwrap();
    write_session(
        &root,
        "session.json",
        &session("windows-root-id", vec![message("one", "user", "body")]),
    );
    let authority = discover_continue_root(&root)
        .unwrap()
        .root_authority()
        .clone();
    let original_identity =
        source::metadata_identity(&fs::symlink_metadata(&root).unwrap(), &root).unwrap();

    let held = temp.path().join("held-sessions");
    fs::rename(&root, &held).unwrap();
    fs::create_dir(&root).unwrap();
    fs::copy(held.join("session.json"), root.join("session.json")).unwrap();
    let replacement_identity =
        source::metadata_identity(&fs::symlink_metadata(&root).unwrap(), &root).unwrap();

    assert_ne!(
        original_identity, replacement_identity,
        "Windows root identity must include volume/file ID and change evidence"
    );
    assert!(!authority.revalidate().unwrap().authoritative);
}

#[test]
fn exact_pending_page_does_not_claim_root_deletion_authority() {
    let temp = tempdir().unwrap();
    let selected = write_session(
        temp.path(),
        "selected.json",
        &session("selected", vec![message("one", "user", "selected")]),
    );
    write_session(
        temp.path(),
        "not-pending.json",
        &session("not-pending", vec![message("two", "user", "not selected")]),
    );

    let discovery =
        observe_continue_pending_paths(temp.path(), vec![selected.clone(), selected.clone()])
            .unwrap();
    let first_paths = discovery
        .paths()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let second_paths = discovery
        .paths()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let prepared = collect(&discovery).unwrap();
    assert!(!discovery.root_authority().is_complete());
    assert!(
        !discovery
            .root_authority()
            .revalidate()
            .unwrap()
            .authoritative
    );
    assert_eq!(discovery.stats().scanned_session_paths, 1);
    assert_eq!(first_paths, vec![selected.clone()]);
    assert_eq!(second_paths, first_paths);
    assert_eq!(complete_sources(&prepared).len(), 1);
}

#[test]
fn pending_paths_require_canonical_regular_direct_children() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("root");
    fs::create_dir(&root).unwrap();
    let selected = write_session(
        &root,
        "selected.json",
        &session("selected", vec![message("one", "user", "selected")]),
    );
    let outside = write_session(
        temp.path(),
        "outside.json",
        &session("outside", vec![message("two", "user", "outside")]),
    );
    fs::create_dir(root.join("nested.json")).unwrap();

    for path in [
        outside.clone(),
        root.join("..").join("outside.json"),
        root.join("nested.json"),
    ] {
        assert!(
            observe_continue_pending_paths(&root, vec![path.clone()]).is_err(),
            "pending path must be rejected: {}",
            path.display()
        );
    }

    let discovery = observe_continue_pending_paths(&root, vec![selected.clone()]).unwrap();
    assert_eq!(
        discovery
            .paths()
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap(),
        vec![selected.canonicalize().unwrap()]
    );
}

#[cfg(unix)]
#[test]
fn pending_paths_reject_file_symlinks_even_when_the_target_is_inside_root() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().unwrap();
    let root = temp.path().join("root");
    fs::create_dir(&root).unwrap();
    let target = write_session(
        &root,
        "target.json",
        &session("target", vec![message("one", "user", "target")]),
    );
    let link = root.join("linked.json");
    symlink(&target, &link).unwrap();

    assert!(observe_continue_pending_paths(&root, vec![link]).is_err());
}

#[test]
fn streaming_scale_keeps_one_document_and_page_resident_for_8191_files() {
    const SOURCES: usize = 8_191;

    let temp = tempdir().unwrap();
    for source_ordinal in 0..SOURCES {
        let session_id = format!("scale-{source_ordinal:04}");
        fs::write(
            temp.path().join(format!("{session_id}.json")),
            format!(
                r#"{{"sessionId":"{session_id}","history":[{{"message":{{"role":"user","content":"event-{source_ordinal}"}}}}]}}"#
            ),
        )
        .unwrap();
    }

    let discovery = discover_continue_root(temp.path()).unwrap();
    assert_eq!(discovery.stats().scanned_session_paths, SOURCES);
    assert_eq!(discovery.stats().inventory_entries, 8_192);
    assert!(discovery.stats().maximum_spool_record_bytes < 64 * 1024);
    assert_eq!(
        discovery.stats().maximum_directory_sort_entries,
        SOURCES,
        "flat traversal must retain one vector bounded by the 8,192-entry limit"
    );
    assert!(
        discovery.stats().maximum_directory_sort_entries <= 8_192,
        "directory ordering memory must remain below the fixed traversal limit"
    );
    assert!(
        discovery.stats().maximum_directory_sort_key_bytes <= SOURCES * 32,
        "ordering keys must remain bounded by the fixed entry vector"
    );
    let mut stream = prepare_continue_discovery(&discovery).unwrap();
    let mut published = 0_usize;
    for outcome in stream.by_ref() {
        match outcome.unwrap() {
            ContinueSourceOutcome::Page(page) => {
                assert_eq!(page.events.len(), 1);
                assert!(page.source.is_some());
                assert!(page.terminal);
                published += 1;
            }
            ContinueSourceOutcome::Incomplete(other) => {
                panic!("unexpected incomplete scale outcome: {other:?}")
            }
            ContinueSourceOutcome::Failed(other) => {
                panic!("unexpected failed scale outcome: {other:?}")
            }
        }
    }
    let stats = stream.stats();
    assert_eq!(published, SOURCES);
    assert_eq!(stats.source_content_reads, SOURCES);
    assert_eq!(stats.complete_sources, SOURCES);
    assert_eq!(stats.retained_events, SOURCES);
    assert_eq!(stats.identity_entries_peak, 0);
    assert_eq!(stats.maximum_resident_source_documents, 1);
    assert_eq!(stats.maximum_prepared_page_sources, 1);
}

#[test]
fn bounded_directory_ordering_preserves_exact_lexical_traversal_past_window() {
    const SOURCES: usize = 513;

    let temp = tempdir().unwrap();
    for ordinal in (0..SOURCES).rev() {
        fs::write(
            temp.path().join(format!("session-{ordinal:04}.json")),
            b"{}",
        )
        .unwrap();
    }

    let discovery = discover_continue_root(temp.path()).unwrap();
    let paths = discovery
        .paths()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let expected = (0..SOURCES)
        .map(|ordinal| temp.path().join(format!("session-{ordinal:04}.json")))
        .collect::<Vec<_>>();

    assert_eq!(paths, expected);
    assert_eq!(
        discovery.stats().maximum_directory_sort_entries,
        SOURCES,
        "the exact-order proof must exercise the single sorted directory vector"
    );
}

#[test]
fn dense_history_streams_under_the_row_page_bound() {
    const EVENTS: usize = 9_001;

    let temp = tempdir().unwrap();
    let mut raw = String::from(r#"{"sessionId":"dense-rows","history":["#);
    for ordinal in 0..EVENTS {
        if ordinal > 0 {
            raw.push(',');
        }
        raw.push_str(&format!(
            r#"{{"message":{{"role":"user","content":"dense-row-{ordinal}"}}}}"#
        ));
    }
    raw.push_str("]}");
    fs::write(temp.path().join("dense.json"), raw).unwrap();

    let discovery = discover_continue_root(temp.path()).unwrap();
    let mut stream = prepare_continue_discovery(&discovery).unwrap();
    let mut pages = 0_usize;
    let mut events = 0_usize;
    let mut expected_ordinal = 0_u64;
    for outcome in stream.by_ref() {
        let ContinueSourceOutcome::Page(page) = outcome.unwrap() else {
            panic!("dense source must emit only complete pages");
        };
        assert!(page.row_count <= CONTINUE_NATIVE_MAX_PAGE_ROWS);
        assert!(page.estimated_bytes <= CONTINUE_NATIVE_MAX_PAGE_BYTES);
        assert_eq!(page.page_ordinal, u64::try_from(pages).unwrap());
        assert_eq!(page.source.is_some(), pages == 0);
        for event in &page.events {
            assert_eq!(event.identity.history_ordinal, expected_ordinal);
            expected_ordinal += 1;
        }
        events += page.events.len();
        pages += 1;
        if page.terminal {
            assert_eq!(
                page.authority.as_ref().unwrap().observed_history_items,
                Some(EVENTS)
            );
        }
    }
    let stats = stream.stats();
    assert!(pages >= 3);
    assert_eq!(events, EVENTS);
    assert_eq!(stats.emitted_pages, pages);
    assert_eq!(stats.peak_page_rows, CONTINUE_NATIVE_MAX_PAGE_ROWS);
    assert!(stats.peak_page_bytes <= CONTINUE_NATIVE_MAX_PAGE_BYTES);
    assert_eq!(stats.maximum_resident_source_documents, 1);
    assert_eq!(stats.maximum_prepared_page_sources, 1);
    assert_eq!(stats.identity_entries_peak, 0);
}

#[test]
fn dense_history_streams_under_the_byte_page_bound() {
    const EVENTS: usize = 5;
    const BODY_BYTES: usize = 3 * 1024 * 1024 / 2;

    let temp = tempdir().unwrap();
    let mut history = Vec::with_capacity(EVENTS);
    for ordinal in 0..EVENTS {
        history.push(message(
            &format!("byte-{ordinal}"),
            "assistant",
            format!("{ordinal}-{}", "b".repeat(BODY_BYTES)),
        ));
    }
    write_session(
        temp.path(),
        "dense-bytes.json",
        &session("dense-bytes", history),
    );

    let discovery = discover_continue_root(temp.path()).unwrap();
    let mut stream = prepare_continue_discovery(&discovery).unwrap();
    let mut pages = 0_usize;
    let mut events = 0_usize;
    for outcome in stream.by_ref() {
        let ContinueSourceOutcome::Page(page) = outcome.unwrap() else {
            panic!("byte-dense source must emit only complete pages");
        };
        assert!(page.row_count <= CONTINUE_NATIVE_MAX_PAGE_ROWS);
        assert!(page.estimated_bytes <= CONTINUE_NATIVE_MAX_PAGE_BYTES);
        assert!(
            page.events.len() <= 2,
            "the byte cap, not the row cap, must split this source"
        );
        events += page.events.len();
        pages += 1;
    }
    let stats = stream.stats();
    assert_eq!(events, EVENTS);
    assert!(pages >= 3);
    assert_eq!(stats.emitted_pages, pages);
    assert!(stats.peak_page_bytes <= CONTINUE_NATIVE_MAX_PAGE_BYTES);
    assert!(stats.peak_page_rows < CONTINUE_NATIVE_MAX_PAGE_ROWS);
}
