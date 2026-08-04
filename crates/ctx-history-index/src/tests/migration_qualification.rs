//! Executable same-epoch migration qualification.
//!
//! Hermetic disk gate:
//! `bazel test //crates/ctx-history-index:migration_disk_qualification_tests --test_output=streamed`.
//! Controlled release A/B (only on an idle, fixed host/filesystem):
//! `bazel test //crates/ctx-history-index:migration_release_benchmark --config=release --test_output=streamed --nocache_test_results --test_env=CTX_MIGRATION_QUALIFICATION_ENFORCE_PERF=1`.
//! The release command blocks above 5% only after proving byte-identical output;
//! disk always uses the absolute predecessor-generation denominator `F`.

use super::*;

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashMap},
    env,
    fs::{self, File},
    io::{BufReader, Read, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    rc::Rc,
    time::Instant,
};

use crate::{
    core_contract::TestCoreFingerprintOverride,
    durable_directory::{AtomicWriteStage, AtomicWriteTestHookGuard},
    publication::{CloneTestHookGuard, CloneTestOptions, MigrationStage, MigrationTestHookGuard},
};

mod process;
mod topology;

use process::wait4_operation;
use topology::{first_payload_pair, verify_clone_topology, CloneTopologyProof};

const SUCCESSOR_CORE_FINGERPRINT: &str =
    "bc73c991e160746fbaaddb641fdce8c7bec24e5ba212a406ec26d197cf0c6a5e";
const QUALIFICATION_CASE_ENV: &str = "CTX_MIGRATION_QUALIFICATION_CASE";
const QUALIFICATION_ROOT_ENV: &str = "CTX_MIGRATION_QUALIFICATION_ROOT";
const QUALIFICATION_OUTPUT_ENV: &str = "CTX_MIGRATION_QUALIFICATION_OUTPUT";
const QUALIFICATION_REPORT_ENV: &str = "CTX_MIGRATION_QUALIFICATION_REPORT";
const QUALIFICATION_CLONE_MODE_ENV: &str = "CTX_MIGRATION_QUALIFICATION_CLONE_MODE";
const ENFORCE_PERF_ENV: &str = "CTX_MIGRATION_QUALIFICATION_ENFORCE_PERF";
const RELEASE_DOCUMENTS_ENV: &str = "CTX_MIGRATION_QUALIFICATION_DOCUMENTS";
const RELEASE_BODY_BYTES_ENV: &str = "CTX_MIGRATION_QUALIFICATION_BODY_BYTES";
const RELEASE_SAMPLES_ENV: &str = "CTX_MIGRATION_QUALIFICATION_SAMPLES";
const RELEASE_DEFAULT_DOCUMENTS: usize = 16_384;
const RELEASE_DEFAULT_BODY_BYTES: usize = 4_096;
const EXPECTED_AMPLIFICATION_HUNDREDTHS: u128 = 367;
const BLOCKING_AMPLIFICATION_MULTIPLIER: u128 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QualificationCase {
    Migration,
    CurrentRepublish,
}

impl QualificationCase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Migration => "migration",
            Self::CurrentRepublish => "current_republish",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "migration" => Some(Self::Migration),
            "current_republish" => Some(Self::CurrentRepublish),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CloneMode {
    HardLink,
    CopyFallback,
}

impl CloneMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::HardLink => "hard_link",
            Self::CopyFallback => "copy_fallback",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "hard_link" => Some(Self::HardLink),
            "copy_fallback" => Some(Self::CopyFallback),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct CorpusSpec {
    documents: u64,
    body_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DenominatorScope {
    PredecessorGeneration,
    WholeRoot,
}

#[derive(Debug, Clone)]
struct DiskGateInput {
    denominator_scope: DenominatorScope,
    predecessor_f_bytes: u64,
    declared_predecessor_f_bytes: u64,
    peak_allocated_bytes: u64,
    accounted_allocated_bytes: u64,
    unexplained_paths: Vec<String>,
}

#[derive(Debug, Clone)]
struct DiskGateResult {
    multiplier: f64,
    within_expected_envelope: bool,
}

fn validate_disk_gate(input: &DiskGateInput) -> std::result::Result<DiskGateResult, String> {
    if input.denominator_scope != DenominatorScope::PredecessorGeneration {
        return Err("disk denominator must be the predecessor generation F".to_owned());
    }
    if input.predecessor_f_bytes == 0
        || input.declared_predecessor_f_bytes != input.predecessor_f_bytes
    {
        return Err("declared disk denominator does not equal predecessor generation F".to_owned());
    }
    if !input.unexplained_paths.is_empty() {
        return Err(format!(
            "unexplained migration paths: {}",
            input.unexplained_paths.join(", ")
        ));
    }
    if input.accounted_allocated_bytes != input.peak_allocated_bytes {
        return Err(format!(
            "unexplained migration bytes: peak={} accounted={}",
            input.peak_allocated_bytes, input.accounted_allocated_bytes
        ));
    }
    let peak = u128::from(input.peak_allocated_bytes);
    let predecessor = u128::from(input.predecessor_f_bytes);
    if peak > predecessor.saturating_mul(BLOCKING_AMPLIFICATION_MULTIPLIER) {
        return Err(format!(
            "migration disk amplification exceeds blocking >5F limit: {peak}/{predecessor}"
        ));
    }
    Ok(DiskGateResult {
        multiplier: input.peak_allocated_bytes as f64 / input.predecessor_f_bytes as f64,
        within_expected_envelope: peak.saturating_mul(100)
            <= predecessor.saturating_mul(EXPECTED_AMPLIFICATION_HUNDREDTHS),
    })
}

#[derive(Debug, Clone)]
struct OutputIdentity {
    sha256: String,
    declared_bytes: u64,
    actual_bytes: u64,
}

#[derive(Debug, Clone)]
struct PerformanceSample {
    output: OutputIdentity,
    wall_seconds: f64,
    cpu_seconds: f64,
    peak_rss_bytes: u64,
}

fn validate_output_identity(output: &OutputIdentity) -> std::result::Result<(), String> {
    if output.declared_bytes != output.actual_bytes {
        return Err(format!(
            "declared total output bytes {} do not equal bytes written {}",
            output.declared_bytes, output.actual_bytes
        ));
    }
    Ok(())
}

fn comparable_regressions(
    current: &PerformanceSample,
    migration: &PerformanceSample,
) -> std::result::Result<BTreeMap<&'static str, f64>, String> {
    validate_output_identity(&current.output)?;
    validate_output_identity(&migration.output)?;
    if current.output.sha256 != migration.output.sha256
        || current.output.actual_bytes != migration.output.actual_bytes
    {
        return Err(
            "the 5% performance gate requires byte-identical current/migration payloads".to_owned(),
        );
    }
    let mut regressions = BTreeMap::new();
    for (name, baseline, candidate) in [
        ("wall", current.wall_seconds, migration.wall_seconds),
        ("cpu", current.cpu_seconds, migration.cpu_seconds),
        (
            "peak_rss",
            current.peak_rss_bytes as f64,
            migration.peak_rss_bytes as f64,
        ),
    ] {
        if !baseline.is_finite() || !candidate.is_finite() || baseline <= 0.0 || candidate < 0.0 {
            return Err(format!("invalid {name} measurement"));
        }
        regressions.insert(name, candidate / baseline - 1.0);
    }
    Ok(regressions)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum AllocationCategory {
    PredecessorGeneration,
    CandidateGeneration,
    SharedGeneration,
    Manifest,
    Pointer,
    Certification,
    Temporary,
    Control,
}

impl AllocationCategory {
    fn as_str(&self) -> &'static str {
        match self {
            Self::PredecessorGeneration => "predecessor_generation",
            Self::CandidateGeneration => "candidate_generation",
            Self::SharedGeneration => "shared_generation",
            Self::Manifest => "manifests",
            Self::Pointer => "pointer",
            Self::Certification => "integrity_certifications",
            Self::Temporary => "temporary",
            Self::Control => "control",
        }
    }
}

#[derive(Debug)]
struct AccountedPath {
    inode: (u64, u64),
    allocated_bytes: u64,
    logical_bytes: u64,
    category: AllocationCategory,
    is_file: bool,
}

#[derive(Debug, Clone)]
struct FilesystemSnapshot {
    allocated_bytes: u64,
    accounted_allocated_bytes: u64,
    file_count: u64,
    allocated_by_category: BTreeMap<String, u64>,
    logical_by_category: BTreeMap<String, u64>,
}

fn is_atomic_temporary(name: &str) -> bool {
    name.starts_with(".ctx-tantivy-atomic-") && name.ends_with(".tmp")
}

fn valid_generation_file(name: &str) -> bool {
    matches!(
        name,
        ".managed.json" | "meta.json" | ".tantivy-meta.lock" | ".tantivy-writer.lock"
    ) || ["fast", "fieldnorm", "idx", "pos", "store", "term"]
        .iter()
        .any(|extension| name.ends_with(&format!(".{extension}")))
        || is_atomic_temporary(name)
}

fn valid_certification_file(name: &str) -> bool {
    name.strip_suffix(".physical-certification.json")
        .and_then(|name| name.strip_prefix("generation-"))
        .is_some_and(|suffix| {
            suffix.len() == 32
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

fn classify_path(
    relative: &Path,
    is_file: bool,
    predecessor_directory: &str,
) -> std::result::Result<AllocationCategory, String> {
    let components = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    match components.as_slice() {
        [] if !is_file => Ok(AllocationCategory::Control),
        [name] if is_file && name == "active-generation.json" => Ok(AllocationCategory::Pointer),
        [name] if is_file && name == ".ctx-generation-writer.lock" => {
            Ok(AllocationCategory::Control)
        }
        [name] if is_file && is_atomic_temporary(name) => Ok(AllocationCategory::Temporary),
        [name] if !is_file && matches!(name.as_str(), "ctx-generations" | "index-generations") => {
            Ok(AllocationCategory::Control)
        }
        [name] if !is_file && name == "integrity-certifications" => {
            Ok(AllocationCategory::Certification)
        }
        [directory, name]
            if directory == "integrity-certifications" && is_file && is_atomic_temporary(name) =>
        {
            Ok(AllocationCategory::Temporary)
        }
        [directory, name]
            if directory == "integrity-certifications"
                && is_file
                && valid_certification_file(name) =>
        {
            Ok(AllocationCategory::Certification)
        }
        [directory, name]
            if directory == "ctx-generations"
                && is_file
                && is_generation_id(name.strip_suffix(".json").unwrap_or_default()) =>
        {
            Ok(AllocationCategory::Manifest)
        }
        [directory, name]
            if directory == "ctx-generations" && is_file && is_atomic_temporary(name) =>
        {
            Ok(AllocationCategory::Temporary)
        }
        [directory, generation]
            if directory == "index-generations"
                && !is_file
                && generation.starts_with("generation-") =>
        {
            if generation == predecessor_directory {
                Ok(AllocationCategory::PredecessorGeneration)
            } else {
                Ok(AllocationCategory::CandidateGeneration)
            }
        }
        [directory, generation, name]
            if directory == "index-generations"
                && is_file
                && generation.starts_with("generation-")
                && valid_generation_file(name) =>
        {
            if is_atomic_temporary(name) {
                Ok(AllocationCategory::Temporary)
            } else if generation == predecessor_directory {
                Ok(AllocationCategory::PredecessorGeneration)
            } else {
                Ok(AllocationCategory::CandidateGeneration)
            }
        }
        _ => Err(format!(
            "unaccounted filesystem path {}",
            relative.display()
        )),
    }
}

fn filesystem_snapshot(
    root: &Path,
    predecessor_directory: &str,
) -> std::result::Result<FilesystemSnapshot, String> {
    fn visit(
        root: &Path,
        path: &Path,
        predecessor_directory: &str,
        paths: &mut Vec<AccountedPath>,
    ) -> std::result::Result<(), String> {
        let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() || (!metadata.is_file() && !metadata.is_dir()) {
            return Err(format!("unaccounted non-regular path {}", path.display()));
        }
        let relative = path.strip_prefix(root).map_err(|error| error.to_string())?;
        let category = classify_path(relative, metadata.is_file(), predecessor_directory)?;
        paths.push(AccountedPath {
            inode: (metadata.dev(), metadata.ino()),
            allocated_bytes: metadata.blocks().saturating_mul(512),
            logical_bytes: metadata.len(),
            category,
            is_file: metadata.is_file(),
        });
        if metadata.is_dir() {
            let mut entries = fs::read_dir(path)
                .map_err(|error| error.to_string())?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?;
            entries.sort_by_key(std::fs::DirEntry::file_name);
            for entry in entries {
                visit(root, &entry.path(), predecessor_directory, paths)?;
            }
        }
        Ok(())
    }

    let mut paths = Vec::new();
    visit(root, root, predecessor_directory, &mut paths)?;
    let mut inode_paths: HashMap<(u64, u64), Vec<&AccountedPath>> = HashMap::new();
    let mut logical_by_category = BTreeMap::new();
    let mut file_count = 0_u64;
    for path in &paths {
        if path.is_file {
            file_count = file_count.saturating_add(1);
        }
        *logical_by_category
            .entry(path.category.as_str().to_owned())
            .or_insert(0) += path.logical_bytes;
        inode_paths.entry(path.inode).or_default().push(path);
    }
    let mut allocated_by_category = BTreeMap::new();
    for aliases in inode_paths.values() {
        let allocated = aliases[0].allocated_bytes;
        if aliases
            .iter()
            .any(|alias| alias.allocated_bytes != allocated)
        {
            return Err("one inode reported inconsistent allocated bytes".to_owned());
        }
        let categories = aliases
            .iter()
            .map(|alias| alias.category.clone())
            .collect::<BTreeSet<_>>();
        let category = if categories.contains(&AllocationCategory::PredecessorGeneration)
            && categories.contains(&AllocationCategory::CandidateGeneration)
        {
            AllocationCategory::SharedGeneration
        } else if categories.len() == 1 {
            categories.iter().next().cloned().unwrap()
        } else {
            return Err(format!(
                "inode crosses unexplained categories: {categories:?}"
            ));
        };
        *allocated_by_category
            .entry(category.as_str().to_owned())
            .or_insert(0) += allocated;
    }
    let allocated_bytes = inode_paths
        .values()
        .map(|aliases| aliases[0].allocated_bytes)
        .sum();
    let accounted_allocated_bytes = allocated_by_category.values().sum();
    Ok(FilesystemSnapshot {
        allocated_bytes,
        accounted_allocated_bytes,
        file_count,
        allocated_by_category,
        logical_by_category,
    })
}

#[derive(Debug)]
struct DiskTracker {
    root: PathBuf,
    predecessor_directory: String,
    predecessor_f_bytes: u64,
    peak: FilesystemSnapshot,
    observations: u64,
    first_error: Option<String>,
}

impl DiskTracker {
    fn new(root: &Path) -> std::result::Result<Self, String> {
        let pointer = load_active_generation_pointer(root)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "qualification root has no active generation".to_owned())?;
        let predecessor_directory = pointer.active().directory().to_owned();
        let initial = filesystem_snapshot(root, &predecessor_directory)?;
        let predecessor_f_bytes = initial
            .allocated_by_category
            .get(AllocationCategory::PredecessorGeneration.as_str())
            .copied()
            .unwrap_or_default();
        if predecessor_f_bytes == 0 {
            return Err("filesystem reported zero allocated predecessor bytes".to_owned());
        }
        Ok(Self {
            root: root.to_path_buf(),
            predecessor_directory,
            predecessor_f_bytes,
            peak: initial,
            observations: 1,
            first_error: None,
        })
    }

    fn sample(&mut self) {
        if self.first_error.is_some() {
            return;
        }
        match filesystem_snapshot(&self.root, &self.predecessor_directory) {
            Ok(snapshot) => {
                self.observations = self.observations.saturating_add(1);
                if snapshot.allocated_bytes > self.peak.allocated_bytes {
                    self.peak = snapshot;
                }
            }
            Err(error) => self.first_error = Some(error),
        }
    }

    fn gate(&self) -> std::result::Result<DiskGateResult, String> {
        validate_disk_gate(&DiskGateInput {
            denominator_scope: DenominatorScope::PredecessorGeneration,
            predecessor_f_bytes: self.predecessor_f_bytes,
            declared_predecessor_f_bytes: self.predecessor_f_bytes,
            peak_allocated_bytes: self.peak.allocated_bytes,
            accounted_allocated_bytes: self.peak.accounted_allocated_bytes,
            unexplained_paths: self.first_error.iter().cloned().collect(),
        })
    }
}

fn deterministic_body(bytes: usize, sequence: u64) -> String {
    let prefix = "same-epoch migration qualification ";
    assert!(bytes >= prefix.len());
    let mut body = String::with_capacity(bytes);
    body.push_str(prefix);
    let mut state = sequence ^ 0x9e37_79b9_7f4a_7c15;
    const ALPHABET: &[u8; 64] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz-_";
    while body.len() < bytes {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        body.push(char::from(ALPHABET[(state & 63) as usize]));
    }
    assert_eq!(body.len(), bytes);
    body
}

fn relabel_active_manifest(root: &Path, mutate: impl FnOnce(&mut GenerationManifest)) {
    let pointer = load_active_generation_pointer(root).unwrap().unwrap();
    let index = open_slot_index(root, pointer.active()).unwrap();
    let metas = index.load_metas().unwrap();
    let publication = load_publication_for_metas(root, &metas).unwrap();
    let prior_generation_id = publication.generation_id;
    let mut manifest = publication.manifest;
    mutate(&mut manifest);
    manifest.validate_contract().unwrap();
    let generation_id = manifest.generation_id().unwrap();
    assert_ne!(generation_id, prior_generation_id);
    write_manifest(root, &generation_id, &manifest).unwrap();
    let payload =
        canonical_commit_payload(&generation_id, publication.metadata.as_deref()).unwrap();
    let mut writer = index
        .writer_with_num_threads::<TantivyDocument>(1, INDEX_MEMORY_MIN_PER_THREAD)
        .unwrap();
    writer.set_merge_policy(Box::<NoMergePolicy>::default());
    let mut prepared = writer.prepare_commit().unwrap();
    prepared.set_payload(&payload);
    prepared.commit().unwrap();
    writer.wait_merging_threads().unwrap();
    let generation_path = active_generation_path(root);
    sync_generation(&generation_path).unwrap();
    let slot = GenerationSlot::new(
        generation_id,
        pointer.active().directory().to_owned(),
        physical_integrity_digest(&index, &generation_path).unwrap(),
    )
    .unwrap();
    publish_active_generation_pointer(root, &ActiveGenerationPointer::new(slot, None).unwrap())
        .unwrap();
    fs::remove_file(manifest_path(root, &prior_generation_id)).unwrap();
    sync_directory(manifest_path(root, &prior_generation_id).parent().unwrap()).unwrap();
}

fn build_generated_predecessor(root: &Path, corpus: CorpusSpec) {
    {
        // Produce equivalent records through the current writer, then bind the
        // generation to the exact deployed pre-projector-4 Core/policy pair.
        let _successor = TestCoreFingerprintOverride::set(SUCCESSOR_CORE_FINGERPRINT);
        let source = source("migration-qualification.jsonl");
        let options = WriterOptions {
            indexer_threads: 1,
            memory_bytes: INDEX_MEMORY_MIN_PER_THREAD,
        };
        let mut writer = GenerationWriter::open(root, options)
            .unwrap()
            .into_writer()
            .unwrap();
        writer.begin_source(source.clone()).unwrap();
        for sequence in 1..=corpus.documents {
            let body = deterministic_body(corpus.body_bytes, sequence);
            writer
                .add_core_record(document(&source, sequence, &body))
                .unwrap();
        }
        writer
            .certify_source(certificate(&source, 1, corpus.documents))
            .unwrap();
        writer.commit(|_| true).unwrap();
        relabel_active_manifest(root, |manifest| {
            manifest.core_record_contract_fingerprint =
                SAME_EPOCH_PREDECESSOR_CORE_FINGERPRINT.to_owned();
            manifest.policy_schema_hash =
                SAME_EPOCH_PREDECESSOR_SOURCE_GENERATION_POLICY_HASH.to_owned();
        });
    }

    let verified = VerifiedIndex::open(root).unwrap();
    assert_eq!(verified.document_count(), corpus.documents);
    assert_eq!(
        verified.manifest().core_record_contract_fingerprint,
        SAME_EPOCH_PREDECESSOR_CORE_FINGERPRINT
    );
}

fn relabel_as_current(root: &Path) {
    let _successor = TestCoreFingerprintOverride::set(SUCCESSOR_CORE_FINGERPRINT);
    relabel_active_manifest(root, |manifest| {
        manifest.core_record_contract_fingerprint = SUCCESSOR_CORE_FINGERPRINT.to_owned();
        manifest.policy_schema_hash = current_source_generation_policy_hash().unwrap();
    });
    assert!(!VerifiedIndex::open(root)
        .unwrap()
        .uses_allowlisted_predecessor_contract());
}

fn stored_payload(index: &VerifiedIndex) -> Vec<u8> {
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
                .unwrap()
                .to_vec();
            let event_id = CoreRecord::decode_stored(&encoded).unwrap().event_id;
            (event_id, encoded)
        })
        .collect::<Vec<_>>();
    records.sort_by_key(|(event_id, _)| event_id.as_uuid());
    let mut output = Vec::new();
    for (_, record) in records {
        output.extend_from_slice(&record);
        output.push(b'\n');
    }
    output
}

#[repr(C)]
struct ProcessTimespec {
    tv_sec: i64,
    tv_nsec: i64,
}

unsafe extern "C" {
    fn clock_gettime(clock_id: i32, timespec: *mut ProcessTimespec) -> i32;
}

fn process_cpu_seconds() -> f64 {
    const CLOCK_PROCESS_CPUTIME_ID: i32 = 2;
    let mut value = ProcessTimespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `value` is a valid writable timespec and Linux defines clock id 2
    // as the process CPU clock used by this Linux-only qualification module.
    let result = unsafe { clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &mut value) };
    assert_eq!(result, 0, "clock_gettime(CLOCK_PROCESS_CPUTIME_ID) failed");
    value.tv_sec as f64 + value.tv_nsec as f64 / 1_000_000_000.0
}

fn json_u64(value: &serde_json::Value, key: &str) -> u64 {
    value[key]
        .as_u64()
        .unwrap_or_else(|| panic!("qualification report lacks u64 {key}"))
}

fn json_f64(value: &serde_json::Value, key: &str) -> f64 {
    value[key]
        .as_f64()
        .unwrap_or_else(|| panic!("qualification report lacks f64 {key}"))
}

fn report_sample(report: &serde_json::Value) -> PerformanceSample {
    PerformanceSample {
        output: OutputIdentity {
            sha256: report["output_sha256"].as_str().unwrap().to_owned(),
            declared_bytes: json_u64(report, "output_bytes"),
            actual_bytes: json_u64(report, "output_file_bytes"),
        },
        wall_seconds: json_f64(report, "wall_seconds"),
        cpu_seconds: json_f64(report, "cpu_seconds"),
        peak_rss_bytes: json_u64(report, "peak_rss_bytes"),
    }
}

fn clone_guard(mode: CloneMode) -> Option<CloneTestHookGuard> {
    (mode == CloneMode::CopyFallback).then(|| {
        CloneTestHookGuard::set(
            CloneTestOptions {
                force_copy: true,
                available_bytes: None,
            },
            |_stage, _path| Ok(()),
        )
    })
}

fn completed_pointer(outcome: PredecessorMigrationOutcome) -> ActiveGenerationPointer {
    match outcome {
        PredecessorMigrationOutcome::Unchanged(pointer)
        | PredecessorMigrationOutcome::Migrated(pointer)
        | PredecessorMigrationOutcome::CommittedVisible { pointer, .. } => pointer,
        PredecessorMigrationOutcome::CommittedRecoveryRequired { recovery } => {
            panic!("qualification operation requires committed recovery: {recovery:?}")
        }
    }
}

fn execute_qualification_operation(case: QualificationCase, root: &Path) {
    match case {
        QualificationCase::Migration => {
            drop(
                GenerationWriter::open(root, WriterOptions::default())
                    .unwrap()
                    .into_writer()
                    .unwrap(),
            );
        }
        QualificationCase::CurrentRepublish => {
            let lease = GenerationWriter::open(root, WriterOptions::default())
                .unwrap()
                .into_writer()
                .unwrap();
            let pointer = load_active_generation_pointer(root).unwrap().unwrap();
            let outcome =
                republish_current_for_qualification(root, &pointer, &WriterOptions::default())
                    .unwrap();
            let current = completed_pointer(outcome);
            best_effort_post_migration_cleanup(root, &current);
            drop(lease);
        }
    }
}

fn materialize_output_identity(root: &Path, output_path: &Path) -> OutputIdentity {
    let verified = VerifiedIndex::open(root).unwrap();
    let output = stored_payload(&verified);
    let mut output_file = File::create(output_path).unwrap();
    output_file.write_all(&output).unwrap();
    output_file.flush().unwrap();
    output_file.sync_all().unwrap();
    let identity = OutputIdentity {
        sha256: sha256_hex(&output),
        declared_bytes: output.len() as u64,
        actual_bytes: output_file.metadata().unwrap().len(),
    };
    validate_output_identity(&identity).unwrap();
    identity
}

fn require_same_output(
    left: (&OutputIdentity, &Path),
    right: (&OutputIdentity, &Path),
    context: &str,
) {
    let (left_identity, left_path) = left;
    let (right_identity, right_path) = right;
    validate_output_identity(left_identity).unwrap();
    validate_output_identity(right_identity).unwrap();
    assert_eq!(
        (left_identity.sha256.as_str(), left_identity.actual_bytes),
        (right_identity.sha256.as_str(), right_identity.actual_bytes),
        "{context} requires exact output bytes and hash"
    );
    let mut left_file = BufReader::new(File::open(left_path).unwrap());
    let mut right_file = BufReader::new(File::open(right_path).unwrap());
    let mut left_chunk = [0_u8; 64 * 1024];
    let mut right_chunk = [0_u8; 64 * 1024];
    loop {
        let left_len = left_file.read(&mut left_chunk).unwrap();
        let right_len = right_file.read(&mut right_chunk).unwrap();
        assert_eq!(left_len, right_len, "{context} output lengths diverged");
        assert_eq!(
            &left_chunk[..left_len],
            &right_chunk[..right_len],
            "{context} output bytes diverged"
        );
        if left_len == 0 {
            break;
        }
    }
}

fn topology_report(proof: &CloneTopologyProof) -> serde_json::Value {
    serde_json::json!({
        "payload_files": proof.payload_files,
        "payload_bytes": proof.payload_bytes,
        "shared_payload_files": proof.shared_payload_files,
        "shared_payload_bytes": proof.shared_payload_bytes,
    })
}

#[test]
fn qualification_subprocess_worker() {
    let Ok(case_value) = env::var(QUALIFICATION_CASE_ENV) else {
        return;
    };
    let case = QualificationCase::parse(&case_value).expect("unknown qualification case");
    let clone_mode = CloneMode::parse(
        &env::var(QUALIFICATION_CLONE_MODE_ENV).unwrap_or_else(|_| "hard_link".to_owned()),
    )
    .expect("unknown qualification clone mode");
    let root = PathBuf::from(env::var_os(QUALIFICATION_ROOT_ENV).unwrap());
    let output_path = PathBuf::from(env::var_os(QUALIFICATION_OUTPUT_ENV).unwrap());
    let report_path = PathBuf::from(env::var_os(QUALIFICATION_REPORT_ENV).unwrap());
    let _successor = TestCoreFingerprintOverride::set(SUCCESSOR_CORE_FINGERPRINT);
    let _clone_guard = clone_guard(clone_mode);
    let tracker = Rc::new(RefCell::new(DiskTracker::new(&root).unwrap()));
    let migration_tracker = Rc::clone(&tracker);
    let migration_hook = MigrationTestHookGuard::set(move |_stage: MigrationStage, _path| {
        migration_tracker.borrow_mut().sample();
        Ok(())
    });
    let atomic_tracker = Rc::clone(&tracker);
    let atomic_hook = AtomicWriteTestHookGuard::set(move |stage, _target| {
        if stage == AtomicWriteStage::AfterTemporarySyncBeforeReplace {
            atomic_tracker.borrow_mut().sample();
        }
        Ok(())
    });

    execute_qualification_operation(case, &root);
    tracker.borrow_mut().sample();
    drop(atomic_hook);
    drop(migration_hook);
    let output = materialize_output_identity(&root, &output_path);
    let topology = verify_clone_topology(&root, clone_mode).unwrap();

    let tracker = tracker.borrow();
    let gate = tracker.gate().unwrap();
    let report = serde_json::json!({
        "schema_version": 1,
        "case": case.as_str(),
        "clone_mode": clone_mode.as_str(),
        "output_bytes": output.declared_bytes,
        "output_file_bytes": output.actual_bytes,
        "output_sha256": output.sha256,
        "clone_topology": topology_report(&topology),
        "predecessor_f_bytes": tracker.predecessor_f_bytes,
        "peak_disk_bytes": tracker.peak.allocated_bytes,
        "accounted_peak_disk_bytes": tracker.peak.accounted_allocated_bytes,
        "disk_amplification": gate.multiplier,
        "within_expected_3_67f": gate.within_expected_envelope,
        "blocking_limit": ">5F",
        "filesystem_observations": tracker.observations,
        "peak_file_count": tracker.peak.file_count,
        "peak_allocated_by_category": tracker.peak.allocated_by_category,
        "peak_logical_by_category": tracker.peak.logical_by_category,
    });
    let report_bytes = serde_json::to_vec(&report).unwrap();
    let mut report_file = File::create(report_path).unwrap();
    report_file.write_all(&report_bytes).unwrap();
    report_file.flush().unwrap();
    report_file.sync_all().unwrap();
}

#[test]
fn qualification_performance_subprocess_worker() {
    let Ok(case_value) = env::var(QUALIFICATION_CASE_ENV) else {
        return;
    };
    let case = QualificationCase::parse(&case_value).expect("unknown qualification case");
    let clone_mode = CloneMode::parse(
        &env::var(QUALIFICATION_CLONE_MODE_ENV).unwrap_or_else(|_| "hard_link".to_owned()),
    )
    .expect("unknown qualification clone mode");
    let root = PathBuf::from(env::var_os(QUALIFICATION_ROOT_ENV).unwrap());
    let report_path = PathBuf::from(env::var_os(QUALIFICATION_REPORT_ENV).unwrap());
    let _successor = TestCoreFingerprintOverride::set(SUCCESSOR_CORE_FINGERPRINT);
    let _clone_guard = clone_guard(clone_mode);

    let cpu_before = process_cpu_seconds();
    let started = Instant::now();
    execute_qualification_operation(case, &root);
    let wall_seconds = started.elapsed().as_secs_f64();
    let cpu_seconds = process_cpu_seconds() - cpu_before;
    fs::write(
        report_path,
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 2,
            "case": case.as_str(),
            "clone_mode": clone_mode.as_str(),
            "wall_seconds": wall_seconds,
            "cpu_seconds": cpu_seconds,
        }))
        .unwrap(),
    )
    .unwrap();
}

fn run_case(
    case: QualificationCase,
    clone_mode: CloneMode,
    corpus: CorpusSpec,
) -> serde_json::Value {
    let temp = tempdir().unwrap();
    let root = temp.path().join("index");
    build_generated_predecessor(&root, corpus);
    run_prepared_disk_case(case, clone_mode, &root, temp.path())
}

fn run_prepared_disk_case(
    case: QualificationCase,
    clone_mode: CloneMode,
    root: &Path,
    output_root: &Path,
) -> serde_json::Value {
    if case == QualificationCase::CurrentRepublish {
        relabel_as_current(root);
    }
    let output_path = output_root.join(format!("{}-payload.jsonl", case.as_str()));
    let report_path = output_root.join(format!("{}-report.json", case.as_str()));
    let completed = Command::new(env::current_exe().unwrap())
        .arg("--exact")
        .arg("tests::migration_qualification::qualification_subprocess_worker")
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env(QUALIFICATION_CASE_ENV, case.as_str())
        .env(QUALIFICATION_CLONE_MODE_ENV, clone_mode.as_str())
        .env(QUALIFICATION_ROOT_ENV, root)
        .env(QUALIFICATION_OUTPUT_ENV, &output_path)
        .env(QUALIFICATION_REPORT_ENV, &report_path)
        .output()
        .unwrap();
    assert!(
        completed.status.success(),
        "qualification child failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&completed.stdout),
        String::from_utf8_lossy(&completed.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
    assert_eq!(report["case"], case.as_str());
    assert_eq!(report["clone_mode"], clone_mode.as_str());
    assert_eq!(
        json_u64(&report, "output_bytes"),
        fs::metadata(output_path).unwrap().len()
    );
    assert!(json_u64(&report, "filesystem_observations") >= 4);
    report
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir(destination).unwrap();
    let mut entries = fs::read_dir(source)
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            assert!(entry.file_type().unwrap().is_file());
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn run_paired_cases(
    order: [QualificationCase; 2],
    clone_mode: CloneMode,
    corpus: CorpusSpec,
) -> [serde_json::Value; 2] {
    let temp = tempdir().unwrap();
    let base = temp.path().join("physical-base");
    build_generated_predecessor(&base, corpus);
    let migration_root = temp.path().join("migration-root");
    let current_root = temp.path().join("current-root");
    copy_tree(&base, &migration_root);
    copy_tree(&base, &current_root);
    relabel_as_current(&current_root);
    let migration_before_path = temp.path().join("migration-preflight-payload.jsonl");
    let current_before_path = temp.path().join("current-preflight-payload.jsonl");
    let migration_before = materialize_output_identity(&migration_root, &migration_before_path);
    let current_before = materialize_output_identity(&current_root, &current_before_path);
    require_same_output(
        (&migration_before, &migration_before_path),
        (&current_before, &current_before_path),
        "pre-measurement migration/current comparison",
    );
    order.map(|case| {
        let (root, before, before_path) = match case {
            QualificationCase::Migration => {
                (&migration_root, &migration_before, &migration_before_path)
            }
            QualificationCase::CurrentRepublish => {
                (&current_root, &current_before, &current_before_path)
            }
        };
        run_prepared_performance_case(case, clone_mode, root, temp.path(), before, before_path)
    })
}

fn run_prepared_performance_case(
    case: QualificationCase,
    clone_mode: CloneMode,
    root: &Path,
    output_root: &Path,
    preflight_output: &OutputIdentity,
    preflight_output_path: &Path,
) -> serde_json::Value {
    let report_path = output_root.join(format!("{}-performance-report.json", case.as_str()));
    let stdout_path = output_root.join(format!("{}-performance-stdout.log", case.as_str()));
    let stderr_path = output_root.join(format!("{}-performance-stderr.log", case.as_str()));
    let stdout = File::create(&stdout_path).unwrap();
    let stderr = File::create(&stderr_path).unwrap();
    let child = Command::new(env::current_exe().unwrap())
        .arg("--exact")
        .arg("tests::migration_qualification::qualification_performance_subprocess_worker")
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env(QUALIFICATION_CASE_ENV, case.as_str())
        .env(QUALIFICATION_CLONE_MODE_ENV, clone_mode.as_str())
        .env(QUALIFICATION_ROOT_ENV, root)
        .env(QUALIFICATION_REPORT_ENV, &report_path)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .unwrap();
    let usage = wait4_operation(&child).unwrap();
    drop(child);
    assert!(
        usage.status.success(),
        "qualification operation child failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&fs::read(&stdout_path).unwrap()),
        String::from_utf8_lossy(&fs::read(&stderr_path).unwrap())
    );
    let operation: serde_json::Value =
        serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
    assert_eq!(operation["case"], case.as_str());
    assert_eq!(operation["clone_mode"], clone_mode.as_str());
    let operation_cpu_seconds = json_f64(&operation, "cpu_seconds");
    assert!(
        usage.process_cpu_seconds + 0.000_010 >= operation_cpu_seconds,
        "wait4 child CPU must cover the bracketed operation CPU"
    );

    let output_path = output_root.join(format!("{}-post-payload.jsonl", case.as_str()));
    let output = materialize_output_identity(root, &output_path);
    require_same_output(
        (preflight_output, preflight_output_path),
        (&output, &output_path),
        "post-operation output verification",
    );
    let topology = verify_clone_topology(root, clone_mode).unwrap();
    serde_json::json!({
        "schema_version": 2,
        "case": case.as_str(),
        "clone_mode": clone_mode.as_str(),
        "wall_seconds": json_f64(&operation, "wall_seconds"),
        "cpu_seconds": operation_cpu_seconds,
        "peak_rss_bytes": usage.peak_rss_bytes,
        "isolated_process_cpu_seconds": usage.process_cpu_seconds,
        "measurement_scope": "dedicated operation child; wall/cpu bracket operation; RSS wait4 ru_maxrss; child setup/report symmetric",
        "output_bytes": output.declared_bytes,
        "output_file_bytes": output.actual_bytes,
        "output_sha256": output.sha256,
        "preflight_output_bytes": preflight_output.actual_bytes,
        "preflight_output_sha256": preflight_output.sha256,
        "exact_output_bytes_compared": true,
        "clone_topology": topology_report(&topology),
    })
}

fn env_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .map(|value| {
            value
                .parse::<usize>()
                .expect("qualification size must be usize")
        })
        .unwrap_or(default)
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    }
}

fn aggregate(samples: &[PerformanceSample]) -> PerformanceSample {
    assert!(!samples.is_empty());
    let output = samples[0].output.clone();
    for sample in samples {
        validate_output_identity(&sample.output).unwrap();
        assert_eq!(sample.output.sha256, output.sha256);
        assert_eq!(sample.output.actual_bytes, output.actual_bytes);
    }
    PerformanceSample {
        output,
        wall_seconds: median(samples.iter().map(|sample| sample.wall_seconds).collect()),
        cpu_seconds: median(samples.iter().map(|sample| sample.cpu_seconds).collect()),
        peak_rss_bytes: median(
            samples
                .iter()
                .map(|sample| sample.peak_rss_bytes as f64)
                .collect(),
        ) as u64,
    }
}

#[test]
#[ignore = "deterministic generated-index migration disk qualification; owned by nightly"]
fn generated_small_and_large_disk_qualification() {
    let cases = [
        (
            "small_hard_link",
            CloneMode::HardLink,
            CorpusSpec {
                documents: 64,
                body_bytes: 512,
            },
        ),
        (
            "large_copy_fallback",
            CloneMode::CopyFallback,
            CorpusSpec {
                documents: 4_096,
                body_bytes: 2_048,
            },
        ),
    ];
    for (label, clone_mode, corpus) in cases {
        let report = run_case(QualificationCase::Migration, clone_mode, corpus);
        assert!(json_f64(&report, "disk_amplification") <= 5.0);
        let topology = &report["clone_topology"];
        eprintln!(
            "migration_disk_qualification label={label} documents={} body_bytes={} F={} peak={} multiplier={:.4} expected_3_67f={} output_bytes={} output_sha256={} payload_files={} shared_payload_files={} shared_payload_bytes={}",
            corpus.documents,
            corpus.body_bytes,
            json_u64(&report, "predecessor_f_bytes"),
            json_u64(&report, "peak_disk_bytes"),
            json_f64(&report, "disk_amplification"),
            report["within_expected_3_67f"],
            json_u64(&report, "output_bytes"),
            report["output_sha256"].as_str().unwrap(),
            json_u64(topology, "payload_files"),
            json_u64(topology, "shared_payload_files"),
            json_u64(topology, "shared_payload_bytes"),
        );
    }
}

#[test]
#[ignore = "controlled-host migration/current A/B benchmark; invoke the release target"]
fn release_benchmark_report() {
    let sample_count = env_usize(RELEASE_SAMPLES_ENV, 3);
    assert!(
        sample_count >= 3,
        "release qualification requires at least 3 samples"
    );
    let corpus = CorpusSpec {
        documents: env_usize(RELEASE_DOCUMENTS_ENV, RELEASE_DEFAULT_DOCUMENTS) as u64,
        body_bytes: env_usize(RELEASE_BODY_BYTES_ENV, RELEASE_DEFAULT_BODY_BYTES),
    };
    let clone_mode = CloneMode::parse(
        &env::var(QUALIFICATION_CLONE_MODE_ENV).unwrap_or_else(|_| "hard_link".to_owned()),
    )
    .expect("unknown qualification clone mode");
    let enforce = env::var(ENFORCE_PERF_ENV).as_deref() == Ok("1");
    if enforce {
        assert_eq!(
            clone_mode,
            CloneMode::HardLink,
            "the 5% gate is defined only for the production hard-link path"
        );
    }
    let mut current_reports = Vec::new();
    let mut migration_reports = Vec::new();
    for sample in 0..sample_count {
        let order = if sample % 2 == 0 {
            [
                QualificationCase::CurrentRepublish,
                QualificationCase::Migration,
            ]
        } else {
            [
                QualificationCase::Migration,
                QualificationCase::CurrentRepublish,
            ]
        };
        for report in run_paired_cases(order, clone_mode, corpus) {
            let case = QualificationCase::parse(report["case"].as_str().unwrap()).unwrap();
            match case {
                QualificationCase::Migration => migration_reports.push(report),
                QualificationCase::CurrentRepublish => current_reports.push(report),
            }
        }
    }
    let current_samples = current_reports
        .iter()
        .map(report_sample)
        .collect::<Vec<_>>();
    let migration_samples = migration_reports
        .iter()
        .map(report_sample)
        .collect::<Vec<_>>();
    let paired_samples = current_samples
        .iter()
        .zip(&migration_samples)
        .enumerate()
        .map(|(pair, (current, migration))| {
            let regressions = comparable_regressions(current, migration).unwrap();
            serde_json::json!({
                "pair": pair + 1,
                "current": {
                    "wall_seconds": current.wall_seconds,
                    "cpu_seconds": current.cpu_seconds,
                    "dedicated_child_cpu_seconds": json_f64(
                        &current_reports[pair],
                        "isolated_process_cpu_seconds",
                    ),
                    "peak_rss_bytes": current.peak_rss_bytes,
                },
                "migration": {
                    "wall_seconds": migration.wall_seconds,
                    "cpu_seconds": migration.cpu_seconds,
                    "dedicated_child_cpu_seconds": json_f64(
                        &migration_reports[pair],
                        "isolated_process_cpu_seconds",
                    ),
                    "peak_rss_bytes": migration.peak_rss_bytes,
                },
                "migration_regression": regressions,
                "output_bytes": migration.output.actual_bytes,
                "output_sha256": migration.output.sha256,
                "migration_clone_topology": migration_reports[pair]["clone_topology"],
                "current_clone_topology": current_reports[pair]["clone_topology"],
            })
        })
        .collect::<Vec<_>>();
    let current = aggregate(&current_samples);
    let migration = aggregate(&migration_samples);
    let regressions = comparable_regressions(&current, &migration).unwrap();
    if enforce {
        for (metric, regression) in &regressions {
            assert!(
                *regression <= 0.05,
                "controlled byte-identical migration {metric} regression {:.2}% exceeds 5%",
                regression * 100.0
            );
        }
    }
    let report = serde_json::json!({
        "schema_version": 2,
        "qualification": "same_epoch_core_predecessor_migration",
        "samples_per_path": sample_count,
        "documents": corpus.documents,
        "body_bytes_per_document": corpus.body_bytes,
        "clone_mode": clone_mode.as_str(),
        "payload_byte_identical": true,
        "exact_output_bytes_compared_before_measurement": true,
        "total_output_bytes": migration.output.actual_bytes,
        "output_sha256": migration.output.sha256,
        "measurement_scope": "dedicated isolated child per operation; output/accounting/topology outside wall/cpu/RSS; wall/cpu bracket operation; RSS from wait4 ru_maxrss; child setup/report symmetric",
        "paired_samples": paired_samples,
        "current_median": {
            "wall_seconds": current.wall_seconds,
            "cpu_seconds": current.cpu_seconds,
            "peak_rss_bytes": current.peak_rss_bytes,
        },
        "migration_median": {
            "wall_seconds": migration.wall_seconds,
            "cpu_seconds": migration.cpu_seconds,
            "peak_rss_bytes": migration.peak_rss_bytes,
        },
        "migration_regression": regressions,
        "five_percent_perf_gate_enforced": enforce,
        "five_percent_gate_semantics": "only exact-byte-identical production hard-link wall/cpu/interval-process-peak-rss",
        "disk_semantics": "absolute multiplier against predecessor generation F; expected <=3.67F; block only >5F",
        "forced_copy_qualification": "reported separately by migration_disk_qualification_tests; excluded from the 5% gate",
    });
    let report_bytes = serde_json::to_vec_pretty(&report).unwrap();
    if let Some(outputs) = env::var_os("TEST_UNDECLARED_OUTPUTS_DIR") {
        let path = PathBuf::from(outputs).join("migration-qualification-report.json");
        fs::write(path, &report_bytes).unwrap();
    }
    eprintln!(
        "MIGRATION_QUALIFICATION_REPORT {}",
        String::from_utf8(report_bytes).unwrap()
    );
}

fn migrate_and_prove_topology(root: &Path, clone_mode: CloneMode) -> CloneTopologyProof {
    let _successor = TestCoreFingerprintOverride::set(SUCCESSOR_CORE_FINGERPRINT);
    let clone_guard = clone_guard(clone_mode);
    execute_qualification_operation(QualificationCase::Migration, root);
    drop(clone_guard);
    verify_clone_topology(root, clone_mode).unwrap()
}

#[test]
fn hard_link_topology_rejects_a_silently_copied_payload() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("index");
    build_generated_predecessor(
        &root,
        CorpusSpec {
            documents: 64,
            body_bytes: 512,
        },
    );
    let proof = migrate_and_prove_topology(&root, CloneMode::HardLink);
    assert_eq!(proof.shared_payload_files, proof.payload_files);
    assert_eq!(proof.shared_payload_bytes, proof.payload_bytes);

    let (predecessor, candidate) = first_payload_pair(&root).unwrap();
    let replacement = candidate
        .parent()
        .unwrap()
        .join(".qualification-copied-payload");
    fs::copy(predecessor, &replacement).unwrap();
    fs::remove_file(&candidate).unwrap();
    fs::rename(replacement, candidate).unwrap();
    let error = verify_clone_topology(&root, CloneMode::HardLink).unwrap_err();
    assert!(
        error.contains("silently copied payload"),
        "unexpected topology error: {error}"
    );
}

#[test]
fn forced_copy_topology_rejects_a_shared_payload_inode() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("index");
    build_generated_predecessor(
        &root,
        CorpusSpec {
            documents: 64,
            body_bytes: 512,
        },
    );
    let proof = migrate_and_prove_topology(&root, CloneMode::CopyFallback);
    assert_eq!(proof.shared_payload_files, 0);
    assert_eq!(proof.shared_payload_bytes, 0);

    let (predecessor, candidate) = first_payload_pair(&root).unwrap();
    fs::remove_file(&candidate).unwrap();
    fs::hard_link(predecessor, candidate).unwrap();
    let error = verify_clone_topology(&root, CloneMode::CopyFallback).unwrap_err();
    assert!(
        error.contains("shares payload inode"),
        "unexpected topology error: {error}"
    );
}

#[test]
fn disk_gate_rejects_wrong_denominator_scope_or_value() {
    let valid = DiskGateInput {
        denominator_scope: DenominatorScope::PredecessorGeneration,
        predecessor_f_bytes: 100,
        declared_predecessor_f_bytes: 100,
        peak_allocated_bytes: 367,
        accounted_allocated_bytes: 367,
        unexplained_paths: Vec::new(),
    };
    assert!(validate_disk_gate(&valid).is_ok());
    let mut wrong_scope = valid.clone();
    wrong_scope.denominator_scope = DenominatorScope::WholeRoot;
    assert!(validate_disk_gate(&wrong_scope).is_err());
    let mut wrong_value = valid;
    wrong_value.declared_predecessor_f_bytes = 200;
    assert!(validate_disk_gate(&wrong_value).is_err());
}

#[test]
fn disk_gate_rejects_unexplained_files_and_bytes() {
    assert_eq!(
        classify_path(
            Path::new(
                "integrity-certifications/generation-0123456789abcdef0123456789abcdef.physical-certification.json",
            ),
            true,
            "generation-base",
        ),
        Ok(AllocationCategory::Certification)
    );
    assert!(classify_path(
        Path::new("integrity-certifications/unbound.physical-certification.json"),
        true,
        "generation-base",
    )
    .is_err());
    assert!(classify_path(Path::new("surprise.bin"), true, "generation-base").is_err());
    let unexplained_path = DiskGateInput {
        denominator_scope: DenominatorScope::PredecessorGeneration,
        predecessor_f_bytes: 100,
        declared_predecessor_f_bytes: 100,
        peak_allocated_bytes: 200,
        accounted_allocated_bytes: 200,
        unexplained_paths: vec!["surprise.bin".to_owned()],
    };
    assert!(validate_disk_gate(&unexplained_path).is_err());
    let mut unexplained_bytes = unexplained_path;
    unexplained_bytes.unexplained_paths.clear();
    unexplained_bytes.accounted_allocated_bytes = 199;
    assert!(validate_disk_gate(&unexplained_bytes).is_err());
}

#[test]
fn disk_gate_rejects_more_than_five_f() {
    let input = DiskGateInput {
        denominator_scope: DenominatorScope::PredecessorGeneration,
        predecessor_f_bytes: 100,
        declared_predecessor_f_bytes: 100,
        peak_allocated_bytes: 501,
        accounted_allocated_bytes: 501,
        unexplained_paths: Vec::new(),
    };
    assert!(validate_disk_gate(&input).is_err());
}

#[test]
fn performance_gate_rejects_non_identical_payloads_before_five_percent() {
    let current = PerformanceSample {
        output: OutputIdentity {
            sha256: "current".to_owned(),
            declared_bytes: 100,
            actual_bytes: 100,
        },
        wall_seconds: 1.0,
        cpu_seconds: 1.0,
        peak_rss_bytes: 100,
    };
    let mut migration = current.clone();
    migration.output.sha256 = "migration".to_owned();
    migration.wall_seconds = 1.01;
    assert!(comparable_regressions(&current, &migration).is_err());
}

#[test]
fn output_accounting_rejects_percent_only_or_wrong_total_bytes() {
    let output = OutputIdentity {
        sha256: "payload".to_owned(),
        declared_bytes: 99,
        actual_bytes: 100,
    };
    assert!(validate_output_identity(&output).is_err());
}
