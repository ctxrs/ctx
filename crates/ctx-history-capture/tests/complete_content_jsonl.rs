use std::{fs, path::PathBuf};

use ctx_history_capture::complete_content::jsonl::{
    JsonlCompleteContentResolver, EXACT_JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
    JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
};
use ctx_history_capture::complete_content::{
    verified_content_profile, AuthorizedSourceRoute, CompleteContentBodyDigest,
    CompleteContentErrorKind, CompleteContentHashAuthority, CompleteContentResolver,
    CompleteContentSourceFamily, CompleteContentSourceLocator, CompleteMessageRequest,
    SourceAccessBroker, SourceSnapshot, VerifiedContentLocatorsV1, VerifiedContentRole,
    VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
};
use ctx_history_capture::{
    import_antigravity_cli_history, import_codebuddy_history, import_kimi_code_cli_history,
    import_mistral_vibe_history, import_openclaw_history, AntigravityCliImportOptions,
    CodeBuddyImportOptions, KimiCodeCliImportOptions, MistralVibeImportOptions,
    OpenClawImportOptions, ProviderImportWorkResult,
};
use ctx_history_core::{CaptureProvider, ContentRef};
use ctx_history_store::{RawSqlOptions, RawSqlValue, Store};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use uuid::Uuid;

const INDEXED_LIMIT: usize = 16_000;
const CAPABILITY_MATRIX: &str =
    include_str!("../../../docs/complete-content-provider-capabilities.json");

fn message_locator(
    value: &serde_json::Value,
) -> ctx_history_capture::complete_content::VerifiedContentLocatorV1 {
    VerifiedContentLocatorsV1::from_metadata_value(value)
        .unwrap()
        .locator(VerifiedContentRole::MessageBody)
        .unwrap()
        .clone()
}

fn provider_cursor_count(store: &Store) -> i64 {
    let result = store
        .raw_sql_query(
            "SELECT COUNT(*) FROM sync_cursors WHERE stream LIKE 'provider:%'",
            RawSqlOptions::default(),
        )
        .unwrap();
    match &result.rows[0][0] {
        RawSqlValue::Integer(count) => *count,
        value => panic!("unexpected cursor count value: {value:?}"),
    }
}

fn provider_cursor_rows(store: &Store) -> Vec<(String, String, String)> {
    let result = store
        .raw_sql_query(
            "SELECT device_id, stream FROM sync_cursors WHERE stream LIKE 'provider:%' ORDER BY stream",
            RawSqlOptions::default(),
        )
        .unwrap();
    result
        .rows
        .iter()
        .map(|row| {
            let text = |value: &RawSqlValue| match value {
                RawSqlValue::Text { value, .. } => value.clone(),
                value => panic!("unexpected cursor value: {value:?}"),
            };
            let device_id = text(&row[0]);
            let stream = text(&row[1]);
            let cursor = store
                .get_sync_cursor(None, &device_id, &stream)
                .unwrap()
                .unwrap()
                .cursor;
            (device_id, stream, cursor)
        })
        .collect()
}

fn provider_cursor_payload_rows(store: &Store) -> Vec<(String, String)> {
    provider_cursor_rows(store)
        .into_iter()
        .map(|(_, stream, cursor)| {
            let envelope = serde_json::from_str::<Value>(&cursor).unwrap();
            let provider_cursor = envelope["provider_cursor"].as_str().unwrap().to_owned();
            (stream, provider_cursor)
        })
        .collect()
}

type ProviderEventIdentity = (Uuid, Option<Uuid>, Option<Uuid>, Option<String>);

#[derive(Debug, PartialEq, Eq)]
struct ProviderIdentitySnapshot {
    sources: Vec<(Uuid, Option<String>, Option<String>)>,
    sessions: Vec<(Uuid, Option<Uuid>, Option<String>)>,
    events: Vec<ProviderEventIdentity>,
}

fn provider_identity_snapshot(
    store: &Store,
    provider: CaptureProvider,
) -> ProviderIdentitySnapshot {
    let mut sources = store
        .list_capture_sources()
        .unwrap()
        .into_iter()
        .filter(|source| source.descriptor.provider == provider)
        .map(|source| {
            (
                source.id,
                source.descriptor.source_identity,
                source.descriptor.external_session_id,
            )
        })
        .collect::<Vec<_>>();
    sources.sort_by_key(|source| source.0);

    let provider_sessions = store
        .list_sessions()
        .unwrap()
        .into_iter()
        .filter(|session| session.provider == provider)
        .collect::<Vec<_>>();
    let mut sessions = provider_sessions
        .iter()
        .map(|session| {
            (
                session.id,
                session.capture_source_id,
                session.external_session_id.clone(),
            )
        })
        .collect::<Vec<_>>();
    sessions.sort_by_key(|session| session.0);

    let mut events = provider_sessions
        .iter()
        .flat_map(|session| store.events_for_session(session.id).unwrap())
        .map(|event| {
            (
                event.id,
                event.session_id,
                event.capture_source_id,
                event.dedupe_key,
            )
        })
        .collect::<Vec<_>>();
    events.sort_by_key(|event| event.0);

    ProviderIdentitySnapshot {
        sources,
        sessions,
        events,
    }
}

struct Fixture {
    temp: TempDir,
    path: PathBuf,
    record: Vec<u8>,
    body: String,
    request: CompleteMessageRequest,
}

#[allow(clippy::too_many_arguments)]
fn admit_jsonl(
    event_id: Uuid,
    provider: CaptureProvider,
    source_format: &str,
    path: PathBuf,
    root: Option<PathBuf>,
    source_identity: Option<String>,
    source_snapshot: SourceSnapshot,
) -> ctx_history_capture::complete_content::BrokeredSourceAccess {
    SourceAccessBroker::new()
        .admit(
            AuthorizedSourceRoute {
                source_id: Uuid::new_v4(),
                provider,
                source_format: source_format.to_owned(),
                family: CompleteContentSourceFamily::Jsonl,
                raw_source_path: path,
                source_root: root,
                source_identity,
                source_snapshot,
            },
            event_id,
        )
        .unwrap()
}

fn refresh_antigravity_access(fixture: &mut Fixture, snapshot_size: u64) {
    fixture.request.source_access = admit_jsonl(
        fixture.request.event_id,
        CaptureProvider::Antigravity,
        "antigravity_cli_transcript_jsonl_tree",
        fixture.path.clone(),
        Some(fixture.temp.path().to_path_buf()),
        Some("stable-source-identity".to_owned()),
        SourceSnapshot {
            size_bytes: Some(snapshot_size),
            modified_at_ms: None,
            sha256: None,
        },
    );
}

fn antigravity_fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("session.jsonl");
    let body = "snowman ☃, quote \" and slash \\\n".repeat(700);
    assert!(body.chars().count() > INDEXED_LIMIT);
    let mut record = serde_json::to_vec(&json!({
        "step_index": 0,
        "source": "user",
        "type": "USER_INPUT",
        "status": "ok",
        "created_at": "2026-07-21T12:00:00Z",
        "content": body,
    }))
    .unwrap();
    record.push(b'\n');
    fs::write(&path, &record).unwrap();
    let range = range_locator(0, record.len() as u64);
    let event_id = Uuid::from_u128(1);
    let source_access = admit_jsonl(
        event_id,
        CaptureProvider::Antigravity,
        "antigravity_cli_transcript_jsonl_tree",
        path.clone(),
        Some(temp.path().to_path_buf()),
        Some("stable-source-identity".to_owned()),
        SourceSnapshot {
            size_bytes: Some(record.len() as u64),
            modified_at_ms: None,
            sha256: None,
        },
    );
    let request = CompleteMessageRequest {
        event_id,
        provider: CaptureProvider::Antigravity,
        source_format: "antigravity_cli_transcript_jsonl_tree".to_owned(),
        source_access,
        source_family: Some(CompleteContentSourceFamily::Jsonl),
        content_profile: verified_content_profile(
            CaptureProvider::Antigravity,
            "antigravity_cli_transcript_jsonl_tree",
            CompleteContentSourceFamily::Jsonl,
            VerifiedContentRole::MessageBody,
        )
        .unwrap()
        .to_owned(),
        source_locator: Some(range),
        provider_session_id: Some("complete-antigravity".to_owned()),
        source_record_ordinal: 0,
        source_record_subrecord_index: 0,
        expected_provider_event_hash: "step-0".to_owned(),
        expected_hash_authority: CompleteContentHashAuthority::ProviderSupplied,
        expected_native_record_id: Some("step-0".to_owned()),
        expected_record_digest: Some(digest_bytes(record_payload_bytes(&record))),
        expected_content_ref: ContentRef::from_bytes(body.as_bytes()),
        indexed_text: body.chars().take(INDEXED_LIMIT).collect(),
        indexed_limit_chars: INDEXED_LIMIT,
    };
    Fixture {
        temp,
        path,
        record,
        body,
        request,
    }
}

fn range_locator(start: u64, end: u64) -> CompleteContentSourceLocator {
    let mut value = Vec::with_capacity(16);
    value.extend_from_slice(&start.to_be_bytes());
    value.extend_from_slice(&end.to_be_bytes());
    CompleteContentSourceLocator::new(JSONL_COMPLETE_CONTENT_LOCATOR_KIND, value).unwrap()
}

fn digest_bytes(bytes: &[u8]) -> CompleteContentBodyDigest {
    CompleteContentBodyDigest::parse(format!("{:x}", Sha256::digest(bytes))).unwrap()
}

fn record_payload_bytes(record: &[u8]) -> &[u8] {
    let without_newline = record.strip_suffix(b"\n").unwrap_or(record);
    without_newline
        .strip_suffix(b"\r")
        .unwrap_or(without_newline)
}

#[test]
fn resolves_exact_unicode_body_after_append_and_move() {
    let mut fixture = antigravity_fixture();
    let resolver = JsonlCompleteContentResolver::new();

    let result = resolver.resolve(&[fixture.request.clone()]).unwrap();
    assert_eq!(result[0].text.as_bytes(), fixture.body.as_bytes());

    let mut appended = fixture.record.clone();
    appended.extend_from_slice(b"{\"step_index\":1,\"type\":\"SYSTEM_MESSAGE\"}\n");
    fs::write(&fixture.path, appended).unwrap();
    let original_size = fixture.record.len() as u64;
    refresh_antigravity_access(&mut fixture, original_size);
    let result = resolver.resolve(&[fixture.request.clone()]).unwrap();
    assert_eq!(result[0].text, fixture.body);

    let moved = fixture.temp.path().join("moved.jsonl");
    fs::rename(&fixture.path, &moved).unwrap();
    fixture.path = moved;
    refresh_antigravity_access(&mut fixture, original_size);
    let result = resolver.resolve(&[fixture.request]).unwrap();
    assert_eq!(result[0].text, fixture.body);
}

#[test]
fn resolves_crlf_record_ranges_without_changing_the_record_digest() {
    let mut fixture = antigravity_fixture();
    fixture.record.insert(fixture.record.len() - 1, b'\r');
    fs::write(&fixture.path, &fixture.record).unwrap();
    fixture.request.source_locator = Some(range_locator(0, fixture.record.len() as u64));
    let snapshot_size = fixture.record.len() as u64;
    refresh_antigravity_access(&mut fixture, snapshot_size);

    let result = JsonlCompleteContentResolver::new()
        .resolve(&[fixture.request])
        .unwrap();
    assert_eq!(result[0].text.as_bytes(), fixture.body.as_bytes());
}

#[test]
fn rewrite_truncate_and_delete_fail_closed() {
    let resolver = JsonlCompleteContentResolver::new();

    let fixture = antigravity_fixture();
    let mut rewritten = fixture.record.clone();
    let position = rewritten.iter().position(|byte| *byte == b's').unwrap();
    rewritten[position] = b'S';
    fs::write(&fixture.path, rewritten).unwrap();
    let error = resolver.resolve(&[fixture.request]).unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::SourceChanged);

    let fixture = antigravity_fixture();
    fs::write(&fixture.path, b"{}\n").unwrap();
    let error = resolver.resolve(&[fixture.request]).unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::SourceRecordMissing);

    let fixture = antigravity_fixture();
    fs::remove_file(&fixture.path).unwrap();
    let error = resolver.resolve(&[fixture.request]).unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::SourceChanged);
}

#[test]
fn locator_digest_and_indexed_prefix_mismatches_fail_atomically() {
    let resolver = JsonlCompleteContentResolver::new();

    let mut fixture = antigravity_fixture();
    fixture.request.expected_record_digest = Some(CompleteContentBodyDigest::from_text("wrong"));
    let error = resolver.resolve(&[fixture.request]).unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::SourceChanged);

    let mut fixture = antigravity_fixture();
    fixture.request.indexed_text.push('x');
    let error = resolver.resolve(&[fixture.request]).unwrap_err();
    assert_eq!(
        error.kind,
        CompleteContentErrorKind::ContentVerificationFailed
    );

    let mut fixture = antigravity_fixture();
    fixture.request.expected_native_record_id = Some("step-9".to_owned());
    let error = resolver.resolve(&[fixture.request]).unwrap_err();
    assert_eq!(
        error.kind,
        CompleteContentErrorKind::ContentVerificationFailed
    );

    let mut fixture = antigravity_fixture();
    fixture.request.expected_provider_event_hash = "step-9".to_owned();
    let error = resolver.resolve(&[fixture.request]).unwrap_err();
    assert_eq!(
        error.kind,
        CompleteContentErrorKind::ContentVerificationFailed
    );

    let mut fixture = antigravity_fixture();
    let mut second_value: serde_json::Value =
        serde_json::from_slice(record_payload_bytes(&fixture.record)).unwrap();
    second_value["step_index"] = json!(1);
    let mut second_record = serde_json::to_vec(&second_value).unwrap();
    second_record.push(b'\n');
    let second_start = fixture.record.len() as u64;
    let total_length = second_start + second_record.len() as u64;
    let mut source = fixture.record.clone();
    source.extend_from_slice(&second_record);
    fs::write(&fixture.path, source).unwrap();
    refresh_antigravity_access(&mut fixture, total_length);
    let mut second_request = fixture.request.clone();
    second_request.event_id = Uuid::from_u128(2);
    second_request.source_locator = Some(range_locator(second_start, total_length));
    second_request.source_record_ordinal = 1;
    second_request.expected_provider_event_hash = "step-1".to_owned();
    second_request.expected_native_record_id = Some("step-1".to_owned());
    second_request.expected_record_digest =
        Some(digest_bytes(record_payload_bytes(&second_record)));
    second_request.expected_content_ref = ContentRef::from_bytes(b"wrong");
    let error = resolver
        .resolve(&[fixture.request, second_request])
        .unwrap_err();
    assert_eq!(
        error.kind,
        CompleteContentErrorKind::ContentVerificationFailed
    );
}

#[test]
fn traversal_and_missing_locator_are_rejected_without_scanning() {
    let resolver = JsonlCompleteContentResolver::new();
    let fixture = antigravity_fixture();
    let error = SourceAccessBroker::new()
        .admit(
            AuthorizedSourceRoute {
                source_id: Uuid::new_v4(),
                provider: CaptureProvider::Antigravity,
                source_format: fixture.request.source_format.clone(),
                family: CompleteContentSourceFamily::Jsonl,
                raw_source_path: PathBuf::from("../outside.jsonl"),
                source_root: Some(fixture.temp.path().to_path_buf()),
                source_identity: Some("stable-source-identity".to_owned()),
                source_snapshot: SourceSnapshot::default(),
            },
            fixture.request.event_id,
        )
        .unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::SourceUnreadable);

    let mut fixture = antigravity_fixture();
    fixture.request.source_locator = None;
    let error = resolver.resolve(&[fixture.request]).unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::HydrationUnsupported);
}

#[cfg(unix)]
#[test]
fn symlinked_sources_are_rejected() {
    use std::os::unix::fs::symlink;

    let fixture = antigravity_fixture();
    let link = fixture.temp.path().join("linked.jsonl");
    symlink(&fixture.path, &link).unwrap();
    let error = SourceAccessBroker::new()
        .admit(
            AuthorizedSourceRoute {
                source_id: Uuid::new_v4(),
                provider: CaptureProvider::Antigravity,
                source_format: fixture.request.source_format.clone(),
                family: CompleteContentSourceFamily::Jsonl,
                raw_source_path: link,
                source_root: Some(fixture.temp.path().to_path_buf()),
                source_identity: Some("stable-source-identity".to_owned()),
                source_snapshot: SourceSnapshot::default(),
            },
            fixture.request.event_id,
        )
        .unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::SourceUnreadable);

    let linked_parent = fixture.temp.path().join("linked-parent");
    let real_parent = fixture.temp.path().join("real-parent");
    fs::create_dir(&real_parent).unwrap();
    let nested = real_parent.join("nested.jsonl");
    fs::copy(&fixture.path, &nested).unwrap();
    symlink(&real_parent, &linked_parent).unwrap();
    let error = SourceAccessBroker::new()
        .admit(
            AuthorizedSourceRoute {
                source_id: Uuid::new_v4(),
                provider: CaptureProvider::Antigravity,
                source_format: fixture.request.source_format,
                family: CompleteContentSourceFamily::Jsonl,
                raw_source_path: linked_parent.join("nested.jsonl"),
                source_root: Some(fixture.temp.path().to_path_buf()),
                source_identity: Some("stable-source-identity".to_owned()),
                source_snapshot: SourceSnapshot::default(),
            },
            fixture.request.event_id,
        )
        .unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::SourceUnreadable);
}

#[test]
fn import_persists_only_truncated_message_locators_and_the_resolver_consumes_them() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("brain");
    let logs = root
        .join("complete-import")
        .join(".system_generated")
        .join("logs");
    fs::create_dir_all(&logs).unwrap();
    let path = logs.join("transcript_full.jsonl");
    let long_body = "imported unicode 雪 and escaped quote \"\n".repeat(600);
    assert!(long_body.chars().count() > INDEXED_LIMIT);
    let short = json!({
        "step_index": 0,
        "source": "user",
        "type": "USER_INPUT",
        "created_at": "2026-07-21T12:00:00Z",
        "content": "short body",
    });
    let long = json!({
        "step_index": 1,
        "source": "planner",
        "type": "PLANNER_RESPONSE",
        "created_at": "2026-07-21T12:00:01Z",
        "content": long_body,
    });
    let source = format!(
        "{}\n{}\n",
        serde_json::to_string(&short).unwrap(),
        serde_json::to_string(&long).unwrap()
    );
    fs::write(&path, source.as_bytes()).unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_antigravity_cli_history(
        &root,
        &mut store,
        AntigravityCliImportOptions {
            source_path: Some(root.clone()),
            ..AntigravityCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(summary.imported_events, 2, "{:?}", summary.failures);
    let session = store.list_sessions().unwrap().pop().unwrap();
    let events = store.events_for_session(session.id).unwrap();
    assert_eq!(events.len(), 2);
    assert!(events[0].sync.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY].is_null());

    let event = &events[1];
    let persisted = message_locator(&event.sync.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY]);
    assert_eq!(persisted.family(), CompleteContentSourceFamily::Jsonl);
    assert_eq!(persisted.kind(), JSONL_COMPLETE_CONTENT_LOCATOR_KIND);
    let source_access = admit_jsonl(
        event.id,
        CaptureProvider::Antigravity,
        "antigravity_cli_transcript_jsonl_tree",
        path,
        Some(root),
        Some("stable-import-source".to_owned()),
        SourceSnapshot {
            size_bytes: Some(source.len() as u64),
            modified_at_ms: None,
            sha256: None,
        },
    );
    let request = CompleteMessageRequest {
        event_id: event.id,
        provider: CaptureProvider::Antigravity,
        source_format: "antigravity_cli_transcript_jsonl_tree".to_owned(),
        source_access,
        source_family: Some(CompleteContentSourceFamily::Jsonl),
        content_profile: persisted.content_profile().to_owned(),
        source_locator: persisted.source_locator(),
        provider_session_id: Some("complete-import".to_owned()),
        source_record_ordinal: event.sync.metadata["source_record_ordinal"]
            .as_u64()
            .unwrap(),
        source_record_subrecord_index: event.sync.metadata["source_record_subrecord_index"]
            .as_u64()
            .unwrap() as u32,
        expected_provider_event_hash: event.payload["provider_event_hash"]
            .as_str()
            .unwrap()
            .to_owned(),
        expected_hash_authority: CompleteContentHashAuthority::ProviderSupplied,
        expected_native_record_id: Some(persisted.native_record_id().to_owned()),
        expected_record_digest: Some(persisted.record_sha256().clone()),
        expected_content_ref: Some(persisted.content_ref().clone()),
        indexed_text: event.payload["body"]["text"].as_str().unwrap().to_owned(),
        indexed_limit_chars: INDEXED_LIMIT,
    };
    let messages = JsonlCompleteContentResolver::new()
        .resolve(&[request])
        .unwrap();
    assert_eq!(messages[0].text.as_bytes(), long_body.as_bytes());
}

#[test]
fn provider_root_move_preserves_import_identity_restart_and_complete_content() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("work.sqlite");
    let old_root = temp.path().join("old-brain");
    let old_logs = old_root
        .join("moved-session")
        .join(".system_generated")
        .join("logs");
    fs::create_dir_all(&old_logs).unwrap();
    let old_path = old_logs.join("transcript_full.jsonl");
    let long_body = "provider root move complete unicode 雪\n".repeat(600);
    let first = json!({
        "step_index": 0,
        "source": "user",
        "type": "USER_INPUT",
        "created_at": "2026-07-21T12:00:00Z",
        "content": "move the provider root",
    });
    let second = json!({
        "step_index": 1,
        "source": "planner",
        "type": "PLANNER_RESPONSE",
        "created_at": "2026-07-21T12:00:01Z",
        "content": long_body,
    });
    let original = format!(
        "{}\n{}\n",
        serde_json::to_string(&first).unwrap(),
        serde_json::to_string(&second).unwrap()
    );
    fs::write(&old_path, original.as_bytes()).unwrap();

    let mut store = Store::open(&database).unwrap();
    let initial = import_antigravity_cli_history(
        &old_root,
        &mut store,
        AntigravityCliImportOptions {
            source_path: Some(old_root.clone()),
            ..AntigravityCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(initial.imported_events, 2, "{:?}", initial.failures);
    let original_session = store.list_sessions().unwrap().pop().unwrap();
    let original_events = store.events_for_session(original_session.id).unwrap();
    let original_event_ids: Vec<Uuid> = original_events.iter().map(|event| event.id).collect();
    let original_source = store
        .get_capture_source(original_session.capture_source_id.unwrap())
        .unwrap();
    let original_source_identity = original_source.descriptor.source_identity.clone();
    let original_locator =
        original_events[1].sync.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY].clone();
    assert_eq!(provider_cursor_count(&store), 1);

    let new_root = temp.path().join("new-brain");
    fs::rename(&old_root, &new_root).unwrap();
    let new_path = new_root
        .join("moved-session")
        .join(".system_generated")
        .join("logs")
        .join("transcript_full.jsonl");
    drop(store);

    let mut store = Store::open(&database).unwrap();
    let moved = import_antigravity_cli_history(
        &new_root,
        &mut store,
        AntigravityCliImportOptions {
            source_path: Some(new_root.clone()),
            ..AntigravityCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(moved.imported_sessions, 0, "{:?}", moved.failures);
    assert_eq!(moved.imported_events, 0, "{:?}", moved.failures);
    let moved_session = store.list_sessions().unwrap().pop().unwrap();
    let moved_events = store.events_for_session(moved_session.id).unwrap();
    let moved_event_ids: Vec<Uuid> = moved_events.iter().map(|event| event.id).collect();
    let moved_source = store
        .get_capture_source(moved_session.capture_source_id.unwrap())
        .unwrap();
    assert_eq!(moved_session.id, original_session.id);
    assert_eq!(moved_event_ids, original_event_ids);
    assert_eq!(moved_source.id, original_source.id);
    assert_eq!(
        moved_source.descriptor.source_identity,
        original_source_identity
    );
    assert_eq!(
        moved_source.descriptor.raw_source_path.as_deref(),
        Some(new_path.to_string_lossy().as_ref())
    );
    assert_eq!(
        moved_events[1].sync.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
        original_locator
    );
    assert_eq!(provider_cursor_count(&store), 2);

    let persisted = message_locator(&original_locator);
    let source_access = admit_jsonl(
        moved_events[1].id,
        CaptureProvider::Antigravity,
        "antigravity_cli_transcript_jsonl_tree",
        new_path.clone(),
        Some(new_root.clone()),
        moved_source.descriptor.source_identity.clone(),
        SourceSnapshot {
            size_bytes: Some(original.len() as u64),
            modified_at_ms: None,
            sha256: None,
        },
    );
    let request = CompleteMessageRequest {
        event_id: moved_events[1].id,
        provider: CaptureProvider::Antigravity,
        source_format: "antigravity_cli_transcript_jsonl_tree".to_owned(),
        source_access,
        source_family: Some(CompleteContentSourceFamily::Jsonl),
        content_profile: persisted.content_profile().to_owned(),
        source_locator: persisted.source_locator(),
        provider_session_id: Some("moved-session".to_owned()),
        source_record_ordinal: moved_events[1].sync.metadata["source_record_ordinal"]
            .as_u64()
            .unwrap(),
        source_record_subrecord_index: moved_events[1].sync.metadata
            ["source_record_subrecord_index"]
            .as_u64()
            .unwrap() as u32,
        expected_provider_event_hash: moved_events[1].payload["provider_event_hash"]
            .as_str()
            .unwrap()
            .to_owned(),
        expected_hash_authority: CompleteContentHashAuthority::ProviderSupplied,
        expected_native_record_id: Some(persisted.native_record_id().to_owned()),
        expected_record_digest: Some(persisted.record_sha256().clone()),
        expected_content_ref: Some(persisted.content_ref().clone()),
        indexed_text: moved_events[1].payload["body"]["text"]
            .as_str()
            .unwrap()
            .to_owned(),
        indexed_limit_chars: INDEXED_LIMIT,
    };
    let complete = JsonlCompleteContentResolver::new()
        .resolve(&[request])
        .unwrap();
    assert_eq!(complete[0].text, long_body);

    let third = json!({
        "step_index": 2,
        "source": "planner",
        "type": "PLANNER_RESPONSE",
        "created_at": "2026-07-21T12:00:02Z",
        "content": "append after the move",
    });
    let appended = format!("{original}{}\n", serde_json::to_string(&third).unwrap());
    fs::write(&new_path, appended).unwrap();
    let appended_summary = import_antigravity_cli_history(
        &new_root,
        &mut store,
        AntigravityCliImportOptions {
            source_path: Some(new_root.clone()),
            ..AntigravityCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(
        appended_summary.imported_events, 1,
        "{:?}",
        appended_summary.failures
    );
    let appended_events = store.events_for_session(original_session.id).unwrap();
    assert_eq!(appended_events.len(), 3);
    assert_eq!(appended_events[0].id, original_event_ids[0]);
    assert_eq!(appended_events[1].id, original_event_ids[1]);
    drop(store);

    let mut reopened = Store::open(&database).unwrap();
    let replay = import_antigravity_cli_history(
        &new_root,
        &mut reopened,
        AntigravityCliImportOptions {
            source_path: Some(new_root.clone()),
            ..AntigravityCliImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(replay.imported_events, 0, "{:?}", replay.failures);
    assert_eq!(
        reopened
            .events_for_session(original_session.id)
            .unwrap()
            .len(),
        3
    );
    assert_eq!(provider_cursor_count(&reopened), 2);
}

fn persisted_request_for_provider(
    store: &Store,
    provider: CaptureProvider,
) -> CompleteMessageRequest {
    let mut observed = Vec::new();
    for session in store.list_sessions().unwrap() {
        let events = store.events_for_session(session.id).unwrap();
        let Some(event) = events
            .iter()
            .find(|event| event.sync.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY].is_object())
        else {
            continue;
        };
        let source = store
            .get_capture_source(event.capture_source_id.unwrap())
            .unwrap();
        observed.push((
            source.descriptor.provider,
            source.descriptor.raw_source_path.clone(),
            events
                .iter()
                .filter(|event| {
                    event.sync.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY].is_object()
                })
                .count(),
        ));
        if source.descriptor.provider != provider {
            continue;
        }
        let persisted =
            message_locator(&event.sync.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY]);
        assert_eq!(persisted.kind(), EXACT_JSONL_COMPLETE_CONTENT_LOCATOR_KIND);
        assert!(!event
            .payload
            .to_string()
            .contains(VERIFIED_CONTENT_LOCATORS_METADATA_KEY));
        let authority = match event.sync.metadata["provider_event_hash_authority"].as_str() {
            Some("provider_supplied") => CompleteContentHashAuthority::ProviderSupplied,
            Some("normalized_payload_fallback") => {
                CompleteContentHashAuthority::NormalizedPayloadFallback
            }
            value => panic!("unexpected hash authority: {value:?}"),
        };
        let source_format = source.descriptor.source_format.clone().unwrap();
        let snapshot = SourceSnapshot {
            size_bytes: source.sync.metadata["last_imported_size_bytes"].as_u64(),
            modified_at_ms: source.sync.metadata["last_imported_modified_at_ms"].as_i64(),
            sha256: source.sync.metadata["last_imported_sha256"]
                .as_str()
                .map(str::to_owned),
        };
        let source_access = admit_jsonl(
            event.id,
            provider,
            &source_format,
            PathBuf::from(source.descriptor.raw_source_path.clone().unwrap()),
            source.descriptor.source_root.clone().map(PathBuf::from),
            source.descriptor.source_identity.clone(),
            snapshot,
        );
        let (source_record_ordinal, source_record_subrecord_index) =
            if provider == CaptureProvider::CodeBuddy {
                assert_eq!(
                    event.sync.metadata.get("source_record_ordinal"),
                    Some(&Value::Null),
                    "CodeBuddy typed NativePath fixture must not claim a legacy source ordinal"
                );
                assert_eq!(
                    event.sync.metadata.get("source_record_subrecord_index"),
                    Some(&Value::Null),
                    "CodeBuddy typed NativePath fixture must not claim a legacy subrecord index"
                );
                assert_eq!(
                    event.sync.metadata["fixture_line"].as_u64(),
                    Some(1),
                    "CodeBuddy matrix fixture is exactly one JSONL record"
                );
                (0, 0)
            } else {
                (
                    event.sync.metadata["source_record_ordinal"]
                        .as_u64()
                        .unwrap(),
                    event.sync.metadata["source_record_subrecord_index"]
                        .as_u64()
                        .and_then(|value| u32::try_from(value).ok())
                        .unwrap(),
                )
            };
        return CompleteMessageRequest {
            event_id: event.id,
            provider,
            source_format,
            source_access,
            source_family: Some(CompleteContentSourceFamily::Jsonl),
            content_profile: persisted.content_profile().to_owned(),
            source_locator: persisted.source_locator(),
            provider_session_id: session.external_session_id,
            source_record_ordinal,
            source_record_subrecord_index,
            expected_provider_event_hash: event.sync.metadata["provider_event_hash"]
                .as_str()
                .unwrap()
                .to_owned(),
            expected_hash_authority: authority,
            expected_native_record_id: Some(persisted.native_record_id().to_owned()),
            expected_record_digest: Some(persisted.record_sha256().clone()),
            expected_content_ref: Some(persisted.content_ref().clone()),
            indexed_text: event.payload["body"]["text"].as_str().unwrap().to_owned(),
            indexed_limit_chars: INDEXED_LIMIT,
        };
    }
    panic!("missing persisted complete-content event for {provider:?}; observed {observed:?}")
}

#[test]
fn four_provider_capability_matrix_imports_persists_and_recovers_exact_content() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Store::open(temp.path().join("parity.sqlite")).unwrap();
    let body = |provider: &str| format!("{provider} unicode 雪 exact body\n").repeat(900);

    let codebuddy_root = temp.path().join(".codebuddy");
    let codebuddy_path = codebuddy_root.join("projects/project/session.jsonl");
    fs::create_dir_all(codebuddy_path.parent().unwrap()).unwrap();
    let codebuddy_body = body("CodeBuddy CLI");
    fs::write(
        &codebuddy_path,
        format!(
            "{}\n",
            json!({
                "id": "codebuddy-long-1", "sessionId": "codebuddy-session",
                "type": "message", "role": "assistant", "content": codebuddy_body,
                "timestamp": "2026-07-22T12:00:00Z"
            })
        ),
    )
    .unwrap();
    import_codebuddy_history(
        &codebuddy_root,
        &mut store,
        CodeBuddyImportOptions {
            source_path: Some(codebuddy_root.clone()),
            ..CodeBuddyImportOptions::default()
        },
    )
    .unwrap();

    let mistral_root = temp.path().join("vibe");
    let mistral_session = mistral_root.join("session-1");
    fs::create_dir_all(&mistral_session).unwrap();
    fs::write(
        mistral_session.join("meta.json"),
        json!({"session_id":"mistral-session","start_time":"2026-07-22T12:00:00Z"}).to_string(),
    )
    .unwrap();
    let mistral_body = body("Mistral Vibe");
    fs::write(
        mistral_session.join("messages.jsonl"),
        format!(
            "{}\n",
            json!({"message_id":"mistral-long-1","role":"assistant","content":mistral_body})
        ),
    )
    .unwrap();
    import_mistral_vibe_history(
        &mistral_root,
        &mut store,
        MistralVibeImportOptions {
            source_path: Some(mistral_root.clone()),
            ..MistralVibeImportOptions::default()
        },
    )
    .unwrap();

    let openclaw_root = temp.path().join("openclaw");
    let openclaw_sessions = openclaw_root.join("agents/main/sessions");
    fs::create_dir_all(&openclaw_sessions).unwrap();
    fs::write(
        openclaw_sessions.join("sessions.json"),
        json!({"session-1":{"sessionId":"session-1"}}).to_string(),
    )
    .unwrap();
    let openclaw_body = body("OpenClaw");
    fs::write(
        openclaw_sessions.join("session-1.jsonl"),
        format!(
            "{}\n{}\n",
            json!({"type":"session","id":"session-1","timestamp":"2026-07-22T12:00:00Z"}),
            json!({"type":"message","id":"openclaw-long-1","message":{"role":"assistant","content":openclaw_body}})
        ),
    )
    .unwrap();
    let openclaw_summary = import_openclaw_history(
        &openclaw_root,
        &mut store,
        OpenClawImportOptions {
            source_path: Some(openclaw_root.clone()),
            ..OpenClawImportOptions::default()
        },
    )
    .unwrap();
    assert!(
        openclaw_summary.imported_events > 0,
        "{:?}",
        openclaw_summary.failures
    );

    let kimi_root = temp.path().join(".kimi-code");
    let kimi_session = kimi_root.join("sessions/work/session-1");
    let kimi_agents = kimi_session.join("agents/main");
    fs::create_dir_all(&kimi_agents).unwrap();
    fs::write(
        kimi_root.join("session_index.jsonl"),
        format!(
            "{}\n",
            json!({"sessionId":"session-1","sessionDir":kimi_session})
        ),
    )
    .unwrap();
    fs::write(
        kimi_session.join("state.json"),
        json!({"agents":{"main":{"type":"main"}}}).to_string(),
    )
    .unwrap();
    let kimi_body = body("Kimi Code CLI");
    fs::write(
        kimi_agents.join("wire.jsonl"),
        format!(
            "{}\n{}\n",
            json!({"type":"metadata","created_at":1784731200000_i64}),
            json!({"type":"turn.prompt","time":1784731200001_i64,"input":kimi_body})
        ),
    )
    .unwrap();
    import_kimi_code_cli_history(
        &kimi_root,
        &mut store,
        KimiCodeCliImportOptions {
            source_path: Some(kimi_root.clone()),
            ..KimiCodeCliImportOptions::default()
        },
    )
    .unwrap();

    let providers = [
        CaptureProvider::CodeBuddy,
        CaptureProvider::MistralVibe,
        CaptureProvider::OpenClaw,
        CaptureProvider::KimiCodeCli,
    ];
    let stable_events = providers
        .iter()
        .map(|provider| {
            let request = persisted_request_for_provider(&store, *provider);
            (
                *provider,
                request.event_id,
                request.source_locator.unwrap().value().to_vec(),
            )
        })
        .collect::<Vec<_>>();
    let stable_cursors = provider_cursor_rows(&store);
    let stable_provider_cursors = provider_cursor_payload_rows(&store);
    let stable_mistral_identity = provider_identity_snapshot(&store, CaptureProvider::MistralVibe);
    assert_eq!(stable_cursors.len(), 4);

    let reimport_all = |store: &mut Store| {
        import_codebuddy_history(
            &codebuddy_root,
            store,
            CodeBuddyImportOptions {
                source_path: Some(codebuddy_root.clone()),
                ..CodeBuddyImportOptions::default()
            },
        )
        .unwrap();
        import_mistral_vibe_history(
            &mistral_root,
            store,
            MistralVibeImportOptions {
                source_path: Some(mistral_root.clone()),
                ..MistralVibeImportOptions::default()
            },
        )
        .unwrap();
        import_openclaw_history(
            &openclaw_root,
            store,
            OpenClawImportOptions {
                source_path: Some(openclaw_root.clone()),
                ..OpenClawImportOptions::default()
            },
        )
        .unwrap();
        import_kimi_code_cli_history(
            &kimi_root,
            store,
            KimiCodeCliImportOptions {
                source_path: Some(kimi_root.clone()),
                ..KimiCodeCliImportOptions::default()
            },
        )
        .unwrap();
    };
    reimport_all(&mut store);
    assert_eq!(provider_cursor_rows(&store), stable_cursors);

    for (device_id, stream, _) in provider_cursor_rows(&store) {
        let mut cursor = store
            .get_sync_cursor(None, &device_id, &stream)
            .unwrap()
            .unwrap();
        let mut envelope = serde_json::from_str::<Value>(&cursor.cursor).unwrap();
        assert_eq!(envelope["version"].as_u64(), Some(1));
        let provider_cursor = envelope["provider_cursor"].as_str().unwrap();
        let mut provider_wire = serde_json::from_str::<Value>(provider_cursor).unwrap();
        if stream.starts_with("provider:codebuddy:") {
            assert_eq!(provider_wire["version"].as_u64(), Some(1));
            assert_eq!(provider_wire["shape"].as_str(), Some("cli"));
            provider_wire["source_revision"] = json!("stale-codebuddy-source-revision");
        } else if stream.starts_with("provider:mistral_vibe:") {
            assert_eq!(provider_wire["version"].as_u64(), Some(1));
            assert_eq!(
                provider_wire["kind"].as_str(),
                Some("mistral-vibe-nativepath")
            );
            assert_eq!(
                provider_wire["checkpoint"]["capture_revision"].as_u64(),
                Some(4)
            );
            assert_eq!(
                provider_wire["checkpoint"]["policy_revision"].as_u64(),
                Some(8)
            );
            provider_wire["checkpoint"]["policy_revision"] = json!(7);
        } else if stream.starts_with("provider:openclaw:") {
            assert_eq!(provider_wire["version"].as_u64(), Some(1));
            assert_eq!(
                provider_wire["kind"].as_str(),
                Some("openclaw-nativepath-jsonl")
            );
            assert_eq!(
                provider_wire["checkpoint"]["parser_revision"].as_u64(),
                Some(1)
            );
            assert_eq!(
                provider_wire["checkpoint"]["policy_revision"].as_u64(),
                Some(1)
            );
            provider_wire["checkpoint"]["policy_revision"] = json!(0);
        } else if stream.starts_with("provider:kimi_code_cli:") {
            assert_eq!(provider_wire["v"].as_u64(), Some(1));
            assert_eq!(provider_wire["p"].as_u64(), Some(5));
            assert_eq!(provider_wire["o"].as_u64(), Some(7));
            provider_wire["o"] = json!(6);
        } else {
            panic!("unexpected four-provider cursor stream: {stream}");
        }
        envelope["provider_cursor"] = Value::String(serde_json::to_string(&provider_wire).unwrap());
        cursor.cursor = serde_json::to_string(&envelope).unwrap();
        store.upsert_sync_cursor(&cursor).unwrap();
    }

    assert_ne!(provider_cursor_rows(&store), stable_cursors);
    reimport_all(&mut store);
    let repaired_provider_cursors = provider_cursor_payload_rows(&store);
    assert_eq!(
        repaired_provider_cursors.len(),
        stable_provider_cursors.len()
    );
    for ((stream, repaired), (stable_stream, stable)) in repaired_provider_cursors
        .iter()
        .zip(&stable_provider_cursors)
    {
        assert_eq!(stream, stable_stream);
        if stream.starts_with("provider:mistral_vibe:") {
            let repaired = serde_json::from_str::<Value>(repaired).unwrap();
            let stable = serde_json::from_str::<Value>(stable).unwrap();
            assert_eq!(repaired["checkpoint"]["policy_revision"].as_u64(), Some(8));
            assert_eq!(
                repaired["checkpoint"]["generation"], stable["checkpoint"]["generation"],
                "Mistral Vibe policy upgrade must preserve the source generation"
            );
        }
        assert_eq!(
            repaired, stable,
            "policy repair did not reconstruct the canonical cursor for {stream}"
        );
    }
    assert_eq!(
        provider_identity_snapshot(&store, CaptureProvider::MistralVibe),
        stable_mistral_identity,
        "Mistral Vibe locator repair must not add or replace sources, sessions, or events"
    );
    let upgraded_cursors = provider_cursor_rows(&store);
    let no_op = import_mistral_vibe_history(
        &mistral_root,
        &mut store,
        MistralVibeImportOptions {
            source_path: Some(mistral_root.clone()),
            ..MistralVibeImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(no_op.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(provider_cursor_rows(&store), upgraded_cursors);
    reimport_all(&mut store);
    assert_eq!(provider_cursor_rows(&store), upgraded_cursors);
    assert_eq!(
        provider_identity_snapshot(&store, CaptureProvider::MistralVibe),
        stable_mistral_identity
    );

    for (provider, event_id, locator_value) in stable_events {
        let rebuilt = persisted_request_for_provider(&store, provider);
        assert_eq!(rebuilt.event_id, event_id, "provider {provider:?}");
        assert_eq!(
            rebuilt.source_locator.unwrap().value(),
            locator_value,
            "provider {provider:?}"
        );
    }

    for (provider, expected) in [
        (CaptureProvider::CodeBuddy, codebuddy_body.trim().to_owned()),
        (CaptureProvider::MistralVibe, mistral_body),
        (CaptureProvider::OpenClaw, openclaw_body),
        (CaptureProvider::KimiCodeCli, kimi_body),
    ] {
        let request = persisted_request_for_provider(&store, provider);
        assert!(JsonlCompleteContentResolver::new().supports(provider, &request.source_format));
        let recovered = JsonlCompleteContentResolver::new()
            .resolve(std::slice::from_ref(&request))
            .unwrap_or_else(|error| {
                panic!("complete-content recovery failed for {provider:?}: {error:?}")
            });
        assert_eq!(recovered[0].text, expected, "provider {provider:?}");
        if provider == CaptureProvider::CodeBuddy {
            let mut malformed = request.clone();
            let mut malformed_value = malformed.source_locator.as_ref().unwrap().value().to_vec();
            malformed_value.pop();
            malformed.source_locator = CompleteContentSourceLocator::new(
                EXACT_JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
                malformed_value,
            );
            let error = JsonlCompleteContentResolver::new()
                .resolve(&[malformed])
                .unwrap_err();
            assert_eq!(error.kind, CompleteContentErrorKind::HydrationUnsupported);

            let mut oversize = request;
            let mut oversize_value = oversize.source_locator.as_ref().unwrap().value().to_vec();
            oversize_value[..8].copy_from_slice(&0_u64.to_be_bytes());
            oversize_value[8..16].copy_from_slice(&u64::MAX.to_be_bytes());
            oversize.source_locator = CompleteContentSourceLocator::new(
                EXACT_JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
                oversize_value,
            );
            let error = JsonlCompleteContentResolver::new()
                .resolve(&[oversize])
                .unwrap_err();
            assert_eq!(error.kind, CompleteContentErrorKind::ContentTooLarge);
        }
    }

    let mut substituted = persisted_request_for_provider(&store, CaptureProvider::CodeBuddy);
    let copied_codebuddy = codebuddy_path.with_file_name("copied-session.jsonl");
    fs::copy(&codebuddy_path, &copied_codebuddy).unwrap();
    substituted.source_access = admit_jsonl(
        substituted.event_id,
        CaptureProvider::CodeBuddy,
        &substituted.source_format,
        copied_codebuddy,
        Some(codebuddy_root.clone()),
        Some("codebuddy-substitution".to_owned()),
        SourceSnapshot::default(),
    );
    let error = JsonlCompleteContentResolver::new()
        .resolve(&[substituted])
        .unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::SourceChanged);

    let mistral_request = persisted_request_for_provider(&store, CaptureProvider::MistralVibe);
    fs::write(
        mistral_session.join("meta.json"),
        json!({"session_id":"rewritten-session","start_time":"2026-07-22T12:00:00Z"}).to_string(),
    )
    .unwrap();
    let error = JsonlCompleteContentResolver::new()
        .resolve(&[mistral_request])
        .unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::SourceChanged);

    let codebuddy_request = persisted_request_for_provider(&store, CaptureProvider::CodeBuddy);
    let moved_codebuddy = codebuddy_path.with_extension("moved");
    fs::rename(&codebuddy_path, &moved_codebuddy).unwrap();
    let error = JsonlCompleteContentResolver::new()
        .resolve(std::slice::from_ref(&codebuddy_request))
        .unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::SourceChanged);
    fs::rename(&moved_codebuddy, &codebuddy_path).unwrap();
    fs::write(&codebuddy_path, b"{}\n").unwrap();
    let error = JsonlCompleteContentResolver::new()
        .resolve(std::slice::from_ref(&codebuddy_request))
        .unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::SourceRecordMissing);
    fs::remove_file(&codebuddy_path).unwrap();
    let error = JsonlCompleteContentResolver::new()
        .resolve(&[codebuddy_request])
        .unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::SourceRecordMissing);

    for (provider, path) in [
        (
            CaptureProvider::OpenClaw,
            openclaw_sessions.join("session-1.jsonl"),
        ),
        (CaptureProvider::KimiCodeCli, kimi_agents.join("wire.jsonl")),
    ] {
        let request = persisted_request_for_provider(&store, provider);
        let mut rewritten = fs::read(&path).unwrap();
        rewritten.extend_from_slice(b"\n");
        fs::write(&path, rewritten).unwrap();
        let error = JsonlCompleteContentResolver::new()
            .resolve(&[request])
            .unwrap_err();
        assert_eq!(error.kind, CompleteContentErrorKind::SourceChanged);
    }
}

#[test]
fn public_matrix_declares_all_four_exact_jsonl_routes() {
    let document: serde_json::Value = serde_json::from_str(CAPABILITY_MATRIX).unwrap();
    let expected = [
        ("codebuddy", "codebuddy_history_json"),
        ("mistral_vibe", "mistral_vibe_session_jsonl"),
        ("openclaw", "openclaw_session_jsonl_tree"),
        ("kimi_code_cli", "kimi_code_cli_wire_jsonl"),
    ];
    for (provider, source_format) in expected {
        let entry = document["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["id"] == provider)
            .unwrap();
        assert!(entry["routes"].as_array().unwrap().iter().any(|route| {
            route["family"] == "jsonl"
                && route["source_format"] == source_format
                && route["locator_kind"] == EXACT_JSONL_COMPLETE_CONTENT_LOCATOR_KIND
        }));
    }
}
