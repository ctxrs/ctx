use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use ctx_history_core::{AgentScope, CaptureProvider, CoreRecord, SourceKey};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use serde_json::{json, Value};

use crate::{
    provider::source_backed::family::jsonl::set_after_jsonl_semantic_preflight_hook,
    refresh_source_backed_generation, register_landed_source_backed_route, ProviderCatalogSupport,
    ProviderImportSupport, ProviderSource, ProviderSourceKind, ProviderSourceStatus,
    SourceBackedProviderRegistry, SourceBackedRoute, SourceBackedRouteSelection,
    SourceBackedSelectorAuthority, SourceBackedSourceFailureClass,
};

fn transcript_path(root: &Path) -> std::path::PathBuf {
    root.join("tmp/project/chats/neutral-session.jsonl")
}

fn copy_authentic_resume_fixture(root: &Path) {
    let fixture_chats = Path::new(env!("CARGO_MANIFEST_DIR")).join(
        "../../tests/fixtures/provider-history/gemini/v0.52.0-resume/.gemini/tmp/workspace/chats",
    );
    let chats = root.join("tmp/workspace/chats");
    fs::create_dir_all(&chats).unwrap();
    for name in [
        "session-2026-08-24T18-33-65900000.jsonl",
        "session-2026-08-24T18-34-65900000.jsonl",
    ] {
        fs::copy(fixture_chats.join(name), chats.join(name)).unwrap();
    }
}

fn gemini_writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

fn header() -> Value {
    recording_header(
        "neutral-gemini-session",
        "2026-08-16T00:00:00Z",
        "synthetic-project-hash",
    )
}

fn recording_header(session_id: &str, start_time: &str, project_hash: &str) -> Value {
    json!({
        "sessionId": session_id,
        "projectHash": project_hash,
        "startTime": start_time,
        "kind": "main"
    })
}

fn message(id: &str, timestamp: &str, role: &str, text: &str) -> Value {
    json!({
        "id": id,
        "timestamp": timestamp,
        "type": role,
        "content": text
    })
}

fn write_transcript(path: &Path, rows: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = Vec::new();
    for row in rows {
        serde_json::to_writer(&mut bytes, row).unwrap();
        bytes.push(b'\n');
    }
    fs::write(path, bytes).unwrap();
}

fn append_transcript(path: &Path, row: &Value) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    serde_json::to_writer(&mut file, row).unwrap();
    file.write_all(b"\n").unwrap();
    file.sync_all().unwrap();
}

fn registry(root: &Path) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        gemini_provider_source(root),
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    assert_eq!(registry.routes().len(), 1);
    registry
}

fn legacy_v1_registry(root: &Path) -> SourceBackedProviderRegistry {
    let driver = ctx_history_capture_composition::gemini_legacy_v1_source_backed_driver_for_test(
        root.to_path_buf(),
    );
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(
        SourceBackedRoute::automatic(
            gemini_provider_source(root),
            SourceBackedSelectorAuthority::DiscoveredWinner,
            driver,
        )
        .unwrap(),
    );
    registry
}

fn gemini_provider_source(root: &Path) -> ProviderSource {
    ProviderSource {
        provider: CaptureProvider::Gemini,
        path: root.to_path_buf(),
        exists: true,
        source_format: ctx_history_provider_gemini::GEMINI_CLI_SOURCE_FORMAT,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
        route_provenance: Default::default(),
    }
}

fn indexed_records(index: &Path) -> Vec<CoreRecord> {
    let verified = VerifiedIndex::open(index).unwrap();
    let source = verified
        .manifest()
        .sources
        .iter()
        .find(|source| source.observation().source().provider() == CaptureProvider::Gemini.as_str())
        .unwrap()
        .observation()
        .source()
        .clone();
    let mut records = verified
        .core_source_event_page(&source, None, 64)
        .unwrap()
        .items
        .into_iter()
        .map(|item| item.core_record)
        .collect::<Vec<_>>();
    records.sort_by_key(|record| record.event_sequence);
    records
}

fn gemini_sources(index: &Path) -> Vec<SourceKey> {
    VerifiedIndex::open(index)
        .unwrap()
        .manifest()
        .sources
        .iter()
        .filter(|source| {
            source.observation().source().provider() == CaptureProvider::Gemini.as_str()
        })
        .map(|source| source.observation().source().clone())
        .collect()
}

fn all_gemini_records(index: &Path) -> Vec<CoreRecord> {
    let verified = VerifiedIndex::open(index).unwrap();
    let sources = verified
        .manifest()
        .sources
        .iter()
        .filter(|source| {
            source.observation().source().provider() == CaptureProvider::Gemini.as_str()
        })
        .map(|source| source.observation().source().clone())
        .collect::<Vec<_>>();
    let mut records = Vec::new();
    for source in sources {
        records.extend(
            verified
                .core_source_event_page(&source, None, 64)
                .unwrap()
                .items
                .into_iter()
                .map(|item| item.core_record),
        );
    }
    records
}

fn certified_prefix_bytes(index: &Path) -> u64 {
    let verified = VerifiedIndex::open(index).unwrap();
    verified
        .manifest()
        .sources
        .iter()
        .find(|source| source.observation().source().provider() == CaptureProvider::Gemini.as_str())
        .unwrap()
        .frontier()
        .expect("Gemini publication must persist a checkpoint frontier")
        .certified_prefix_bytes()
}

fn assert_literal_bodies(records: &[CoreRecord], expected: &[&str]) {
    assert_eq!(
        records
            .iter()
            .map(|record| record.content.normalized_body.as_deref().unwrap())
            .collect::<Vec<_>>(),
        expected
    );
}

#[test]
fn gemini_route_publishes_cold_append_and_recovers_from_carried_checkpoint() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join(".gemini");
    let transcript = transcript_path(&root);
    let index = temp.path().join("gemini-index");
    write_transcript(
        &transcript,
        &[
            header(),
            message(
                "literal-first",
                "2026-08-16T00:00:01Z",
                "user",
                "literal first",
            ),
        ],
    );
    let registry = registry(&root);
    let options = || WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    };

    let cold = refresh_source_backed_generation(&index, &registry, options()).unwrap();
    assert!(cold.failed_routes.is_empty());
    assert_eq!(cold.successful_route_ids.len(), 1);
    let cold_records = indexed_records(&index);
    assert_literal_bodies(&cold_records, &["literal first"]);
    let cold_checkpoint = certified_prefix_bytes(&index);
    assert_eq!(cold_checkpoint, fs::metadata(&transcript).unwrap().len());

    let noop = refresh_source_backed_generation(&index, &registry, options()).unwrap();
    assert!(noop.failed_routes.is_empty());
    assert_eq!(noop.commit.generation_id, cold.commit.generation_id);
    assert_eq!(indexed_records(&index), cold_records);

    append_transcript(
        &transcript,
        &message(
            "literal-second",
            "2026-08-16T00:00:02Z",
            "gemini",
            "literal second",
        ),
    );
    let appended = refresh_source_backed_generation(&index, &registry, options()).unwrap();
    assert!(appended.failed_routes.is_empty());
    let appended_records = indexed_records(&index);
    assert_literal_bodies(&appended_records, &["literal first", "literal second"]);
    assert_eq!(appended_records[0].event_id, cold_records[0].event_id);
    let appended_checkpoint = certified_prefix_bytes(&index);
    assert!(appended_checkpoint > cold_checkpoint);
    assert_eq!(
        appended_checkpoint,
        fs::metadata(&transcript).unwrap().len()
    );

    append_transcript(
        &transcript,
        &message(
            "literal-racing",
            "2026-08-16T00:00:03Z",
            "gemini",
            "race-before",
        ),
    );
    let hook_path = fs::canonicalize(&transcript).unwrap();
    set_after_jsonl_semantic_preflight_hook(hook_path.clone(), move || {
        let before = fs::read_to_string(&hook_path).unwrap();
        let after = before.replace("race-before", "race-after!");
        assert_eq!(before.len(), after.len());
        assert_ne!(before, after);
        fs::write(&hook_path, after).unwrap();
    });

    let failed = refresh_source_backed_generation(&index, &registry, options()).unwrap();
    assert!(matches!(
        failed.failed_routes.as_slice(),
        [failure]
            if failure.class == SourceBackedSourceFailureClass::SourceChanged
                && failure.carried_forward
    ));
    assert_eq!(certified_prefix_bytes(&index), appended_checkpoint);
    assert_eq!(indexed_records(&index), appended_records);

    let recovered = refresh_source_backed_generation(&index, &registry, options()).unwrap();
    assert!(recovered.failed_routes.is_empty());
    let recovered_records = indexed_records(&index);
    assert_literal_bodies(
        &recovered_records,
        &["literal first", "literal second", "race-after!"],
    );
    assert_eq!(recovered_records[0].event_id, cold_records[0].event_id);
    assert!(recovered_records
        .iter()
        .all(|record| record.agent_scope == Some(AgentScope::Primary)));
    assert_eq!(
        certified_prefix_bytes(&index),
        fs::metadata(&transcript).unwrap().len()
    );

    let source_before_relocation = gemini_sources(&index);
    let relocated = root.join("tmp/project/chats/parent-session/relocated-session.jsonl");
    fs::create_dir_all(relocated.parent().unwrap()).unwrap();
    fs::rename(&transcript, &relocated).unwrap();
    let relocation = refresh_source_backed_generation(&index, &registry, options()).unwrap();
    assert!(relocation.failed_routes.is_empty());
    assert_eq!(gemini_sources(&index), source_before_relocation);
    let relocated_records = indexed_records(&index);
    assert_eq!(relocated_records, recovered_records);
    assert!(relocated_records.iter().all(|record| {
        record.agent_scope == Some(AgentScope::Primary)
            && record.parent_session_id.is_none()
            && record.session_relationship.is_none()
    }));

    write_transcript(
        &relocated,
        &[
            header(),
            message(
                "literal-first",
                "2026-08-16T00:00:01Z",
                "user",
                "literal rewritten",
            ),
        ],
    );
    let rewrite = refresh_source_backed_generation(&index, &registry, options()).unwrap();
    assert!(rewrite.failed_routes.is_empty());
    assert_eq!(gemini_sources(&index), source_before_relocation);
    let rewritten_records = indexed_records(&index);
    assert_literal_bodies(&rewritten_records, &["literal rewritten"]);
    assert_eq!(rewritten_records[0].event_id, cold_records[0].event_id);
}

#[test]
fn gemini_resumed_recordings_share_provider_session_metadata_not_source_identity() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join(".gemini");
    let chats = root.join("tmp/project/chats");
    let first = chats.join("first.jsonl");
    let resumed = chats.join("resumed.jsonl");
    let index = temp.path().join("gemini-resumed-index");
    write_transcript(
        &first,
        &[
            recording_header(
                "shared-provider-session",
                "2026-08-23T15:53:00Z",
                "same-project",
            ),
            message(
                "same-native-event-id",
                "2026-08-23T15:53:01Z",
                "user",
                "first recording",
            ),
        ],
    );
    write_transcript(
        &resumed,
        &[
            recording_header(
                "shared-provider-session",
                "2026-08-23T16:03:00Z",
                "same-project",
            ),
            message(
                "same-native-event-id",
                "2026-08-23T16:03:01Z",
                "user",
                "resumed recording",
            ),
        ],
    );

    let receipt = refresh_source_backed_generation(
        &index,
        &registry(&root),
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();

    assert!(receipt.failed_routes.is_empty());
    assert_eq!(receipt.sources.len(), 2);
    let sources = gemini_sources(&index);
    assert_eq!(sources.len(), 2);
    assert_ne!(sources[0], sources[1]);
    let mut records = all_gemini_records(&index);
    records.sort_by(|left, right| {
        left.content
            .meaningful_text()
            .cmp(right.content.meaningful_text())
    });
    assert_literal_bodies(&records, &["first recording", "resumed recording"]);
    assert!(records.iter().all(|record| {
        record.provider_session_id.as_deref() == Some("shared-provider-session")
    }));
    assert_ne!(records[0].source, records[1].source);
    assert_ne!(records[0].session_id, records[1].session_id);
    assert_ne!(records[0].event_id, records[1].event_id);
}

#[test]
fn authentic_gemini_resume_capture_fails_v1_and_publishes_both_v2_recordings() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join(".gemini");
    copy_authentic_resume_fixture(&root);
    let legacy_error = refresh_source_backed_generation(
        temp.path().join("gemini-authentic-v1-index"),
        &legacy_v1_registry(&root),
        gemini_writer_options(),
    )
    .unwrap_err();
    assert!(
        legacy_error.to_string().contains("same recording identity"),
        "{legacy_error}"
    );

    let index = temp.path().join("gemini-authentic-v2-index");
    let current_registry = registry(&root);
    let imported =
        refresh_source_backed_generation(&index, &current_registry, gemini_writer_options())
            .unwrap();
    assert!(imported.failed_routes.is_empty());
    assert_eq!(imported.sources.len(), 2);
    let sources = gemini_sources(&index);
    assert_eq!(sources.len(), 2);
    assert_ne!(sources[0], sources[1]);
    assert!(sources
        .iter()
        .all(|source| source.provider_identity_version() == 2));

    let records = all_gemini_records(&index);
    assert_eq!(records.len(), 2);
    assert!(records.iter().all(|record| {
        record.provider_session_id.as_deref() == Some("65900000-0000-4000-8000-000000000659")
    }));
    let bodies = records
        .iter()
        .map(|record| record.content.meaningful_text())
        .collect::<Vec<_>>();
    for expected in [
        "authentic gemini first turn",
        "authentic gemini resumed turn",
    ] {
        assert!(
            bodies.contains(&expected),
            "missing {expected:?} in {bodies:?}"
        );
    }

    let noop = refresh_source_backed_generation(&index, &current_registry, gemini_writer_options())
        .unwrap();
    assert!(noop.failed_routes.is_empty());
    assert_eq!(noop.commit.generation_id, imported.commit.generation_id);
    assert_eq!(gemini_sources(&index), sources);
    assert_eq!(all_gemini_records(&index), records);
}

#[cfg(unix)]
#[test]
fn gemini_hardlink_recording_aliases_publish_one_logical_source() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join(".gemini");
    let chats = root.join("tmp/project/chats");
    let first = chats.join("first.jsonl");
    let alias = chats.join("alias.jsonl");
    let index = temp.path().join("gemini-alias-index");
    write_transcript(
        &first,
        &[
            recording_header(
                "aliased-provider-session",
                "2026-08-23T15:53:00Z",
                "same-project",
            ),
            message(
                "aliased-event",
                "2026-08-23T15:53:01Z",
                "user",
                "one physical recording",
            ),
        ],
    );
    fs::hard_link(&first, &alias).unwrap();

    let receipt = refresh_source_backed_generation(
        &index,
        &registry(&root),
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();

    assert!(receipt.failed_routes.is_empty());
    assert_eq!(gemini_sources(&index).len(), 1);
    assert_literal_bodies(&indexed_records(&index), &["one physical recording"]);
}

#[test]
fn gemini_copied_recording_aliases_publish_one_logical_source() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join(".gemini");
    let chats = root.join("tmp/project/chats");
    let first = chats.join("first.jsonl");
    let alias = chats.join("alias.jsonl");
    let index = temp.path().join("gemini-copied-alias-index");
    write_transcript(
        &first,
        &[
            recording_header(
                "copied-provider-session",
                "2026-08-23T15:53:00Z",
                "same-project",
            ),
            message(
                "copied-event",
                "2026-08-23T15:53:01Z",
                "user",
                "one copied recording",
            ),
        ],
    );
    fs::copy(&first, &alias).unwrap();

    let receipt = refresh_source_backed_generation(
        &index,
        &registry(&root),
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();

    assert!(receipt.failed_routes.is_empty());
    assert_eq!(gemini_sources(&index).len(), 1);
    assert_literal_bodies(&indexed_records(&index), &["one copied recording"]);
}

#[cfg(unix)]
#[test]
fn gemini_hardlink_alias_replacement_after_discovery_fails_closed() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join(".gemini");
    let chats = root.join("tmp/project/chats");
    let canonical = chats.join("a-canonical.jsonl");
    let alias = chats.join("z-alias.jsonl");
    let replacement = temp.path().join("replacement.jsonl");
    let rows = [
        recording_header(
            "racing-alias-session",
            "2026-08-23T15:53:00Z",
            "same-project",
        ),
        message(
            "racing-alias-event",
            "2026-08-23T15:53:01Z",
            "user",
            "one physical recording",
        ),
    ];
    write_transcript(&canonical, &rows);
    fs::hard_link(&canonical, &alias).unwrap();
    write_transcript(&replacement, &rows);

    let hook_ran = Arc::new(AtomicBool::new(false));
    let hook_observation = Arc::clone(&hook_ran);
    ctx_history_provider_gemini::nativepath::install_after_gemini_recording_discovery_hook(
        move || {
            fs::remove_file(&alias).unwrap();
            fs::rename(&replacement, &alias).unwrap();
            hook_observation.store(true, Ordering::SeqCst);
        },
    );

    let error = refresh_source_backed_generation(
        temp.path().join("gemini-alias-race-index"),
        &registry(&root),
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap_err();

    assert!(hook_ran.load(Ordering::SeqCst));
    assert!(matches!(
        error,
        crate::SourceBackedCoordinatorError::NoUsableSourceRoutes { failed_routes }
            if failed_routes.len() == 1
                && failed_routes[0].class == SourceBackedSourceFailureClass::SourceChanged
                && !failed_routes[0].carried_forward
    ));
}

#[test]
fn gemini_legacy_v1_generation_migrates_atomically_to_all_v2_recordings_then_noops() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join(".gemini");
    let chats = root.join("tmp/project/chats");
    let first = chats.join("first.jsonl");
    let resumed = chats.join("resumed.jsonl");
    let index = temp.path().join("gemini-v1-to-v2-index");
    write_transcript(
        &first,
        &[
            recording_header(
                "migrating-provider-session",
                "2026-08-23T15:53:00Z",
                "same-project",
            ),
            message(
                "first-event",
                "2026-08-23T15:53:01Z",
                "user",
                "legacy first recording",
            ),
        ],
    );

    let seeded = refresh_source_backed_generation(
        &index,
        &legacy_v1_registry(&root),
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    assert!(seeded.failed_routes.is_empty());
    assert_eq!(seeded.sources.len(), 1);
    assert_eq!(
        seeded.sources[0]
            .observation()
            .source()
            .provider_identity_version(),
        1
    );
    assert_eq!(
        seeded.sources[0].parser_revision(),
        "gemini-nativepath-core-activity-v1"
    );
    let legacy_source = seeded.sources[0].observation().source().clone();
    let legacy_records = all_gemini_records(&index);
    assert_eq!(legacy_records.len(), 1);
    let legacy_record = &legacy_records[0];
    let legacy_session_id = legacy_record.session_id;
    let legacy_event_id = legacy_record.event_id;
    let legacy_provider_session_id = legacy_record.provider_session_id.clone();
    let legacy_native_event_id = legacy_record.native_event_id.clone();

    write_transcript(
        &resumed,
        &[
            recording_header(
                "migrating-provider-session",
                "2026-08-23T16:03:00Z",
                "same-project",
            ),
            message(
                "resumed-event",
                "2026-08-23T16:03:01Z",
                "user",
                "current resumed recording",
            ),
        ],
    );

    let current_registry = registry(&root);
    let migrated = refresh_source_backed_generation(
        &index,
        &current_registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    assert!(
        migrated.failed_routes.is_empty(),
        "unexpected migration failures: {:?}",
        migrated.failed_routes
    );
    assert_ne!(migrated.commit.generation_id, seeded.commit.generation_id);
    assert_eq!(migrated.sources.len(), 2);
    assert!(migrated.sources.iter().all(|source| {
        source.observation().source().provider_identity_version() == 2
            && source.parser_revision() == "gemini-nativepath-core-activity-v2-record-rejections"
    }));
    let sources = gemini_sources(&index);
    assert_eq!(sources.len(), 2);
    assert!(sources
        .iter()
        .all(|source| source.provider_identity_version() == 2));
    assert!(!sources.iter().any(|source| source == &legacy_source));
    let mut records = all_gemini_records(&index);
    records.sort_by(|left, right| {
        left.content
            .meaningful_text()
            .cmp(right.content.meaningful_text())
    });
    assert_literal_bodies(
        &records,
        &["current resumed recording", "legacy first recording"],
    );
    assert!(records.iter().all(|record| {
        record.provider_session_id.as_deref() == Some("migrating-provider-session")
    }));
    assert!(records
        .iter()
        .all(|record| record.session_id != legacy_session_id));
    assert!(records
        .iter()
        .all(|record| record.event_id != legacy_event_id));
    let migrated_first = records
        .iter()
        .find(|record| record.content.meaningful_text() == "legacy first recording")
        .unwrap();
    assert_ne!(migrated_first.source, legacy_source);
    assert_ne!(migrated_first.session_id, legacy_session_id);
    assert_ne!(migrated_first.event_id, legacy_event_id);
    assert_eq!(
        migrated_first.provider_session_id,
        legacy_provider_session_id
    );
    assert_eq!(migrated_first.native_event_id, legacy_native_event_id);
    let migrated_records = records.clone();

    let noop = refresh_source_backed_generation(
        &index,
        &current_registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    assert!(noop.failed_routes.is_empty());
    assert_eq!(noop.commit.generation_id, migrated.commit.generation_id);
    assert_eq!(gemini_sources(&index), sources);
    let mut noop_records = all_gemini_records(&index);
    noop_records.sort_by(|left, right| {
        left.content
            .meaningful_text()
            .cmp(right.content.meaningful_text())
    });
    assert_eq!(noop_records, migrated_records);
}

#[test]
fn gemini_ambiguous_complete_recording_anchors_fail_before_writer_staging() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join(".gemini");
    let chats = root.join("tmp/project/chats");
    let identity = recording_header(
        "ambiguous-provider-session",
        "2026-08-23T15:53:00Z",
        "same-project",
    );
    write_transcript(
        &chats.join("first.jsonl"),
        &[
            identity.clone(),
            message("first", "2026-08-23T15:53:01Z", "user", "divergent first"),
        ],
    );
    write_transcript(
        &chats.join("second.jsonl"),
        &[
            identity,
            message("second", "2026-08-23T15:53:02Z", "user", "divergent second"),
        ],
    );

    let error = refresh_source_backed_generation(
        temp.path().join("gemini-ambiguous-index"),
        &registry(&root),
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap_err();
    let detail = error.to_string();

    assert!(detail.contains("same recording identity"), "{detail}");
    assert!(!detail.contains("source replacement has already started"));
}
