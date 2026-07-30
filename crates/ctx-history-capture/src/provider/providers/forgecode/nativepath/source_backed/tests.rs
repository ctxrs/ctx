use std::{fs, path::Path};

use ctx_history_core::{
    EventHydrationRequest, HydrationFailureKind, LocatorRevisionPolicy, NativeRecordCoordinate,
    SessionHydrationRequest, TypedKey,
};
use rusqlite::Connection;
use serde_json::{json, Value};

use super::*;

struct ScanResult {
    source: ForgeCodeSourceBackedSourceV0,
    documents: Vec<LexicalDocument>,
    page_sizes: Vec<usize>,
    retained_bytes: Vec<usize>,
    certificate: CertifiedSource,
}

#[test]
fn forgecode_source_backed_cold_scan_is_bounded_and_stable() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join(".forge.db");
    let messages = Value::Array(
        (0..18)
            .map(|index| text_message(&format!("cold-message-{index}")))
            .collect(),
    );
    write_source(&source_path, "cold-conversation", messages);

    let first = scan(ForgeCodeSourceSelectionV0::selected(
        crate::test_provider_sqlite_data_root(),
        &source_path,
    ));
    assert_eq!(first.documents.len(), 18);
    assert_eq!(first.page_sizes, [16, 2]);
    assert!(first
        .retained_bytes
        .iter()
        .all(|bytes| *bytes <= FORGECODE_NATIVE_PAGE_MAX_BYTES));
    assert!(first
        .documents
        .iter()
        .all(|document| !document.body.is_empty()));
    let canonical_source_path = fs::canonicalize(&source_path)
        .unwrap()
        .display()
        .to_string();
    for document in &first.documents {
        assert_eq!(document.parent_session_id, None);
        assert_eq!(document.root_session_id, document.session_id);
        assert_eq!(
            document.provider_session_id.as_deref(),
            Some("cold-conversation")
        );
        assert_eq!(document.branch.as_deref(), Some("main"));
        assert_eq!(
            document.source_path.as_deref(),
            Some(canonical_source_path.as_str())
        );
        assert_eq!(document.agent_type, "primary");
        assert!(document.is_primary);
    }
    assert_eq!(first.certificate.counts().complete_records, 18);
    assert_eq!(first.certificate.counts().retained_records, 18);
    assert_eq!(first.certificate.counts().indexed_documents, 18);
    assert!(first.certificate.counts().certified_bytes > 0);

    let replay = scan(ForgeCodeSourceSelectionV0::selected(
        crate::test_provider_sqlite_data_root(),
        &source_path,
    ));
    assert_eq!(
        first
            .documents
            .iter()
            .map(|document| document.event_id)
            .collect::<Vec<_>>(),
        replay
            .documents
            .iter()
            .map(|document| document.event_id)
            .collect::<Vec<_>>()
    );
    assert_eq!(first.certificate, replay.certificate);
}

#[test]
fn forgecode_source_backed_exact_resolver_uses_compound_row_coordinates() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join(".forge.db");
    let complete = format!("{}-forgecode-tail-sentinel", "x".repeat(8_192));
    write_source(
        &source_path,
        "exact-conversation",
        json!([
            text_message(&complete),
            text_message("second exact message")
        ]),
    );
    let scanned = scan(ForgeCodeSourceSelectionV0::selected(
        crate::test_provider_sqlite_data_root(),
        &source_path,
    ));
    assert_eq!(scanned.documents[0].body, complete);
    assert!(scanned.documents[0]
        .body
        .ends_with("forgecode-tail-sentinel"));
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key,
        row_version,
    } = scanned.documents[0].locator.coordinate()
    else {
        panic!("expected ForgeCode SQLite coordinate");
    };
    assert_eq!(logical_relation, FORGECODE_LOCATOR_RELATION);
    assert!(matches!(
        primary_key,
        TypedKey::Composite(parts)
            if matches!(
                parts.as_slice(),
                [
                    TypedKey::Utf8(conversation),
                    TypedKey::U64(0)
                ] if conversation == "exact-conversation"
            )
    ));
    assert!(matches!(
        row_version,
        Some(TypedKey::Bytes(digest)) if digest.len() == 32
    ));
    assert_eq!(
        scanned.documents[0].locator.revision_policy(),
        LocatorRevisionPolicy::StableRecordEvidence
    );
    assert!(scanned.documents[0]
        .locator
        .certified_source_revision_digest()
        .is_none());

    let resolver = ForgeCodeSourceBackedResolverV0::new(
        crate::test_provider_sqlite_data_root(),
        [scanned.source.clone()],
    )
    .unwrap();
    let requests = scanned
        .documents
        .iter()
        .map(|document| {
            EventHydrationRequest::new(document.event_id, document.locator.clone()).unwrap()
        })
        .collect::<Vec<_>>();
    let event = resolver.hydrate_event(&requests[0]).unwrap();
    assert_eq!(event.provider_bytes, complete.as_bytes());
    let session = resolver
        .hydrate_session(
            &SessionHydrationRequest::new(scanned.documents[0].session_id, requests).unwrap(),
        )
        .unwrap();
    assert_eq!(session.len(), 2);
    assert_eq!(session[1].provider_bytes, b"second exact message");
}

#[test]
fn source_backed_forgecode_adapter_has_no_preview_or_store_body_fallback() {
    let source = include_str!("../source_backed.rs");
    assert!(!source.contains("MAX_BODY_PREVIEW_CHARS"));
    assert!(!source.contains("ctx_history_store"));
}

#[test]
fn forgecode_source_backed_row_mutation_requires_snapshot_replacement() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join(".forge.db");
    write_source(
        &source_path,
        "replacement-conversation",
        json!([text_message("before replacement")]),
    );
    let before = scan(ForgeCodeSourceSelectionV0::selected(
        crate::test_provider_sqlite_data_root(),
        &source_path,
    ));
    let old_request = EventHydrationRequest::new(
        before.documents[0].event_id,
        before.documents[0].locator.clone(),
    )
    .unwrap();

    replace_messages(
        &source_path,
        json!([text_message("after replacement with changed bytes")]),
    );
    let stale = ForgeCodeSourceBackedResolverV0::new(
        crate::test_provider_sqlite_data_root(),
        [before.source.clone()],
    )
    .unwrap()
    .hydrate_event(&old_request)
    .unwrap_err();
    assert_eq!(stale.kind, HydrationFailureKind::StaleRecordEvidence);

    let after = scan(ForgeCodeSourceSelectionV0::selected(
        crate::test_provider_sqlite_data_root(),
        &source_path,
    ));
    assert!(before
        .source
        .source()
        .exact_descriptor_eq(after.source.source()));
    assert_eq!(before.documents[0].event_id, after.documents[0].event_id);
    assert_ne!(
        before.certificate.observation().revision(),
        after.certificate.observation().revision()
    );
    assert_ne!(
        before.certificate.content_digest(),
        after.certificate.content_digest()
    );
    assert_ne!(
        before.documents[0].locator.record_digest(),
        after.documents[0].locator.record_digest()
    );

    let resolver = ForgeCodeSourceBackedResolverV0::new(
        crate::test_provider_sqlite_data_root(),
        [after.source.clone()],
    )
    .unwrap();
    let request = EventHydrationRequest::new(
        after.documents[0].event_id,
        after.documents[0].locator.clone(),
    )
    .unwrap();
    assert_eq!(
        resolver.hydrate_event(&request).unwrap().provider_bytes,
        b"after replacement with changed bytes"
    );
}

#[test]
fn forgecode_source_backed_selection_keeps_one_winner_and_manual_lineage() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let root = directory.path().join("forge");
    fs::create_dir(&root).unwrap();
    let selected_path = root.join(".forge.db");
    let nonselected_path = root.join("other-forge.db");
    write_source(
        &selected_path,
        "selected-conversation",
        json!([text_message("selected database")]),
    );
    write_source(
        &nonselected_path,
        "manual-conversation",
        json!([text_message("manual database")]),
    );

    let selected = scan(ForgeCodeSourceSelectionV0::selected(
        crate::test_provider_sqlite_data_root(),
        &root,
    ));
    assert_eq!(selected.documents.len(), 1);
    assert_eq!(selected.documents[0].body, "selected database");
    assert_eq!(
        selected.source.canonical_path(),
        fs::canonicalize(&selected_path).unwrap()
    );
    let selected_exact = scan(ForgeCodeSourceSelectionV0::selected(
        crate::test_provider_sqlite_data_root(),
        &selected_path,
    ));
    assert_eq!(
        selected.documents[0].event_id,
        selected_exact.documents[0].event_id
    );

    let manual = scan(ForgeCodeSourceSelectionV0::explicit(
        crate::test_provider_sqlite_data_root(),
        &nonselected_path,
        [7; 32],
    ));
    assert_eq!(manual.documents.len(), 1);
    assert_eq!(manual.documents[0].body, "manual database");
    assert_ne!(
        selected.source.source().identity(),
        manual.source.source().identity()
    );
}

fn scan(selection: ForgeCodeSourceSelectionV0) -> ScanResult {
    let mut scanner = match open_forgecode_source_backed_v0(selection).unwrap() {
        ForgeCodeSourceBackedDiscoveryV0::Live(scanner) => scanner,
        ForgeCodeSourceBackedDiscoveryV0::Missing { preferred_path } => {
            panic!("missing fixture source at {preferred_path:?}")
        }
    };
    let source = scanner.source().clone();
    let mut documents = Vec::new();
    let mut page_sizes = Vec::new();
    let mut retained_bytes = Vec::new();
    while let Some(page) = scanner.next_page().unwrap() {
        assert!(page.failures.is_empty(), "{:?}", page.failures);
        assert_eq!(page.ignored_records, 0);
        page_sizes.push(page.documents.len());
        retained_bytes.push(page.retained_bytes);
        documents.extend(page.documents);
        if page.terminal {
            break;
        }
    }
    let certificate = scanner.finish().unwrap();
    ScanResult {
        source,
        documents,
        page_sizes,
        retained_bytes,
        certificate,
    }
}

fn write_source(path: &Path, conversation_id: &str, messages: Value) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE conversations (
                conversation_id TEXT NOT NULL,
                title TEXT,
                workspace_id INTEGER NOT NULL,
                context TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT,
                metrics TEXT
            );",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO conversations
             (conversation_id, title, workspace_id, context, created_at, updated_at, metrics)
             VALUES (?1, 'test', 7, ?2, '2026-01-01T00:00:00Z',
                     '2026-01-01T00:00:01Z', NULL)",
            rusqlite::params![
                conversation_id,
                json!({
                    "initiator": "forge",
                    "branch": "main",
                    "messages": messages
                })
                .to_string()
            ],
        )
        .unwrap();
}

fn replace_messages(path: &Path, messages: Value) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute(
            "UPDATE conversations SET context = ?1, updated_at = ?2",
            rusqlite::params![
                json!({
                    "initiator": "forge",
                    "branch": "main",
                    "messages": messages
                })
                .to_string(),
                "2026-01-01T00:00:02Z",
            ],
        )
        .unwrap();
}

fn text_message(text: &str) -> Value {
    json!({"message": {"text": {"role": "user", "content": text}}})
}
