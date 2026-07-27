use ctx_history_core::{CaptureProvider, EventRole, EventType, ProviderEventEnvelope};
use serde_json::{json, Value};

use super::*;
use crate::{PROVIDER_MAX_PREVIEW_CHARS, PROVIDER_MAX_TEXT_CHARS};

#[test]
fn standalone_result_content_profile_tokens_are_stable_and_unique() {
    use crate::provider::providers::{
        claude::CLAUDE_RESULT_CONTENT_PROFILE, kimi::KIMI_RESULT_CONTENT_PROFILE,
        mistral_vibe::MISTRAL_VIBE_RESULT_CONTENT_PROFILE,
        openclaw::OPENCLAW_RESULT_CONTENT_PROFILE, openhands::OPENHANDS_RESULT_CONTENT_PROFILE,
        pi::PI_RESULT_CONTENT_PROFILE, rovodev::ROVODEV_RESULT_CONTENT_PROFILE,
        task_json::TASK_JSON_RESULT_CONTENT_PROFILE, zed::ZED_RESULT_CONTENT_PROFILE,
    };

    let profiles = [
        CLAUDE_RESULT_CONTENT_PROFILE,
        KIMI_RESULT_CONTENT_PROFILE,
        MISTRAL_VIBE_RESULT_CONTENT_PROFILE,
        OPENCLAW_RESULT_CONTENT_PROFILE,
        OPENHANDS_RESULT_CONTENT_PROFILE,
        PI_RESULT_CONTENT_PROFILE,
        ROVODEV_RESULT_CONTENT_PROFILE,
        TASK_JSON_RESULT_CONTENT_PROFILE,
        ZED_RESULT_CONTENT_PROFILE,
    ];
    assert_eq!(
        profiles,
        [
            "claude.result-body.v1",
            "kimi.result-body.v1",
            "mistral-vibe.result-body.v1",
            "openclaw-legacy-jsonl.result-body.v1",
            "openhands.result-body.v1",
            "pi.result-body.v1",
            "rovodev.result-body.v1",
            "task-json.result-body.v1",
            "zed.result-body.v1",
        ]
    );
    let unique = profiles
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(unique.len(), profiles.len());
}

fn test_native_event(event_type: EventType, text: &str, body: Value) -> ProviderEventEnvelope {
    test_native_event_for_provider(CaptureProvider::Codex, event_type, text, body)
}

fn test_native_event_for_provider(
    provider: CaptureProvider,
    event_type: EventType,
    text: &str,
    body: Value,
) -> ProviderEventEnvelope {
    native_event(NativeEventDraft {
        provider,
        source_format: "test_provider",
        provider_session_id: "session-1".to_owned(),
        provider_event_index: 1,
        provider_event_hash: None,
        cursor: "line:1".to_owned(),
        event_type,
        role: Some(EventRole::Assistant),
        occurred_at: "2026-07-07T12:00:00Z".parse().unwrap(),
        text: text.to_owned(),
        body,
        metadata: json!({}),
    })
}

#[test]
fn every_non_codex_provider_routes_through_safe_result_evidence_policy() {
    let narrative = "RESULT_NARRATIVE_MUST_NOT_RETAIN";
    let hash = "0123456789abcdef0123456789abcdef01234567";
    let url = "https://github.com/ctxrs/ctx/pull/123?token=secret#fragment";
    let text = format!("created commit {hash} {url} {narrative}");
    let providers = crate::provider_source_specs()
        .iter()
        .map(|spec| spec.provider)
        .filter(|provider| *provider != CaptureProvider::Codex)
        .collect::<Vec<_>>();
    assert_eq!(providers.len(), 40);

    for provider in providers {
        let call_id = format!("{}-call", provider.as_str());
        let event = test_native_event_for_provider(
            provider,
            EventType::CommandOutput,
            &text,
            json!({
                "call_id": call_id,
                "exit_code": 0,
                "output": text,
            }),
        );
        let rendered = event.payload.to_string();
        assert_eq!(event.payload["result_outcome"], "success");
        assert!(
            rendered.contains(hash),
            "{} lost commit id",
            provider.as_str()
        );
        assert!(
            rendered.contains("https://github.com/ctxrs/ctx/pull/123"),
            "{} lost forge artifact URL",
            provider.as_str()
        );
        assert!(
            rendered.contains(&call_id),
            "{} lost call/result correlation",
            provider.as_str()
        );
        assert!(
            !rendered.contains(narrative),
            "{} leaked narrative",
            provider.as_str()
        );
        assert!(!rendered.contains("token=secret"));
        assert!(!rendered.contains("fragment"));
    }
}

#[test]
fn result_evidence_abstains_on_unknown_and_keeps_failure_correlation_only() {
    let unknown = test_native_event(
        EventType::ToolOutput,
        "UNKNOWN_RESULT_NARRATIVE",
        json!({"call_id": "unknown-call"}),
    );
    assert_eq!(unknown.payload["text"], "");
    assert_eq!(unknown.payload["result_outcome"], Value::Null);
    assert_eq!(unknown.payload["text_retention"]["mode"], "none");
    assert!(unknown.payload.to_string().contains("unknown-call"));
    assert!(!unknown
        .payload
        .to_string()
        .contains("UNKNOWN_RESULT_NARRATIVE"));

    let failed = test_native_event(
        EventType::ToolOutput,
        "FAILED_RESULT_DIAGNOSTIC",
        json!({"call_id": "failed-call", "exit_code": 2}),
    );
    assert!(!failed
        .payload
        .to_string()
        .contains("FAILED_RESULT_DIAGNOSTIC"));
    assert!(failed.payload.to_string().contains("failed-call"));
    assert_eq!(failed.payload["result_outcome"], "failure");
    assert_eq!(failed.payload["text_retention"]["mode"], "none");
}

#[test]
fn result_outcome_requires_one_bounded_explicit_consistent_signal() {
    let providers = [
        CaptureProvider::Claude,
        CaptureProvider::Cursor,
        CaptureProvider::OpenHands,
    ];
    for provider in providers {
        let event =
            |body| test_native_event_for_provider(provider, EventType::ToolOutput, "", body);
        assert_eq!(
            event(json!({"call_id": "unknown"})).payload["result_outcome"],
            Value::Null,
            "{} treated call-ID-only evidence as success",
            provider.as_str()
        );
        assert_eq!(
            event(json!({"call_id": "success", "exit_code": 0})).payload["result_outcome"],
            "success"
        );
        assert_eq!(
            event(json!({"call_id": "failure", "success": false})).payload["result_outcome"],
            "failure"
        );
        assert_eq!(
            event(json!({"call_id": "truncated", "truncated": true})).payload["result_outcome"],
            Value::Null
        );
        assert_eq!(
            event(json!({
                "results": [
                    {"call_id": "one", "success": true},
                    {"call_id": "two", "exit_code": 1},
                ]
            }))
            .payload["result_outcome"],
            Value::Null,
            "{} collapsed ambiguous multi-call outcomes",
            provider.as_str()
        );
    }
}

#[test]
fn result_evidence_distinguishes_git_commit_summaries_from_other_oids() {
    let produced = test_native_event(
        EventType::CommandOutput,
        "[main 0123456789ab] add bounded evidence",
        json!({
            "call_id": "commit-call",
            "exit_code": 0,
            "output": "[main 0123456789ab] add bounded evidence",
        }),
    );
    assert_eq!(
        produced.payload["result_evidence"],
        json!([
            {"kind": "call_id", "value": "commit-call"},
            {"kind": "git_commit_summary_id", "value": "0123456789ab"},
        ])
    );

    let referenced = test_native_event(
        EventType::CommandOutput,
        "inspected 0123456789abcdef0123456789abcdef01234567",
        json!({
            "call_id": "show-call",
            "exit_code": 0,
            "output": "inspected 0123456789abcdef0123456789abcdef01234567",
        }),
    );
    assert_eq!(
        referenced.payload["result_evidence"],
        json!([
            {"kind": "call_id", "value": "show-call"},
            {"kind": "git_oid", "value": "0123456789abcdef0123456789abcdef01234567"},
        ])
    );

    let saturated_call_ids = (0..MAX_RESULT_EVIDENCE_IDENTIFIERS)
        .map(|index| json!({"tool_call_id": format!("call-{index}")}))
        .collect::<Vec<_>>();
    let saturated = provider_result_identifier_evidence(
        EventType::CommandOutput,
        "[main 0123456789ab] must not exceed the evidence bound",
        &json!({"success": true, "results": saturated_call_ids}),
    );
    assert_eq!(
        saturated.as_array().map(Vec::len),
        Some(MAX_RESULT_EVIDENCE_IDENTIFIERS)
    );
}

#[test]
fn native_event_retains_real_text_and_omits_noisy_body_fields() {
    let event = test_native_event(
        EventType::Message,
        "real conversation oracle",
        json!({
            "content": "real conversation oracle",
            "toolCallStates": {
                "output": "successful-output-oracle"
            },
            "diff": "*** Begin Patch\n- secret old\n+ secret new\n*** End Patch"
        }),
    );
    let rendered = event.payload.to_string();

    assert!(rendered.contains("real conversation oracle"));
    assert!(rendered.contains("field_retention"));
    assert!(rendered.contains("original_bytes"));
    assert!(rendered.contains("contained_patch_or_diff"));
    assert!(!rendered.contains("successful-output-oracle"));
    assert!(!rendered.contains("*** Begin Patch"));
    assert!(!rendered.contains("secret old"));
    assert!(!rendered.contains("secret new"));
}

#[test]
fn native_event_output_policy_keeps_typed_result_evidence_without_body_text() {
    let success = test_native_event(
            EventType::CommandOutput,
            "Created commit 0123456789abcdef0123456789abcdef01234567; https://github.com/ctxrs/ctx/pull/123",
            json!({
                "call_id": "call-success",
                "exit_code": 0,
                "output": "Created commit 0123456789abcdef0123456789abcdef01234567; https://github.com/ctxrs/ctx/pull/123"
            }),
        );
    let failed = test_native_event(
        EventType::CommandOutput,
        "failed-output-oracle",
        json!({
            "call_id": "call-failure",
            "exit_code": 2,
            "output": "failed-output-oracle"
        }),
    );
    let nested_failed = test_native_event(
        EventType::CommandOutput,
        "nested-failed-output-oracle",
        json!({
            "message": {
                "exitCode": 2,
                "output": "nested-failed-output-oracle"
            }
        }),
    );
    let http_success = test_native_event(
        EventType::CommandOutput,
        "http-success-output-oracle",
        json!({
            "statusCode": 200,
            "error": false,
            "output": "http-success-output-oracle"
        }),
    );
    let http_failed = test_native_event(
        EventType::CommandOutput,
        "http-failed-output-oracle",
        json!({
            "statusCode": 500,
            "output": "http-failed-output-oracle"
        }),
    );
    let failed_diff = test_native_event(
        EventType::CommandOutput,
        "diff --git a/src/lib.rs b/src/lib.rs\n@@\n-old raw diff\n+new raw diff\n",
        json!({
            "exit_code": 1,
            "output": "diff --git a/src/lib.rs b/src/lib.rs\n@@\n-old raw diff\n+new raw diff\n"
        }),
    );
    let successful_diff = test_native_event(
        EventType::CommandOutput,
        "diff --git a/src/lib.rs b/src/lib.rs\n@@\n-old success diff\n+new success diff\n",
        json!({
            "call_id": "call-success-diff",
            "exit_code": 0,
            "output": "diff --git a/src/lib.rs b/src/lib.rs\n@@\n-old success diff\n+new success diff\n"
        }),
    );
    let oversized_text = format!(
        "{}TAIL-SHOULD-BE-TRUNCATED",
        "x".repeat(PROVIDER_MAX_PREVIEW_CHARS)
    );
    let oversized = test_native_event(
        EventType::ToolOutput,
        &oversized_text,
        json!({
            "call_id": "call-oversized",
            "success": true,
            "output": oversized_text,
        }),
    );

    assert_eq!(
        success.payload["text_retention"],
        json!({
            "mode": "none",
            "limit_chars": null,
            "truncated": false,
            "omission_policy": "none",
            "omission_applied": false,
        })
    );
    let success_payload = success.payload.to_string();
    assert!(success_payload.contains("0123456789abcdef0123456789abcdef01234567"));
    assert!(success_payload.contains("https://github.com/ctxrs/ctx/pull/123"));
    assert!(success_payload.contains("call-success"));
    assert!(!success_payload.contains("Created commit"));

    assert_eq!(
        failed.payload["text_retention"],
        json!({
            "mode": "none",
            "limit_chars": null,
            "truncated": false,
            "omission_policy": "none",
            "omission_applied": false,
        })
    );
    let failed_payload = failed.payload.to_string();
    assert!(!failed_payload.contains("failed-output-oracle"));
    assert!(failed_payload.contains("call-failure"));

    let nested_failed_payload = nested_failed.payload.to_string();
    assert!(!nested_failed_payload.contains("nested-failed-output-oracle"));

    let http_success_payload = http_success.payload.to_string();
    assert!(!http_success_payload.contains("http-success-output-oracle"));

    let http_failed_payload = http_failed.payload.to_string();
    assert!(!http_failed_payload.contains("http-failed-output-oracle"));

    assert_eq!(
        failed_diff.payload["text_retention"],
        json!({
            "mode": "none",
            "limit_chars": null,
            "truncated": false,
            "omission_policy": "none",
            "omission_applied": false,
        })
    );
    let failed_diff_payload = failed_diff.payload.to_string();
    assert!(!failed_diff_payload.contains("diff --git"));
    assert!(!failed_diff_payload.contains("old raw diff"));
    assert!(!failed_diff_payload.contains("new raw diff"));

    assert_eq!(successful_diff.payload["text_retention"]["mode"], "none");
    let successful_diff_payload = successful_diff.payload.to_string();
    assert_eq!(
        successful_diff.payload["result_evidence"],
        json!([{"kind": "call_id", "value": "call-success-diff"}])
    );
    assert!(!successful_diff_payload.contains("diff --git"));
    assert!(!successful_diff_payload.contains("old success diff"));
    assert!(!successful_diff_payload.contains("new success diff"));

    assert_eq!(oversized.payload["text"], "");
    assert_eq!(oversized.payload["text_retention"]["mode"], "none");
    assert_eq!(
        oversized.payload["result_evidence"],
        json!([{"kind": "call_id", "value": "call-oversized"}])
    );
    assert!(!oversized
        .payload
        .to_string()
        .contains("TAIL-SHOULD-BE-TRUNCATED"));

    let unsafe_call_ids = provider_result_identifier_evidence(
        EventType::ToolOutput,
        "",
        &json!({
            "tool_use_id": "secret token with spaces",
            "tool_call_id": "x".repeat(MAX_RESULT_EVIDENCE_CALL_ID_CHARS + 1),
            "success": true,
        }),
    );
    assert!(unsafe_call_ids.is_null());
}

#[test]
fn native_event_omits_patch_arguments_from_tool_metadata_body() {
    let event = test_native_event(
        EventType::ToolCall,
        "apply_patch file touches: modified:src/main.rs",
        json!({
            "tool_name": "Edit",
            "input": "*** Begin Patch\n*** Update File: src/main.rs\n@@\n-old\n+new\n*** End Patch"
        }),
    );
    let rendered = event.payload.to_string();

    assert!(rendered.contains("apply_patch file touches: modified:src/main.rs"));
    assert!(rendered.contains("field_retention"));
    assert!(rendered.contains("original_bytes"));
    assert!(rendered.contains("contained_patch_or_diff"));
    assert!(!rendered.contains("*** Begin Patch"));
    assert!(!rendered.contains("-old"));
    assert!(!rendered.contains("+new"));
}

#[test]
fn native_event_reports_bounded_text_limit_separately_from_truncation() {
    let text = "x".repeat(PROVIDER_MAX_TEXT_CHARS + 1);
    let event = test_native_event(EventType::Message, &text, json!({"content": text}));

    assert_eq!(
        event.payload["text"].as_str().unwrap().chars().count(),
        PROVIDER_MAX_TEXT_CHARS
    );
    assert_eq!(
        event.payload["text_retention"],
        json!({
            "mode": "bounded",
            "limit_chars": PROVIDER_MAX_TEXT_CHARS,
            "truncated": true,
            "omission_policy": "none",
            "omission_applied": false,
        })
    );
    assert!(event.payload.get("content_retention").is_none());
    assert!(event.payload.get("truncated").is_none());
}

#[test]
fn native_event_reports_preview_limit_without_claiming_full_text() {
    let event = test_native_event(
        EventType::ToolCall,
        "read_file src/lib.rs",
        json!({"tool_name": "read_file", "path": "src/lib.rs"}),
    );

    assert_eq!(
        event.payload["text_retention"],
        json!({
            "mode": "bounded",
            "limit_chars": PROVIDER_MAX_PREVIEW_CHARS,
            "truncated": false,
            "omission_policy": "none",
            "omission_applied": false,
        })
    );
}
