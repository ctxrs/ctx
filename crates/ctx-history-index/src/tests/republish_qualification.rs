//! Same-epoch republish topology gate and raw controlled-host measurement.
//!
//! The ordinary test proves exact bytes and the hard-link/copy topology. Run
//! the measurement only on a controlled host:
//! `bazel test //crates/ctx-history-index:republish_raw_measurement --config=release --test_output=streamed --nocache_test_results`.

use super::*;
use crate::publication::{
    CloneTestHookGuard, CloneTestOptions, CurrentRepublishOutcome, RepublishTestHookGuard,
};
use ctx_history_index_generation::AtomicWriteTestHookGuard;
use std::{
    cell::Cell,
    collections::BTreeSet,
    env, fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    rc::Rc,
    time::Instant,
};

mod process;
mod topology;

use process::wait4_operation;
use topology::{verify_clone_topology, CloneTopologyProof};

const MEASUREMENT_ROOT_ENV: &str = "CTX_REPUBLISH_MEASUREMENT_ROOT";
const CLONE_MODE_ENV: &str = "CTX_REPUBLISH_CLONE_MODE";
const DOCUMENTS_ENV: &str = "CTX_REPUBLISH_DOCUMENTS";
const BODY_BYTES_ENV: &str = "CTX_REPUBLISH_BODY_BYTES";
const DEFAULT_DOCUMENTS: usize = 16_384;
const DEFAULT_BODY_BYTES: usize = 4_096;
const BLOCKING_DISK_MULTIPLIER: u64 = 5;

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

fn deterministic_body(bytes: usize, sequence: u64) -> String {
    let prefix = "same-epoch republish qualification ";
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

fn build_generated_predecessor(root: &Path, corpus: CorpusSpec) {
    let source = source("republish-qualification.jsonl");
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
        writer
            .add_core_record(document(
                &source,
                sequence,
                &deterministic_body(corpus.body_bytes, sequence),
            ))
            .unwrap();
    }
    writer
        .certify_source(certificate(&source, 1, corpus.documents))
        .unwrap();
    writer.commit(|_| true).unwrap();
    assert_eq!(
        VerifiedIndex::open(root).unwrap().document_count(),
        corpus.documents
    );
}

fn stored_payload(root: &Path) -> Vec<u8> {
    let (searcher, _) = open_unverified_generation(root);
    let fields = fields_from_schema(searcher.schema()).unwrap();
    let mut records = searcher
        .search(&AllQuery, &DocSetCollector)
        .unwrap()
        .into_iter()
        .map(|address| {
            let document: TantivyDocument = searcher.doc(address).unwrap();
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

fn clone_guard(mode: CloneMode) -> CloneTestHookGuard {
    let options = match mode {
        CloneMode::HardLink => CloneTestOptions {
            force_copy: false,
            force_reflink_fallback: true,
            force_hardlink_fallback: false,
            available_bytes: None,
            rechecked_available_bytes: None,
        },
        CloneMode::CopyFallback => CloneTestOptions {
            force_copy: true,
            force_reflink_fallback: false,
            force_hardlink_fallback: false,
            available_bytes: None,
            rechecked_available_bytes: None,
        },
    };
    CloneTestHookGuard::set(options, |_stage, _path| Ok(()))
}

fn completed_pointer(outcome: CurrentRepublishOutcome) -> ActiveGenerationPointer {
    match outcome {
        CurrentRepublishOutcome::Published(pointer)
        | CurrentRepublishOutcome::CommittedVisible { pointer, .. } => pointer,
        CurrentRepublishOutcome::CommittedRecoveryRequired { recovery } => {
            panic!("qualification operation requires committed recovery: {recovery:?}")
        }
    }
}

fn execute_republish(root: &Path) -> ActiveGenerationPointer {
    let lease = GenerationWriter::open(root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let pointer = load_active_generation_pointer(root).unwrap().unwrap();
    let current = completed_pointer(
        republish_current_for_qualification(root, &pointer, &WriterOptions::default()).unwrap(),
    );
    best_effort_post_republish_cleanup(root, &current);
    drop(lease);
    current
}

fn regular_file_allocated_bytes(root: &Path) -> u64 {
    fn visit(path: &Path, inodes: &mut BTreeSet<(u64, u64)>) -> u64 {
        let metadata = fs::symlink_metadata(path).unwrap();
        assert!(
            !metadata.file_type().is_symlink(),
            "unexpected symlink: {}",
            path.display()
        );
        if metadata.is_file() {
            return inodes
                .insert((metadata.dev(), metadata.ino()))
                .then_some(metadata.blocks().saturating_mul(512))
                .unwrap_or_default();
        }
        assert!(
            metadata.is_dir(),
            "unexpected filesystem object: {}",
            path.display()
        );
        fs::read_dir(path)
            .unwrap()
            .map(|entry| visit(&entry.unwrap().path(), inodes))
            .sum()
    }

    visit(root, &mut BTreeSet::new())
}

fn predecessor_bytes(root: &Path, pointer: &ActiveGenerationPointer) -> u64 {
    regular_file_allocated_bytes(
        &root
            .join(INDEX_GENERATIONS_DIRECTORY)
            .join(pointer.active().directory()),
    )
}

struct PeakRootBytes {
    root: PathBuf,
    peak: Rc<Cell<u64>>,
    _republish_hook: RepublishTestHookGuard,
    _atomic_hook: AtomicWriteTestHookGuard,
}

impl PeakRootBytes {
    fn start(root: &Path) -> Self {
        let root = root.to_path_buf();
        let peak = Rc::new(Cell::new(regular_file_allocated_bytes(&root)));
        let republish_root = root.clone();
        let republish_peak = Rc::clone(&peak);
        let republish_hook = RepublishTestHookGuard::set(move |_stage, _path| {
            republish_peak.set(
                republish_peak
                    .get()
                    .max(regular_file_allocated_bytes(&republish_root)),
            );
            Ok(())
        });
        let atomic_root = root.clone();
        let atomic_peak = Rc::clone(&peak);
        let atomic_hook = AtomicWriteTestHookGuard::set(move |_stage, _path| {
            atomic_peak.set(
                atomic_peak
                    .get()
                    .max(regular_file_allocated_bytes(&atomic_root)),
            );
            Ok(())
        });
        Self {
            root,
            peak,
            _republish_hook: republish_hook,
            _atomic_hook: atomic_hook,
        }
    }

    fn peak(&self) -> u64 {
        self.peak
            .get()
            .max(regular_file_allocated_bytes(&self.root))
    }
}

fn assert_topology(mode: CloneMode, proof: &CloneTopologyProof) {
    assert!(proof.payload_files > 0);
    assert!(proof.payload_bytes > 0);
    match mode {
        CloneMode::HardLink => {
            assert_eq!(proof.shared_payload_files, proof.payload_files);
            assert_eq!(proof.shared_payload_bytes, proof.payload_bytes);
        }
        CloneMode::CopyFallback => {
            assert_eq!(proof.shared_payload_files, 0);
            assert_eq!(proof.shared_payload_bytes, 0);
        }
    }
}

fn exercise_topology(mode: CloneMode, corpus: CorpusSpec) {
    let temp = tempdir().unwrap();
    let root = temp.path().join(mode.as_str());
    build_generated_predecessor(&root, corpus);
    let before_pointer = load_active_generation_pointer(&root).unwrap().unwrap();
    let before_payload = stored_payload(&root);
    let predecessor_f = predecessor_bytes(&root, &before_pointer);
    assert!(predecessor_f > 0);

    let disk = PeakRootBytes::start(&root);
    let clone_guard = clone_guard(mode);
    let after_pointer = execute_republish(&root);
    drop(clone_guard);

    assert_eq!(
        after_pointer.active().generation_id(),
        before_pointer.active().generation_id()
    );
    assert_eq!(after_pointer.previous(), Some(before_pointer.active()));
    assert_eq!(stored_payload(&root), before_payload);
    let proof = verify_clone_topology(&root, mode).unwrap();
    assert_topology(mode, &proof);
    let final_bytes = regular_file_allocated_bytes(&root);
    let peak_bytes = disk.peak();
    drop(disk);
    assert!(
        peak_bytes <= predecessor_f.saturating_mul(BLOCKING_DISK_MULTIPLIER),
        "republish peak disk amplification exceeds {BLOCKING_DISK_MULTIPLIER}F: {peak_bytes}/{predecessor_f}"
    );
    eprintln!(
        "REPUBLISH_TOPOLOGY mode={} output_bytes={} output_sha256={} payload_files={} payload_bytes={} shared_payload_files={} shared_payload_bytes={} predecessor_f_regular_file_bytes={} final_root_regular_file_bytes={} peak_root_regular_file_bytes={} final_disk_amplification={:.6} peak_disk_amplification={:.6}",
        mode.as_str(),
        before_payload.len(),
        sha256_hex(&before_payload),
        proof.payload_files,
        proof.payload_bytes,
        proof.shared_payload_files,
        proof.shared_payload_bytes,
        predecessor_f,
        final_bytes,
        peak_bytes,
        final_bytes as f64 / predecessor_f as f64,
        peak_bytes as f64 / predecessor_f as f64,
    );
}

#[test]
fn hard_link_and_copy_topology_preserve_exact_bytes() {
    let corpus = CorpusSpec {
        documents: 64,
        body_bytes: 512,
    };
    exercise_topology(CloneMode::HardLink, corpus);
    exercise_topology(CloneMode::CopyFallback, corpus);
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
    // SAFETY: `value` is writable and Linux clock id 2 is the process CPU clock.
    assert_eq!(
        unsafe { clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &mut value) },
        0
    );
    value.tv_sec as f64 + value.tv_nsec as f64 / 1_000_000_000.0
}

#[derive(Default)]
struct ProcIo {
    rchar: u64,
    wchar: u64,
    read_bytes: u64,
    write_bytes: u64,
}

fn proc_io() -> ProcIo {
    let mut result = ProcIo::default();
    for line in fs::read_to_string("/proc/self/io").unwrap().lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().parse().unwrap();
        match name {
            "rchar" => result.rchar = value,
            "wchar" => result.wchar = value,
            "read_bytes" => result.read_bytes = value,
            "write_bytes" => result.write_bytes = value,
            _ => {}
        }
    }
    result
}

fn open_fd_count() -> usize {
    fs::read_dir("/proc/self/fd").unwrap().count()
}

#[test]
#[ignore = "internal worker for the raw controlled-host measurement"]
fn raw_measurement_worker() {
    let Some(root) = env::var_os(MEASUREMENT_ROOT_ENV).map(PathBuf::from) else {
        return;
    };
    let mode = CloneMode::parse(&env::var(CLONE_MODE_ENV).unwrap()).unwrap();
    let _clone_guard = clone_guard(mode);
    let pointer = load_active_generation_pointer(&root).unwrap().unwrap();
    let predecessor_f = predecessor_bytes(&root, &pointer);
    let disk = PeakRootBytes::start(&root);
    let io_before = proc_io();
    let fds_before = open_fd_count();
    let cpu_before = process_cpu_seconds();
    let started = Instant::now();
    execute_republish(&root);
    let operation_wall_seconds = started.elapsed().as_secs_f64();
    let operation_cpu_seconds = process_cpu_seconds() - cpu_before;
    let fds_after = open_fd_count();
    let io_after = proc_io();
    let final_bytes = regular_file_allocated_bytes(&root);
    let peak_bytes = disk.peak();
    drop(disk);
    eprintln!(
        "REPUBLISH_OPERATION_RAW mode={} wall_seconds={operation_wall_seconds:.9} cpu_seconds={operation_cpu_seconds:.9} fds_before={fds_before} fds_after={fds_after} logical_read_bytes={} logical_write_bytes={} physical_read_bytes={} physical_write_bytes={} predecessor_f_regular_file_bytes={} final_root_regular_file_bytes={} peak_root_regular_file_bytes={} final_disk_amplification={:.6} peak_disk_amplification={:.6}",
        mode.as_str(),
        io_after.rchar.saturating_sub(io_before.rchar),
        io_after.wchar.saturating_sub(io_before.wchar),
        io_after.read_bytes.saturating_sub(io_before.read_bytes),
        io_after.write_bytes.saturating_sub(io_before.write_bytes),
        predecessor_f,
        final_bytes,
        peak_bytes,
        final_bytes as f64 / predecessor_f as f64,
        peak_bytes as f64 / predecessor_f as f64,
    );
}

fn env_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .map(|value| value.parse().expect("measurement size must be usize"))
        .unwrap_or(default)
}

#[test]
#[ignore = "raw controlled-host measurement; invoke the dedicated release target"]
fn controlled_host_measurement() {
    let mode = CloneMode::parse(
        &env::var(CLONE_MODE_ENV).unwrap_or_else(|_| CloneMode::HardLink.as_str().to_owned()),
    )
    .unwrap();
    let corpus = CorpusSpec {
        documents: env_usize(DOCUMENTS_ENV, DEFAULT_DOCUMENTS) as u64,
        body_bytes: env_usize(BODY_BYTES_ENV, DEFAULT_BODY_BYTES),
    };
    let temp = tempdir().unwrap();
    let root = temp.path().join("index");
    build_generated_predecessor(&root, corpus);
    let before_pointer = load_active_generation_pointer(&root).unwrap().unwrap();
    let before_payload = stored_payload(&root);
    let predecessor_f = predecessor_bytes(&root, &before_pointer);
    let root_before = regular_file_allocated_bytes(&root);

    let started = Instant::now();
    let child = Command::new(env::current_exe().unwrap())
        .arg("--ignored")
        .arg("--exact")
        .arg("tests::republish_qualification::raw_measurement_worker")
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env(MEASUREMENT_ROOT_ENV, &root)
        .env(CLONE_MODE_ENV, mode.as_str())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let usage = wait4_operation(&child).unwrap();
    let whole_child_wall_seconds = started.elapsed().as_secs_f64();
    drop(child);
    assert!(usage.status.success(), "raw measurement worker failed");

    let after_pointer = load_active_generation_pointer(&root).unwrap().unwrap();
    let after_payload = stored_payload(&root);
    assert_eq!(
        after_pointer.active().generation_id(),
        before_pointer.active().generation_id()
    );
    assert_eq!(after_pointer.previous(), Some(before_pointer.active()));
    assert_eq!(after_payload, before_payload);
    let proof = verify_clone_topology(&root, mode).unwrap();
    assert_topology(mode, &proof);
    let root_after = regular_file_allocated_bytes(&root);

    eprintln!(
        "REPUBLISH_HOST_RAW mode={} documents={} body_bytes={} whole_child_wall_seconds={whole_child_wall_seconds:.9} whole_child_cpu_seconds={:.9} whole_tree_peak_rss_bytes={} filesystem_read_block_operations={} filesystem_write_block_operations={} output_bytes={} output_sha256={} payload_files={} payload_bytes={} shared_payload_files={} shared_payload_bytes={} predecessor_f_regular_file_bytes={} root_before_regular_file_bytes={} final_root_regular_file_bytes={} final_disk_amplification={:.6} measurement_scope=single_descendant_free_child",
        mode.as_str(),
        corpus.documents,
        corpus.body_bytes,
        usage.process_cpu_seconds,
        usage.peak_rss_bytes,
        usage.filesystem_read_block_operations,
        usage.filesystem_write_block_operations,
        before_payload.len(),
        sha256_hex(&before_payload),
        proof.payload_files,
        proof.payload_bytes,
        proof.shared_payload_files,
        proof.shared_payload_bytes,
        predecessor_f,
        root_before,
        root_after,
        root_after as f64 / predecessor_f as f64,
    );
}
