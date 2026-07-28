use std::path::Path;

use ctx_history_core::{
    ContentSourceResolver, EventHydrationRequest, HydrationFailureKind, LocatorRevisionPolicy,
    NativeRecordCoordinate, SourceRecordLocator, TypedKey,
};
use ctx_history_index::{LexicalDocument, MAX_BODY_PREVIEW_CHARS};
use rusqlite::Connection;

use super::super::{
    GooseSourceBackedAdapterV0, GooseSourceBackedSelectionV0, GooseSourceBackedSnapshotV0,
    GooseSourceRouteV0,
};
use super::{create_goose_tables, insert_message, insert_session};

fn create_database(path: &Path) -> Connection {
    let connection = Connection::open(path).unwrap();
    create_goose_tables(&connection);
    connection.pragma_update(None, "user_version", 14).unwrap();
    connection
}

fn collect(
    selection: GooseSourceBackedSelectionV0,
) -> (Vec<LexicalDocument>, GooseSourceBackedSnapshotV0) {
    let adapter = GooseSourceBackedAdapterV0::open(selection).unwrap();
    let mut scan = adapter.scan().unwrap();
    let mut documents = Vec::new();
    while let Some(page) = scan.next_page().unwrap() {
        assert!(page.documents().len() <= 64);
        assert_eq!(
            page.complete_records(),
            page.retained_records() + page.rejected_records() + page.ignored_records()
        );
        documents.extend(page.into_documents());
    }
    let snapshot = scan.finish().unwrap();
    assert!(adapter.revalidate(&snapshot));
    (documents, snapshot)
}

#[test]
fn goose_source_backed_cold_scan_is_bounded_stable_and_exactly_selected() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let selected_root = temp.path().join("selected-root");
    let selected_database = selected_root.join("data/sessions/sessions.db");
    std::fs::create_dir_all(selected_database.parent().unwrap()).unwrap();
    let selected = create_database(&selected_database);
    insert_session(&selected, "selected-session");
    let complete_text = format!(
        "selected source text {}",
        "x".repeat(MAX_BODY_PREVIEW_CHARS + 64)
    );
    insert_message(&selected, 1, "selected-session", &complete_text);
    drop(selected);

    let retained_root = temp.path().join("explicit-retained-root");
    let retained_database = retained_root.join("data/sessions/sessions.db");
    std::fs::create_dir_all(retained_database.parent().unwrap()).unwrap();
    let retained = create_database(&retained_database);
    insert_session(&retained, "retained-session");
    insert_message(
        &retained,
        99,
        "retained-session",
        "EXPLICIT_RETAINED_ROUTE_MUST_NOT_BE_SCANNED",
    );
    drop(retained);

    let selection = GooseSourceBackedSelectionV0::exact(&selected_database, &selected_root)
        .with_explicit_retained_routes(vec![GooseSourceRouteV0::exact(
            &retained_database,
            &retained_root,
        )])
        .unwrap();
    let (documents, snapshot) = collect(selection.clone());
    assert_eq!(documents.len(), 1);
    assert!(documents[0].body.starts_with("selected source text"));
    assert!(!documents[0]
        .body
        .contains("EXPLICIT_RETAINED_ROUTE_MUST_NOT_BE_SCANNED"));
    assert_eq!(documents[0].body.chars().count(), MAX_BODY_PREVIEW_CHARS);
    assert_eq!(
        documents[0].provider_session_id.as_deref(),
        Some("selected-session")
    );
    assert_eq!(documents[0].parent_session_id, None);
    assert_eq!(documents[0].root_session_id, documents[0].session_id);
    assert_eq!(documents[0].branch, None);
    assert_eq!(
        documents[0].source_path.as_deref(),
        selected_database.to_str()
    );
    assert_eq!(documents[0].agent_type, "goose");
    assert!(documents[0].is_primary);
    assert_eq!(documents[0].cwd.as_deref(), Some("/workspace/goose"));
    assert_eq!(
        snapshot.selection().selected().selected_database(),
        selected_database
    );
    assert_eq!(
        snapshot.selection().selected().platform_root(),
        selected_root
    );
    assert_eq!(snapshot.selection().retained(), selection.retained());
    assert_eq!(snapshot.certificate().counts().complete_records, 2);
    assert_eq!(snapshot.certificate().counts().retained_records, 1);
    assert_eq!(snapshot.certificate().counts().ignored_records, 1);
    assert_eq!(snapshot.certificate().counts().indexed_documents, 1);
    assert!(snapshot.certificate().frontier().is_none());
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key,
        row_version,
    } = documents[0].locator.coordinate()
    else {
        panic!("Goose document did not use a SQLite locator");
    };
    assert_eq!(logical_relation, "goose-logical-row-v3");
    assert!(matches!(primary_key, TypedKey::Bytes(value) if value.len() == 9));
    assert!(
        matches!(row_version, Some(TypedKey::Bytes(value)) if value.as_slice() == documents[0].locator.record_digest())
    );

    let relocated_root = temp.path().join("relocated-root");
    let relocated_database = relocated_root.join("sessions.db");
    std::fs::create_dir_all(&relocated_root).unwrap();
    let relocated = create_database(&relocated_database);
    insert_session(&relocated, "selected-session");
    insert_message(&relocated, 1, "selected-session", &complete_text);
    drop(relocated);
    let (relocated_documents, relocated_snapshot) = collect(GooseSourceBackedSelectionV0::exact(
        &relocated_database,
        &relocated_root,
    ));
    assert_eq!(relocated_documents[0].session_id, documents[0].session_id);
    assert_eq!(relocated_documents[0].event_id, documents[0].event_id);
    assert_eq!(
        relocated_snapshot
            .certificate()
            .observation()
            .source()
            .identity(),
        snapshot.certificate().observation().source().identity()
    );
}

#[test]
fn goose_source_backed_exact_row_resolver_reopens_complete_content() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("goose-root");
    let database = root.join("sessions.db");
    std::fs::create_dir_all(&root).unwrap();
    let connection = create_database(&database);
    insert_session(&connection, "exact-session");
    let complete_text = format!(
        "complete Goose row {}",
        "z".repeat(MAX_BODY_PREVIEW_CHARS + 512)
    );
    insert_message(&connection, 7, "exact-session", &complete_text);
    drop(connection);

    let selection = GooseSourceBackedSelectionV0::exact(&database, &root);
    let (documents, _) = collect(selection.clone());
    let document = &documents[0];
    assert_eq!(document.body.chars().count(), MAX_BODY_PREVIEW_CHARS);
    let request = EventHydrationRequest::new(document.event_id, document.locator.clone()).unwrap();
    let resolver = super::super::GooseSourceBackedResolverV0::new(selection).unwrap();
    let hydrated = resolver.hydrate_event(&request).unwrap();
    assert_eq!(hydrated.event_id, document.event_id);
    assert_eq!(
        String::from_utf8(hydrated.provider_bytes).unwrap(),
        complete_text
    );

    let mut wrong_digest = *document.locator.record_digest();
    wrong_digest[0] ^= 0xff;
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key,
        ..
    } = document.locator.coordinate()
    else {
        panic!("Goose document did not use a SQLite locator");
    };
    let tampered = SourceRecordLocator::new(
        document.source.clone(),
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation: logical_relation.clone(),
            primary_key: primary_key.clone(),
            row_version: Some(TypedKey::bytes(wrong_digest.to_vec()).unwrap()),
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        document.locator.certified_source_revision_digest().copied(),
        wrong_digest,
    )
    .unwrap();
    let tampered_request = EventHydrationRequest::new(document.event_id, tampered).unwrap();
    assert_eq!(
        resolver.hydrate_event(&tampered_request).unwrap_err().kind,
        HydrationFailureKind::StaleRecordEvidence
    );
}

#[test]
fn goose_source_backed_snapshot_change_requires_exact_replacement() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("goose-root");
    let database = root.join("sessions.db");
    std::fs::create_dir_all(&root).unwrap();
    let connection = create_database(&database);
    insert_session(&connection, "replacement-session");
    insert_message(&connection, 11, "replacement-session", "before replacement");

    let selection = GooseSourceBackedSelectionV0::exact(&database, &root);
    let (before_documents, before_snapshot) = collect(selection.clone());
    connection
        .execute(
            "update messages
             set content_json = '[{\"type\":\"text\",\"text\":\"after replacement\"}]'
             where id = 11",
            [],
        )
        .unwrap();
    drop(connection);
    let (after_documents, after_snapshot) = collect(selection.clone());

    assert_eq!(
        before_documents[0].session_id,
        after_documents[0].session_id
    );
    assert_eq!(before_documents[0].event_id, after_documents[0].event_id);
    assert_ne!(
        before_snapshot.certificate().observation().revision(),
        after_snapshot.certificate().observation().revision()
    );
    assert_ne!(
        before_snapshot.certificate().content_digest(),
        after_snapshot.certificate().content_digest()
    );
    assert_ne!(
        before_documents[0].locator.record_digest(),
        after_documents[0].locator.record_digest()
    );
    assert_ne!(
        before_documents[0]
            .locator
            .certified_source_revision_digest(),
        after_documents[0]
            .locator
            .certified_source_revision_digest()
    );
    assert!(before_snapshot.certificate().frontier().is_none());
    assert!(after_snapshot.certificate().frontier().is_none());

    let resolver = super::super::GooseSourceBackedResolverV0::new(selection).unwrap();
    let stale_request = EventHydrationRequest::new(
        before_documents[0].event_id,
        before_documents[0].locator.clone(),
    )
    .unwrap();
    assert_eq!(
        resolver.hydrate_event(&stale_request).unwrap_err().kind,
        HydrationFailureKind::StaleSourceEvidence
    );
    let current_request = EventHydrationRequest::new(
        after_documents[0].event_id,
        after_documents[0].locator.clone(),
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(
            resolver
                .hydrate_event(&current_request)
                .unwrap()
                .provider_bytes
        )
        .unwrap(),
        "after replacement"
    );
}
