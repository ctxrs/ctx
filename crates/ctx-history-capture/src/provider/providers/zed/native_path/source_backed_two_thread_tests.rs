use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use ctx_history_core::{CertifiedSource, ScannedSourceCounts};
use ctx_history_core::{EventRole, EventType};
use ctx_history_index::{GenerationWriter, VerifiedIndex, WriterOptions};

use super::*;

#[test]
fn source_backed_zed_two_threads_project_distinct_sessions_with_exact_hydration() {
    let temp = tempfile::tempdir().unwrap();
    let source_root = temp.path().join("source");
    fs::create_dir(&source_root).unwrap();
    let database = source_root.join("threads.db");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/provider-history/zed/v1/threads.db"),
        &database,
    )
    .unwrap();

    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let mut snapshot = acquire_snapshot(&data_root, &database).unwrap();
    let snapshot_revision = snapshot.snapshot_revision.clone();
    let physical_locator = snapshot.physical_locator.clone();
    let source = zed_source_key().unwrap();
    let mut writer = GenerationWriter::open(&index_root, WriterOptions::default()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    let mut sink = ZedSourceBackedSinkV0::new(
        &mut writer,
        snapshot.connection().unwrap(),
        source.clone(),
        snapshot_revision_digest(&snapshot_revision),
        database.to_string_lossy().into_owned(),
    )
    .unwrap();
    let scan = scan_zed_native_snapshot(
        snapshot.connection().unwrap(),
        &physical_locator,
        &snapshot_revision,
        &mut sink,
    )
    .unwrap();
    assert_eq!(scan.counters.sessions_retained, 2);
    assert_eq!(scan.counters.retained_events, 5);
    assert_eq!(sink.staged_documents(), 5);
    assert!(sink.take_failure().is_none());
    drop(sink);
    snapshot.finish().unwrap();

    let observation = source_observation(&source, &snapshot_revision).unwrap();
    writer
        .certify_source(
            CertifiedSource::certify(
                observation.clone(),
                observation,
                "zed-nativepath-source-backed-v0",
                decode_sha256_hex(&scan.source_integrity_digest).unwrap(),
                ScannedSourceCounts {
                    complete_records: 5,
                    retained_records: 5,
                    rejected_records: 0,
                    ignored_records: 0,
                    indexed_documents: 5,
                    certified_bytes: scan.counters.certified_logical_bytes,
                },
            )
            .unwrap(),
        )
        .unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(&index_root).unwrap();
    let page = index.source_event_page(&source, None, 16).unwrap();
    assert!(page.terminal);
    assert_eq!(page.items.len(), 5);
    let sessions = page
        .items
        .iter()
        .map(|event| (event.provider_session_id.clone().unwrap(), event.session_id))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(sessions.len(), 2);
    assert_eq!(
        sessions["zed-root"],
        zed_session_identity(&source, "zed-root").unwrap()
    );
    assert_eq!(
        sessions["zed-child"],
        zed_session_identity(&source, "zed-child").unwrap()
    );
    assert_eq!(
        sessions["zed-root"].to_string(),
        "9297e773-a7a9-8d7b-bb47-fd24429fa1fc"
    );
    assert_eq!(
        sessions["zed-child"].to_string(),
        "c0b6d44d-f2ec-8655-8b9c-1dbf4df37d9f"
    );
    assert_ne!(sessions["zed-root"], sessions["zed-child"]);
    let event_ids = page
        .items
        .iter()
        .map(|event| {
            (
                (
                    event.provider_session_id.clone().unwrap(),
                    event.event_sequence,
                ),
                event.event_id.to_string(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        event_ids[&("zed-child".to_owned(), 0)],
        "ff762302-0f1a-8f62-9444-7e77fd867833"
    );
    assert_eq!(
        event_ids[&("zed-child".to_owned(), 2)],
        "10589418-38f7-88b3-8245-39c5814021d8"
    );
    assert_eq!(
        event_ids[&("zed-root".to_owned(), 0)],
        "79a8c6e8-2811-88c8-9698-46e38553ed4d"
    );
    assert_eq!(
        event_ids[&("zed-root".to_owned(), 2)],
        "1ad66a77-b057-8ae9-b94b-00d525101137"
    );
    assert_eq!(
        event_ids[&("zed-root".to_owned(), 4)],
        "bea728bb-1983-8ac5-9e04-e75259b71e33"
    );

    let resolver = ZedLocatorResolverV0::new(&data_root, &database).unwrap();
    let hydrated = page
        .items
        .iter()
        .map(|event| {
            resolver
                .hydrate(&event.locator)
                .unwrap()
                .decoded_display_text
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        hydrated,
        BTreeSet::from([
            "zed child oracle answer".to_owned(),
            "zed child oracle prompt".to_owned(),
            "zed compacted summary oracle".to_owned(),
            "zed sqlite oracle answer\ntool call: write_file\ntool input: present".to_owned(),
            "zed sqlite oracle prompt".to_owned(),
        ])
    );
}

#[test]
fn zed_lexical_document_retains_full_tail_beyond_legacy_preview_fields() {
    let source = zed_source_key().unwrap();
    let session_id = zed_session_identity(&source, "thread-full-body").unwrap();
    let context = ZedSessionProjectionContextV0 {
        session: ZedNativeSession {
            thread_id: "thread-full-body".to_owned(),
            parent_thread_id: None,
            root_thread_id: "thread-full-body".to_owned(),
            title: "Full body".to_owned(),
            summary: String::new(),
            created_at: "2026-07-28T12:00:00Z".parse().unwrap(),
            updated_at: "2026-07-28T12:00:01Z".parse().unwrap(),
            cwd: Some("/workspace/zed".to_owned()),
            folder_paths: vec!["/workspace/zed".to_owned()],
            encoding: super::super::dto::ZedNativeEncoding::Json,
        },
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
    };
    let full_body = format!("{}zed-tail", "zed full body ".repeat(400));
    let event = ZedNativeEvent::from_draft(
        1,
        "thread-full-body",
        super::super::model::ZedDecodedCoreEvent {
            provider_message_id: Some("message-full-body".to_owned()),
            thread_ordinal: 0,
            message_ordinal: 0,
            event_type: EventType::Message,
            role: EventRole::User,
            occurred_at: "2026-07-28T12:00:01Z".parse().unwrap(),
            kind: "user",
            call_ids: Vec::new(),
            body: full_body.clone(),
            safe_file_touches: Vec::new(),
        },
        CompleteContentBodyDigest::from_text(&full_body),
    )
    .unwrap();
    let document =
        zed_lexical_document(&source, [0x5a; 32], "/tmp/threads.db", &context, event).unwrap();
    assert_eq!(document.body, full_body);
    assert!(document.body.ends_with("zed-tail"));
}
