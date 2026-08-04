use super::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    env, io,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::publication::{CloneMetrics, CloneStage, CloneTestHookGuard, CloneTestOptions};
use crate::{
    durable_directory::{AtomicWriteStage, AtomicWriteTestHookGuard},
    publication::{
        MigrationStage, MigrationTestHookGuard, PointerReconciliationTestHookGuard,
        PortableCloneMetrics, PortableCloneStage, PortableCloneTestGuard, PortableCloneTestOptions,
    },
};

const SUCCESSOR_CORE_FINGERPRINT: &str =
    "bc73c991e160746fbaaddb641fdce8c7bec24e5ba212a406ec26d197cf0c6a5e";
const PUBLICATION_METADATA: &[u8] = b"source-catalog-frontier-receipt-v1";
const GOLDEN_GENERATION_ID: &str =
    "a71ac367a8192609dc5b739e8f68e83124ee369e7cb3975a88e873eafe9f0283";
const SUBPROCESS_MODE_ENV: &str = "CTX_PREDECESSOR_MIGRATION_CHILD";
const SUBPROCESS_ROOT_ENV: &str = "CTX_PREDECESSOR_MIGRATION_ROOT";
const SUBPROCESS_MARKER_ENV: &str = "CTX_PREDECESSOR_MIGRATION_MARKER";
const SUBPROCESS_CONTINUE_ENV: &str = "CTX_PREDECESSOR_MIGRATION_CONTINUE";
const SUBPROCESS_RESULT_ENV: &str = "CTX_PREDECESSOR_MIGRATION_RESULT";
const SUBPROCESS_TIMEOUT: Duration = Duration::from_secs(20);

mod portable;
mod readers;

struct GoldenPredecessor {
    temp: TempDir,
    source: SourceKey,
}

impl GoldenPredecessor {
    fn copy() -> Self {
        let temp = tempdir().unwrap();
        copy_fixture_tree(&fixture_root().join("index"), temp.path());
        Self {
            temp,
            source: source("golden-predecessor.jsonl"),
        }
    }

    fn root(&self) -> &Path {
        self.temp.path()
    }

    fn generation_id(&self) -> &str {
        GOLDEN_GENERATION_ID
    }
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join("core-predecessor-7552eee7")
}

fn copy_fixture_tree(source: &Path, destination: &Path) {
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        let metadata = fs::metadata(entry.path()).unwrap();
        if metadata.is_dir() {
            fs::create_dir(&target).unwrap();
            copy_fixture_tree(&entry.path(), &target);
        } else {
            assert!(metadata.is_file());
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn stored_core_bytes(index: &VerifiedIndex) -> Vec<Vec<u8>> {
    let fields = fields_from_schema(index.searcher.schema()).unwrap();
    let mut records = index
        .searcher
        .search(&AllQuery, &DocSetCollector)
        .unwrap()
        .into_iter()
        .map(|address| {
            let document: TantivyDocument = index.searcher.doc(address).unwrap();
            let encoded = document
                .get_first(fields.core_record)
                .and_then(|value| value.as_bytes())
                .unwrap();
            let record = CoreRecord::decode_stored(encoded).unwrap();
            (record.event_id, encoded.to_vec())
        })
        .collect::<Vec<_>>();
    records.sort_by_key(|(event_id, _)| event_id.as_uuid());
    records.into_iter().map(|(_, encoded)| encoded).collect()
}

fn active_meta_generation(root: &Path) -> BTreeMap<String, Option<u64>> {
    let pointer = load_active_generation_pointer(root).unwrap().unwrap();
    let index = open_slot_index(root, pointer.active()).unwrap();
    meta_generation(&index.load_metas().unwrap())
}

fn manifest_without_migration_identities(manifest: &GenerationManifest) -> serde_json::Value {
    let mut value = serde_json::to_value(manifest).unwrap();
    let object = value.as_object_mut().unwrap();
    object.remove("core_record_contract_fingerprint");
    object.remove("policy_schema_hash");
    value
}

fn open_writer_error(root: &Path) -> IndexError {
    match GenerationWriter::open(root, WriterOptions::default()) {
        Ok(_) => panic!("generation writer unexpectedly opened"),
        Err(error) => error,
    }
}

fn fixture_file_paths(root: &Path) -> BTreeSet<String> {
    fn visit(base: &Path, path: &Path, files: &mut BTreeSet<String>) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                visit(base, &entry.path(), files);
            } else {
                files.insert(
                    entry
                        .path()
                        .strip_prefix(base)
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .to_owned(),
                );
            }
        }
    }
    let mut files = BTreeSet::new();
    visit(root, root, &mut files);
    files
}

#[test]
fn checked_in_predecessor_fixture_has_exact_provenance_and_hashes() {
    let root = fixture_root();
    let provenance: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("PROVENANCE.json")).unwrap()).unwrap();
    assert_eq!(provenance["version"], 1);
    assert_eq!(
        provenance["source_commit"],
        "a0ff045f8a223468b2f00b1e6e1d9a51709d208f"
    );
    assert_eq!(
        provenance["core_record_contract_fingerprint"],
        SAME_EPOCH_PREDECESSOR_CORE_FINGERPRINT
    );
    assert_eq!(provenance["generation_id"], GOLDEN_GENERATION_ID);
    let manifest: GenerationManifest = serde_json::from_slice(
        &fs::read(
            root.join("index")
                .join("ctx-generations")
                .join(format!("{GOLDEN_GENERATION_ID}.json")),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        manifest.core_record_contract_fingerprint,
        SAME_EPOCH_PREDECESSOR_CORE_FINGERPRINT
    );
    assert_eq!(
        manifest.policy_schema_hash,
        SAME_EPOCH_PREDECESSOR_SOURCE_GENERATION_POLICY_HASH
    );

    let mut declared = BTreeSet::new();
    for file in provenance["files"].as_array().unwrap() {
        let relative = file["path"].as_str().unwrap();
        declared.insert(relative.to_owned());
        let bytes = fs::read(root.join(relative)).unwrap();
        assert_eq!(file["bytes"], bytes.len() as u64, "{relative}");
        assert_eq!(file["sha256"], sha256_hex(&bytes), "{relative}");
    }
    let actual = fixture_file_paths(&root.join("index"))
        .into_iter()
        .map(|path| format!("index/{path}"))
        .collect::<BTreeSet<_>>();
    assert_eq!(declared, actual);
}

#[test]
fn allowlisted_predecessor_is_queryable_through_every_shared_open_path() {
    let predecessor = GoldenPredecessor::copy();

    assert_eq!(
        VerifiedIndex::active_generation_id(predecessor.root())
            .unwrap()
            .as_deref(),
        Some(predecessor.generation_id())
    );
    let audited = VerifiedIndex::open(predecessor.root()).unwrap();
    let pinned = VerifiedIndex::open_pinned(predecessor.root()).unwrap();
    let exact =
        VerifiedIndex::open_pinned_generation(predecessor.root(), predecessor.generation_id())
            .unwrap();
    for index in [&audited, &pinned, &exact] {
        assert_eq!(index.generation_id(), predecessor.generation_id());
        assert!(index.uses_allowlisted_predecessor_contract());
        assert_eq!(index.document_count(), 3);
        assert_eq!(index.count_term("predecessor").unwrap(), 3);
        assert_eq!(
            index.manifest().core_record_contract_fingerprint,
            SAME_EPOCH_PREDECESSOR_CORE_FINGERPRINT
        );
    }

    let first = pinned
        .source_event_page(&predecessor.source, None, 1)
        .unwrap();
    assert_eq!(first.items.len(), 1);
    assert!(!first.terminal);
    let second = pinned
        .source_event_page(&predecessor.source, first.next_cursor.as_ref(), 1)
        .unwrap();
    assert_eq!(second.items.len(), 1);
    assert!(!second.terminal);
    let third = pinned
        .source_event_page(&predecessor.source, second.next_cursor.as_ref(), 1)
        .unwrap();
    assert_eq!(third.items.len(), 1);
    assert!(third.terminal);
}

#[test]
fn writer_lease_migrates_without_sources_and_preserves_records_segments_and_receipt() {
    assert_eq!(
        ctx_history_core::core_record_contract_fingerprint(),
        SUCCESSOR_CORE_FINGERPRINT,
        "the decisive migration path must use the integrated successor Core contract"
    );
    let predecessor = GoldenPredecessor::copy();
    let predecessor_pointer = load_active_generation_pointer(predecessor.root())
        .unwrap()
        .unwrap();
    let predecessor_reader = VerifiedIndex::open_pinned(predecessor.root()).unwrap();
    let predecessor_bytes = stored_core_bytes(&predecessor_reader);
    let predecessor_segments = active_meta_generation(predecessor.root());
    let predecessor_manifest = predecessor_reader.manifest().clone();
    assert_eq!(
        predecessor_manifest.policy_schema_hash,
        SAME_EPOCH_PREDECESSOR_SOURCE_GENERATION_POLICY_HASH
    );

    let outcome = GenerationWriter::open(predecessor.root(), WriterOptions::default()).unwrap();
    assert!(outcome.committed_migration_recovery().is_none());
    let writer = outcome.into_writer().unwrap();
    let current_manifest = writer.base_manifest().unwrap();
    assert_eq!(
        current_manifest.core_record_contract_fingerprint,
        SUCCESSOR_CORE_FINGERPRINT
    );
    assert_eq!(
        current_manifest.policy_schema_hash,
        current_source_generation_policy_hash().unwrap()
    );
    assert_eq!(
        manifest_without_migration_identities(current_manifest),
        manifest_without_migration_identities(&predecessor_manifest)
    );

    let current_pointer = load_active_generation_pointer(predecessor.root())
        .unwrap()
        .unwrap();
    assert_ne!(
        current_pointer.active().generation_id(),
        predecessor.generation_id()
    );
    assert_eq!(
        current_pointer.previous().unwrap(),
        predecessor_pointer.active()
    );
    assert_eq!(
        active_meta_generation(predecessor.root()),
        predecessor_segments
    );

    crate::publication::reset_verification_activity();
    for _ in 0..3 {
        let reopened = VerifiedIndex::open_pinned(predecessor.root()).unwrap();
        assert_eq!(
            reopened.generation_id(),
            current_pointer.active().generation_id()
        );
    }
    assert_eq!(crate::publication::verification_activity().0, 0);
    assert_eq!(crate::publication::hashed_artifact_bytes(), 0);

    let current_reader = VerifiedIndex::open_pinned(predecessor.root()).unwrap();
    assert!(!current_reader.uses_allowlisted_predecessor_contract());
    assert_eq!(stored_core_bytes(&current_reader), predecessor_bytes);
    assert_eq!(
        current_reader.publication_metadata(),
        Some(PUBLICATION_METADATA)
    );
    assert_eq!(current_reader.document_count(), 3);
    assert_eq!(current_reader.count_term("migration").unwrap(), 3);

    let retained_predecessor =
        VerifiedIndex::open_pinned_generation(predecessor.root(), predecessor.generation_id())
            .unwrap();
    assert_eq!(retained_predecessor.document_count(), 3);
    assert_eq!(predecessor_reader.count_term("evidence").unwrap(), 3);
    drop(writer);
}

#[test]
fn every_prepublication_migration_failure_keeps_the_predecessor_pointer_and_queries() {
    let stages = [
        MigrationStage::BeforePredecessorVerification,
        MigrationStage::AfterPredecessorVerification,
        MigrationStage::BeforeCandidateCreation,
        MigrationStage::AfterCandidateCreation,
        MigrationStage::BeforeCandidateCommit,
        MigrationStage::AfterCandidateCommit,
        MigrationStage::BeforeCandidateSync,
        MigrationStage::AfterCandidateSync,
        MigrationStage::AfterCandidateVerification,
        MigrationStage::BeforePointerPublication,
    ];

    for fault_stage in stages {
        let predecessor = GoldenPredecessor::copy();
        let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
        let fault = MigrationTestHookGuard::set(move |stage, _| {
            if stage == fault_stage {
                return Err(std::io::Error::other(format!(
                    "injected predecessor migration fault at {stage:?}"
                ))
                .into());
            }
            Ok(())
        });

        assert!(matches!(
            open_writer_error(predecessor.root()),
            IndexError::Io(_)
        ));
        assert_eq!(
            fs::read(predecessor.root().join("active-generation.json")).unwrap(),
            pointer_before,
            "pointer changed at {fault_stage:?}"
        );
        let reader = VerifiedIndex::open(predecessor.root()).unwrap();
        assert_eq!(reader.generation_id(), predecessor.generation_id());
        assert_eq!(reader.count_term("evidence").unwrap(), 3);
        drop(fault);
        let retry = GenerationWriter::open(predecessor.root(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        assert_eq!(
            retry
                .base_manifest()
                .unwrap()
                .core_record_contract_fingerprint,
            SUCCESSOR_CORE_FINGERPRINT
        );
        assert_eq!(reader.count_term("evidence").unwrap(), 3);
    }
}

#[test]
fn corrupt_migration_candidate_never_changes_or_damages_the_predecessor() {
    let predecessor = GoldenPredecessor::copy();
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
    let fault = MigrationTestHookGuard::set(|stage, path| {
        if stage == MigrationStage::BeforeCandidateVerification {
            fs::write(path.unwrap().join("meta.json"), b"corrupt candidate meta")?;
        }
        Ok(())
    });

    let _error = open_writer_error(predecessor.root());
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
    let reader = VerifiedIndex::open(predecessor.root()).unwrap();
    assert_eq!(reader.generation_id(), predecessor.generation_id());
    assert_eq!(reader.count_term("evidence").unwrap(), 3);
    drop(fault);
    let retry = GenerationWriter::open(predecessor.root(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    assert_eq!(
        retry
            .base_manifest()
            .unwrap()
            .core_record_contract_fingerprint,
        SUCCESSOR_CORE_FINGERPRINT
    );
}

#[test]
fn unknown_core_fingerprint_fails_all_reads_and_never_starts_source_rebuild() {
    let predecessor = GoldenPredecessor::copy();
    let pointer = load_active_generation_pointer(predecessor.root())
        .unwrap()
        .unwrap();
    let index = open_slot_index(predecessor.root(), pointer.active()).unwrap();
    let metas = index.load_metas().unwrap();
    let mut manifest = load_publication_for_metas(predecessor.root(), &metas)
        .unwrap()
        .manifest;
    let unknown = "f".repeat(64);
    assert_ne!(unknown, SUCCESSOR_CORE_FINGERPRINT);
    manifest.core_record_contract_fingerprint = unknown.clone();
    publish_unchecked_generation(predecessor.root(), &index, manifest, &[], Vec::new());

    let unknown_generation_id = load_active_generation_pointer(predecessor.root())
        .unwrap()
        .unwrap()
        .active()
        .generation_id()
        .to_owned();
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
    let generation_directories_before =
        fs::read_dir(predecessor.root().join(INDEX_GENERATIONS_DIRECTORY))
            .unwrap()
            .count();
    for error in [
        VerifiedIndex::active_generation_id(predecessor.root()).unwrap_err(),
        match VerifiedIndex::open(predecessor.root()) {
            Ok(_) => panic!("unknown fingerprint unexpectedly opened"),
            Err(error) => error,
        },
        match VerifiedIndex::open_pinned(predecessor.root()) {
            Ok(_) => panic!("unknown fingerprint unexpectedly opened pinned"),
            Err(error) => error,
        },
        match VerifiedIndex::open_pinned_generation(predecessor.root(), &unknown_generation_id) {
            Ok(_) => panic!("unknown fingerprint unexpectedly opened by generation"),
            Err(error) => error,
        },
        open_writer_error(predecessor.root()),
    ] {
        assert!(matches!(
            error,
            IndexError::CoreRecordContractMismatch { ref actual, .. } if actual == &unknown
        ));
    }
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
    assert_eq!(
        fs::read_dir(predecessor.root().join(INDEX_GENERATIONS_DIRECTORY))
            .unwrap()
            .count(),
        generation_directories_before,
        "unknown fingerprint entered the source-rebuild candidate path"
    );
}

fn publish_predecessor_with_encoded_mutation(
    root: &Path,
    mutate: impl FnOnce(&[u8]) -> Vec<u8>,
) -> String {
    let (searcher, manifest) = open_unverified_generation(root);
    let fields = fields_from_schema(searcher.schema()).unwrap();
    let address = searcher
        .search(&AllQuery, &DocSetCollector)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let stored: TantivyDocument = searcher.doc(address).unwrap();
    let encoded = stored
        .get_first(fields.core_record)
        .and_then(|value| value.as_bytes())
        .unwrap();
    let encoded = mutate(encoded);
    let mut forged = TantivyDocument::default();
    for (field, value) in stored.field_values() {
        if field != fields.core_record {
            forged.add_field_value(field, value);
        }
    }
    forged.add_bytes(fields.core_record, &encoded);
    let index = searcher.index().clone();
    drop(searcher);
    let source = source("golden-predecessor.jsonl");
    publish_unchecked_generation(root, &index, manifest, &[source], vec![forged]);
    load_active_generation_pointer(root)
        .unwrap()
        .unwrap()
        .active()
        .generation_id()
        .to_owned()
}

fn publish_predecessor_with_nested_successor_member(
    root: &Path,
    value: serde_json::Value,
) -> String {
    publish_predecessor_with_encoded_mutation(root, |encoded| {
        let mut json = serde_json::from_slice::<serde_json::Value>(encoded).unwrap();
        json["content"]
            .as_object_mut()
            .unwrap()
            .insert("mcp_exchange".to_owned(), value);
        serde_json::to_vec(&json).unwrap()
    })
}

fn publish_predecessor_with_raw_content_member_prefix(root: &Path, prefix: &[u8]) -> String {
    publish_predecessor_with_encoded_mutation(root, |encoded| {
        const CONTENT_OBJECT_PREFIX: &[u8] = br#""content":{"#;

        let content_offset = encoded
            .windows(CONTENT_OBJECT_PREFIX.len())
            .position(|window| window == CONTENT_OBJECT_PREFIX)
            .unwrap()
            + CONTENT_OBJECT_PREFIX.len();
        let mut malformed = Vec::with_capacity(encoded.len() + prefix.len());
        malformed.extend_from_slice(&encoded[..content_offset]);
        malformed.extend_from_slice(prefix);
        malformed.extend_from_slice(&encoded[content_offset..]);
        malformed
    })
}

fn assert_predecessor_shape_error(error: IndexError, expected_member: &'static str) {
    assert!(matches!(
        error,
        IndexError::PredecessorCoreRecordShapeMismatch { member }
            if member == expected_member
    ));
}

fn assert_predecessor_shape_rejected_on_every_path(
    predecessor: &GoldenPredecessor,
    generation_id: &str,
    expected_member: &'static str,
) {
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();

    assert_predecessor_shape_error(
        VerifiedIndex::active_generation_id(predecessor.root()).unwrap_err(),
        expected_member,
    );
    assert_predecessor_shape_error(
        match VerifiedIndex::open(predecessor.root()) {
            Ok(_) => panic!("malformed predecessor unexpectedly opened"),
            Err(error) => error,
        },
        expected_member,
    );
    assert_predecessor_shape_error(
        match VerifiedIndex::open_pinned(predecessor.root()) {
            Ok(_) => panic!("malformed predecessor unexpectedly opened pinned"),
            Err(error) => error,
        },
        expected_member,
    );
    assert_predecessor_shape_error(
        match VerifiedIndex::open_pinned_generation(predecessor.root(), generation_id) {
            Ok(_) => panic!("malformed predecessor unexpectedly opened by generation"),
            Err(error) => error,
        },
        expected_member,
    );
    assert_predecessor_shape_error(open_writer_error(predecessor.root()), expected_member);
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
}

#[test]
fn predecessor_label_rejects_null_or_present_nested_successor_member_on_every_path() {
    for successor_member in [
        serde_json::Value::Null,
        serde_json::json!({"provider_call_id": "fixture-call"}),
    ] {
        let predecessor = GoldenPredecessor::copy();
        let generation_id =
            publish_predecessor_with_nested_successor_member(predecessor.root(), successor_member);
        assert_predecessor_shape_rejected_on_every_path(
            &predecessor,
            &generation_id,
            "content.mcp_exchange",
        );
    }
}

#[test]
fn predecessor_label_rejects_escaped_and_duplicate_nested_successor_keys_on_every_path() {
    for raw_prefix in [
        br#""mcp\u005fexchange":null,"#.as_slice(),
        br#""mcp_exchange":null,"mcp_exchange":null,"#.as_slice(),
    ] {
        let predecessor = GoldenPredecessor::copy();
        let generation_id =
            publish_predecessor_with_raw_content_member_prefix(predecessor.root(), raw_prefix);
        assert_predecessor_shape_rejected_on_every_path(
            &predecessor,
            &generation_id,
            "content.mcp_exchange",
        );
    }
}

#[test]
fn unexpected_regular_file_is_not_cloned_and_keeps_predecessor_queryable() {
    let predecessor = GoldenPredecessor::copy();
    fs::write(
        active_generation_path(predecessor.root()).join("untrusted.extra"),
        b"not authenticated by the active Tantivy metadata",
    )
    .unwrap();
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();

    assert!(matches!(
        open_writer_error(predecessor.root()),
        IndexError::PredecessorMigrationSourceTopology("unexpected directory entry")
    ));
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
    assert_eq!(
        VerifiedIndex::open(predecessor.root())
            .unwrap()
            .count_term("evidence")
            .unwrap(),
        3
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn symlink_in_predecessor_generation_is_rejected_before_clone() {
    use std::os::unix::fs::symlink;

    let predecessor = GoldenPredecessor::copy();
    let generation = active_generation_path(predecessor.root());
    let active_file = fs::read_dir(&generation)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().and_then(|extension| extension.to_str()) == Some("store"))
        .unwrap();
    let escaped_file = predecessor.root().join("escaped-active-segment.store");
    fs::rename(&active_file, &escaped_file).unwrap();
    symlink(&escaped_file, &active_file).unwrap();
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();

    assert!(matches!(
        open_writer_error(predecessor.root()),
        IndexError::ChecksumMismatch
            | IndexError::PredecessorMigrationSourceTopology(
                "symlinked or non-directory migration source"
            )
    ));
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn symlinked_active_generation_directory_is_rejected_without_following_it() {
    use std::os::unix::fs::symlink;

    let predecessor = GoldenPredecessor::copy();
    let held_reader = VerifiedIndex::open(predecessor.root()).unwrap();
    let active = active_generation_path(predecessor.root());
    let escaped = predecessor.root().join("escaped-active-generation");
    fs::rename(&active, &escaped).unwrap();
    symlink(&escaped, &active).unwrap();
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();

    assert!(matches!(
        open_writer_error(predecessor.root()),
        IndexError::ChecksumMismatch
            | IndexError::PredecessorMigrationSourceTopology(
                "symlinked or non-directory migration source"
            )
    ));
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
    assert_eq!(held_reader.count_term("evidence").unwrap(), 3);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn active_generation_replacement_race_is_detected_by_descriptor_identity() {
    let predecessor = GoldenPredecessor::copy();
    let held_reader = VerifiedIndex::open(predecessor.root()).unwrap();
    let active = active_generation_path(predecessor.root());
    let displaced = predecessor.root().join("displaced-active-generation");
    let hook_active = active.clone();
    let hook_displaced = displaced.clone();
    let mut replaced = false;
    let fault = CloneTestHookGuard::set(CloneTestOptions::default(), move |stage, _| {
        if stage == CloneStage::BeforeFile && !replaced {
            fs::rename(&hook_active, &hook_displaced)?;
            fs::create_dir(&hook_active)?;
            replaced = true;
        }
        Ok(())
    });
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();

    assert!(matches!(
        open_writer_error(predecessor.root()),
        IndexError::PredecessorMigrationSourceTopology(
            "active generation directory changed during migration"
        )
    ));
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
    assert_eq!(held_reader.count_term("evidence").unwrap(), 3);
    drop(fault);
    fs::remove_dir(&active).unwrap();
    fs::rename(displaced, active).unwrap();
    assert_eq!(
        VerifiedIndex::open(predecessor.root())
            .unwrap()
            .count_term("evidence")
            .unwrap(),
        3
    );
}

#[test]
fn managed_metadata_path_escape_is_rejected_before_clone() {
    let predecessor = GoldenPredecessor::copy();
    let managed = active_generation_path(predecessor.root()).join(".managed.json");
    let mut paths = serde_json::from_slice::<Vec<String>>(&fs::read(&managed).unwrap()).unwrap();
    paths.push("../outside".to_owned());
    fs::write(&managed, serde_json::to_vec(&paths).unwrap()).unwrap();
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();

    assert!(matches!(
        open_writer_error(predecessor.root()),
        IndexError::PredecessorMigrationSourceTopology("managed path escapes generation directory")
    ));
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
}

#[test]
fn oversized_managed_metadata_hits_the_real_clone_byte_bound_before_copy() {
    let predecessor = GoldenPredecessor::copy();
    let managed = active_generation_path(predecessor.root()).join(".managed.json");
    let mut paths = serde_json::from_slice::<Vec<String>>(&fs::read(&managed).unwrap()).unwrap();
    let repeated = paths[0].clone();
    let maximum = 1024 * 1024_u64;
    let encoded_path_bytes = serde_json::to_vec(&repeated).unwrap().len() + 1;
    let repeat_count = usize::try_from(maximum).unwrap() / encoded_path_bytes + 2;
    paths.extend(std::iter::repeat_n(repeated, repeat_count));
    let encoded = serde_json::to_vec(&paths).unwrap();
    let actual = encoded.len() as u64;
    assert!(actual > maximum);
    fs::write(&managed, encoded).unwrap();
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();

    let error = open_writer_error(predecessor.root());
    assert!(
        matches!(
            error,
            IndexError::PredecessorMigrationByteLimit {
                actual: error_actual,
                maximum: error_maximum
            } if error_actual == actual && error_maximum == maximum
        ),
        "{error:?}"
    );
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
    assert_eq!(
        VerifiedIndex::open(predecessor.root())
            .unwrap()
            .count_term("evidence")
            .unwrap(),
        3
    );
}

fn fill_generation_past_migration_entry_cap(generation: &Path) {
    const OVERFLOW_ENTRY_COUNT: usize = 4_097;
    let existing = fs::read_dir(generation).unwrap().count();
    assert!(existing < OVERFLOW_ENTRY_COUNT);
    for ordinal in existing..OVERFLOW_ENTRY_COUNT {
        fs::write(
            generation.join(format!("unmanaged-overflow-{ordinal:04}")),
            b"",
        )
        .unwrap();
    }
    assert_eq!(
        fs::read_dir(generation).unwrap().count(),
        OVERFLOW_ENTRY_COUNT
    );
}

#[test]
fn native_clone_enforces_the_entry_cap_during_enumeration() {
    let predecessor = GoldenPredecessor::copy();
    fill_generation_past_migration_entry_cap(&active_generation_path(predecessor.root()));
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();

    assert!(matches!(
        open_writer_error(predecessor.root()),
        IndexError::PredecessorMigrationFileLimit {
            actual: 4_097,
            maximum: 4_096
        }
    ));
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn forced_copy_fallback_is_bounded_instrumented_and_migrates() {
    let predecessor = GoldenPredecessor::copy();
    let hook = CloneTestHookGuard::set(
        CloneTestOptions {
            force_copy: true,
            available_bytes: None,
        },
        |_, _| Ok(()),
    );

    let writer = GenerationWriter::open(predecessor.root(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let metrics: CloneMetrics = hook.metrics();
    assert!(metrics.planned_files > 2);
    assert_eq!(metrics.linked_files, 0);
    assert_eq!(metrics.copied_files, metrics.planned_files);
    assert_eq!(metrics.copied_bytes, metrics.logical_bytes);
    assert!(metrics.required_headroom > metrics.logical_bytes);
    assert!(metrics.available_bytes >= metrics.required_headroom);
    assert!(writer.base_manifest().is_some());
    assert_eq!(
        VerifiedIndex::open(predecessor.root())
            .unwrap()
            .count_term("evidence")
            .unwrap(),
        3
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn forced_copy_write_failures_preserve_predecessor_pointer_and_queries() {
    for raw_error in [libc::ENOSPC, libc::EIO] {
        let predecessor = GoldenPredecessor::copy();
        let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
        let fault = CloneTestHookGuard::set(
            CloneTestOptions {
                force_copy: true,
                available_bytes: None,
            },
            move |stage, _| {
                if stage == CloneStage::BeforeCopy {
                    return Err(io::Error::from_raw_os_error(raw_error).into());
                }
                Ok(())
            },
        );

        assert!(matches!(
            open_writer_error(predecessor.root()),
            IndexError::Io(ref error) if error.raw_os_error() == Some(raw_error)
        ));
        assert_eq!(
            fs::read(predecessor.root().join("active-generation.json")).unwrap(),
            pointer_before
        );
        assert_eq!(
            VerifiedIndex::open(predecessor.root())
                .unwrap()
                .count_term("evidence")
                .unwrap(),
            3
        );
        drop(fault);
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn native_copy_detects_growth_without_writing_past_authenticated_length() {
    use std::io::Write as _;

    let predecessor = GoldenPredecessor::copy();
    let source_generation = active_generation_path(predecessor.root());
    let source_file = fs::read_dir(&source_generation)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().and_then(|extension| extension.to_str()) == Some("store"))
        .unwrap();
    let source_name = source_file.file_name().unwrap().to_owned();
    let original_bytes = fs::metadata(&source_file).unwrap().len();
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
    let source_for_hook = source_file.clone();
    let mut grew = false;
    let guard = CloneTestHookGuard::set(
        CloneTestOptions {
            force_copy: true,
            available_bytes: None,
        },
        move |stage, relative| {
            if stage == CloneStage::BeforeCopy && relative == Path::new(&source_name) && !grew {
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&source_for_hook)?
                    .write_all(b"growth-after-authentication")?;
                grew = true;
            }
            Ok(())
        },
    );

    assert!(matches!(
        open_writer_error(predecessor.root()),
        IndexError::PredecessorMigrationSourceTopology("source file grew while cloning")
    ));
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
    drop(guard);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&source_file)
        .unwrap()
        .set_len(original_bytes)
        .unwrap();
    assert_eq!(
        VerifiedIndex::open(predecessor.root())
            .unwrap()
            .count_term("evidence")
            .unwrap(),
        3
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn native_candidate_replacement_is_rejected_and_cleanup_preserves_replacement() {
    for replacement_stage in [
        MigrationStage::AfterCandidateCreation,
        MigrationStage::BeforeCandidateCommit,
        MigrationStage::BeforeCandidateSync,
        MigrationStage::BeforeCandidateVerification,
        MigrationStage::BeforePointerPublication,
    ] {
        let predecessor = GoldenPredecessor::copy();
        let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
        let replacement = std::rc::Rc::new(std::cell::RefCell::new(None::<(PathBuf, PathBuf)>));
        let replacement_for_hook = std::rc::Rc::clone(&replacement);
        let mut replaced = false;
        let hook = MigrationTestHookGuard::set(move |stage, path| {
            if stage == replacement_stage && !replaced {
                let candidate = path.unwrap().to_path_buf();
                let displaced = candidate.with_file_name(format!(
                    "{}-authenticated-orphan",
                    candidate.file_name().unwrap().to_string_lossy()
                ));
                fs::rename(&candidate, &displaced)?;
                fs::create_dir(&candidate)?;
                fs::write(
                    candidate.join("replacement-sentinel"),
                    b"must survive native cleanup",
                )?;
                *replacement_for_hook.borrow_mut() = Some((candidate, displaced));
                replaced = true;
            }
            Ok(())
        });

        assert!(matches!(
            open_writer_error(predecessor.root()),
            IndexError::PredecessorMigrationSourceTopology(
                "active generation directory changed during migration"
            )
        ));
        assert_eq!(
            fs::read(predecessor.root().join("active-generation.json")).unwrap(),
            pointer_before,
            "pointer changed at {replacement_stage:?}"
        );
        let (replacement, displaced) = replacement.borrow().clone().unwrap();
        assert_eq!(
            fs::read(replacement.join("replacement-sentinel")).unwrap(),
            b"must survive native cleanup",
            "replacement was mutated at {replacement_stage:?}"
        );
        fs::remove_dir_all(displaced).unwrap();
        assert_eq!(
            VerifiedIndex::open(predecessor.root())
                .unwrap()
                .count_term("evidence")
                .unwrap(),
            3
        );
        drop(hook);
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn insufficient_clone_headroom_is_rejected_before_writes() {
    let predecessor = GoldenPredecessor::copy();
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
    let fault = CloneTestHookGuard::set(
        CloneTestOptions {
            force_copy: false,
            available_bytes: Some(0),
        },
        |_, _| panic!("headroom rejection must precede clone work"),
    );

    assert!(matches!(
        open_writer_error(predecessor.root()),
        IndexError::PredecessorMigrationInsufficientHeadroom {
            available: 0,
            required
        } if required > 0
    ));
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
    assert_eq!(
        VerifiedIndex::open(predecessor.root())
            .unwrap()
            .count_term("evidence")
            .unwrap(),
        3
    );
    drop(fault);
}

#[test]
fn pointer_directory_fsync_error_returns_committed_visible_status_and_restarts_current() {
    let predecessor = GoldenPredecessor::copy();
    let fault = AtomicWriteTestHookGuard::set(|stage, target| {
        if stage == AtomicWriteStage::AfterReplaceBeforeDirectorySync
            && target.file_name().and_then(|name| name.to_str()) == Some("active-generation.json")
        {
            return Err(io::Error::from_raw_os_error(5));
        }
        Ok(())
    });

    let outcome = GenerationWriter::open(predecessor.root(), WriterOptions::default()).unwrap();
    let recovery = outcome
        .committed_migration_recovery()
        .expect("visible pointer durability uncertainty must be reported as committed")
        .clone();
    let writer = outcome.into_writer().unwrap();
    assert_eq!(
        recovery.generation_id(),
        writer.base_manifest().unwrap().generation_id().unwrap()
    );
    assert_ne!(recovery.generation_id(), predecessor.generation_id());
    drop(writer);
    drop(fault);

    let current = VerifiedIndex::open(predecessor.root()).unwrap();
    assert!(!current.uses_allowlisted_predecessor_contract());
    assert_eq!(current.count_term("evidence").unwrap(), 3);
    let restarted = GenerationWriter::open(predecessor.root(), WriterOptions::default()).unwrap();
    assert!(restarted.committed_migration_recovery().is_none());
    drop(restarted.into_writer().unwrap());
}

#[test]
fn malformed_pointer_reload_after_visibility_is_repaired_as_committed_outcome() {
    let predecessor = GoldenPredecessor::copy();
    let failed_once = std::rc::Rc::new(std::cell::Cell::new(false));
    let atomic_failed_once = std::rc::Rc::clone(&failed_once);
    let atomic_fault = AtomicWriteTestHookGuard::set(move |stage, target| {
        if stage == AtomicWriteStage::AfterReplaceBeforeDirectorySync
            && target.file_name().and_then(|name| name.to_str()) == Some("active-generation.json")
            && !atomic_failed_once.replace(true)
        {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        Ok(())
    });
    let reconciliation_fault = PointerReconciliationTestHookGuard::set(|root| {
        fs::write(root.join("active-generation.json"), b"{malformed-pointer")?;
        load_active_generation_pointer(root)
    });

    let outcome = GenerationWriter::open(predecessor.root(), WriterOptions::default())
        .expect("visible migration must not be reported as an ordinary error");
    assert!(matches!(
        &outcome,
        GenerationWriterOpenOutcome::RecoveredCommittedMigration { .. }
    ));
    drop(outcome.into_writer().unwrap());
    assert!(!VerifiedIndex::open(predecessor.root())
        .unwrap()
        .uses_allowlisted_predecessor_contract());
    drop(reconciliation_fault);
    drop(atomic_fault);
}

#[test]
fn unreadable_pointer_reconciliation_and_failed_repair_require_committed_recovery() {
    let predecessor = GoldenPredecessor::copy();
    let pointer_visible = std::rc::Rc::new(std::cell::Cell::new(false));
    let atomic_pointer_visible = std::rc::Rc::clone(&pointer_visible);
    let atomic_fault = AtomicWriteTestHookGuard::set(move |stage, target| {
        if target.file_name().and_then(|name| name.to_str()) != Some("active-generation.json") {
            return Ok(());
        }
        if stage == AtomicWriteStage::AfterReplaceBeforeDirectorySync
            && !atomic_pointer_visible.replace(true)
        {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        if stage == AtomicWriteStage::BeforeTemporaryWrite && atomic_pointer_visible.get() {
            return Err(io::Error::from_raw_os_error(libc::ENOSPC));
        }
        Ok(())
    });
    let reconciliation_fault = PointerReconciliationTestHookGuard::set(|_| {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "injected unreadable successor pointer",
        )
        .into())
    });

    let outcome = GenerationWriter::open(predecessor.root(), WriterOptions::default())
        .expect("postvisibility unknown state must be a committed outcome");
    let recovery = match outcome {
        GenerationWriterOpenOutcome::CommittedMigrationRecoveryRequired { recovery } => recovery,
        _ => panic!("failed pointer repair unexpectedly produced a usable writer"),
    };
    assert_ne!(recovery.generation_id(), predecessor.generation_id());
    assert!(recovery.detail().contains("pointer reload failed"));
    assert!(recovery.detail().contains("pointer repair failed"));
    assert!(!VerifiedIndex::open(predecessor.root())
        .unwrap()
        .uses_allowlisted_predecessor_contract());

    drop(reconciliation_fault);
    drop(atomic_fault);
    let restarted = GenerationWriter::open(predecessor.root(), WriterOptions::default()).unwrap();
    assert!(matches!(restarted, GenerationWriterOpenOutcome::Ready(_)));
}

#[test]
fn atomic_pointer_rename_failure_is_previsibility_and_preserves_predecessor_bytes() {
    let predecessor = GoldenPredecessor::copy();
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
    let fault = AtomicWriteTestHookGuard::set(|stage, target| {
        if stage == AtomicWriteStage::BeforeReplace
            && target.file_name().and_then(|name| name.to_str()) == Some("active-generation.json")
        {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        Ok(())
    });

    assert!(matches!(
        open_writer_error(predecessor.root()),
        IndexError::Io(ref error) if error.raw_os_error() == Some(libc::EIO)
    ));
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
    assert!(VerifiedIndex::open(predecessor.root())
        .unwrap()
        .uses_allowlisted_predecessor_contract());
    drop(fault);
}

#[test]
fn post_publication_cleanup_failure_is_best_effort_and_restart_keeps_current_authority() {
    let predecessor = GoldenPredecessor::copy();
    let fault = MigrationTestHookGuard::set(|stage, _| {
        if stage == MigrationStage::PostPublicationCleanup {
            return Err(io::Error::other("injected post-publication cleanup failure").into());
        }
        Ok(())
    });

    let outcome = GenerationWriter::open(predecessor.root(), WriterOptions::default()).unwrap();
    assert!(outcome.committed_migration_recovery().is_none());
    let writer = outcome.into_writer().unwrap();
    drop(writer);
    drop(fault);
    let current_id = VerifiedIndex::active_generation_id(predecessor.root())
        .unwrap()
        .unwrap();
    assert_ne!(current_id, predecessor.generation_id());
    drop(
        GenerationWriter::open(predecessor.root(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap(),
    );
    assert_eq!(
        VerifiedIndex::active_generation_id(predecessor.root())
            .unwrap()
            .unwrap(),
        current_id
    );
}

fn subprocess_paths(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
    (
        root.join("migration-child.marker"),
        root.join("migration-child.continue"),
        root.join("migration-child.result"),
    )
}

fn pause_subprocess(marker: &Path, continue_path: &Path, witness: &str) -> io::Result<()> {
    fs::write(marker, witness)?;
    while !continue_path.exists() {
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

#[test]
fn predecessor_migration_subprocess_worker() {
    let Ok(mode) = env::var(SUBPROCESS_MODE_ENV) else {
        return;
    };
    let root = PathBuf::from(env::var_os(SUBPROCESS_ROOT_ENV).unwrap());
    let marker = PathBuf::from(env::var_os(SUBPROCESS_MARKER_ENV).unwrap());
    let continue_path = PathBuf::from(env::var_os(SUBPROCESS_CONTINUE_ENV).unwrap());
    let result = PathBuf::from(env::var_os(SUBPROCESS_RESULT_ENV).unwrap());

    let mut migration_guard = None;
    let mut atomic_guard = None;
    if let Some(stage_name) = mode.strip_prefix("pause-migration:") {
        let stage_name = stage_name.to_owned();
        let marker = marker.clone();
        let continue_path = continue_path.clone();
        migration_guard = Some(MigrationTestHookGuard::set(move |stage, _| {
            if format!("{stage:?}") == stage_name {
                pause_subprocess(&marker, &continue_path, &stage_name)?;
            }
            Ok(())
        }));
    } else if mode == "pause-after-pointer-temp-sync" {
        let marker = marker.clone();
        let continue_path = continue_path.clone();
        atomic_guard = Some(AtomicWriteTestHookGuard::set(move |stage, target| {
            if stage == AtomicWriteStage::AfterTemporarySyncBeforeReplace
                && target.file_name().and_then(|name| name.to_str())
                    == Some("active-generation.json")
            {
                pause_subprocess(&marker, &continue_path, "pointer-temp-synced")?;
            }
            Ok(())
        }));
    } else if mode == "pause-after-pointer-replace" {
        let marker = marker.clone();
        let continue_path = continue_path.clone();
        atomic_guard = Some(AtomicWriteTestHookGuard::set(move |stage, target| {
            if stage == AtomicWriteStage::AfterReplaceBeforeDirectorySync
                && target.file_name().and_then(|name| name.to_str())
                    == Some("active-generation.json")
            {
                pause_subprocess(&marker, &continue_path, "pointer-replaced")?;
            }
            Ok(())
        }));
    } else if mode == "fail-pointer-directory-sync" {
        atomic_guard = Some(AtomicWriteTestHookGuard::set(|stage, target| {
            if stage == AtomicWriteStage::AfterReplaceBeforeDirectorySync
                && target.file_name().and_then(|name| name.to_str())
                    == Some("active-generation.json")
            {
                return Err(io::Error::from_raw_os_error(5));
            }
            Ok(())
        }));
    } else if mode == "fail-pointer-write-enospc" {
        atomic_guard = Some(AtomicWriteTestHookGuard::set(|stage, target| {
            if stage == AtomicWriteStage::BeforeTemporaryWrite
                && target.file_name().and_then(|name| name.to_str())
                    == Some("active-generation.json")
            {
                return Err(io::Error::from_raw_os_error(28));
            }
            Ok(())
        }));
    } else {
        panic!("unknown predecessor migration child mode {mode}");
    }

    let detail = match GenerationWriter::open(&root, WriterOptions::default()) {
        Ok(outcome) => {
            let recovered = outcome.committed_migration_recovery().is_some();
            let generation_id = outcome
                .into_writer()
                .unwrap()
                .base_manifest()
                .unwrap()
                .generation_id()
                .unwrap()
                .to_owned();
            format!("COMMITTED {generation_id} {recovered}")
        }
        Err(error) => format!("ERROR {error:?}\n{error}"),
    };
    fs::write(result, detail).unwrap();
    drop(atomic_guard);
    drop(migration_guard);
}

fn spawn_migration_subprocess(root: &Path, mode: &str) -> Child {
    let (marker, continue_path, result) = subprocess_paths(root);
    for path in [&marker, &continue_path, &result] {
        let _ = fs::remove_file(path);
    }
    Command::new(env::current_exe().unwrap())
        .arg("--exact")
        .arg("tests::migration::predecessor_migration_subprocess_worker")
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env(SUBPROCESS_MODE_ENV, mode)
        .env(SUBPROCESS_ROOT_ENV, root)
        .env(SUBPROCESS_MARKER_ENV, marker)
        .env(SUBPROCESS_CONTINUE_ENV, continue_path)
        .env(SUBPROCESS_RESULT_ENV, result)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap()
}

fn wait_for_subprocess_marker(child: &mut Child, marker: &Path) {
    let deadline = Instant::now() + SUBPROCESS_TIMEOUT;
    while Instant::now() < deadline {
        if marker.exists() {
            return;
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!("migration child exited before checkpoint: {status}");
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for migration child checkpoint");
}

fn kill_migration_subprocess(child: &mut Child) {
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
}

#[test]
fn subprocess_process_death_around_commit_sync_and_pointer_rename_recovers_correct_authority() {
    for (mode, successor_visible) in [
        ("pause-migration:AfterCandidateCommit", false),
        ("pause-migration:AfterCandidateSync", false),
        ("pause-after-pointer-temp-sync", false),
        ("pause-after-pointer-replace", true),
    ] {
        let predecessor = GoldenPredecessor::copy();
        let held_reader = VerifiedIndex::open(predecessor.root()).unwrap();
        let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
        let (marker, _, _) = subprocess_paths(predecessor.root());
        let mut child = spawn_migration_subprocess(predecessor.root(), mode);
        wait_for_subprocess_marker(&mut child, &marker);
        kill_migration_subprocess(&mut child);

        let pointer_after = fs::read(predecessor.root().join("active-generation.json")).unwrap();
        assert_eq!(pointer_after != pointer_before, successor_visible, "{mode}");
        assert_eq!(held_reader.count_term("evidence").unwrap(), 3);
        let before_restart = VerifiedIndex::open(predecessor.root()).unwrap();
        assert_eq!(
            before_restart.uses_allowlisted_predecessor_contract(),
            !successor_visible,
            "{mode}"
        );

        drop(
            GenerationWriter::open(predecessor.root(), WriterOptions::default())
                .unwrap()
                .into_writer()
                .unwrap(),
        );
        let after_restart = VerifiedIndex::open(predecessor.root()).unwrap();
        assert!(!after_restart.uses_allowlisted_predecessor_contract());
        assert_eq!(after_restart.count_term("evidence").unwrap(), 3);
    }
}

#[test]
fn subprocess_pointer_enospc_is_prepublication_failure_and_retry_migrates() {
    let predecessor = GoldenPredecessor::copy();
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
    let mut child = spawn_migration_subprocess(predecessor.root(), "fail-pointer-write-enospc");
    assert!(child.wait().unwrap().success());
    let (_, _, result) = subprocess_paths(predecessor.root());
    assert!(fs::read_to_string(result).unwrap().starts_with("ERROR"));
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
    assert!(VerifiedIndex::open(predecessor.root())
        .unwrap()
        .uses_allowlisted_predecessor_contract());
    drop(
        GenerationWriter::open(predecessor.root(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap(),
    );
    assert!(!VerifiedIndex::open(predecessor.root())
        .unwrap()
        .uses_allowlisted_predecessor_contract());
}

#[test]
fn subprocess_post_rename_fsync_failure_is_committed_and_restart_reads_successor() {
    let predecessor = GoldenPredecessor::copy();
    let mut child = spawn_migration_subprocess(predecessor.root(), "fail-pointer-directory-sync");
    assert!(child.wait().unwrap().success());
    let (_, _, result) = subprocess_paths(predecessor.root());
    let result = fs::read_to_string(result).unwrap();
    assert!(result.starts_with("COMMITTED "), "{result}");
    assert!(result.ends_with(" true"), "{result}");
    let current = VerifiedIndex::open(predecessor.root()).unwrap();
    assert!(!current.uses_allowlisted_predecessor_contract());
    drop(
        GenerationWriter::open(predecessor.root(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap(),
    );
    assert_eq!(
        VerifiedIndex::active_generation_id(predecessor.root())
            .unwrap()
            .as_deref(),
        Some(current.generation_id())
    );
}
