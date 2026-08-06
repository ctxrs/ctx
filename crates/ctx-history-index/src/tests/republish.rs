use super::*;
use std::{
    collections::BTreeSet,
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
        republish_current_for_qualification, CurrentRepublishOutcome,
        PointerReconciliationTestHookGuard, PortableCloneMetrics, PortableCloneStage,
        PortableCloneTestGuard, PortableCloneTestOptions, RepublishRecovery, RepublishStage,
        RepublishTestHookGuard,
    },
};

const PUBLICATION_METADATA: &[u8] = b"source-catalog-frontier-receipt-v1";
const GOLDEN_GENERATION_ID: &str =
    "a71ac367a8192609dc5b739e8f68e83124ee369e7cb3975a88e873eafe9f0283";
const RETIRED_CORE_FINGERPRINT: &str =
    "7552eee7cae0695a98f202b02f52cbf5680845cb7bacea4ed754e283bc15f051";
const RETIRED_SOURCE_GENERATION_POLICY_HASH: &str =
    "e728b5d7b76d04248e9dccc91fc11d915fcbcd714b445090725ba0604b8e8b37";
const SUBPROCESS_MODE_ENV: &str = "CTX_CURRENT_REPUBLISH_CHILD";
const SUBPROCESS_ROOT_ENV: &str = "CTX_CURRENT_REPUBLISH_ROOT";
const SUBPROCESS_MARKER_ENV: &str = "CTX_CURRENT_REPUBLISH_MARKER";
const SUBPROCESS_CONTINUE_ENV: &str = "CTX_CURRENT_REPUBLISH_CONTINUE";
const SUBPROCESS_RESULT_ENV: &str = "CTX_CURRENT_REPUBLISH_RESULT";
const SUBPROCESS_TIMEOUT: Duration = Duration::from_secs(20);

mod portable;
mod readers;

struct GoldenPredecessor {
    temp: TempDir,
    source: SourceKey,
    generation_id: String,
}

impl GoldenPredecessor {
    fn copy() -> Self {
        let temp = tempdir().unwrap();
        let source = source("golden-predecessor.jsonl");
        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        writer.begin_source(source.clone()).unwrap();
        for sequence in 1..=3 {
            writer
                .add_core_record(document(
                    &source,
                    sequence,
                    &format!("golden current republish evidence {sequence}"),
                ))
                .unwrap();
        }
        writer.certify_source(certificate(&source, 1, 3)).unwrap();
        let generation_id = writer
            .commit_with_publication_metadata(|_| true, |_| Ok(PUBLICATION_METADATA.to_vec()))
            .unwrap()
            .into_parts()
            .0
            .generation_id;
        Self {
            temp,
            source,
            generation_id,
        }
    }

    fn legacy_copy() -> Self {
        let temp = tempdir().unwrap();
        copy_fixture_tree(&fixture_root().join("index"), temp.path());
        Self {
            temp,
            source: source("golden-predecessor.jsonl"),
            generation_id: GOLDEN_GENERATION_ID.to_owned(),
        }
    }

    fn root(&self) -> &Path {
        self.temp.path()
    }

    fn generation_id(&self) -> &str {
        &self.generation_id
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

fn open_writer_error(root: &Path) -> IndexError {
    match open_republish_writer(root) {
        Ok(_) => panic!("generation writer unexpectedly opened"),
        Err(error) => error,
    }
}

enum RepublishWriterOpenOutcome {
    Ready(GenerationWriter),
    CommittedVisible {
        writer: GenerationWriter,
        recovery: RepublishRecovery,
    },
    CommittedRecoveryRequired {
        recovery: RepublishRecovery,
    },
}

impl RepublishWriterOpenOutcome {
    fn committed_republish_recovery(&self) -> Option<&RepublishRecovery> {
        match self {
            Self::CommittedVisible { recovery, .. }
            | Self::CommittedRecoveryRequired { recovery } => Some(recovery),
            Self::Ready(_) => None,
        }
    }

    fn into_writer(self) -> std::result::Result<GenerationWriter, RepublishRecovery> {
        match self {
            Self::Ready(writer) | Self::CommittedVisible { writer, .. } => Ok(writer),
            Self::CommittedRecoveryRequired { recovery } => Err(recovery),
        }
    }
}

fn open_republish_writer(root: &Path) -> Result<RepublishWriterOpenOutcome> {
    let lease = GenerationWriter::open(root, WriterOptions::default())?
        .into_writer()
        .map_err(|_| IndexError::WriterInvariant("unexpected committed recovery"))?;
    let pointer = load_active_generation_pointer(root)?.ok_or(IndexError::WriterInvariant(
        "current-format qualification requires an active generation",
    ))?;
    let outcome = republish_current_for_qualification(root, &pointer, &WriterOptions::default());
    drop(lease);
    match outcome? {
        CurrentRepublishOutcome::Published(_) => {
            let writer = GenerationWriter::open(root, WriterOptions::default())?
                .into_writer()
                .map_err(|_| IndexError::WriterInvariant("unexpected committed recovery"))?;
            Ok(RepublishWriterOpenOutcome::Ready(writer))
        }
        CurrentRepublishOutcome::CommittedVisible { recovery, .. } => {
            let writer = GenerationWriter::open(root, WriterOptions::default())?
                .into_writer()
                .map_err(|_| IndexError::WriterInvariant("unexpected committed recovery"))?;
            Ok(RepublishWriterOpenOutcome::CommittedVisible { writer, recovery })
        }
        CurrentRepublishOutcome::CommittedRecoveryRequired { recovery } => {
            Ok(RepublishWriterOpenOutcome::CommittedRecoveryRequired { recovery })
        }
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
        RETIRED_CORE_FINGERPRINT
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
        RETIRED_CORE_FINGERPRINT
    );
    assert_eq!(
        manifest.policy_schema_hash,
        RETIRED_SOURCE_GENERATION_POLICY_HASH
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
fn schema_17_fixture_is_rejected_then_rebuilt_only_from_source_authority() {
    let predecessor = GoldenPredecessor::legacy_copy();
    let pointer_bytes = fs::read(predecessor.root().join("active-generation.json")).unwrap();
    let open_error = |result| match result {
        Ok(_) => panic!("schema-17 generation unexpectedly opened"),
        Err(error) => error,
    };
    for error in [
        open_error(VerifiedIndex::open(predecessor.root())),
        open_error(VerifiedIndex::open_pinned(predecessor.root())),
        open_error(VerifiedIndex::open_pinned_generation(
            predecessor.root(),
            predecessor.generation_id(),
        )),
    ] {
        assert!(matches!(
            error,
            IndexError::GenerationContractMismatch { schema: 17, .. }
                | IndexError::SchemaMismatch(LEXICAL_SCHEMA_VERSION)
        ));
    }

    let _no_clone = RepublishTestHookGuard::set(|stage, _| {
        panic!("schema-17 rebuild entered clone republish at {stage:?}")
    });
    let mut writer = GenerationWriter::open(predecessor.root(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    assert!(writer.base_manifest().is_none());
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_bytes
    );

    writer.begin_source(predecessor.source.clone()).unwrap();
    for sequence in 1..=3 {
        writer
            .add_core_record(document(
                &predecessor.source,
                sequence,
                &format!("source authoritative schema 18 replacement {sequence}"),
            ))
            .unwrap();
    }
    writer
        .certify_source(certificate(&predecessor.source, 2, 3))
        .unwrap();
    let replacement = writer.commit(|_| true).unwrap();
    assert_ne!(replacement.generation_id, predecessor.generation_id());
    assert_ne!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_bytes
    );

    let current = VerifiedIndex::open(predecessor.root()).unwrap();
    assert_eq!(current.generation_id(), replacement.generation_id);
    assert_eq!(
        current.manifest().lexical_schema_version,
        LEXICAL_SCHEMA_VERSION
    );
    assert_eq!(current.document_count(), 3);
    assert_eq!(current.count_term("replacement").unwrap(), 3);
    assert!(load_active_generation_pointer(predecessor.root())
        .unwrap()
        .unwrap()
        .previous()
        .is_none());
}

#[test]
fn every_prepublication_republish_failure_keeps_the_base_pointer_and_queries() {
    let stages = [
        RepublishStage::AfterCandidateCreation,
        RepublishStage::BeforeCandidateCommit,
        RepublishStage::AfterCandidateCommit,
        RepublishStage::BeforeCandidateSync,
        RepublishStage::AfterCandidateSync,
        RepublishStage::BeforeCandidateVerification,
        RepublishStage::AfterCandidateVerification,
        RepublishStage::BeforePointerPublication,
    ];

    for fault_stage in stages {
        let predecessor = GoldenPredecessor::copy();
        let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
        let fault = RepublishTestHookGuard::set(move |stage, _| {
            if stage == fault_stage {
                return Err(std::io::Error::other(format!(
                    "injected predecessor republish fault at {stage:?}"
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
        let retry = open_republish_writer(predecessor.root())
            .unwrap()
            .into_writer()
            .unwrap();
        assert_eq!(
            retry
                .base_manifest()
                .unwrap()
                .core_record_contract_fingerprint,
            current_core_record_contract_fingerprint()
        );
        assert_eq!(reader.count_term("evidence").unwrap(), 3);
    }
}

#[test]
fn corrupt_republish_candidate_never_changes_or_damages_the_predecessor() {
    let predecessor = GoldenPredecessor::copy();
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
    let fault = RepublishTestHookGuard::set(|stage, path| {
        if stage == RepublishStage::BeforeCandidateVerification {
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
    let retry = open_republish_writer(predecessor.root())
        .unwrap()
        .into_writer()
        .unwrap();
    assert_eq!(
        retry
            .base_manifest()
            .unwrap()
            .core_record_contract_fingerprint,
        current_core_record_contract_fingerprint()
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
    assert_ne!(unknown, current_core_record_contract_fingerprint());
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

#[test]
fn schema_18_rejects_the_retired_predecessor_fingerprint_policy_pair() {
    let generation = GoldenPredecessor::copy();
    let pointer = load_active_generation_pointer(generation.root())
        .unwrap()
        .unwrap();
    let index = open_slot_index(generation.root(), pointer.active()).unwrap();
    let metas = index.load_metas().unwrap();
    let mut manifest = load_publication_for_metas(generation.root(), &metas)
        .unwrap()
        .manifest;
    manifest.core_record_contract_fingerprint = RETIRED_CORE_FINGERPRINT.to_owned();
    manifest.policy_schema_hash = RETIRED_SOURCE_GENERATION_POLICY_HASH.to_owned();
    publish_unchecked_generation(generation.root(), &index, manifest, &[], Vec::new());

    let pointer_before = fs::read(generation.root().join("active-generation.json")).unwrap();
    for error in [
        VerifiedIndex::active_generation_id(generation.root()).unwrap_err(),
        match VerifiedIndex::open(generation.root()) {
            Ok(_) => panic!("retired predecessor identity unexpectedly opened"),
            Err(error) => error,
        },
        match VerifiedIndex::open_pinned(generation.root()) {
            Ok(_) => panic!("retired predecessor identity unexpectedly opened pinned"),
            Err(error) => error,
        },
        open_writer_error(generation.root()),
    ] {
        assert!(matches!(
            error,
            IndexError::CoreRecordContractMismatch { ref actual, .. }
                if actual == RETIRED_CORE_FINGERPRINT
        ));
    }
    assert_eq!(
        fs::read(generation.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
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
        IndexError::CurrentRepublishSourceTopology("unexpected directory entry")
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
            | IndexError::CurrentRepublishSourceTopology(
                "symlinked or non-directory republish source"
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
            | IndexError::CurrentRepublishSourceTopology(
                "symlinked or non-directory republish source"
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
        IndexError::CurrentRepublishSourceTopology(
            "active generation directory changed during republish"
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
        IndexError::CurrentRepublishSourceTopology("managed path escapes generation directory")
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
            IndexError::CurrentRepublishByteLimit {
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

fn fill_generation_past_republish_entry_cap(generation: &Path) {
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
    fill_generation_past_republish_entry_cap(&active_generation_path(predecessor.root()));
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();

    assert!(matches!(
        open_writer_error(predecessor.root()),
        IndexError::CurrentRepublishFileLimit {
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

    let writer = open_republish_writer(predecessor.root())
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
fn forced_copy_write_failures_preserve_base_pointer_and_queries() {
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
        IndexError::CurrentRepublishSourceTopology("source file grew while cloning")
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
        RepublishStage::AfterCandidateCreation,
        RepublishStage::BeforeCandidateCommit,
        RepublishStage::BeforeCandidateSync,
        RepublishStage::BeforeCandidateVerification,
        RepublishStage::BeforePointerPublication,
    ] {
        let predecessor = GoldenPredecessor::copy();
        let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
        let replacement = std::rc::Rc::new(std::cell::RefCell::new(None::<(PathBuf, PathBuf)>));
        let replacement_for_hook = std::rc::Rc::clone(&replacement);
        let mut replaced = false;
        let hook = RepublishTestHookGuard::set(move |stage, path| {
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
            IndexError::CurrentRepublishSourceTopology(
                "active generation directory changed during republish"
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
        IndexError::CurrentRepublishInsufficientHeadroom {
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

    let outcome = open_republish_writer(predecessor.root()).unwrap();
    let recovery = outcome
        .committed_republish_recovery()
        .expect("visible pointer durability uncertainty must be reported as committed")
        .clone();
    let writer = outcome.into_writer().unwrap();
    assert_eq!(
        recovery.generation_id(),
        writer.base_manifest().unwrap().generation_id().unwrap()
    );
    assert_eq!(recovery.generation_id(), predecessor.generation_id());
    drop(writer);
    drop(fault);

    let current = VerifiedIndex::open(predecessor.root()).unwrap();
    assert_eq!(current.count_term("evidence").unwrap(), 3);
    let restarted = GenerationWriter::open(predecessor.root(), WriterOptions::default()).unwrap();
    assert!(matches!(restarted, GenerationWriterOpenOutcome::Ready(_)));
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

    let outcome = open_republish_writer(predecessor.root())
        .expect("visible republish must not be reported as an ordinary error");
    assert!(matches!(
        &outcome,
        RepublishWriterOpenOutcome::CommittedVisible { .. }
    ));
    drop(outcome.into_writer().unwrap());
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
            "injected unreadable replacement pointer",
        )
        .into())
    });

    let outcome = open_republish_writer(predecessor.root())
        .expect("postvisibility unknown state must be a committed outcome");
    let recovery = match outcome {
        RepublishWriterOpenOutcome::CommittedRecoveryRequired { recovery } => recovery,
        _ => panic!("failed pointer repair unexpectedly produced a usable writer"),
    };
    assert_eq!(recovery.generation_id(), predecessor.generation_id());
    assert!(recovery.detail().contains("pointer reload failed"));
    assert!(recovery.detail().contains("pointer repair failed"));
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
    drop(fault);
}

#[test]
fn post_publication_cleanup_failure_is_best_effort_and_restart_keeps_current_authority() {
    let predecessor = GoldenPredecessor::copy();
    let fault = RepublishTestHookGuard::set(|stage, _| {
        if stage == RepublishStage::PostPublicationCleanup {
            return Err(io::Error::other("injected post-publication cleanup failure").into());
        }
        Ok(())
    });

    let outcome = open_republish_writer(predecessor.root()).unwrap();
    assert!(outcome.committed_republish_recovery().is_none());
    let writer = outcome.into_writer().unwrap();
    drop(writer);
    drop(fault);
    let current_id = VerifiedIndex::active_generation_id(predecessor.root())
        .unwrap()
        .unwrap();
    assert_eq!(current_id, predecessor.generation_id());
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
        root.join("republish-child.marker"),
        root.join("republish-child.continue"),
        root.join("republish-child.result"),
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
fn predecessor_republish_subprocess_worker() {
    let Ok(mode) = env::var(SUBPROCESS_MODE_ENV) else {
        return;
    };
    let root = PathBuf::from(env::var_os(SUBPROCESS_ROOT_ENV).unwrap());
    let marker = PathBuf::from(env::var_os(SUBPROCESS_MARKER_ENV).unwrap());
    let continue_path = PathBuf::from(env::var_os(SUBPROCESS_CONTINUE_ENV).unwrap());
    let result = PathBuf::from(env::var_os(SUBPROCESS_RESULT_ENV).unwrap());

    let mut republish_guard = None;
    let mut atomic_guard = None;
    if let Some(stage_name) = mode.strip_prefix("pause-republish:") {
        let stage_name = stage_name.to_owned();
        let marker = marker.clone();
        let continue_path = continue_path.clone();
        republish_guard = Some(RepublishTestHookGuard::set(move |stage, _| {
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
        panic!("unknown predecessor republish child mode {mode}");
    }

    let detail = match open_republish_writer(&root) {
        Ok(outcome) => {
            let recovered = outcome.committed_republish_recovery().is_some();
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
    drop(republish_guard);
}

fn spawn_republish_subprocess(root: &Path, mode: &str) -> Child {
    let (marker, continue_path, result) = subprocess_paths(root);
    for path in [&marker, &continue_path, &result] {
        let _ = fs::remove_file(path);
    }
    Command::new(env::current_exe().unwrap())
        .arg("--exact")
        .arg("tests::republish::predecessor_republish_subprocess_worker")
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
            panic!("republish child exited before checkpoint: {status}");
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for republish child checkpoint");
}

fn kill_republish_subprocess(child: &mut Child) {
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
}

#[test]
fn subprocess_process_death_around_commit_sync_and_pointer_rename_recovers_correct_authority() {
    for (mode, successor_visible) in [
        ("pause-republish:AfterCandidateCommit", false),
        ("pause-republish:AfterCandidateSync", false),
        ("pause-after-pointer-temp-sync", false),
        ("pause-after-pointer-replace", true),
    ] {
        let predecessor = GoldenPredecessor::copy();
        let held_reader = VerifiedIndex::open(predecessor.root()).unwrap();
        let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
        let (marker, _, _) = subprocess_paths(predecessor.root());
        let mut child = spawn_republish_subprocess(predecessor.root(), mode);
        wait_for_subprocess_marker(&mut child, &marker);
        kill_republish_subprocess(&mut child);

        let pointer_after = fs::read(predecessor.root().join("active-generation.json")).unwrap();
        assert_eq!(pointer_after != pointer_before, successor_visible, "{mode}");
        assert_eq!(held_reader.count_term("evidence").unwrap(), 3);
        VerifiedIndex::open(predecessor.root()).unwrap();

        drop(
            open_republish_writer(predecessor.root())
                .unwrap()
                .into_writer()
                .unwrap(),
        );
        let after_restart = VerifiedIndex::open(predecessor.root()).unwrap();
        assert_eq!(after_restart.count_term("evidence").unwrap(), 3);
    }
}

#[test]
fn subprocess_pointer_enospc_is_prepublication_failure_and_retry_migrates() {
    let predecessor = GoldenPredecessor::copy();
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
    let mut child = spawn_republish_subprocess(predecessor.root(), "fail-pointer-write-enospc");
    assert!(child.wait().unwrap().success());
    let (_, _, result) = subprocess_paths(predecessor.root());
    assert!(fs::read_to_string(result).unwrap().starts_with("ERROR"));
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
    drop(
        open_republish_writer(predecessor.root())
            .unwrap()
            .into_writer()
            .unwrap(),
    );
}

#[test]
fn subprocess_post_rename_fsync_failure_is_committed_and_restart_reads_successor() {
    let predecessor = GoldenPredecessor::copy();
    let mut child = spawn_republish_subprocess(predecessor.root(), "fail-pointer-directory-sync");
    assert!(child.wait().unwrap().success());
    let (_, _, result) = subprocess_paths(predecessor.root());
    let result = fs::read_to_string(result).unwrap();
    assert!(result.starts_with("COMMITTED "), "{result}");
    assert!(result.ends_with(" true"), "{result}");
    let current = VerifiedIndex::open(predecessor.root()).unwrap();
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
