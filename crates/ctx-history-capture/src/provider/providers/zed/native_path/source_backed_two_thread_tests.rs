use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use ctx_history_core::{CertifiedSource, ScannedSourceCounts};
use ctx_history_core::{EventRole, EventType};
use ctx_history_index::{GenerationWriter, VerifiedIndex, WriterOptions};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

use super::*;
use crate::provider::source_backed::{
    refresh_source_backed_generation, register_landed_source_backed_route_with_data_root,
    SourceBackedProviderRegistry, SourceBackedRouteSelection,
};
use crate::{
    discover_provider_sources_for_provider_with_context, record_evidence::RecordDigest,
    DiscoveryContext, DiscoveryIssueKind, DiscoveryPlatform, DiscoveryPlatformDirs,
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus, ZED_THREADS_SQLITE_SOURCE_FORMAT,
};

#[test]
fn source_backed_zed_preserves_selected_flatpak_platform_root() {
    let temp = tempfile::tempdir().unwrap();
    let flatpak = temp.path().join("flatpak-data");
    let xdg = temp.path().join("xdg-data");
    let selected = flatpak.join("zed/threads/threads.db");
    let suppressed = xdg.join("zed/threads/threads.db");
    fs::create_dir_all(selected.parent().unwrap()).unwrap();
    fs::create_dir_all(suppressed.parent().unwrap()).unwrap();
    super::tests::create_database(&selected, "selected flatpak sentinel");
    super::tests::create_database(&suppressed, "suppressed xdg sentinel");
    let context = discovery_context(temp.path())
        .with_env("FLATPAK_XDG_DATA_HOME", flatpak.as_os_str())
        .with_env("XDG_DATA_HOME", xdg.as_os_str());
    let report =
        discover_provider_sources_for_provider_with_context(&context, CaptureProvider::Zed);
    assert_eq!(report.sources.len(), 1);
    assert_eq!(report.sources[0].path, selected);
    let record = super::tests::project_root_record(&report.sources[0].path);
    assert_eq!(
        record.content.meaningful_text(),
        "selected flatpak sentinel"
    );
}

#[test]
fn source_backed_zed_unsafe_relative_flatpak_root_is_suppressed() {
    let temp = tempfile::tempdir().unwrap();
    let xdg = temp.path().join("xdg-data");
    let fallback = xdg.join("zed/threads/threads.db");
    fs::create_dir_all(fallback.parent().unwrap()).unwrap();
    super::tests::create_database(&fallback, "must not be imported");
    let context = discovery_context(temp.path())
        .with_env("FLATPAK_XDG_DATA_HOME", "relative-flatpak-data")
        .with_env("XDG_DATA_HOME", xdg.as_os_str());
    let report =
        discover_provider_sources_for_provider_with_context(&context, CaptureProvider::Zed);
    assert!(report.sources.is_empty());
    assert_eq!(report.issues.len(), 1);
    assert_eq!(
        report.issues[0].kind,
        DiscoveryIssueKind::SelectorUnreconstructible
    );
}

fn discovery_context(root: &Path) -> DiscoveryContext {
    DiscoveryContext::new(
        root.join("home"),
        root.join("cwd"),
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs {
            data: Some(root.join("platform-data")),
            config: Some(root.join("platform-config")),
            state: Some(root.join("platform-state")),
            local_data: Some(root.join("platform-local-data")),
        },
    )
}

#[test]
fn source_backed_zed_two_threads_project_distinct_sessions_with_complete_core() {
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
    let source = zed_source_key().unwrap();
    let mut writer = GenerationWriter::open(&index_root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    let mut sink =
        ZedSourceBackedSinkV0::new(&mut writer, snapshot.connection().unwrap(), source.clone())
            .unwrap();
    let scan = scan_zed_native_snapshot(
        snapshot.connection().unwrap(),
        &snapshot_revision,
        &mut sink,
    )
    .unwrap();
    assert_eq!(scan.counters.sessions_retained, 2);
    assert_eq!(scan.counters.retained_events, 5);
    assert_eq!(sink.staged_core_records(), 5);
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
    let page = index.core_source_event_page(&source, None, 16).unwrap();
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

    let bodies = page
        .items
        .iter()
        .map(|event| event.core_record.content.meaningful_text().to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        bodies,
        BTreeSet::from([
            "zed child oracle answer".to_owned(),
            "zed child oracle prompt".to_owned(),
            "zed compacted summary oracle".to_owned(),
            "zed sqlite oracle answer\ntool call: write_file\ntool input: present".to_owned(),
            "zed sqlite oracle prompt".to_owned(),
        ])
    );
    let tool_call = page
        .items
        .iter()
        .find(|event| event.event_type == EventType::ToolCall.as_str())
        .expect("fixture contains one retained Zed tool call");
    assert_eq!(
        tool_call.role.as_deref(),
        Some(EventRole::Assistant.as_str())
    );
    assert!(tool_call.native_event_id.is_some());
    let native_parts = tool_call
        .core_record
        .content
        .structured_content
        .as_ref()
        .and_then(|value| value.pointer("/native_message/content/content"))
        .and_then(serde_json::Value::as_array)
        .expect("tool call retains its decoded native content");
    let native_tool = native_parts
        .iter()
        .find(|part| part.get("type").and_then(serde_json::Value::as_str) == Some("tool_use"))
        .expect("tool call retains structured tool input");
    assert_eq!(
        native_tool.get("name").and_then(serde_json::Value::as_str),
        Some("write_file")
    );
    assert!(native_tool
        .get("input")
        .is_some_and(|value| !value.is_null()));
}

#[test]
fn zed_core_record_retains_full_tail_beyond_sixteen_kibibytes() {
    const TAIL: &str = "zedpostsixteenkilobytesentinel";

    let source = zed_source_key().unwrap();
    let session_id = zed_session_identity(&source, "thread-full-body").unwrap();
    let context = ZedSessionProjectionContextV0 {
        session: ZedNativeSession {
            sqlite_rowid: 1,
            thread_id: "thread-full-body".to_owned(),
            parent_thread_id: None,
            title: "Full body".to_owned(),
            payload_title: Some("Full body".to_owned()),
            summary: String::new(),
            created_at: "2026-07-28T12:00:00Z".parse().unwrap(),
            updated_at: "2026-07-28T12:00:01Z".parse().unwrap(),
            native_created_at: Some("2026-07-28T12:00:00Z".to_owned()),
            native_updated_at: "2026-07-28T12:00:01Z".to_owned(),
            cwd: Some("/workspace/zed".to_owned()),
            folder_paths: vec!["/workspace/zed".to_owned()],
            native_folder_paths: Some("/workspace/zed".to_owned()),
            native_folder_paths_order: Some("0".to_owned()),
            native_data_type: "json".to_owned(),
            encoding: super::super::dto::ZedNativeEncoding::Json,
        },
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        root_thread_id: "thread-full-body".to_owned(),
    };
    let full_body = format!(
        r#"{{"arguments":{{"padding":"{}","tail":"{TAIL}"}},"tool":"write_file"}}"#,
        "x".repeat(17_000)
    );
    assert!(full_body.find(TAIL).unwrap() > 16 * 1_024);
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
            native_content: serde_json::json!({
                "kind": "user",
                "content": [{"type": "text"}],
            }),
            body: full_body.clone(),
            safe_file_touches: Vec::new(),
        },
        RecordDigest::from_text(&full_body),
    )
    .unwrap();
    let record = zed_core_record(&source, &context, event).unwrap();
    assert_eq!(record.content.meaningful_text(), full_body);
    assert_eq!(
        record
            .content
            .structured_content
            .as_ref()
            .and_then(|value| value.pointer("/native_message/content/content/0/type"))
            .and_then(serde_json::Value::as_str),
        Some("text")
    );
    let structured: serde_json::Value =
        serde_json::from_str(record.content.meaningful_text()).unwrap();
    assert_eq!(
        structured
            .pointer("/arguments/tail")
            .and_then(serde_json::Value::as_str),
        Some(TAIL)
    );
    let encoded = String::from_utf8(record.encode_stored().unwrap()).unwrap();
    assert!(!encoded.contains("\"locator\""));
    assert!(!encoded.contains("\"source_path\""));
}

#[test]
fn pinned_zed_core_survives_source_movement_and_change() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("threads.db");
    let moved = temp.path().join("threads-moved.db");
    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    let full_body = format!("zed-head-{}-zed-tail", "z".repeat(20_000));
    super::tests::create_database(&database, &full_body);
    let registry = zed_registry(&database, &data_root);
    let options = WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    };

    refresh_source_backed_generation(&index_root, &registry, options.clone()).unwrap();
    let source = zed_source_key().unwrap();
    let pinned = VerifiedIndex::open(&index_root).unwrap();
    let page = pinned.core_source_event_page(&source, None, 8).unwrap();
    assert_eq!(page.items.len(), 1);
    let event_id = page.items[0].event_id;
    let session_id = page.items[0].session_id;
    assert_eq!(
        page.items[0].core_record.content.meaningful_text(),
        full_body
    );
    fs::rename(&database, &moved).unwrap();
    assert_pinned_core(&pinned, event_id, &full_body);
    fs::rename(&moved, &database).unwrap();
    super::tests::replace_thread(&database, "zed changed source body");
    assert_pinned_core(&pinned, event_id, &full_body);

    refresh_source_backed_generation(&index_root, &registry, options).unwrap();
    let rewritten = VerifiedIndex::open(&index_root).unwrap();
    let rewritten_page = rewritten.core_source_event_page(&source, None, 8).unwrap();
    assert_eq!(rewritten_page.items.len(), 1);
    assert_eq!(rewritten_page.items[0].event_id, event_id);
    assert_eq!(rewritten_page.items[0].session_id, session_id);
    assert_eq!(
        rewritten_page.items[0]
            .core_record
            .content
            .meaningful_text(),
        "zed changed source body"
    );
}

#[test]
fn checkpoint_vacuum_and_shm_churn_are_zero_projection_zero_publication_replays() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("threads.db");
    let data_root = temp.path().join("data-root");
    let index_root = temp.path().join("index");
    super::tests::create_database(&database, "logical no-op sentinel");
    let writer = Connection::open(&database).unwrap();
    writer
        .execute("update threads set rowid = 42 where id = 'thread-1'", [])
        .unwrap();
    writer.pragma_update(None, "journal_mode", "wal").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    writer
        .execute_batch(
            "begin immediate;
             update threads set rowid = 43 where id = 'thread-1';
             update threads set rowid = 42 where id = 'thread-1';
             commit;",
        )
        .unwrap();
    let registry = zed_registry(&database, &data_root);
    let options = WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    };

    reset_source_backed_work();
    let cold = refresh_source_backed_generation(&index_root, &registry, options.clone()).unwrap();
    assert_eq!(cold.commit.indexed_documents, 1);
    assert!(cold.removals.is_empty());
    assert_eq!(
        source_backed_work(),
        ZedSourceBackedWork {
            logical_observation_passes: 1,
            projection_passes: 1,
            projected_core_records: 1,
        }
    );
    let source = zed_source_key().unwrap();
    let pinned = VerifiedIndex::open(&index_root).unwrap();
    let cold_page = pinned.core_source_event_page(&source, None, 8).unwrap();
    assert_eq!(cold_page.items.len(), 1);
    let cold_event_id = cold_page.items[0].event_id;
    assert_eq!(
        cold_page.items[0].core_record.content.meaningful_text(),
        "logical no-op sentinel"
    );
    let cold_generation = cold.commit.generation_id.clone();
    let cold_opstamp = cold.commit.opstamp;
    let cold_sources = cold.sources.clone();

    let before_checkpoint = sqlite_persistent_evidence(&database);
    writer
        .execute_batch("pragma wal_checkpoint(truncate)")
        .unwrap();
    assert_ne!(sqlite_persistent_evidence(&database), before_checkpoint);
    reset_source_backed_work();
    let checkpoint =
        refresh_source_backed_generation(&index_root, &registry, options.clone()).unwrap();
    assert_zed_physical_replay(&checkpoint, &cold_generation, cold_opstamp, &cold_sources);
    assert_pinned_core(&pinned, cold_event_id, "logical no-op sentinel");

    writer
        .execute("update threads set rowid = 84 where id = 'thread-1'", [])
        .unwrap();
    let before_vacuum = sqlite_persistent_evidence(&database);
    writer.execute_batch("vacuum").unwrap();
    assert_ne!(sqlite_persistent_evidence(&database), before_vacuum);
    let vacuumed_rowid = writer
        .query_row(
            "select rowid from threads where id = 'thread-1'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_ne!(vacuumed_rowid, 42);
    reset_source_backed_work();
    let vacuum = refresh_source_backed_generation(&index_root, &registry, options.clone()).unwrap();
    assert_zed_physical_replay(&vacuum, &cold_generation, cold_opstamp, &cold_sources);
    assert_pinned_core(&pinned, cold_event_id, "logical no-op sentinel");

    let shm = sqlite_component_path(&database, "-shm");
    let before_shm = fs::read(&shm).unwrap();
    rewrite_same_shm_bytes(&shm);
    assert_eq!(fs::read(&shm).unwrap(), before_shm);
    reset_source_backed_work();
    let shm_replay =
        refresh_source_backed_generation(&index_root, &registry, options.clone()).unwrap();
    assert_zed_physical_replay(&shm_replay, &cold_generation, cold_opstamp, &cold_sources);
    assert_pinned_core(&pinned, cold_event_id, "logical no-op sentinel");

    super::tests::replace_thread(&database, "logical replacement sentinel");
    reset_source_backed_work();
    let replacement = refresh_source_backed_generation(&index_root, &registry, options).unwrap();
    assert_ne!(replacement.commit.generation_id, cold_generation);
    assert!(replacement.commit.opstamp > cold_opstamp);
    assert_ne!(replacement.sources, cold_sources);
    assert!(replacement.removals.is_empty());
    assert_eq!(
        source_backed_work(),
        ZedSourceBackedWork {
            logical_observation_passes: 1,
            projection_passes: 1,
            projected_core_records: 1,
        }
    );
    assert_pinned_core(&pinned, cold_event_id, "logical no-op sentinel");
    let replacement_page = VerifiedIndex::open(&index_root)
        .unwrap()
        .core_source_event_page(&source, None, 8)
        .unwrap();
    assert_eq!(replacement_page.items.len(), 1);
    assert_eq!(replacement_page.items[0].event_id, cold_event_id);
    assert_eq!(
        replacement_page.items[0]
            .core_record
            .content
            .meaningful_text(),
        "logical replacement sentinel"
    );
    drop(writer);
}

#[test]
fn physical_mutation_during_a_pinned_snapshot_fails_terminal_authority() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("threads.db");
    super::tests::create_database(&database, "terminal authority sentinel");
    let mut snapshot = acquire_snapshot(&temp.path().join("data-root"), &database).unwrap();

    rewrite_same_database_bytes(&database);
    let error = snapshot.finish().unwrap_err();
    assert!(matches!(
        error,
        ZedNativePathError::Capture(crate::CaptureError::SourceChangedDuringCapture)
    ));
}

fn zed_registry(database: &Path, data_root: &Path) -> SourceBackedProviderRegistry {
    let source = ProviderSource {
        provider: CaptureProvider::Zed,
        path: database.to_path_buf(),
        exists: true,
        source_format: ZED_THREADS_SQLITE_SOURCE_FORMAT,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
    };
    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route_with_data_root(
        &mut registry,
        source,
        SourceBackedRouteSelection::Automatic,
        data_root,
    )
    .unwrap();
    registry
}

fn assert_zed_physical_replay(
    receipt: &crate::provider::source_backed::SourceBackedRefreshReceipt,
    generation: &str,
    opstamp: u64,
    sources: &[CertifiedSource],
) {
    assert_eq!(receipt.commit.generation_id, generation);
    assert_eq!(receipt.commit.opstamp, opstamp);
    assert_eq!(receipt.commit.indexed_documents, 1);
    assert_eq!(receipt.sources, sources);
    assert!(receipt.removals.is_empty());
    assert_eq!(
        source_backed_work(),
        ZedSourceBackedWork {
            logical_observation_passes: 1,
            projection_passes: 0,
            projected_core_records: 0,
        }
    );
}

fn assert_pinned_core(
    index: &VerifiedIndex,
    event_id: ctx_history_core::StableEntityId,
    expected: &str,
) {
    let record = index
        .core_record_by_id(event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(record.content.meaningful_text(), expected);
}

fn sqlite_persistent_evidence(database: &Path) -> Vec<(u64, [u8; 32])> {
    ["", "-wal"]
        .into_iter()
        .map(|suffix| {
            let bytes = fs::read(sqlite_component_path(database, suffix)).unwrap();
            (
                u64::try_from(bytes.len()).unwrap(),
                Sha256::digest(bytes).into(),
            )
        })
        .collect()
}

fn sqlite_component_path(database: &Path, suffix: &str) -> PathBuf {
    let mut component = database.as_os_str().to_os_string();
    component.push(suffix);
    PathBuf::from(component)
}

fn rewrite_same_shm_bytes(path: &Path) {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    let mut byte = [0_u8; 1];
    file.read_exact(&mut byte).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(&[byte[0] ^ 1]).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(&byte).unwrap();
    file.sync_all().unwrap();
}

fn rewrite_same_database_bytes(path: &Path) {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    let mut byte = [0_u8; 1];
    file.read_exact(&mut byte).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(&[byte[0] ^ 1]).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(&byte).unwrap();
    file.sync_all().unwrap();
}
