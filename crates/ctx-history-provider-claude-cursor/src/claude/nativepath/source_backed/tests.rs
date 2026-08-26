use std::{collections::HashMap, path::PathBuf};

use ctx_history_capture_model::{ProviderRootDefinition, ProviderRootSourceIdentity};
use ctx_history_core::{
    ActivityJsonCapture, ActivityTextCapture, LiteralFactKind, ProjectionContractError, TypedKey,
};
use sha2::{Digest, Sha256};

use super::preflight::{
    checked_preflight_identity_count, classify_typed_record_key_error,
    scope_claude_row_validation_error, stable_native_event_identity, ClaudePreflightError,
    ClaudePreflightIdentity, ClaudeRecordKeyField, ClaudeRecordValidationError,
    ClaudeRowValidationError, MAX_PREFLIGHT_EVENT_IDENTITIES,
};
use super::{
    claude_annotation, parse_native_record, session_identity, session_typed_key, source_key,
    ClaudePhysicalLocator, ClaudeSessionKey,
};

fn canonical_identity_hex(identity: ctx_history_core::StableEntityId) -> String {
    use std::fmt::Write as _;

    let mut hex = String::with_capacity(ctx_history_core::StableEntityId::CANONICAL_LEN * 2);
    for byte in identity.encode_canonical().unwrap() {
        write!(&mut hex, "{byte:02x}").unwrap();
    }
    hex
}

#[test]
fn unreadable_leaf_scope_accepts_only_stable_leaf_local_open_failures() {
    let permission = ctx_history_provider_runtime::CaptureError::Io(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "denied",
    ));
    assert!(super::is_quarantinable_claude_leaf_error(&permission));

    for reason in [
        ctx_history_source_io::SYMLINK_PROVIDER_SOURCE_REASON,
        ctx_history_source_io::REPARSE_PROVIDER_SOURCE_REASON,
        ctx_history_source_io::NON_REGULAR_PROVIDER_SOURCE_REASON,
    ] {
        let rejected = ctx_history_provider_runtime::CaptureError::InvalidProviderTranscriptPath {
            path: PathBuf::from("rejected.jsonl"),
            reason,
        };
        assert!(super::is_quarantinable_claude_leaf_error(&rejected));
    }

    for systemic in [
        ctx_history_provider_runtime::CaptureError::SourceChangedDuringCapture,
        ctx_history_provider_runtime::CaptureError::SystemInvariant("invariant"),
        ctx_history_provider_runtime::CaptureError::WorkerPanicked("worker"),
        ctx_history_provider_runtime::CaptureError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "race",
        )),
        ctx_history_provider_runtime::CaptureError::Io(std::io::Error::other("resource")),
        ctx_history_provider_runtime::CaptureError::SystemIo {
            operation: "read",
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "system denied"),
        },
        ctx_history_provider_runtime::CaptureError::InvalidProviderTranscriptPath {
            path: PathBuf::from("other.jsonl"),
            reason: "other invalid path",
        },
    ] {
        assert!(!super::is_quarantinable_claude_leaf_error(&systemic));
    }
}

#[test]
fn source_claims_quarantine_exact_duplicates_and_reject_digest_collisions() {
    let key = ClaudeSessionKey {
        root_session_id: "duplicate-session".to_owned(),
        workflow_run_id: None,
        agent_id: None,
    };
    let source = source_key(None, &key).unwrap();
    let mut claims = HashMap::new();
    assert_eq!(
        super::claim_claude_source(&mut claims, &source).unwrap(),
        super::ClaudeSourceClaim::New
    );
    assert_eq!(
        super::claim_claude_source(&mut claims, &source).unwrap(),
        super::ClaudeSourceClaim::Duplicate
    );

    let other = source_key(
        None,
        &ClaudeSessionKey {
            root_session_id: "other-session".to_owned(),
            workflow_run_id: None,
            agent_id: None,
        },
    )
    .unwrap();
    let mut collision = HashMap::from([(source.exact_descriptor_digest(), other)]);
    let collision = super::claim_claude_source(&mut collision, &source).unwrap_err();
    assert!(collision
        .to_string()
        .contains("source descriptor digest collision"));
}

#[test]
fn identical_native_sessions_under_distinct_logical_roots_have_distinct_sources() {
    let key = super::ClaudeSessionKey {
        root_session_id: "shared-session".to_owned(),
        workflow_run_id: None,
        agent_id: None,
    };
    let personal_source = super::source_key(Some([1; 32]), &key).unwrap();
    let work_source = super::source_key(Some([2; 32]), &key).unwrap();

    assert!(!personal_source.exact_descriptor_eq(&work_source));
    assert!(personal_source.exact_descriptor_eq(&super::source_key(Some([1; 32]), &key).unwrap()));
}

#[test]
fn automatic_source_identity_keeps_the_released_unqualified_lineage() {
    let key = super::ClaudeSessionKey {
        root_session_id: "released-session".to_owned(),
        workflow_run_id: None,
        agent_id: None,
    };
    let released = ctx_history_core::SourceKey::derive_provider_native(
        ctx_history_core::CaptureProvider::Claude.as_str(),
        crate::CLAUDE_PROJECTS_SOURCE_FORMAT,
        super::SOURCE_SCHEMA_VARIANT,
        1,
        super::SOURCE_ANCHOR_NAMESPACE,
        super::session_typed_key(&key).unwrap(),
    )
    .unwrap();

    assert!(released.exact_descriptor_eq(&super::source_key(None, &key).unwrap()));
}

#[test]
fn named_v1_source_session_and_event_keep_released_identity_bytes() {
    let mut root = ProviderRootDefinition {
        id: "personal".to_owned(),
        provider: ctx_history_core::CaptureProvider::Claude,
        path: PathBuf::from("/old/claude"),
        group: None,
        kind: None,
    };
    let root_lineage = ProviderRootSourceIdentity::NamedV1.lineage(&root).unwrap();
    root.path = PathBuf::from("/new/claude");
    assert_eq!(
        Some(root_lineage),
        ProviderRootSourceIdentity::NamedV1.lineage(&root)
    );

    let key = ClaudeSessionKey {
        root_session_id: "claude-root".to_owned(),
        workflow_run_id: Some("claude-workflow".to_owned()),
        agent_id: Some("claude-agent".to_owned()),
    };
    let session_key = session_typed_key(&key).unwrap();
    let released_source_key = TypedKey::composite(vec![
        TypedKey::bytes(root_lineage.to_vec()).unwrap(),
        session_key.clone(),
    ])
    .unwrap();
    let released_source = ctx_history_core::SourceKey::derive_provider_native(
        ctx_history_core::CaptureProvider::Claude.as_str(),
        crate::CLAUDE_PROJECTS_SOURCE_FORMAT,
        super::SOURCE_SCHEMA_VARIANT,
        1,
        super::SOURCE_ANCHOR_NAMESPACE,
        released_source_key,
    )
    .unwrap();
    let source = source_key(Some(root_lineage), &key).unwrap();
    assert_eq!(
        released_source.identity().encode_canonical().unwrap(),
        source.identity().encode_canonical().unwrap()
    );

    let released_session = session_identity(&released_source, session_key.clone()).unwrap();
    let session = session_identity(&source, session_key).unwrap();
    assert_eq!(
        released_session.encode_canonical().unwrap(),
        session.encode_canonical().unwrap()
    );

    let bytes = serde_json::to_vec(&serde_json::json!({
        "type": "user",
        "uuid": "claude-event",
        "message": {"role": "user", "content": "golden"}
    }))
    .unwrap();
    let row = parse_native_record(&bytes, 0, &locator(&bytes))
        .unwrap()
        .rows
        .remove(0);
    let released_event = stable_native_event_identity(&row, &released_source, released_session)
        .unwrap()
        .unwrap();
    let event = stable_native_event_identity(&row, &source, session)
        .unwrap()
        .unwrap();
    assert_eq!(
        released_event.encode_canonical().unwrap(),
        event.encode_canonical().unwrap()
    );

    assert_eq!(
        (
            source.identity().to_string(),
            session.to_string(),
            event.to_string(),
        ),
        (
            "bca84967-56b2-8a5d-b967-a26b647726e9".to_owned(),
            "6a372a35-7990-8cd6-8c3c-e6ad6314aaa3".to_owned(),
            "ec128734-2e71-8466-a518-206e5f6d47a3".to_owned(),
        )
    );
    assert_eq!(
        (
            canonical_identity_hex(source.identity()),
            canonical_identity_hex(session),
            canonical_identity_hex(event),
        ),
        (
            "000101bca8496756b2ca5db967a26b647726e9ee9da311ce2457f6c58b9367b49e08fcbca8496756b2ca5db967a26b647726e9ee9da311ce2457f6c58b9367b49e08fc0000000000000000000000000000000000000000000000000000000000000000bca8496756b28a5db967a26b647726e9".to_owned(),
            "0001026a372a357990acd60c3ce6ad6314aaa3c3087d560dd76332dbcd34e923371ebcbca8496756b2ca5db967a26b647726e9ee9da311ce2457f6c58b9367b49e08fc8377fefbe49ac985ec68bcef1e9aba226c3eb2339ac43f822adf0e5343040df96a372a3579908cd68c3ce6ad6314aaa3".to_owned(),
            "000103ec1287342e71d4666518206e5f6d47a3dfc76f7b4577bb569bd0ae41d79f1366bca8496756b2ca5db967a26b647726e9ee9da311ce2457f6c58b9367b49e08fc8377fefbe49ac985ec68bcef1e9aba226c3eb2339ac43f822adf0e5343040df9ec1287342e718466a518206e5f6d47a3".to_owned(),
        )
    );
}

fn locator(bytes: &[u8]) -> ClaudePhysicalLocator {
    ClaudePhysicalLocator {
        path: PathBuf::from("fixture.jsonl"),
        byte_start: 0,
        byte_end_exclusive: bytes.len() as u64,
        line_number: 1,
        record_sha256: Sha256::digest(bytes).into(),
    }
}

#[test]
fn malformed_record_is_rejected_before_any_core_activity_exists() {
    let bytes = b"not-json";
    assert!(parse_native_record(bytes, 0, &locator(bytes)).is_err());
}

#[test]
fn repeated_native_uuid_and_subrecord_index_repeat_the_stable_event_identity() {
    let key = ClaudeSessionKey {
        root_session_id: "session".to_owned(),
        workflow_run_id: None,
        agent_id: None,
    };
    let source = source_key(None, &key).unwrap();
    let session_id = session_identity(&source, session_typed_key(&key).unwrap()).unwrap();
    let parse = |body: &str| {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "type": "user",
            "uuid": "repeated",
            "message": {"role": "user", "content": body}
        }))
        .unwrap();
        parse_native_record(&bytes, 0, &locator(&bytes))
            .unwrap()
            .rows
            .remove(0)
    };

    assert_eq!(
        stable_native_event_identity(&parse("first"), &source, session_id).unwrap(),
        stable_native_event_identity(&parse("second"), &source, session_id).unwrap()
    );
}

#[test]
fn only_explicit_record_errors_enter_record_rejection_scope() {
    let typed_error = TypedKey::utf8("").unwrap_err();
    assert!(matches!(
        scope_claude_row_validation_error(classify_typed_record_key_error(
            ClaudeRecordKeyField::NativeRecordId,
            typed_error,
        )),
        ClaudePreflightError::RecordRejection { .. }
    ));

    let generic = scope_claude_row_validation_error(classify_typed_record_key_error(
        ClaudeRecordKeyField::NativeRecordId,
        ProjectionContractError::SourceChanged,
    ));
    assert!(matches!(
        generic,
        ClaudePreflightError::Internal(ctx_history_provider_runtime::CaptureError::InvalidPayload(
            detail
        )) if detail == "source certification compared different sources"
    ));
}

#[test]
fn preflight_identity_cost_is_bounded_and_cap_failure_is_internal() {
    assert_eq!(
        checked_preflight_identity_count(MAX_PREFLIGHT_EVENT_IDENTITIES, 0).unwrap(),
        MAX_PREFLIGHT_EVENT_IDENTITIES
    );
    let error = checked_preflight_identity_count(MAX_PREFLIGHT_EVENT_IDENTITIES, 1)
        .expect_err("one identity beyond the cap must fail");
    assert!(matches!(
        error,
        ClaudePreflightError::Internal(
            ctx_history_provider_runtime::CaptureError::SystemInvariant(
                "Claude preflight event identity bound exceeded"
            )
        )
    ));
    assert!(
        MAX_PREFLIGHT_EVENT_IDENTITIES
            .checked_mul(std::mem::size_of::<ClaudePreflightIdentity>())
            .is_some_and(|bytes| bytes <= 40 * 1024 * 1024),
        "the fixed logical identity payload must remain at or below 40 MiB"
    );
}

#[test]
fn oversized_provider_call_id_is_an_explicit_record_error() {
    let bytes = br#"{"type":"assistant","uuid":"row","message":{"role":"assistant","content":[{"type":"tool_use","id":"call","name":"tool","input":{}}]}}"#;
    let mut parsed = parse_native_record(bytes, 0, &locator(bytes)).unwrap();
    parsed.rows[0]
        .tool_call
        .as_mut()
        .expect("tool-call row")
        .call_id = Some("x".repeat(64 * 1024 + 1));
    assert!(matches!(
        claude_annotation(&parsed.rows[0], None, None),
        Err(ClaudeRowValidationError::Record(
            ClaudeRecordValidationError::InvalidProviderCallId(
                ProjectionContractError::FieldTooLarge { .. }
            )
        ))
    ));
}

#[test]
fn claude_result_preserves_complete_native_block_without_renaming() {
    let bytes = br#"{"type":"user","uuid":"row","message":{"content":[{"type":"tool_result","tool_use_id":" call-1 ","is_error":true,"content":" exact text ","unknown":{"future":[1,2]}}]}}"#;
    let parsed = parse_native_record(bytes, 0, &locator(bytes)).unwrap();
    let row = &parsed.rows[0];
    let native = serde_json::json!({
        "type":"tool_result",
        "tool_use_id":" call-1 ",
        "is_error":true,
        "content":" exact text ",
        "unknown":{"future":[1,2]},
    });
    assert_eq!(row.tool_result.as_ref().unwrap().native_content, native);
    let annotation = claude_annotation(row, None, None).unwrap();
    assert_eq!(annotation.structured_content.as_ref(), Some(&native));
    let result = annotation.activity.unwrap().result.unwrap();
    assert_eq!(
        result.text,
        ActivityTextCapture::Present {
            value: " exact text ".to_owned(),
        }
    );
    assert_eq!(
        result.structured_content,
        ActivityJsonCapture::Present { value: native }
    );
}

#[test]
fn claude_flattened_mcp_name_stays_native_and_facts_keep_raw_order() {
    let bytes = br#"{"type":"assistant","uuid":"row","message":{"role":"assistant","content":[{"type":"tool_use","id":"call","name":"mcp__forge__read","input":{"command":" c ","path":" p ","url":" u "}}]}}"#;
    let parsed = parse_native_record(bytes, 0, &locator(bytes)).unwrap();
    let annotation = claude_annotation(&parsed.rows[0], None, None).unwrap();
    let activity = annotation.activity.unwrap();
    let invocation = activity.invocation.unwrap();
    assert_eq!(invocation.tool, "mcp__forge__read");
    assert_eq!((invocation.protocol, invocation.server), (None, None));
    assert_eq!(
        activity
            .facts
            .iter()
            .map(|fact| (fact.kind, fact.value.as_str()))
            .collect::<Vec<_>>(),
        [
            (LiteralFactKind::Command, " c "),
            (LiteralFactKind::File, " p "),
            (LiteralFactKind::Url, " u "),
        ]
    );
}

#[test]
fn claude_duplicate_result_content_retains_row_and_marks_capture_unavailable() {
    let bytes = br#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"call","content":"one","content":"two"}]}}"#;
    let parsed = parse_native_record(bytes, 0, &locator(bytes)).unwrap();
    assert_eq!(parsed.rows.len(), 1);
    let annotation = claude_annotation(&parsed.rows[0], None, None).unwrap();
    assert!(annotation.structured_content.is_none());
    let result = annotation.activity.unwrap().result.unwrap();
    assert_eq!(result.text, ActivityTextCapture::Unavailable);
    assert_eq!(result.structured_content, ActivityJsonCapture::Unavailable);
}
