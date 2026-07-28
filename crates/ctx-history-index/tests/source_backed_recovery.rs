#![cfg(target_os = "linux")]

use std::{
    env,
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, EventIdentityInput, LocatorRevisionPolicy,
    NativeItemKey, NativeRecordCoordinate, NativeSessionKey, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceKey, SourceObservation, SourceRecordLocator,
    TypedKey,
};
use ctx_history_index::{
    CommitReceipt, GenerationWriter, IndexError, LexicalDocument, VerifiedIndex, WriterOptions,
};
use tempfile::{tempdir, TempDir};

const CHILD_MODE_ENV: &str = "CTX_SOURCE_RECOVERY_CHILD_MODE";
const CHILD_ROOT_ENV: &str = "CTX_SOURCE_RECOVERY_ROOT";
const CHILD_MARKER_ENV: &str = "CTX_SOURCE_RECOVERY_MARKER";
const CHILD_RESULT_ENV: &str = "CTX_SOURCE_RECOVERY_RESULT";
const FAULT_SHIM_ENV: &str = "CTX_SOURCE_RECOVERY_FAULT_SHIM";
const PREVIOUS_BODY: &str = "previous baseline content";
const CANDIDATE_BODY: &str = "candidate replacement content";
const CHILD_TIMEOUT: Duration = Duration::from_secs(20);

#[test]
fn subprocess_generation_worker() {
    let Ok(mode) = env::var(CHILD_MODE_ENV) else {
        return;
    };
    let root = required_env_path(CHILD_ROOT_ENV);
    match mode.as_str() {
        "pause_after_writer_open" => {
            let _writer = GenerationWriter::open(&root, writer_options()).unwrap();
            checkpoint_and_stop("writer-open");
        }
        "pause_before_commit" => {
            let writer = staged_replacement(&root);
            checkpoint_and_stop("before-commit");
            writer.commit(|_| true).unwrap();
        }
        "pause_after_commit" => {
            let receipt = staged_replacement(&root).commit(|_| true).unwrap();
            write_child_result(&receipt.generation_id);
            checkpoint_and_stop("after-commit");
        }
        "commit" => {
            let receipt = staged_replacement(&root).commit(|_| true).unwrap();
            write_child_result(&receipt.generation_id);
        }
        "commit_expect_error" => {
            let error = staged_replacement(&root).commit(|_| true).unwrap_err();
            write_child_result(&format!("{error:?}\n{error}"));
        }
        other => panic!("unknown child mode {other}"),
    }
}

#[test]
fn process_death_before_commit_preserves_and_can_advance_the_previous_generation() {
    let fixture = RecoveryFixture::new();
    let old_reader = VerifiedIndex::open(&fixture.root).unwrap();
    let mut child = fixture.spawn_stopped_child("pause_before_commit", None);
    fixture.kill_at_marker(&mut child);

    assert_generation(
        &fixture.root,
        &fixture.baseline.generation_id,
        "previous",
        "candidate",
    );
    assert_reader_terms(&old_reader, "previous", "candidate");

    let receipt = staged_replacement(&fixture.root).commit(|_| true).unwrap();
    assert_generation(
        &fixture.root,
        &receipt.generation_id,
        "candidate",
        "previous",
    );
}

#[test]
fn process_death_after_commit_keeps_new_visibility_and_old_reader_pinning() {
    let fixture = RecoveryFixture::new();
    let old_reader = VerifiedIndex::open(&fixture.root).unwrap();
    let mut child = fixture.spawn_stopped_child("pause_after_commit", None);
    fixture.kill_at_marker(&mut child);

    let generation_id = fs::read_to_string(&fixture.result).unwrap();
    assert_generation(&fixture.root, generation_id.trim(), "candidate", "previous");
    assert_reader_terms(&old_reader, "previous", "candidate");
}

#[test]
fn stale_writer_lock_after_sigkill_is_recoverable() {
    let fixture = RecoveryFixture::new();
    let mut child = fixture.spawn_stopped_child("pause_after_writer_open", None);
    fixture.kill_at_marker(&mut child);

    let stale_lock = fixture.root.join(".tantivy-writer.lock");
    assert!(
        stale_lock.is_file(),
        "SIGKILL did not leave the lock witness"
    );
    let writer = GenerationWriter::open(&fixture.root, writer_options()).unwrap();
    drop(writer);

    assert_generation(
        &fixture.root,
        &fixture.baseline.generation_id,
        "previous",
        "candidate",
    );
}

#[test]
fn manifest_write_permission_failure_preserves_previous_generation() {
    if unsafe { geteuid() } == 0 {
        eprintln!("permission fault requires a non-root test process");
        return;
    }
    let fixture = RecoveryFixture::new();
    let manifest_directory = fixture.root.join("ctx-generations");
    let original_mode = fs::metadata(&manifest_directory)
        .unwrap()
        .permissions()
        .mode();
    let _restore = PermissionRestore::new(&manifest_directory, original_mode);
    fs::set_permissions(&manifest_directory, fs::Permissions::from_mode(0o500)).unwrap();

    let error = staged_replacement(&fixture.root)
        .commit(|_| true)
        .unwrap_err();
    assert!(
        matches!(error, IndexError::Io(_)),
        "unexpected failure classification: {error:?}"
    );

    assert_generation(
        &fixture.root,
        &fixture.baseline.generation_id,
        "previous",
        "candidate",
    );
}

#[test]
fn torn_manifest_and_meta_fail_closed_without_damaging_the_previous_root() {
    let fixture = RecoveryFixture::new();
    let manifest_copy = fixture.temp.path().join("torn-manifest");
    let meta_copy = fixture.temp.path().join("torn-meta");
    copy_tree(&fixture.root, &manifest_copy);
    copy_tree(&fixture.root, &meta_copy);

    let manifest_path = manifest_copy
        .join("ctx-generations")
        .join(format!("{}.json", fixture.baseline.generation_id));
    fs::write(manifest_path, b"{\"manifest_version\":1").unwrap();
    assert!(
        VerifiedIndex::open(&manifest_copy).is_err(),
        "torn manifest was accepted"
    );

    fs::write(meta_copy.join("meta.json"), b"{\"index_settings\":").unwrap();
    assert!(
        VerifiedIndex::open(&meta_copy).is_err(),
        "torn meta.json was accepted"
    );

    assert_generation(
        &fixture.root,
        &fixture.baseline.generation_id,
        "previous",
        "candidate",
    );
}

#[test]
fn active_segment_corruption_is_detected_and_rebuild_is_deterministic() {
    let fixture = RecoveryFixture::new();
    let corrupt_copy = fixture.temp.path().join("corrupt-index");
    let rebuild_root = fixture.temp.path().join("rebuild");
    copy_tree(&fixture.root, &corrupt_copy);

    assert!(VerifiedIndex::open(&corrupt_copy)
        .unwrap()
        .validate_checksums()
        .unwrap()
        .is_empty());
    let damaged_path = corrupt_active_store(&corrupt_copy);
    let corrupt_reader = VerifiedIndex::open(&corrupt_copy).unwrap();
    let damaged = corrupt_reader.validate_checksums().unwrap();
    assert!(
        damaged.iter().any(|path| path == &damaged_path),
        "checksum scrub did not identify {damaged_path:?}: {damaged:?}"
    );

    let rebuilt = build_generation(&rebuild_root, 1, PREVIOUS_BODY);
    assert_eq!(
        rebuilt.generation_id, fixture.baseline.generation_id,
        "same certified source snapshot rebuilt to a different generation ID"
    );
    assert_generation(
        &rebuild_root,
        &fixture.baseline.generation_id,
        "previous",
        "candidate",
    );
    assert_generation(
        &fixture.root,
        &fixture.baseline.generation_id,
        "previous",
        "candidate",
    );
}

#[test]
#[ignore = "requires scripts/source-backed-recovery/run-linux-fault-tests.sh"]
fn exact_manifest_and_tantivy_swap_process_death_matrix() {
    let shim = required_fault_shim();
    let cases = [
        FaultCase::stop("sync", "manifest_temp", "after", None, Visibility::Old),
        FaultCase::stop("rename", "manifest_final", "before", None, Visibility::Old),
        FaultCase::stop("rename", "manifest_final", "after", None, Visibility::Old),
        FaultCase::stop(
            "rename",
            "meta_final",
            "before",
            Some("manifest_rename"),
            Visibility::Old,
        ),
        FaultCase::stop(
            "rename",
            "meta_final",
            "after",
            Some("manifest_rename"),
            Visibility::New,
        ),
        FaultCase::stop(
            "sync",
            "manifest_dir",
            "after",
            Some("manifest_rename"),
            Visibility::Old,
        ),
        FaultCase::stop(
            "sync",
            "root_dir",
            "after",
            Some("meta_rename"),
            Visibility::New,
        ),
    ];

    for case in cases {
        run_stopped_fault_case(&shim, case);
    }
}

#[test]
#[ignore = "release blocker reproduction; requires the Linux fault shim"]
fn retry_after_pre_meta_crash_reclaims_tantivy_candidate_files() {
    let shim = required_fault_shim();
    let fixture = RecoveryFixture::new();
    let case = FaultCase::stop(
        "rename",
        "meta_final",
        "before",
        Some("manifest_rename"),
        Visibility::Old,
    );
    let mut child = fixture.spawn_stopped_child("commit", Some((&shim, case)));
    fixture.kill_at_marker(&mut child);
    assert_generation(
        &fixture.root,
        &fixture.baseline.generation_id,
        "previous",
        "candidate",
    );

    let receipt = staged_replacement(&fixture.root)
        .commit(|_| true)
        .expect("retry after a pre-meta crash must reclaim candidate files");
    assert_generation(
        &fixture.root,
        &receipt.generation_id,
        "candidate",
        "previous",
    );
}

#[test]
#[ignore = "requires scripts/source-backed-recovery/run-linux-fault-tests.sh"]
fn injected_enospc_and_write_sync_failures_preserve_previous_generation() {
    let shim = required_fault_shim();
    let cases = [
        FaultCase::fail("write", "index_data", "ENOSPC", None),
        FaultCase::fail("write", "manifest_temp", "ENOSPC", None),
        FaultCase::fail("sync", "manifest_temp", "EIO", None),
        FaultCase::fail(
            "write",
            "root_atomic_temp",
            "ENOSPC",
            Some("manifest_rename"),
        ),
        FaultCase::fail("sync", "root_atomic_temp", "EIO", Some("manifest_rename")),
        FaultCase::fail("sync", "manifest_dir", "EIO", Some("manifest_rename")),
    ];

    for case in cases {
        let fixture = RecoveryFixture::new();
        let output = fixture.run_fault_child(&shim, "commit_expect_error", case);
        assert!(
            output.status.success(),
            "fault child failed unexpectedly for {case:?}:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let detail = fs::read_to_string(&fixture.result).unwrap();
        assert!(!detail.trim().is_empty(), "fault child recorded no error");
        assert_generation(
            &fixture.root,
            &fixture.baseline.generation_id,
            "previous",
            "candidate",
        );
    }
}

#[test]
#[ignore = "known blocker reproduction; requires the Linux fault shim"]
fn reproduction_crash_orphan_survives_writer_reopen() {
    let shim = required_fault_shim();
    let fixture = RecoveryFixture::new();
    let case = FaultCase::stop("sync", "manifest_temp", "after", None, Visibility::Old);
    let mut child = fixture.spawn_stopped_child("commit", Some((&shim, case)));
    fixture.kill_at_marker(&mut child);

    let manifest_directory = fixture.root.join("ctx-generations");
    let before = atomic_temporary_files(&manifest_directory);
    assert!(
        !before.is_empty(),
        "the crash point did not leave its expected temporary-file witness"
    );

    drop(GenerationWriter::open(&fixture.root, writer_options()).unwrap());
    let after = atomic_temporary_files(&manifest_directory);
    assert_eq!(
        after, before,
        "current writer-open unexpectedly reclaimed crash orphans; invert this reproduction"
    );
}

#[test]
#[ignore = "known blocker reproduction; requires the Linux fault shim"]
fn reproduction_reused_manifest_skips_directory_durability_fence() {
    let shim = required_fault_shim();
    let fixture = RecoveryFixture::new();
    let first_crash = FaultCase::stop("rename", "manifest_final", "after", None, Visibility::Old);
    let mut child = fixture.spawn_stopped_child("commit", Some((&shim, first_crash)));
    fixture.kill_at_marker(&mut child);
    assert_generation(
        &fixture.root,
        &fixture.baseline.generation_id,
        "previous",
        "candidate",
    );

    let retry_fence = FaultCase::stop("sync", "manifest_dir", "after", None, Visibility::New);
    let mut retry = fixture.spawn_stopped_child("commit", Some((&shim, retry_fence)));
    let status = wait_for_exit_without_marker(&mut retry, &fixture.marker);
    assert!(
        status.success(),
        "retry failed before demonstrating the missing manifest fence: {status}"
    );
    let generation_id = fs::read_to_string(&fixture.result).unwrap();
    assert_generation(&fixture.root, generation_id.trim(), "candidate", "previous");
}

#[derive(Clone, Copy, Debug)]
enum Visibility {
    Old,
    New,
}

#[derive(Clone, Copy, Debug)]
struct FaultCase {
    op: &'static str,
    target: &'static str,
    timing: &'static str,
    action: &'static str,
    error: Option<&'static str>,
    arm_after: Option<&'static str>,
    visibility: Visibility,
}

impl FaultCase {
    const fn stop(
        op: &'static str,
        target: &'static str,
        timing: &'static str,
        arm_after: Option<&'static str>,
        visibility: Visibility,
    ) -> Self {
        Self {
            op,
            target,
            timing,
            action: "stop",
            error: None,
            arm_after,
            visibility,
        }
    }

    const fn fail(
        op: &'static str,
        target: &'static str,
        error: &'static str,
        arm_after: Option<&'static str>,
    ) -> Self {
        Self {
            op,
            target,
            timing: "before",
            action: "fail",
            error: Some(error),
            arm_after,
            visibility: Visibility::Old,
        }
    }
}

struct RecoveryFixture {
    temp: TempDir,
    root: PathBuf,
    marker: PathBuf,
    result: PathBuf,
    baseline: CommitReceipt,
}

impl RecoveryFixture {
    fn new() -> Self {
        let temp = tempdir().unwrap();
        let root = temp.path().join("index");
        let marker = temp.path().join("child.marker");
        let result = temp.path().join("child.result");
        let baseline = build_generation(&root, 1, PREVIOUS_BODY);
        Self {
            temp,
            root,
            marker,
            result,
            baseline,
        }
    }

    fn child_command(&self, mode: &str) -> Command {
        let mut command = Command::new(env::current_exe().unwrap());
        command
            .arg("--exact")
            .arg("subprocess_generation_worker")
            .arg("--nocapture")
            .arg("--test-threads=1")
            .env(CHILD_MODE_ENV, mode)
            .env(CHILD_ROOT_ENV, &self.root)
            .env(CHILD_MARKER_ENV, &self.marker)
            .env(CHILD_RESULT_ENV, &self.result);
        command
    }

    fn spawn_stopped_child(&self, mode: &str, fault: Option<(&Path, FaultCase)>) -> Child {
        let _ = fs::remove_file(&self.marker);
        let _ = fs::remove_file(&self.result);
        let mut command = self.child_command(mode);
        command.stdout(Stdio::inherit()).stderr(Stdio::inherit());
        if let Some((shim, case)) = fault {
            configure_fault(&mut command, shim, &self.root, &self.marker, case);
        }
        command.spawn().unwrap()
    }

    fn run_fault_child(&self, shim: &Path, mode: &str, case: FaultCase) -> std::process::Output {
        let _ = fs::remove_file(&self.marker);
        let _ = fs::remove_file(&self.result);
        let mut command = self.child_command(mode);
        configure_fault(&mut command, shim, &self.root, &self.marker, case);
        command.output().unwrap()
    }

    fn kill_at_marker(&self, child: &mut Child) {
        wait_for_marker(child, &self.marker);
        child.kill().unwrap();
        let status = child.wait().unwrap();
        assert!(
            !status.success(),
            "stopped child unexpectedly exited cleanly"
        );
    }
}

struct PermissionRestore {
    path: PathBuf,
    mode: u32,
}

impl PermissionRestore {
    fn new(path: &Path, mode: u32) -> Self {
        Self {
            path: path.to_path_buf(),
            mode,
        }
    }
}

impl Drop for PermissionRestore {
    fn drop(&mut self) {
        fs::set_permissions(&self.path, fs::Permissions::from_mode(self.mode)).unwrap();
    }
}

fn run_stopped_fault_case(shim: &Path, case: FaultCase) {
    let fixture = RecoveryFixture::new();
    eprintln!(
        "fault case {case:?}; baseline generation {}",
        fixture.baseline.generation_id
    );
    let old_reader = VerifiedIndex::open(&fixture.root).unwrap();
    let mut child = fixture.spawn_stopped_child("commit", Some((shim, case)));
    fixture.kill_at_marker(&mut child);

    match case.visibility {
        Visibility::Old => assert_generation(
            &fixture.root,
            &fixture.baseline.generation_id,
            "previous",
            "candidate",
        ),
        Visibility::New => {
            let current = VerifiedIndex::open(&fixture.root).unwrap();
            assert_ne!(current.generation_id(), fixture.baseline.generation_id);
            assert_reader_terms(&current, "candidate", "previous");
        }
    }
    assert_reader_terms(&old_reader, "previous", "candidate");
}

fn configure_fault(
    command: &mut Command,
    shim: &Path,
    root: &Path,
    marker: &Path,
    case: FaultCase,
) {
    command
        .env("LD_PRELOAD", shim)
        .env("CTX_RECOVERY_FAULT_ROOT", root)
        .env("CTX_RECOVERY_FAULT_MARKER", marker)
        .env("CTX_RECOVERY_FAULT_OP", case.op)
        .env("CTX_RECOVERY_FAULT_TARGET", case.target)
        .env("CTX_RECOVERY_FAULT_TIMING", case.timing)
        .env("CTX_RECOVERY_FAULT_ACTION", case.action);
    if let Some(error) = case.error {
        command.env("CTX_RECOVERY_FAULT_ERRNO", error);
    }
    if let Some(arm_after) = case.arm_after {
        command.env("CTX_RECOVERY_FAULT_ARM_AFTER", arm_after);
    }
}

fn build_generation(root: &Path, revision: u8, body: &str) -> CommitReceipt {
    let source = source();
    let mut writer = GenerationWriter::open(root, writer_options()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_document(document(&source, body)).unwrap();
    writer
        .certify_source(certificate(&source, revision))
        .unwrap();
    writer.commit(|_| true).unwrap()
}

fn staged_replacement(root: &Path) -> GenerationWriter {
    let source = source();
    let mut writer = GenerationWriter::open(root, writer_options()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_document(document(&source, CANDIDATE_BODY))
        .unwrap();
    writer.certify_source(certificate(&source, 2)).unwrap();
    writer
}

fn writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 32 * 1024 * 1024,
    }
}

fn source() -> SourceKey {
    SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "session",
        1,
        SourceAnchor::provider_native(
            "session-file",
            TypedKey::utf8("source-backed-recovery.jsonl").unwrap(),
        )
        .unwrap(),
    )
    .unwrap()
}

fn certificate(source: &SourceKey, revision: u8) -> CertifiedSource {
    let observation =
        SourceObservation::new(source.clone(), "regular-file-v1", vec![revision]).unwrap();
    CertifiedSource::certify(
        observation.clone(),
        observation,
        "codex-parser-v1",
        [revision; 32],
        ScannedSourceCounts {
            complete_records: 1,
            retained_records: 1,
            indexed_documents: 1,
            certified_bytes: 100,
            ..ScannedSourceCounts::default()
        },
    )
    .unwrap()
}

fn document(source: &SourceKey, body: &str) -> LexicalDocument {
    let native_session_coordinate = TypedKey::utf8("session").unwrap();
    let session_key =
        NativeSessionKey::native_id("session", native_session_coordinate.clone()).unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let native_item_key =
        NativeItemKey::native_id("message", TypedKey::utf8("event-1").unwrap()).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .unwrap();
    LexicalDocument {
        event_id,
        session_id,
        source: source.clone(),
        locator: SourceRecordLocator::new(
            source.clone(),
            NativeRecordCoordinate::Jsonl {
                byte_offset: 100,
                byte_length: 100,
                physical_ordinal: 1,
                native_session_key: Some(native_session_coordinate),
                native_event_key: Some(TypedKey::U64(1)),
            },
            LocatorRevisionPolicy::StableRecordEvidence,
            None,
            [1; 32],
        )
        .unwrap(),
        provider_session_id: Some("session".to_owned()),
        event_sequence: 1,
        occurred_at_unix_ms: Some(1_700_000_000_001),
        event_type: "message".to_owned(),
        role: Some("user".to_owned()),
        body: body.to_owned(),
        workspace: Some("ctx".to_owned()),
        cwd: Some("/work/ctx".to_owned()),
        touched_files: vec!["src/lib.rs".to_owned()],
    }
}

fn assert_generation(root: &Path, generation_id: &str, present: &str, absent: &str) {
    let index = VerifiedIndex::open(root).unwrap();
    assert_eq!(index.generation_id(), generation_id);
    assert_reader_terms(&index, present, absent);
}

fn assert_reader_terms(index: &VerifiedIndex, present: &str, absent: &str) {
    assert_eq!(
        index.search_event_candidates(present, 10).unwrap().len(),
        1,
        "expected {present:?} in generation {}",
        index.generation_id()
    );
    assert!(
        index
            .search_event_candidates(absent, 10)
            .unwrap()
            .is_empty(),
        "did not expect {absent:?} in generation {}",
        index.generation_id()
    );
}

fn checkpoint_and_stop(label: &str) {
    let marker = required_env_path(CHILD_MARKER_ENV);
    let mut file = File::create(marker).unwrap();
    writeln!(file, "{label}").unwrap();
    file.sync_all().unwrap();
    unsafe {
        raise(SIGSTOP);
    }
}

fn write_child_result(result: &str) {
    let path = required_env_path(CHILD_RESULT_ENV);
    let mut file = File::create(path).unwrap();
    file.write_all(result.as_bytes()).unwrap();
    file.sync_all().unwrap();
}

fn required_env_path(name: &str) -> PathBuf {
    PathBuf::from(env::var_os(name).unwrap_or_else(|| panic!("{name} is required")))
}

fn required_fault_shim() -> PathBuf {
    let path = required_env_path(FAULT_SHIM_ENV);
    assert!(path.is_file(), "fault shim {} is missing", path.display());
    path
}

fn wait_for_marker(child: &mut Child, marker: &Path) {
    let deadline = Instant::now() + CHILD_TIMEOUT;
    loop {
        if marker.is_file() {
            return;
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!(
                "child exited before reaching {}: {status}",
                marker.display()
            );
        }
        assert!(
            Instant::now() < deadline,
            "child did not reach {} within {CHILD_TIMEOUT:?}",
            marker.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_exit_without_marker(child: &mut Child, marker: &Path) -> std::process::ExitStatus {
    let deadline = Instant::now() + CHILD_TIMEOUT;
    loop {
        if marker.is_file() {
            child.kill().unwrap();
            let _ = child.wait();
            panic!(
                "retry synchronized the reused manifest directory; \
                 invert this blocker reproduction"
            );
        }
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "retry neither exited nor reached {} within {CHILD_TIMEOUT:?}",
            marker.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&source_path, &destination_path);
        } else {
            fs::copy(source_path, destination_path).unwrap();
        }
    }
}

fn corrupt_active_store(root: &Path) -> PathBuf {
    let path = fs::read_dir(root)
        .unwrap()
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension() == Some(OsStr::new("store")))
        .expect("active generation did not contain a .store file");
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let length = file.metadata().unwrap().len();
    assert!(length > 64, "{} is unexpectedly short", path.display());
    let offset = length / 2;
    file.seek(SeekFrom::Start(offset)).unwrap();
    let mut byte = [0_u8; 1];
    file.read_exact(&mut byte).unwrap();
    byte[0] ^= 0x5a;
    file.seek(SeekFrom::Start(offset)).unwrap();
    file.write_all(&byte).unwrap();
    file.sync_all().unwrap();
    path.file_name().unwrap().into()
}

fn atomic_temporary_files(directory: &Path) -> Vec<PathBuf> {
    let mut files = fs::read_dir(directory)
        .unwrap()
        .filter_map(std::result::Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".ctx-tantivy-atomic-")
        })
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    files.sort();
    files
}

const SIGSTOP: i32 = 19;

unsafe extern "C" {
    fn raise(signal: i32) -> i32;
    fn geteuid() -> u32;
}
