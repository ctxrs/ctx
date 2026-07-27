use chrono::{TimeZone, Utc};
use ctx_history_capture::complete_content::{
    CompleteContentBodyDigest, CompleteContentResolver, CompleteContentSourceFamily,
    CompleteMessage, PersistedCompleteContentLocatorV1, SourceVerification,
};
use ctx_history_core::{
    CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind, Fidelity,
    SyncMetadata, SyncState, Visibility,
};
use serde_json::json;

use super::*;

struct FileFixtureResolver;

impl CompleteContentResolver for FileFixtureResolver {
    fn family(&self) -> CompleteContentSourceFamily {
        CompleteContentSourceFamily::Fixture
    }

    fn supports(&self, _provider: CaptureProvider, source_format: &str) -> bool {
        source_format == "fixture"
    }

    fn resolve(
        &self,
        requests: &[CompleteMessageRequest],
    ) -> Result<Vec<CompleteMessage>, CompleteContentError> {
        requests
            .iter()
            .map(|request| {
                let text = std::fs::read_to_string(&request.raw_source_path).map_err(|_| {
                    CompleteContentError::new(
                        CompleteContentErrorKind::SourceUnreadable,
                        request.event_id,
                    )
                })?;
                CompleteMessage::verified(request, text, SourceVerification::VERIFIED)
            })
            .collect()
    }
}

struct FailingFixtureResolver;

impl CompleteContentResolver for FailingFixtureResolver {
    fn family(&self) -> CompleteContentSourceFamily {
        CompleteContentSourceFamily::Fixture
    }

    fn supports(&self, _provider: CaptureProvider, source_format: &str) -> bool {
        source_format == "fixture"
    }

    fn resolve(
        &self,
        requests: &[CompleteMessageRequest],
    ) -> Result<Vec<CompleteMessage>, CompleteContentError> {
        Err(CompleteContentError::new(
            CompleteContentErrorKind::SourceChanged,
            requests
                .last()
                .map_or(Uuid::nil(), |request| request.event_id),
        ))
    }
}

fn event(truncated: bool) -> Event {
    let indexed_text = if truncated {
        "x".repeat(INDEXED_MESSAGE_LIMIT_CHARS)
    } else {
        "indexed".to_owned()
    };
    Event {
        id: Uuid::parse_str("018f45d0-0000-7000-8000-000000000010").unwrap(),
        seq: 1,
        history_record_id: None,
        session_id: None,
        run_id: None,
        event_type: EventType::Message,
        role: Some(EventRole::User),
        occurred_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
        capture_source_id: None,
        payload: json!({
            "body": {
                "text": indexed_text,
                "text_retention": {
                    "limit_chars": INDEXED_MESSAGE_LIMIT_CHARS,
                    "truncated": truncated,
                    "omission_applied": false,
                }
            }
        }),
        payload_blob_id: None,
        dedupe_key: None,
        sync: SyncMetadata {
            visibility: Visibility::LocalOnly,
            fidelity: Fidelity::Imported,
            sync_state: SyncState::LocalOnly,
            sync_version: 0,
            deleted_at: None,
            metadata: json!({}),
        },
    }
}

fn install_fixture_source(store: &Store, raw_source_path: &std::path::Path) -> Uuid {
    let id = Uuid::parse_str("018f45d0-0000-7000-8000-000000000020").unwrap();
    store
        .upsert_capture_source(&CaptureSource {
            id,
            descriptor: CaptureSourceDescriptor {
                kind: CaptureSourceKind::ProviderImport,
                provider: CaptureProvider::Custom,
                machine_id: "fixture-machine".to_owned(),
                process_id: None,
                cwd: None,
                raw_source_path: Some(raw_source_path.to_string_lossy().into_owned()),
                source_format: Some("fixture".to_owned()),
                source_root: raw_source_path
                    .parent()
                    .map(|path| path.to_string_lossy().into_owned()),
                source_identity: Some("fixture-source-1".to_owned()),
                external_session_id: None,
            },
            started_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
            ended_at: None,
            sync: SyncMetadata::default(),
        })
        .unwrap();
    id
}

fn make_hydratable(event: &mut Event, source_id: Uuid, ordinal: u64, complete_text: &str) {
    event.id = Uuid::from_u128(0x018f45d0000070008000000000000010 + u128::from(ordinal));
    event.seq = ordinal + 1;
    event.capture_source_id = Some(source_id);
    let locator = PersistedCompleteContentLocatorV1::new(
        CompleteContentSourceFamily::Fixture,
        "ascii-token",
        format!("record-{ordinal}").as_bytes(),
        format!("native-{ordinal}"),
        CompleteContentBodyDigest::from_text(&format!("record-{ordinal}")),
        CompleteContentBodyDigest::from_text(complete_text),
    )
    .unwrap();
    event.sync.metadata = json!({
        COMPLETE_CONTENT_LOCATOR_METADATA_KEY: locator.to_metadata_value(),
        "provider_event_hash": format!("fixture-event-{ordinal}"),
        "provider_event_hash_authority": "provider_supplied",
        "source_record_ordinal": ordinal,
        "source_record_subrecord_index": 0,
    });
}

#[test]
fn untruncated_complete_uses_index_without_a_source() {
    let event = event(false);
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();
    let mut registry = CompleteContentResolverRegistry::new();
    registry.register(FileFixtureResolver);
    let resolved = resolve_event_contents_with_registry(
        &store,
        &[&event],
        ContentPolicy::Complete,
        1024,
        &registry,
    )
    .unwrap();
    let content = resolved.event(&event).unwrap();
    assert_eq!(content.outcome.origin, ContentOrigin::CtxIndex);
    assert!(content.outcome.complete);
    assert!(!content.outcome.source_verified);
}

#[test]
fn indexed_mode_preserves_truncated_content_without_a_source() {
    let event = event(true);
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();
    let resolved = resolve_event_contents_with_registry(
        &store,
        &[&event],
        ContentPolicy::Indexed,
        1,
        &CompleteContentResolverRegistry::new(),
    )
    .unwrap();
    let content = resolved.event(&event).unwrap();
    assert!(content.outcome.stored_truncated);
    assert!(!content.outcome.complete);
}

#[test]
fn output_truncation_metadata_is_truthful_without_source_expansion() {
    let mut event = event(false);
    event.event_type = EventType::CommandOutput;
    event.role = Some(EventRole::Tool);
    let indexed_text = "x".repeat(4_000);
    event.payload = json!({
        "body": {
            "output_bytes": 7_822,
            "output_preview": indexed_text,
            "output_retention": "bounded_preview",
            "output_truncated": true,
            "text": indexed_text,
            "truncated": true
        }
    });
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();

    for policy in [ContentPolicy::Indexed, ContentPolicy::Complete] {
        let resolved = resolve_event_contents_with_registry(
            &store,
            &[&event],
            policy,
            CLI_COMPLETE_CONTENT_MAX_OUTPUT_BYTES,
            &CompleteContentResolverRegistry::new(),
        )
        .unwrap();

        let content = resolved.event(&event).unwrap();
        assert_eq!(content.text, "output_bytes: 7822");
        assert!(content.outcome.stored_truncated);
        assert!(!content.outcome.complete);
        assert_eq!(content.outcome.origin, ContentOrigin::CtxIndex);
        assert!(!content.outcome.source_verified);
    }
}

#[test]
fn metadata_only_tool_output_is_policy_bounded() {
    let mut event = event(false);
    event.event_type = EventType::ToolOutput;
    event.role = Some(EventRole::Tool);
    event.payload = json!({
        "body": {
            "output_retention": "metadata_only",
            "text": "tool result metadata"
        }
    });
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();

    let resolved = resolve_event_contents_with_registry(
        &store,
        &[&event],
        ContentPolicy::Complete,
        CLI_COMPLETE_CONTENT_MAX_OUTPUT_BYTES,
        &CompleteContentResolverRegistry::new(),
    )
    .unwrap();

    let content = resolved.event(&event).unwrap();
    assert_eq!(content.text, "tool_output event");
    assert!(content.outcome.stored_truncated);
    assert!(!content.outcome.complete);
    assert_eq!(content.outcome.origin, ContentOrigin::CtxIndex);
    assert!(!content.outcome.source_verified);
}

#[test]
fn complete_mode_hydrates_one_representative_fixture() {
    let temp = tempfile::tempdir().unwrap();
    let raw_source_path = temp.path().join("session.fixture");
    let complete_text = format!("{}tail", "x".repeat(INDEXED_MESSAGE_LIMIT_CHARS));
    std::fs::write(&raw_source_path, &complete_text).unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();
    let source_id = install_fixture_source(&store, &raw_source_path);
    let mut event = event(true);
    make_hydratable(&mut event, source_id, 0, &complete_text);
    let mut registry = CompleteContentResolverRegistry::new();
    registry.register(FileFixtureResolver);

    let resolved = resolve_event_contents_with_registry(
        &store,
        &[&event],
        ContentPolicy::Complete,
        CLI_COMPLETE_CONTENT_MAX_OUTPUT_BYTES,
        &registry,
    )
    .unwrap();

    let content = resolved.event(&event).unwrap();
    assert_eq!(content.text, complete_text);
    assert_eq!(content.outcome.origin, ContentOrigin::ProviderSource);
    assert!(content.outcome.complete);
    assert!(content.outcome.source_verified);
}

#[test]
fn complete_mode_follows_the_current_locator_after_a_source_move() {
    let temp = tempfile::tempdir().unwrap();
    let old_root = temp.path().join("old-root");
    let new_root = temp.path().join("new-root");
    std::fs::create_dir_all(&old_root).unwrap();
    let old_path = old_root.join("session.fixture");
    let new_path = new_root.join("session.fixture");
    let complete_text = format!("{}tail", "x".repeat(INDEXED_MESSAGE_LIMIT_CHARS));
    std::fs::write(&old_path, &complete_text).unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();
    let source_id = install_fixture_source(&store, &old_path);
    let mut event = event(true);
    make_hydratable(&mut event, source_id, 0, &complete_text);

    std::fs::rename(&old_root, &new_root).unwrap();
    let mut source = store.get_capture_source(source_id).unwrap();
    source.descriptor.raw_source_path = Some(new_path.to_string_lossy().into_owned());
    source.descriptor.source_root = Some(new_root.to_string_lossy().into_owned());
    store.upsert_capture_source(&source).unwrap();
    let mut registry = CompleteContentResolverRegistry::new();
    registry.register(FileFixtureResolver);

    let resolved = resolve_event_contents_with_registry(
        &store,
        &[&event],
        ContentPolicy::Complete,
        CLI_COMPLETE_CONTENT_MAX_OUTPUT_BYTES,
        &registry,
    )
    .unwrap();

    let content = resolved.event(&event).unwrap();
    assert_eq!(content.text, complete_text);
    assert_eq!(content.outcome.origin, ContentOrigin::ProviderSource);
    assert!(content.outcome.complete);
    assert!(content.outcome.source_verified);
}

#[test]
fn complete_mode_rejects_the_whole_selection_when_one_record_fails() {
    let temp = tempfile::tempdir().unwrap();
    let raw_source_path = temp.path().join("session.fixture");
    let store = Store::open(temp.path().join("ctx.db")).unwrap();
    let source_id = install_fixture_source(&store, &raw_source_path);
    let complete_text = format!("{}tail", "x".repeat(INDEXED_MESSAGE_LIMIT_CHARS));
    let mut first = event(true);
    let mut second = event(true);
    make_hydratable(&mut first, source_id, 0, &complete_text);
    make_hydratable(&mut second, source_id, 1, &complete_text);
    let mut registry = CompleteContentResolverRegistry::new();
    registry.register(FailingFixtureResolver);

    let error = resolve_event_contents_with_registry(
        &store,
        &[&first, &second],
        ContentPolicy::Complete,
        CLI_COMPLETE_CONTENT_MAX_OUTPUT_BYTES,
        &registry,
    )
    .unwrap_err();

    assert_eq!(error.kind, CompleteContentErrorKind::SourceChanged);
    assert_eq!(error.event_id, second.id);
}

#[test]
fn complete_mode_enforces_the_aggregate_output_limit() {
    let temp = tempfile::tempdir().unwrap();
    let raw_source_path = temp.path().join("session.fixture");
    let complete_text = format!("{}tail", "x".repeat(INDEXED_MESSAGE_LIMIT_CHARS));
    std::fs::write(&raw_source_path, &complete_text).unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();
    let source_id = install_fixture_source(&store, &raw_source_path);
    let mut event = event(true);
    make_hydratable(&mut event, source_id, 0, &complete_text);
    let mut registry = CompleteContentResolverRegistry::new();
    registry.register(FileFixtureResolver);

    let error = resolve_event_contents_with_registry(
        &store,
        &[&event],
        ContentPolicy::Complete,
        complete_text.len() - 1,
        &registry,
    )
    .unwrap_err();

    assert_eq!(error.kind, CompleteContentErrorKind::ContentTooLarge);
    assert_eq!(error.event_id, event.id);
}

#[test]
fn serialized_json_counter_includes_escaping_metadata_and_line_delimiter() {
    let value = json!({
        "text": "\"\\\n\r\t\u{0000}\u{001f}雪".repeat(40),
        "source": {
            "path": "C:\\\\Users\\\"agent\"\\history\n雪.jsonl",
            "cursor": "line:\"7\"\\next\u{0001}",
        },
        "citation": {
            "source_record_ordinal": 7,
            "provider_event_hash": "sha256:\"quoted\"\\hash",
        },
    });
    let encoded = serde_json::to_vec(&value).unwrap();

    assert_eq!(
        serialized_json_line_bytes(&value).unwrap(),
        encoded.len() + 1
    );
    assert!(encoded.len() > value["text"].as_str().unwrap().len());
}

#[test]
fn final_cli_serialization_is_bounded_after_json_expansion() {
    let event_id = event(false).id;
    let raw_text = "\"\\\n\r\t\u{0000}\u{001f}雪".repeat(40);
    let rendered = serde_json::to_string_pretty(&json!({
        "ctx_event_id": event_id,
        "text": raw_text,
        "source": {"path": "history/\"quoted\"\\source\n雪.jsonl"},
        "citation": {"ordinal": 9, "cursor": "row\\\"9\"\u{0001}"},
    }))
    .unwrap();
    let emitted_bytes = rendered.len() + 1;
    assert!(emitted_bytes > raw_text.len());

    enforce_complete_content_cli_output_limit(
        ContentPolicy::Complete,
        &rendered,
        true,
        emitted_bytes,
        event_id,
    )
    .unwrap();
    let error = enforce_complete_content_cli_output_limit(
        ContentPolicy::Complete,
        &rendered,
        true,
        emitted_bytes - 1,
        event_id,
    )
    .unwrap_err();

    assert_eq!(error.kind, CompleteContentErrorKind::ContentTooLarge);
    assert_eq!(error.event_id, event_id);
}

#[test]
fn indexed_cli_output_is_not_subject_to_the_complete_content_limit() {
    enforce_complete_content_cli_output_limit(
        ContentPolicy::Indexed,
        "oversized\"\\\n\u{0001}",
        true,
        1,
        event(false).id,
    )
    .unwrap();
}
