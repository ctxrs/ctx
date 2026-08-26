use std::{collections::BTreeMap, fs, path::PathBuf};

use ctx_history_core::{
    derive_event_id, derive_session_id, ActivityInvocation, ActivityJsonCapture, ActivityResult,
    ActivityTextCapture, CertifiedSource, CertifiedSourceDeletion, CertifiedSourceInventory,
    CoreActivity, CoreRecord, EventIdentityInput, LiteralFactKind, NativeItemKey, NativeSessionKey,
    ProviderDeclaredFact, ScannedSourceCounts, SessionIdentityInput, SourceAnchor,
    SourceInventoryObservation, SourceKey, SourceObservation, TypedKey, CORE_ACTIVITY_REVISION,
};
use ctx_history_index::{GenerationWriter, WriterOptions};
use ctx_history_index_format::core_content_bytes;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use ctx_history_index_generation::{
    acquire_generation_read_lease, active_index_files, open_slot_index, CloneTestHookGuard,
    CloneTestOptions,
};
use ctx_history_index_generation::{
    acquire_generation_retention_lease, certification_file_for_active, checksum_walks,
    load_active_generation_pointer, release_generation_retention_lease,
    reset_physical_verification_activity, slot_path, GenerationRootTraversalStage,
    GenerationRootTraversalTestHookGuard,
};
use ctx_history_index_query::{
    CoreEventPageBudget, DEFAULT_CORE_EVENT_PAGE_BUDGET, MAX_SOURCE_EVENT_PAGE_ITEMS,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use ctx_history_index_query::{IndexError, VerifiedIndex};
use ctx_history_platform::platform_security::restrict_private_directory;
use tempfile::{tempdir, TempDir};

use super::*;

struct Fixture {
    _temp: TempDir,
    data_root: PathBuf,
    index_root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempdir().unwrap();
        let data_root = temp.path().join("data");
        fs::create_dir(&data_root).unwrap();
        restrict_private_directory(&data_root).unwrap();
        let search = data_root.join("search");
        fs::create_dir(&search).unwrap();
        restrict_private_directory(&search).unwrap();
        let index_root = search.join("lexical");
        fs::create_dir(&index_root).unwrap();
        restrict_private_directory(&index_root).unwrap();
        Self {
            _temp: temp,
            data_root,
            index_root,
        }
    }
}

#[test]
fn concurrent_publication_is_not_reported_as_corruption() {
    assert!(matches!(
        map_generation_error(
            ctx_history_index_generation::GenerationError::ConcurrentGenerationChange,
            &"a".repeat(64),
        ),
        SnapshotError::ConcurrentGenerationChange(_)
    ));
    assert!(matches!(
        map_index_error(
            ctx_history_index_query::IndexError::ConcurrentGenerationChange,
            &"b".repeat(64),
            &SnapshotContract::current().unwrap(),
        ),
        SnapshotError::ConcurrentGenerationChange(_)
    ));
}

#[test]
fn durable_exact_target_opens_after_more_than_two_publications() {
    let fixture = Fixture::new();
    let source = source("durable-retained");
    let retained_id = publish(
        &fixture.index_root,
        1,
        &[(source.clone(), vec![record(&source, 1, "retained")])],
    );
    let authority = acquire_generation_retention_lease(
        &fixture.index_root,
        &retained_id,
        "restartable_catchup",
        &"a".repeat(64),
    )
    .unwrap();
    for revision in 2..=5 {
        publish(
            &fixture.index_root,
            revision,
            &[(
                source.clone(),
                vec![record(&source, 1, &format!("new-{revision}"))],
            )],
        );
    }

    assert!(matches!(
        CoreSnapshot::open(
            &fixture.data_root,
            &retained_id,
            &SnapshotContract::current().unwrap()
        ),
        Err(SnapshotError::NotFound(_))
    ));
    reset_physical_verification_activity();
    let retained = CoreSnapshot::open_retained(
        &fixture.data_root,
        &authority,
        &SnapshotContract::current().unwrap(),
    )
    .unwrap();
    assert_eq!(
        checksum_walks(),
        1,
        "durable target was not fully revalidated"
    );
    assert_eq!(retained.generation_id(), retained_id);
    assert_eq!(
        retained
            .record_page(&source, None, 8, DEFAULT_CORE_EVENT_PAGE_BUDGET)
            .unwrap()
            .items[0]
            .core_record
            .content
            .meaningful_text(),
        "retained"
    );
    drop(retained);

    assert!(release_generation_retention_lease(&fixture.index_root, &authority).unwrap());
    assert!(CoreSnapshot::open_retained(
        &fixture.data_root,
        &authority,
        &SnapshotContract::current().unwrap(),
    )
    .is_err());
}

#[cfg(unix)]
#[test]
fn opened_data_root_survives_parent_path_replacement_without_following_the_decoy() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let source = source("root-replacement");
    let generation_id = publish(
        &fixture.index_root,
        1,
        &[(source.clone(), vec![record(&source, 1, "original")])],
    );
    let moved = fixture._temp.path().join("opened-data-root");
    let decoy = fixture._temp.path().join("decoy");
    fs::create_dir(&decoy).unwrap();
    restrict_private_directory(&decoy).unwrap();
    let data_root = fixture.data_root.clone();
    let moved_for_hook = moved.clone();
    let decoy_for_hook = decoy.clone();
    let _hook = GenerationRootTraversalTestHookGuard::install(move |stage| {
        if stage == GenerationRootTraversalStage::DataRootOpened {
            fs::rename(&data_root, &moved_for_hook).unwrap();
            symlink(&decoy_for_hook, &data_root).unwrap();
        }
    });

    let snapshot = CoreSnapshot::open(
        &fixture.data_root,
        &generation_id,
        &SnapshotContract::current().unwrap(),
    )
    .unwrap();
    assert_eq!(snapshot.generation_id(), generation_id);
    assert!(!decoy.join(".ctx-generation-read-leases-v2.lock").exists());
    drop(snapshot);
    fs::remove_file(&fixture.data_root).unwrap();
    fs::rename(moved, &fixture.data_root).unwrap();
}

#[cfg(unix)]
#[test]
fn symlinked_search_component_is_rejected() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let generation_id = publish(&fixture.index_root, 1, &[]);
    let search = fixture.data_root.join("search");
    let moved = fixture.data_root.join("real-search");
    fs::rename(&search, &moved).unwrap();
    symlink(&moved, &search).unwrap();
    assert!(matches!(
        CoreSnapshot::open(
            &fixture.data_root,
            &generation_id,
            &SnapshotContract::current().unwrap()
        ),
        Err(SnapshotError::UnsafePath(_))
    ));
}

#[cfg(windows)]
#[test]
fn windows_parent_replacement_is_blocked_and_reparse_search_is_rejected() {
    use std::os::windows::fs::symlink_dir;

    let fixture = Fixture::new();
    let source = source("windows-root-replacement");
    let generation_id = publish(
        &fixture.index_root,
        1,
        &[(source.clone(), vec![record(&source, 1, "original")])],
    );
    let data_root = fixture.data_root.clone();
    let replacement = fixture._temp.path().join("replacement-attempt");
    let _hook = GenerationRootTraversalTestHookGuard::install(move |stage| {
        if stage == GenerationRootTraversalStage::DataRootOpened {
            assert!(fs::rename(&data_root, &replacement).is_err());
        }
    });
    assert_eq!(
        CoreSnapshot::open(
            &fixture.data_root,
            &generation_id,
            &SnapshotContract::current().unwrap()
        )
        .unwrap()
        .generation_id(),
        generation_id
    );
    drop(_hook);

    let search = fixture.data_root.join("search");
    let moved = fixture.data_root.join("real-search");
    fs::rename(&search, &moved).unwrap();
    symlink_dir(&moved, &search)
        .unwrap_or_else(|error| panic!("failed to create Windows search reparse point: {error}"));
    assert!(matches!(
        CoreSnapshot::open(
            &fixture.data_root,
            &generation_id,
            &SnapshotContract::current().unwrap()
        ),
        Err(SnapshotError::UnsafePath(_))
    ));
}

fn source(name: &str) -> SourceKey {
    SourceKey::derive(
        "snapshot-test",
        "snapshot-test-jsonl",
        "session",
        1,
        SourceAnchor::provider_native("session", TypedKey::utf8(name).unwrap()).unwrap(),
    )
    .unwrap()
}

fn record(source: &SourceKey, sequence: u64, body: &str) -> CoreRecord {
    let session_key =
        NativeSessionKey::native_id("session", TypedKey::utf8("session").unwrap()).unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let native_item_key = NativeItemKey::native_id("message", TypedKey::U64(sequence)).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .unwrap();
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        source.clone(),
        sequence,
        "message",
        "snapshot-reader-test-v1",
        body,
    )
    .unwrap();
    record.provider_session_id = Some("session".to_owned());
    record.native_event_id = Some(TypedKey::U64(sequence));
    record.role = Some("user".to_owned());
    record.content.activity = Some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: Some(TypedKey::Utf8(format!("call-{sequence}"))),
        invocation: Some(ActivityInvocation {
            protocol: Some("mcp".to_owned()),
            server: Some("snapshot-server".to_owned()),
            tool: "snapshot-tool".to_owned(),
            arguments: ActivityJsonCapture::Present {
                value: serde_json::json!({"sequence": sequence}),
            },
            started_at_unix_ms: Some(100 + sequence as i64),
        }),
        result: Some(ActivityResult {
            status: Some("provider-ok".to_owned()),
            completed_at_unix_ms: Some(200 + sequence as i64),
            duration_ns: Some(sequence),
            text: ActivityTextCapture::NormalizedBody,
            structured_content: ActivityJsonCapture::Present {
                value: serde_json::json!({"complete": body}),
            },
        }),
        facts: vec![
            ProviderDeclaredFact {
                kind: LiteralFactKind::SessionCwd,
                value: "/Literal/../Workspace".to_owned(),
            },
            ProviderDeclaredFact {
                kind: LiteralFactKind::File,
                value: "file:///Literal/Workspace/src/lib.rs".to_owned(),
            },
            ProviderDeclaredFact {
                kind: LiteralFactKind::Commit,
                value: "ProviderLiteralMixedCase".to_owned(),
            },
        ],
    });
    record
}

fn assert_neutral_core_json(stored_json: &[u8]) {
    let rendered = String::from_utf8(stored_json.to_vec()).unwrap();
    for forbidden in [
        "repository_bindings",
        "repository_abstentions",
        "repository_candidate_evidence",
        "repository_file_observations",
        "repository_vcs_observations",
        "change_kind",
        "confidence",
        "file_effect",
        "unique_to_session",
        "certified_ordered_prefix",
    ] {
        assert!(!rendered.contains(forbidden), "{forbidden}");
    }
    assert!(rendered.contains("file:///Literal/Workspace/src/lib.rs"));
    assert!(rendered.contains("ProviderLiteralMixedCase"));
    assert!(rendered.contains("snapshot-server"));
    assert!(rendered.contains("structured_content"));
}

fn certificate(source: &SourceKey, revision: u8, records: usize) -> CertifiedSource {
    let observation =
        SourceObservation::new(source.clone(), "regular-file-v1", vec![revision]).unwrap();
    CertifiedSource::certify(
        observation.clone(),
        observation,
        "snapshot-reader-test-parser-v1",
        [revision; 32],
        ScannedSourceCounts {
            complete_records: records as u64,
            retained_records: records as u64,
            indexed_documents: records as u64,
            certified_bytes: records as u64 * 100,
            ..ScannedSourceCounts::default()
        },
    )
    .unwrap()
}

fn publish(
    index_root: &std::path::Path,
    revision: u8,
    sources: &[(SourceKey, Vec<CoreRecord>)],
) -> String {
    let mut writer = GenerationWriter::open(index_root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    for (source, records) in sources {
        writer.begin_source(source.clone()).unwrap();
        for record in records {
            writer.add_core_record(record.clone()).unwrap();
        }
        writer
            .certify_source(certificate(source, revision, records.len()))
            .unwrap();
    }
    writer.commit(|_| true).unwrap().generation_id
}

fn deletion(
    removed: &SourceKey,
    retained: Vec<SourceKey>,
    revision: u8,
) -> (CertifiedSourceDeletion, CertifiedSourceInventory) {
    let observation = SourceInventoryObservation::new(
        removed.provider(),
        "provider-root",
        TypedKey::utf8("snapshot-test-root").unwrap(),
        "tree-inventory-v1",
        vec![revision],
    )
    .unwrap();
    let inventory = CertifiedSourceInventory::certify(
        observation.clone(),
        observation,
        "snapshot-reader-test-discovery-v1",
        retained,
    )
    .unwrap();
    let proof = CertifiedSourceDeletion::from_inventory(removed.clone(), &inventory).unwrap();
    (proof, inventory)
}

fn publish_replacement_with_addition_and_removal(
    index_root: &std::path::Path,
    replaced: &SourceKey,
    added: &SourceKey,
    removed: &SourceKey,
) -> (String, Vec<CoreRecord>) {
    let replacement = vec![
        record(replaced, 1, "replacement one"),
        record(replaced, 3, "replacement three"),
    ];
    let addition = vec![record(added, 1, "addition")];
    let mut writer = GenerationWriter::open(index_root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    for (source, records) in [
        (replaced, replacement.as_slice()),
        (added, addition.as_slice()),
    ] {
        writer.begin_source(source.clone()).unwrap();
        for record in records {
            writer.add_core_record(record.clone()).unwrap();
        }
        writer
            .certify_source(certificate(source, 2, records.len()))
            .unwrap();
    }
    let (proof, inventory) = deletion(removed, vec![replaced.clone(), added.clone()], 2);
    writer.delete_source(proof, inventory).unwrap();
    let generation_id = writer.commit(|_| true).unwrap().generation_id;
    (generation_id, replacement)
}

fn open(fixture: &Fixture, generation_id: &str) -> CoreSnapshot {
    CoreSnapshot::open(
        &fixture.data_root,
        generation_id,
        &SnapshotContract::current().unwrap(),
    )
    .unwrap()
}

#[test]
fn manifest_delta_and_replayable_exact_json_pages_use_neutral_bounded_state() {
    let fixture = Fixture::new();
    let replaced = source("replaced");
    let removed = source("removed");
    let added = source("added");
    let base_id = publish(
        &fixture.index_root,
        1,
        &[
            (
                replaced.clone(),
                vec![
                    record(&replaced, 1, "base one"),
                    record(&replaced, 2, "base two"),
                ],
            ),
            (removed.clone(), vec![record(&removed, 1, "removed")]),
        ],
    );
    let base = open(&fixture, &base_id);
    let (target_id, expected_replacement) = publish_replacement_with_addition_and_removal(
        &fixture.index_root,
        &replaced,
        &added,
        &removed,
    );
    let target = open(&fixture, &target_id);

    let mut manifest_cursor = None;
    let mut states = Vec::new();
    loop {
        let page = target
            .source_manifest_page(manifest_cursor.as_ref(), 1)
            .unwrap();
        assert!(page.items.len() <= 1);
        for state in &page.items {
            let encoded = serde_json::to_value(state).unwrap();
            for forbidden in [
                "observation",
                "revision",
                "parser_revision",
                "content_digest",
                "frontier",
            ] {
                assert!(encoded.get(forbidden).is_none());
            }
        }
        states.extend(page.items);
        if page.terminal {
            assert!(page.next_cursor.is_none());
            break;
        }
        manifest_cursor = page.next_cursor;
    }
    assert_eq!(states.len(), 2);
    assert!(states
        .iter()
        .any(|state| state.source.exact_descriptor_eq(&replaced)));
    assert!(states
        .iter()
        .any(|state| state.source.exact_descriptor_eq(&added)));

    let mut delta_cursor = None;
    let mut changes = BTreeMap::new();
    loop {
        let page = target
            .source_delta_page(&base, delta_cursor.as_ref(), 1)
            .unwrap();
        assert!(page.items.len() <= 1);
        for change in page.items {
            match change {
                SourceDelta::Added(state) => {
                    changes.insert(state.source.identity().digest(), "added");
                }
                SourceDelta::Replaced { previous, current } => {
                    assert!(previous.source.exact_descriptor_eq(&current.source));
                    changes.insert(current.source.identity().digest(), "replaced");
                }
                SourceDelta::Removed(state) => {
                    changes.insert(state.source.identity().digest(), "removed");
                }
            }
        }
        if page.terminal {
            assert!(page.next_cursor.is_none());
            break;
        }
        let encoded = serde_json::to_vec(page.next_cursor.as_ref().unwrap()).unwrap();
        delta_cursor = Some(serde_json::from_slice(&encoded).unwrap());
    }
    assert_eq!(
        changes.get(&replaced.identity().digest()),
        Some(&"replaced")
    );
    assert_eq!(changes.get(&added.identity().digest()), Some(&"added"));
    assert_eq!(changes.get(&removed.identity().digest()), Some(&"removed"));

    let first = target
        .record_page(&replaced, None, 1, DEFAULT_CORE_EVENT_PAGE_BUDGET)
        .unwrap();
    assert_eq!(first.items.len(), 1);
    assert!(!first.terminal);
    assert!(first.next_cursor.is_some());
    assert_eq!(
        first.encoded_core_bytes,
        first
            .items
            .iter()
            .map(|item| item.stored_json.len())
            .sum::<usize>()
    );
    assert_eq!(
        first.content_bytes,
        first
            .items
            .iter()
            .map(|item| core_content_bytes(&item.core_record.content).unwrap())
            .sum::<usize>()
    );
    for item in &first.items {
        assert_eq!(item.stored_json, item.core_record.encode_stored().unwrap());
        assert_neutral_core_json(&item.stored_json);
        assert_eq!(
            CoreRecord::decode_stored(&item.stored_json).unwrap(),
            item.core_record
        );
    }

    let serialized_cursor = serde_json::to_vec(first.next_cursor.as_ref().unwrap()).unwrap();
    let cursor: CoreRecordPageCursor = serde_json::from_slice(&serialized_cursor).unwrap();
    assert_eq!(cursor.generation_id(), target_id);
    assert!(cursor.source().exact_descriptor_eq(&replaced));
    let second = target
        .record_page(&replaced, Some(&cursor), 1, DEFAULT_CORE_EVENT_PAGE_BUDGET)
        .unwrap();
    let replay = target
        .record_page(&replaced, Some(&cursor), 1, DEFAULT_CORE_EVENT_PAGE_BUDGET)
        .unwrap();
    assert_eq!(second, replay);
    for item in &second.items {
        assert_neutral_core_json(&item.stored_json);
        assert_eq!(item.stored_json, item.core_record.encode_stored().unwrap());
    }
    assert!(second.terminal);
    assert!(second.next_cursor.is_none());
    let durable_page = serde_json::to_vec(&second).unwrap();
    assert_eq!(
        serde_json::from_slice::<CoreRecordPage>(&durable_page).unwrap(),
        second
    );
    let mut observed = first
        .items
        .into_iter()
        .chain(second.items)
        .map(|item| item.core_record)
        .collect::<Vec<_>>();
    observed.sort_by_key(|record| record.event_id.digest());
    let mut expected = expected_replacement;
    expected.sort_by_key(|record| record.event_id.digest());
    assert_eq!(observed, expected);
}

#[test]
fn schema_fingerprint_certification_and_bounds_fail_with_typed_errors_without_hashing() {
    let fixture = Fixture::new();
    let source = source("typed-errors");
    let generation_id = publish(
        &fixture.index_root,
        1,
        &[(source.clone(), vec![record(&source, 1, "record")])],
    );

    let mut wrong_schema = SnapshotContract::current().unwrap();
    wrong_schema.schema.manifest_version += 1;
    assert!(matches!(
        CoreSnapshot::open(&fixture.data_root, &generation_id, &wrong_schema),
        Err(SnapshotError::SchemaMismatch { .. })
    ));
    let mut wrong_fingerprint = SnapshotContract::current().unwrap();
    wrong_fingerprint.core_record_fingerprint = "f".repeat(64);
    assert!(matches!(
        CoreSnapshot::open(&fixture.data_root, &generation_id, &wrong_fingerprint),
        Err(SnapshotError::FingerprintMismatch { .. })
    ));

    let snapshot = open(&fixture, &generation_id);
    assert!(matches!(
        snapshot.source_manifest_page(None, 0),
        Err(SnapshotError::Bounds(_))
    ));
    assert!(matches!(
        snapshot.source_manifest_page(None, MAX_SOURCE_MANIFEST_PAGE_ITEMS + 1),
        Err(SnapshotError::Bounds(_))
    ));
    assert!(matches!(
        snapshot.source_delta_page(&snapshot, None, 0),
        Err(SnapshotError::Bounds(_))
    ));
    assert!(matches!(
        snapshot.record_page(&source, None, 0, DEFAULT_CORE_EVENT_PAGE_BUDGET),
        Err(SnapshotError::Bounds(_))
    ));
    assert!(matches!(
        snapshot.record_page(
            &source,
            None,
            MAX_SOURCE_EVENT_PAGE_ITEMS + 1,
            DEFAULT_CORE_EVENT_PAGE_BUDGET
        ),
        Err(SnapshotError::Bounds(_))
    ));
    assert!(matches!(
        snapshot.record_page(&source, None, 1, CoreEventPageBudget::new(0, 1)),
        Err(SnapshotError::Bounds(_))
    ));
    drop(snapshot);

    fs::remove_file(certification_file_for_active(&fixture.index_root).unwrap()).unwrap();
    reset_physical_verification_activity();
    assert!(matches!(
        CoreSnapshot::open(
            &fixture.data_root,
            &generation_id,
            &SnapshotContract::current().unwrap()
        ),
        Err(SnapshotError::Corrupt(_))
    ));
    assert_eq!(
        checksum_walks(),
        0,
        "reader hashed an uncertified generation"
    );
}

#[test]
fn overlapping_readers_retain_a_real_generation_until_the_last_close() {
    let fixture = Fixture::new();
    let source = source("overlapping");
    let first_id = publish(
        &fixture.index_root,
        1,
        &[(source.clone(), vec![record(&source, 1, "first")])],
    );
    let first_slot = load_active_generation_pointer(&fixture.index_root)
        .unwrap()
        .unwrap()
        .active()
        .clone();
    let first_path = slot_path(&fixture.index_root, &first_slot);
    let first = open(&fixture, &first_id);
    let second = open(&fixture, &first_id);

    publish(
        &fixture.index_root,
        2,
        &[(source.clone(), vec![record(&source, 1, "second")])],
    );
    publish(
        &fixture.index_root,
        3,
        &[(source.clone(), vec![record(&source, 1, "third")])],
    );
    assert!(first_path.is_dir());
    drop(first);
    drop(
        GenerationWriter::open(&fixture.index_root, WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap(),
    );
    assert!(first_path.is_dir());
    drop(second);
    drop(
        GenerationWriter::open(&fixture.index_root, WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap(),
    );
    assert!(!first_path.exists());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn leased_certified_generation_rejects_net_zero_hardlink_churn_without_hashing() {
    use std::os::unix::fs::MetadataExt as _;

    let _clone = CloneTestHookGuard::set(
        CloneTestOptions {
            force_reflink_fallback: true,
            ..CloneTestOptions::default()
        },
        |_, _| Ok(()),
    );
    let fixture = Fixture::new();
    let source = source("pointer-churn");
    let first_id = publish(
        &fixture.index_root,
        1,
        &[(source.clone(), vec![record(&source, 1, "first")])],
    );
    let first_slot = load_active_generation_pointer(&fixture.index_root)
        .unwrap()
        .unwrap()
        .active()
        .clone();
    let first_path = slot_path(&fixture.index_root, &first_slot);
    let first_index = open_slot_index(&fixture.index_root, &first_slot).unwrap();
    let certified_metadata = active_index_files(&first_index)
        .unwrap()
        .into_iter()
        .map(|relative_path| {
            let metadata = fs::metadata(first_path.join(&relative_path)).unwrap();
            (
                relative_path,
                metadata.len(),
                metadata.mode(),
                metadata.mtime(),
                metadata.mtime_nsec(),
                metadata.ctime(),
                metadata.ctime_nsec(),
                metadata.nlink(),
            )
        })
        .collect::<Vec<_>>();
    let lease = acquire_generation_read_lease(&fixture.index_root, &first_id).unwrap();
    publish(
        &fixture.index_root,
        2,
        &[(source.clone(), vec![record(&source, 1, "second")])],
    );
    publish(
        &fixture.index_root,
        3,
        &[(source.clone(), vec![record(&source, 1, "third")])],
    );
    assert!(certified_metadata.iter().any(
        |(relative_path, len, mode, mtime, mtime_nsec, ctime, ctime_nsec, nlink)| {
            let current = fs::metadata(first_path.join(relative_path)).unwrap();
            current.len() == *len
                && current.mode() == *mode
                && current.mtime() == *mtime
                && current.mtime_nsec() == *mtime_nsec
                && current.nlink() == *nlink
                && (current.ctime(), current.ctime_nsec()) != (*ctime, *ctime_nsec)
        }
    ));

    reset_physical_verification_activity();
    assert!(matches!(
        lease
            .with_root_access(|root| VerifiedIndex::open_generation_read_lease(root, &lease))
            .unwrap(),
        Err(IndexError::ChecksumMismatch)
    ));
    assert_eq!(checksum_walks(), 0, "leased reader hashed stale metadata");
}

#[test]
fn current_contract_and_path_validation_are_explicit() {
    let contract = SnapshotContract::current().unwrap();
    assert_eq!(
        contract.schema.manifest_version,
        GENERATION_MANIFEST_VERSION
    );
    assert_eq!(contract.schema.identity_version, IDENTITY_VERSION);
    assert_eq!(contract.schema.core_record_version, CORE_RECORD_VERSION);
    assert_eq!(
        contract.schema.lexical_schema_version,
        LEXICAL_SCHEMA_VERSION
    );
    assert_eq!(
        contract.schema.lexical_analyzer_version,
        LEXICAL_ANALYZER_VERSION
    );
    assert_eq!(contract.core_record_fingerprint.len(), 64);
    assert_eq!(contract.schema.policy_schema_hash.len(), 64);
    assert!(matches!(
        CoreSnapshot::open("relative", &"a".repeat(64), &contract),
        Err(SnapshotError::UnsafePath(_))
    ));
}
