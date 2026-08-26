use super::*;
use std::{
    collections::BTreeSet,
    env, io,
    process::{Child, Command, Stdio},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use crate::publication::{
    republish_current_for_qualification, CurrentRepublishOutcome,
    PointerReconciliationTestHookGuard, PortableCloneMetrics, PortableCloneStage,
    PortableCloneTestGuard, PortableCloneTestOptions, RepublishRecovery, RepublishStage,
    RepublishTestHookGuard,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::publication::{CloneMetrics, CloneStage, CloneTestHookGuard, CloneTestOptions};
use ctx_history_index_generation::{AtomicWriteStage, AtomicWriteTestHookGuard};

const PUBLICATION_METADATA: &[u8] = b"source-catalog-frontier-receipt-v1";
const GOLDEN_GENERATION_ID: &str =
    "a71ac367a8192609dc5b739e8f68e83124ee369e7cb3975a88e873eafe9f0283";
const RETIRED_CORE_FINGERPRINT: &str =
    "7552eee7cae0695a98f202b02f52cbf5680845cb7bacea4ed754e283bc15f051";
const RETIRED_SOURCE_GENERATION_POLICY_HASH: &str =
    "e728b5d7b76d04248e9dccc91fc11d915fcbcd714b445090725ba0604b8e8b37";
const PREDECESSOR_FIXTURE_REPOSITORY_ROOT: &str = "crates/ctx-history-index/testdata/pred";
// crate_universe runs Cargo with its Git checkout nested beneath the Bazel
// module-extension work directory. A supported short-root Windows build still
// spends 103 characters before the dependency's repository-relative path.
// Retain an explicit buffer for checkout implementations that use MAX_PATH.
const WINDOWS_MAX_PATH_CHARS: usize = 260;
const WINDOWS_CARGO_GIT_CHECKOUT_PREFIX_CHARS: usize = 103;
const WINDOWS_CARGO_GIT_CHECKOUT_MARGIN_CHARS: usize = 8;
const WINDOWS_CARGO_REPOSITORY_PATH_LIMIT: usize = WINDOWS_MAX_PATH_CHARS
    - WINDOWS_CARGO_GIT_CHECKOUT_PREFIX_CHARS
    - WINDOWS_CARGO_GIT_CHECKOUT_MARGIN_CHARS;
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
        .join("pred")
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
    // This fixture is intentionally a retired predecessor manifest. Inspect
    // its frozen provenance as inert JSON rather than asking the current
    // manifest type to backfill fields introduced after the fixture shipped.
    let manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(
            root.join("index")
                .join("ctx-generations")
                .join(format!("{GOLDEN_GENERATION_ID}.json")),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        manifest["core_record_contract_fingerprint"],
        RETIRED_CORE_FINGERPRINT
    );
    assert_eq!(
        manifest["policy_schema_hash"],
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
fn checked_in_predecessor_fixture_fits_windows_cargo_git_checkout_budget() {
    let (length, path) = fixture_file_paths(&fixture_root())
        .into_iter()
        .map(|relative| {
            let path = format!("{PREDECESSOR_FIXTURE_REPOSITORY_ROOT}/{relative}");
            (path.len(), path)
        })
        .max_by_key(|(length, _)| *length)
        .unwrap();

    assert!(
        length <= WINDOWS_CARGO_REPOSITORY_PATH_LIMIT,
        "predecessor fixture path exceeds the Windows Cargo Git checkout budget: \
         {length} > {WINDOWS_CARGO_REPOSITORY_PATH_LIMIT}: {path}"
    );
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
fn metadata_only_republish_keeps_stored_core_replay_explicit() {
    let predecessor = GoldenPredecessor::copy();
    let writer = GenerationWriter::open(predecessor.root(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();

    crate::publication::reset_verification_activity();
    ctx_history_index_query::reset_verified_index_reopen_count();
    let verified = writer
        .republish_current_publication_metadata(
            predecessor.generation_id(),
            b"replacement-owner-metadata".to_vec(),
        )
        .unwrap();

    assert_eq!(verified.generation_id(), predecessor.generation_id());
    assert_eq!(
        verified.publication_metadata(),
        Some(b"replacement-owner-metadata".as_slice())
    );
    assert_eq!(
        crate::publication::verification_activity(),
        (3, 0),
        "base, authenticated clone, and committed candidate remain physically checked without a stored-Core pass"
    );
    assert_eq!(
        crate::publication::candidate_identity_verification_activity(),
        (0, 0)
    );
    assert_eq!(
        crate::publication::candidate_projection_verification_activity(),
        0
    );
    assert_eq!(
        crate::publication::candidate_lineage_verification_activity(),
        (0, 0)
    );
    assert_eq!(
        ctx_history_index_query::verified_index_reopen_count(),
        2,
        "republish verifies the inactive candidate and independently returns the activated generation"
    );

    let (searcher, _) = open_unverified_generation(predecessor.root());
    let explicit_scrub =
        crate::publication::verify_searcher_with_metrics(&searcher, verified.manifest(), 1, false)
            .unwrap();
    assert_eq!(
        explicit_scrub.document_decodes, 3,
        "the explicit deep diagnostic remains the only path that decodes stored Core"
    );
}

#[test]
fn metadata_only_republish_rejects_candidate_payload_rebinding_without_logical_replay() {
    let predecessor = GoldenPredecessor::copy();
    let generation_id = predecessor.generation_id().to_owned();
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
    let fault = RepublishTestHookGuard::set(move |stage, path| {
        if stage == RepublishStage::BeforeCandidateVerification {
            let meta_path = path.unwrap().join("meta.json");
            let mut meta: serde_json::Value = serde_json::from_slice(&fs::read(&meta_path)?)?;
            meta["payload"] = serde_json::Value::String(canonical_commit_payload(
                &generation_id,
                Some(b"unauthenticated-owner-metadata"),
            )?);
            fs::write(meta_path, serde_json::to_vec(&meta)?)?;
        }
        Ok(())
    });

    crate::publication::reset_verification_activity();
    assert!(matches!(
        open_writer_error(predecessor.root()),
        IndexError::ConcurrentGenerationChange
    ));
    assert_eq!(crate::publication::verification_activity().1, 0);
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
    drop(fault);
}

#[test]
fn metadata_only_republish_rejects_candidate_physical_corruption_without_logical_replay() {
    let predecessor = GoldenPredecessor::copy();
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
    let fault = RepublishTestHookGuard::set(|stage, path| {
        if stage == RepublishStage::BeforeCandidateVerification {
            let generation_path = path.unwrap();
            let segment = fs::read_dir(generation_path)?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<io::Result<Vec<_>>>()?
                .into_iter()
                .find(|path| {
                    path.extension().and_then(|extension| extension.to_str()) == Some("store")
                })
                .ok_or_else(|| io::Error::other("candidate lacks a store segment"))?;
            let mut bytes = fs::read(&segment)?;
            let offset = bytes
                .len()
                .checked_div(2)
                .filter(|offset| *offset < bytes.len())
                .ok_or_else(|| io::Error::other("candidate store segment is empty"))?;
            bytes[offset] ^= 0x80;
            let replacement = generation_path.join("corrupt-segment-replacement");
            fs::write(&replacement, bytes)?;
            fs::remove_file(&segment)?;
            fs::rename(replacement, segment)?;
        }
        Ok(())
    });

    crate::publication::reset_verification_activity();
    assert!(matches!(
        open_writer_error(predecessor.root()),
        IndexError::ChecksumMismatch
    ));
    assert_eq!(crate::publication::verification_activity().1, 0);
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
    drop(fault);
}

#[test]
fn unknown_core_fingerprint_fails_all_reads_and_never_starts_source_rebuild() {
    let predecessor = GoldenPredecessor::copy();
    let pointer = load_active_generation_pointer(predecessor.root())
        .unwrap()
        .unwrap();
    let index = open_slot_index(predecessor.root(), pointer.active()).unwrap();
    let metas = index.load_metas().unwrap();
    let manifest = load_publication_for_metas(predecessor.root(), &metas)
        .unwrap()
        .into_parts()
        .1;
    let mut manifest = Arc::unwrap_or_clone(manifest);
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
    let manifest = load_publication_for_metas(generation.root(), &metas)
        .unwrap()
        .into_parts()
        .1;
    let mut manifest = Arc::unwrap_or_clone(manifest);
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

    crate::publication::reset_verification_activity();
    assert!(matches!(
        open_writer_error(predecessor.root()),
        IndexError::CurrentRepublishSourceTopology("unexpected directory entry")
    ));
    assert_eq!(crate::publication::verification_activity().1, 0);
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

#[test]
fn arbitrary_managed_extra_is_not_treated_as_a_retired_segment() {
    let predecessor = GoldenPredecessor::copy();
    let generation = active_generation_path(predecessor.root());
    let arbitrary = PathBuf::from("operator-note.txt");
    fs::write(
        generation.join(&arbitrary),
        b"not a Tantivy segment component",
    )
    .unwrap();
    let managed_path = generation.join(".managed.json");
    let mut managed =
        serde_json::from_slice::<Vec<PathBuf>>(&fs::read(&managed_path).unwrap()).unwrap();
    managed.push(arbitrary);
    fs::write(&managed_path, serde_json::to_vec(&managed).unwrap()).unwrap();
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();

    assert!(matches!(
        open_writer_error(predecessor.root()),
        IndexError::CurrentRepublishSourceTopology(
            "managed metadata is not a safe active superset"
        )
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

mod additional;
