use ctx_history_core::{CaptureProvider, EventRole};
use uuid::Uuid;

use super::super::position::{
    decode_nanoclaw_message_locator, initial_nanoclaw_position, nanoclaw_message_locator,
    NanoClawMessageSource,
};
use super::super::projection::NanoClawCapturedBatchProjector;
use super::*;
use crate::captured_batch::NativeLocator;
use crate::complete_content::{
    sqlite::SqliteCompleteContentResolver, AuthorizedSourceRoute, CompleteContentErrorKind,
    CompleteContentHashAuthority, CompleteContentResolver, CompleteContentSourceFamily,
    CompleteContentSourceLocator, CompleteMessageRequest, SourceAccessBroker, SourceSnapshot,
    VerifiedContentLocatorsV1, VerifiedContentRole, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
};
use crate::PROVIDER_MAX_TEXT_CHARS;

fn complete_requests(root: &Path) -> Vec<CompleteMessageRequest> {
    let context = context(root);
    let central_path = root.join("data").join("v2.db");
    let conn = open_provider_sqlite_readonly(&central_path).unwrap();
    let user_version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    let schema_fingerprint = sqlite_schema_fingerprint(&conn).unwrap();
    let mut projector = NanoClawCapturedBatchProjector::new(
        context.clone(),
        root.display().to_string(),
        central_path.display().to_string(),
        user_version,
        schema_fingerprint,
    );
    let mut output = CollectingOutput {
        normalization: ProviderNormalizationResult::default(),
    };
    let batches = capture_batches(root, initial_nanoclaw_position().unwrap());
    let records = batches
        .iter()
        .flat_map(|batch| batch.records())
        .filter(|record| record.record_kind().as_str() == NANOCLAW_MESSAGE_RECORD_KIND)
        .collect::<Vec<_>>();
    for record in &records {
        projector.project_record(record, &mut output).unwrap();
    }
    let source_locators = output
        .normalization
        .captures
        .iter()
        .map(|(_, capture)| {
            let event = capture.event.as_ref().unwrap();
            VerifiedContentLocatorsV1::from_metadata_value(
                event
                    .metadata
                    .get(VERIFIED_CONTENT_LOCATORS_METADATA_KEY)
                    .unwrap(),
            )
            .unwrap()
            .locator(VerifiedContentRole::MessageBody)
            .unwrap()
            .source_locator()
            .unwrap()
        })
        .collect::<Vec<_>>();
    let source_access_event_id = Uuid::new_v4();
    let source_access = SourceAccessBroker::new()
        .admit_for_source_locators(
            AuthorizedSourceRoute {
                source_id: Uuid::new_v4(),
                provider: CaptureProvider::NanoClaw,
                source_format: NANOCLAW_SOURCE_FORMAT.to_owned(),
                family: CompleteContentSourceFamily::Sqlite,
                raw_source_path: root.to_path_buf(),
                source_root: root.parent().map(Path::to_path_buf),
                source_identity: Some("nanoclaw-project:test".to_owned()),
                source_snapshot: SourceSnapshot::default(),
            },
            &source_locators,
            source_access_event_id,
        )
        .unwrap();
    records
        .into_iter()
        .zip(output.normalization.captures)
        .map(|(record, (_, capture))| {
            let event = capture.event.unwrap();
            let locators = VerifiedContentLocatorsV1::from_metadata_value(
                event
                    .metadata
                    .get(VERIFIED_CONTENT_LOCATORS_METADATA_KEY)
                    .unwrap(),
            )
            .unwrap();
            let persisted = locators.locator(VerifiedContentRole::MessageBody).unwrap();
            CompleteMessageRequest {
                event_id: Uuid::new_v4(),
                provider: CaptureProvider::NanoClaw,
                source_format: NANOCLAW_SOURCE_FORMAT.to_owned(),
                source_access: source_access.clone(),
                source_family: Some(CompleteContentSourceFamily::Sqlite),
                content_profile: persisted.content_profile().to_owned(),
                source_locator: persisted.source_locator(),
                provider_session_id: Some(capture.session.provider_session_id),
                source_record_ordinal: record.ordinal(),
                source_record_subrecord_index: 0,
                expected_provider_event_hash: event.provider_event_hash.unwrap(),
                expected_hash_authority: CompleteContentHashAuthority::ProviderSupplied,
                expected_native_record_id: Some(persisted.native_record_id().to_owned()),
                expected_record_digest: Some(persisted.record_sha256().clone()),
                expected_content_ref: Some(persisted.content_ref().clone()),
                indexed_text: event.payload["text"].as_str().unwrap().to_owned(),
                indexed_limit_chars: PROVIDER_MAX_TEXT_CHARS,
            }
        })
        .collect()
}

fn locator_for(
    session_rowid: i64,
    source: NanoClawMessageSource,
    message_rowid: i64,
) -> CompleteContentSourceLocator {
    let native = nanoclaw_message_locator(session_rowid, source, message_rowid).unwrap();
    CompleteContentSourceLocator::new(native.kind(), native.value().to_vec()).unwrap()
}

fn nanoclaw_route(root: &Path) -> AuthorizedSourceRoute {
    AuthorizedSourceRoute {
        source_id: Uuid::new_v4(),
        provider: CaptureProvider::NanoClaw,
        source_format: NANOCLAW_SOURCE_FORMAT.to_owned(),
        family: CompleteContentSourceFamily::Sqlite,
        raw_source_path: root.to_path_buf(),
        source_root: root.parent().map(Path::to_path_buf),
        source_identity: Some("nanoclaw-project:test".to_owned()),
        source_snapshot: SourceSnapshot::default(),
    }
}

#[test]
fn bounded_projection_preserves_message_and_session_metadata() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "parity", 1);
    let (inbound, outbound) = create_message_stores(&root, "session-0000");
    insert_inbound(
        &inbound,
        "in-1",
        1,
        1_782_259_201_000,
        r#"{"text":"legacy parity user"}"#,
    );
    insert_outbound(
        &outbound,
        "out-1",
        2,
        1_782_259_202_000,
        r#"{"text":"legacy parity assistant"}"#,
    );
    let context = context(&root);
    let central_path = root.join("data").join("v2.db");
    let conn = open_provider_sqlite_readonly(&central_path).unwrap();
    let user_version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    let schema_fingerprint = sqlite_schema_fingerprint(&conn).unwrap();
    let mut projector = NanoClawCapturedBatchProjector::new(
        context.clone(),
        root.display().to_string(),
        central_path.display().to_string(),
        user_version,
        schema_fingerprint,
    );
    let mut output = CollectingOutput {
        normalization: ProviderNormalizationResult::default(),
    };
    for record in capture_batches(&root, initial_nanoclaw_position().unwrap())
        .iter()
        .flat_map(|batch| batch.records())
    {
        if matches!(record.payload(), CapturedRecordPayload::SqliteValues(_)) {
            projector.project_record(record, &mut output).unwrap();
        }
    }
    assert!(output.normalization.files_touched.is_empty());
    assert_eq!(output.normalization.captures.len(), 2);
    assert_eq!(
        output
            .normalization
            .captures
            .iter()
            .map(|(_, capture)| capture.session.provider_session_id.as_str())
            .collect::<Vec<_>>(),
        vec!["ag-1/session-0000", "ag-1/session-0000"]
    );
    assert_eq!(
        output
            .normalization
            .captures
            .iter()
            .map(|(_, capture)| {
                let event = capture.event.as_ref().unwrap();
                (
                    event.role,
                    event.payload["text"].as_str().unwrap(),
                    event.metadata["source"].as_str().unwrap(),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (
                Some(EventRole::User),
                r#"{"text":"legacy parity user"}"#,
                "nanoclaw_inbound",
            ),
            (
                Some(EventRole::Assistant),
                r#"{"text":"legacy parity assistant"}"#,
                "nanoclaw_outbound",
            ),
        ]
    );
    assert!(output.normalization.captures.iter().all(|(_, capture)| {
        capture.event.as_ref().is_some_and(|event| {
            event
                .metadata
                .get(VERIFIED_CONTENT_LOCATORS_METADATA_KEY)
                .is_none()
        })
    }));
    let session = &output.normalization.captures[0].1.session;
    assert_eq!(session.metadata["agent_group_id"], "ag-1");
    assert_eq!(session.metadata["agent_group_name"], "Personal");
    assert_eq!(session.metadata["messaging"]["channel_type"], "telegram");
    assert_eq!(session.cwd.as_deref(), Some("/workspace/nanoclaw"));
}

#[test]
fn compound_locator_recovers_exact_inbound_and_outbound_content_without_paths() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "complete", 1);
    let (inbound, outbound) = create_message_stores(&root, "session-0000");
    let inbound_body = format!("inbound:{}", "i".repeat(PROVIDER_MAX_TEXT_CHARS + 64));
    let outbound_body = format!("outbound:{}", "o".repeat(PROVIDER_MAX_TEXT_CHARS + 64));
    insert_inbound(&inbound, "in-long", 1, 1_000, &inbound_body);
    insert_outbound(&outbound, "out-long", 2, 2_000, &outbound_body);

    let requests = complete_requests(&root);
    assert_eq!(requests.len(), 2);
    let coordinates = requests
        .iter()
        .map(|request| {
            let source = request.source_locator.as_ref().unwrap();
            let native =
                crate::captured_batch::NativeLocator::new(source.kind(), source.value().to_vec())
                    .unwrap();
            decode_nanoclaw_message_locator(&native).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(coordinates[0].session_rowid, 1);
    assert_eq!(coordinates[0].source, NanoClawMessageSource::Inbound);
    assert_eq!(coordinates[1].source, NanoClawMessageSource::Outbound);

    let encoded = requests
        .iter()
        .map(|request| serde_json::to_string(request.source_locator.as_ref().unwrap()).unwrap())
        .collect::<String>();
    assert!(!encoded.contains(root.to_str().unwrap()));
    assert!(!encoded.contains("session-0000"));
    assert!(!encoded.contains("ag-1"));

    let resolved = SqliteCompleteContentResolver::new()
        .resolve(&requests)
        .unwrap();
    assert_eq!(resolved[0].text, inbound_body);
    assert_eq!(resolved[1].text, outbound_body);
}

#[test]
fn compound_snapshot_never_reopens_live_databases_after_admission() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "fail-closed", 1);
    let (inbound, outbound) = create_message_stores(&root, "session-0000");
    let inbound_body = "i".repeat(PROVIDER_MAX_TEXT_CHARS + 64);
    let outbound_body = "o".repeat(PROVIDER_MAX_TEXT_CHARS + 64);
    insert_inbound(&inbound, "in-long", 1, 1_000, &inbound_body);
    insert_outbound(&outbound, "out-long", 2, 2_000, &outbound_body);
    let requests = complete_requests(&root);

    Connection::open(root.join("data/v2.db"))
        .unwrap()
        .execute(
            "update sessions set status = 'complete', last_active = last_active + 1 where rowid = 1",
            [],
        )
        .unwrap();
    assert!(SqliteCompleteContentResolver::new()
        .resolve(std::slice::from_ref(&requests[0]))
        .is_ok());

    std::fs::remove_file(&outbound).unwrap();
    Connection::open(&inbound)
        .unwrap()
        .execute(
            "update messages_in set id = 'reused-row', content = ?1 where rowid = 1",
            [&"r".repeat(PROVIDER_MAX_TEXT_CHARS + 64)],
        )
        .unwrap();
    let resolved = SqliteCompleteContentResolver::new()
        .resolve(&requests)
        .unwrap();
    assert_eq!(resolved[0].text, inbound_body);
    assert_eq!(resolved[1].text, outbound_body);
}

#[test]
fn compound_snapshot_is_locator_targeted_and_preserves_missing_component_semantics() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "targeted", 1);
    let (inbound, outbound) = create_message_stores(&root, "session-0000");
    insert_inbound(&inbound, "in", 1, 1_000, r#"{"text":"inbound"}"#);
    std::fs::remove_file(&outbound).unwrap();

    let inbound_locator = locator_for(1, NanoClawMessageSource::Inbound, 1);
    let outbound_locator = locator_for(1, NanoClawMessageSource::Outbound, 1);
    let inbound_access = SourceAccessBroker::new()
        .admit_for_source_locators(
            nanoclaw_route(&root),
            std::slice::from_ref(&inbound_locator),
            Uuid::new_v4(),
        )
        .unwrap();
    let inbound_native =
        NativeLocator::new(inbound_locator.kind(), inbound_locator.value().to_vec()).unwrap();
    assert!(inbound_access
        .open_nanoclaw_project(
            std::slice::from_ref(&inbound_native),
            crate::complete_content::sqlite::CompleteContentSqliteQueryBudget::new(),
            Uuid::new_v4(),
        )
        .unwrap()
        .resolve(&inbound_native)
        .unwrap()
        .is_some());

    let outbound_access = SourceAccessBroker::new()
        .admit_for_source_locators(
            nanoclaw_route(&root),
            std::slice::from_ref(&outbound_locator),
            Uuid::new_v4(),
        )
        .unwrap();
    let outbound_native =
        NativeLocator::new(outbound_locator.kind(), outbound_locator.value().to_vec()).unwrap();
    assert!(outbound_access
        .open_nanoclaw_project(
            std::slice::from_ref(&outbound_native),
            crate::complete_content::sqlite::CompleteContentSqliteQueryBudget::new(),
            Uuid::new_v4(),
        )
        .unwrap()
        .resolve(&outbound_native)
        .unwrap()
        .is_none());
}

#[test]
fn compound_source_has_no_unselected_live_path_fallback() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "no-live-fallback", 1);
    let error = SourceAccessBroker::new()
        .admit(nanoclaw_route(&root), Uuid::new_v4())
        .unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::HydrationUnsupported);
}

#[test]
fn compound_source_cannot_expand_a_central_file_route_above_its_authorized_root() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "contained", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    insert_inbound(&inbound, "in", 1, 1_000, r#"{"text":"inbound"}"#);
    let locator = locator_for(1, NanoClawMessageSource::Inbound, 1);
    let mut route = nanoclaw_route(&root);
    route.raw_source_path = root.join("data/v2.db");
    route.source_root = Some(root.join("data"));

    let error = SourceAccessBroker::new()
        .admit_for_source_locators(route, std::slice::from_ref(&locator), Uuid::new_v4())
        .unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::SourceUnreadable);
}

#[cfg(unix)]
#[test]
fn compound_snapshot_rejects_symlinked_and_duplicate_selected_components() {
    use std::os::unix::fs::symlink;

    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "adversarial", 1);
    let (inbound, outbound) = create_message_stores(&root, "session-0000");
    let inbound_locator = locator_for(1, NanoClawMessageSource::Inbound, 1);
    let outbound_locator = locator_for(1, NanoClawMessageSource::Outbound, 1);

    std::fs::remove_file(&outbound).unwrap();
    symlink(&inbound, &outbound).unwrap();
    let error = SourceAccessBroker::new()
        .admit_for_source_locators(
            nanoclaw_route(&root),
            std::slice::from_ref(&outbound_locator),
            Uuid::new_v4(),
        )
        .unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::SourceUnreadable);

    std::fs::remove_file(&outbound).unwrap();
    std::fs::hard_link(&inbound, &outbound).unwrap();
    let error = SourceAccessBroker::new()
        .admit_for_source_locators(
            nanoclaw_route(&root),
            &[inbound_locator, outbound_locator],
            Uuid::new_v4(),
        )
        .unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::SourceChanged);
}

#[test]
fn compound_snapshot_rejects_component_mutation_during_admission() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "mutation", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    insert_inbound(&inbound, "in", 1, 1_000, r#"{"text":"before"}"#);
    let locator = locator_for(1, NanoClawMessageSource::Inbound, 1);

    let mutate = inbound.clone();
    let _hook = crate::complete_content::source_access::set_nanoclaw_before_source_set_revalidation(
        move || {
            Connection::open(mutate)
                .unwrap()
                .execute(
                    "update messages_in set content = '{\"text\":\"after\"}' where rowid = 1",
                    [],
                )
                .unwrap();
        },
    );
    let error = SourceAccessBroker::new()
        .admit_for_source_locators(
            nanoclaw_route(&root),
            std::slice::from_ref(&locator),
            Uuid::new_v4(),
        )
        .unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::SourceChanged);
}
