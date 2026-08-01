#![cfg(target_os = "linux")]

use std::{
    collections::HashSet,
    env,
    error::Error as StdError,
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
    derive_event_id, derive_session_id, CertifiedSource, CertifiedSourceAppend,
    CertifiedSourceInventory, CoreRecord, EventIdentityInput, NativeItemKey, NativeSessionKey,
    ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceFrontier,
    SourceInventoryObservation, SourceKey, SourceObservation, TypedKey,
};
use ctx_history_index::{
    CommitReceipt, GenerationWriter, IndexError, VerifiedIndex, WriterOptions,
};
use tantivy::{store::Compressor, Index};
use tempfile::{tempdir, TempDir};

const CHILD_MODE_ENV: &str = "CTX_SOURCE_RECOVERY_CHILD_MODE";
const CHILD_ROOT_ENV: &str = "CTX_SOURCE_RECOVERY_ROOT";
const CHILD_MARKER_ENV: &str = "CTX_SOURCE_RECOVERY_MARKER";
const CHILD_RESULT_ENV: &str = "CTX_SOURCE_RECOVERY_RESULT";
const FAULT_SHIM_ENV: &str = "CTX_SOURCE_RECOVERY_FAULT_SHIM";
const REAL_ENOSPC_ENV: &str = "CTX_SOURCE_RECOVERY_REAL_ENOSPC";
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
        "open_expect_lock_error" => {
            let error = match GenerationWriter::open(&root, writer_options()) {
                Ok(_) => panic!("competing process unexpectedly acquired the writer lock"),
                Err(error) => error,
            };
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

    let stale_lock = fixture.root.join(".ctx-generation-writer.lock");
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
fn two_live_processes_contend_for_one_generation_lock_without_torn_state() {
    let fixture = RecoveryFixture::new();
    let mut winner = fixture.spawn_stopped_child("pause_before_commit", None);
    wait_for_marker(&mut winner, &fixture.marker);

    let _ = fs::remove_file(&fixture.result);
    let loser = fixture
        .child_command("open_expect_lock_error")
        .output()
        .unwrap();
    assert!(
        loser.status.success(),
        "competing writer process failed unexpectedly:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&loser.stdout),
        String::from_utf8_lossy(&loser.stderr)
    );
    let loser_error = fs::read_to_string(&fixture.result).unwrap();
    assert!(
        loser_error.contains("LockFailure") && loser_error.contains("LockBusy"),
        "competing writer did not report the deterministic lock loser: {loser_error}"
    );

    winner.kill().unwrap();
    let winner_status = winner.wait().unwrap();
    assert!(
        !winner_status.success(),
        "stopped lock winner unexpectedly exited cleanly"
    );
    assert_generation(
        &fixture.root,
        &fixture.baseline.generation_id,
        "previous",
        "candidate",
    );

    let receipt = staged_replacement(&fixture.root).commit(|_| true).unwrap();
    assert_generation(
        &fixture.root,
        &receipt.generation_id,
        "candidate",
        "previous",
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

    fs::write(
        active_generation_path(&meta_copy).join("meta.json"),
        b"{\"index_settings\":",
    )
    .unwrap();
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
    assert!(
        VerifiedIndex::open(&corrupt_copy).is_err(),
        "verified open admitted a malformed active document"
    );
    let damaged = Index::open_in_dir(active_generation_path(&corrupt_copy))
        .unwrap()
        .validate_checksum()
        .unwrap();
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
fn incompatible_zstd_generation_rebuilds_from_sources_without_cloning_the_slot() {
    let fixture = RecoveryFixture::new();
    let pointer_path = fixture.root.join("active-generation.json");
    let pointer_before = fs::read(&pointer_path).unwrap();
    let old_generation_path = active_generation_path(&fixture.root);
    let meta_path = old_generation_path.join("meta.json");
    let mut meta: serde_json::Value =
        serde_json::from_slice(&fs::read(&meta_path).unwrap()).unwrap();
    meta["index_settings"]["docstore_compression"] =
        serde_json::Value::String("zstd(compression_level=1)".to_owned());
    meta["index_settings"]["docstore_blocksize"] = serde_json::Value::from(64 * 1024);
    fs::write(&meta_path, serde_json::to_vec(&meta).unwrap()).unwrap();
    assert!(matches!(
        VerifiedIndex::open(&fixture.root),
        Err(IndexError::IndexSettingsMismatch(_))
    ));

    let mut rebuild = GenerationWriter::open(&fixture.root, writer_options()).unwrap();
    assert!(rebuild.base_manifest().is_none());
    assert_eq!(fs::read(&pointer_path).unwrap(), pointer_before);
    let candidates = inactive_generation_directories(&fixture.root);
    assert_eq!(candidates.len(), 1);
    let candidate = Index::open_in_dir(&candidates[0]).unwrap();
    assert!(candidate.load_metas().unwrap().segments.is_empty());
    assert_eq!(candidate.settings().docstore_compression, Compressor::Lz4);
    assert_eq!(candidate.settings().docstore_blocksize, 16 * 1024);
    drop(candidate);

    let source = source();
    rebuild.begin_source(source.clone()).unwrap();
    rebuild
        .add_core_record(document(&source, CANDIDATE_BODY))
        .unwrap();
    rebuild.certify_source(certificate(&source, 2)).unwrap();
    let receipt = rebuild.commit(|_| true).unwrap();

    assert_ne!(fs::read(&pointer_path).unwrap(), pointer_before);
    assert!(!old_generation_path.exists());
    assert_generation(
        &fixture.root,
        &receipt.generation_id,
        "candidate",
        "previous",
    );
}

#[test]
#[ignore = "requires scripts/source-backed-recovery/run-linux-fault-tests.sh"]
fn inactive_generation_and_atomic_pointer_process_death_matrix() {
    let shim = required_fault_shim();
    let cases = [
        FaultCase::stop("sync", "manifest_temp", "after", None, Visibility::Old),
        FaultCase::stop("rename", "manifest_final", "before", None, Visibility::Old),
        FaultCase::stop("rename", "manifest_final", "after", None, Visibility::Old),
        FaultCase::stop(
            "sync",
            "manifest_dir",
            "after",
            Some("manifest_rename"),
            Visibility::Old,
        ),
        FaultCase::stop(
            "sync",
            "generation_temp",
            "after",
            Some("manifest_rename"),
            Visibility::Old,
        ),
        FaultCase::stop(
            "rename",
            "generation_meta_final",
            "before",
            Some("manifest_rename"),
            Visibility::Old,
        ),
        FaultCase::stop(
            "rename",
            "generation_meta_final",
            "after",
            Some("manifest_rename"),
            Visibility::Old,
        ),
        FaultCase::stop(
            "sync",
            "generation_dir",
            "after",
            Some("generation_meta_rename"),
            Visibility::Old,
        ),
        FaultCase::stop(
            "sync",
            "pointer_temp",
            "after",
            Some("generation_meta_rename"),
            Visibility::Old,
        ),
        FaultCase::stop(
            "rename",
            "pointer_final",
            "before",
            Some("generation_meta_rename"),
            Visibility::Old,
        ),
        FaultCase::stop(
            "rename",
            "pointer_final",
            "after",
            Some("generation_meta_rename"),
            Visibility::New,
        ),
        FaultCase::stop(
            "sync",
            "root_dir",
            "after",
            Some("pointer_rename"),
            Visibility::New,
        ),
    ];

    for case in cases {
        run_stopped_fault_case(&shim, case);
    }
}

#[test]
#[ignore = "requires scripts/source-backed-recovery/run-linux-fault-tests.sh"]
fn retry_after_pre_pointer_crash_reclaims_inactive_generation() {
    let shim = required_fault_shim();
    let fixture = RecoveryFixture::new();
    let pinned_reader = VerifiedIndex::open(&fixture.root).unwrap();
    let case = FaultCase::stop(
        "rename",
        "pointer_final",
        "before",
        Some("generation_meta_rename"),
        Visibility::Old,
    );
    let mut child = fixture.spawn_stopped_child("commit", Some((&shim, case)));
    fixture.kill_at_marker(&mut child);
    let inactive_before = inactive_generation_directories(&fixture.root);
    assert!(
        inactive_before.len() == 1,
        "the pre-pointer crash did not leave exactly one inactive generation: {inactive_before:?}"
    );
    let inactive_bytes_before = directory_file_bytes(&inactive_before[0]);
    assert!(
        inactive_bytes_before > 0,
        "the pre-pointer crash left an empty inactive generation: {inactive_before:?}"
    );
    assert_generation(
        &fixture.root,
        &fixture.baseline.generation_id,
        "previous",
        "candidate",
    );

    let pointer_path = fixture.root.join("active-generation.json");
    let pointer_before = fs::read(&pointer_path).unwrap();
    let active_path = active_generation_path(&fixture.root);
    let meta_path = active_path.join("meta.json");
    let meta_before = fs::read(&meta_path).unwrap();
    let opstamp_before = Index::open_in_dir(&active_path)
        .unwrap()
        .load_metas()
        .unwrap()
        .opstamp;
    let inventory = complete_inventory(&source(), 1, vec![source()]);
    let mut replay = GenerationWriter::open(&fixture.root, writer_options())
        .expect("preflight recovery must reclaim candidate files");
    replay
        .certify_complete_inventory(inventory.clone())
        .unwrap();
    stage_exact_replay(&mut replay);
    let receipt = replay
        .commit_with_complete_inventory_revalidation(|_| true, |current| current == &inventory)
        .expect("restored base source must remain an exact replay");
    assert_generation(
        &fixture.root,
        &receipt.generation_id,
        "previous",
        "candidate",
    );
    assert_eq!(receipt.generation_id, fixture.baseline.generation_id);
    assert_eq!(receipt.opstamp, fixture.baseline.opstamp);
    assert_eq!(receipt.opstamp, opstamp_before);
    assert_eq!(fs::read(pointer_path).unwrap(), pointer_before);
    assert_eq!(active_generation_path(&fixture.root), active_path);
    assert_eq!(fs::read(meta_path).unwrap(), meta_before);
    assert!(
        inactive_generation_directories(&fixture.root).is_empty(),
        "exact replay left an inactive generation"
    );
    assert!(
        inactive_before.iter().all(|path| !path.exists()),
        "exact replay did not reclaim every inactive generation: {inactive_before:?}"
    );
    assert!(
        atomic_temporary_files(&fixture.root).is_empty(),
        "retry left an abandoned root atomic-write file"
    );
    assert_reader_terms(&pinned_reader, "previous", "candidate");
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
            "pointer_temp",
            "ENOSPC",
            Some("generation_meta_rename"),
        ),
        FaultCase::fail(
            "sync",
            "pointer_temp",
            "EIO",
            Some("generation_meta_rename"),
        ),
        FaultCase::fail("sync", "index_data", "EIO", Some("generation_meta_rename")),
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
#[ignore = "requires scripts/source-backed-recovery/run-bounded-enospc-test.sh"]
fn actual_bounded_filesystem_enospc_preserves_previous_generation() {
    assert_eq!(
        env::var(REAL_ENOSPC_ENV).as_deref(),
        Ok("1"),
        "actual ENOSPC test must run only inside the bounded-filesystem harness"
    );
    let fixture = RecoveryFixture::new();
    let fill_path = fixture.temp.path().join("actual-enospc-fill");
    let mut fill = File::create(&fill_path).unwrap();
    let block = vec![0_u8; 1024 * 1024];
    let mut filled_bytes = 0_u64;
    let fill_error = loop {
        match fill.write_all(&block) {
            Ok(()) => filled_bytes += block.len() as u64,
            Err(error) => break error,
        }
    };
    assert_eq!(
        fill_error.raw_os_error(),
        Some(28),
        "bounded filesystem did not report actual ENOSPC after {filled_bytes} bytes: {fill_error}"
    );
    drop(fill);

    let failure = try_staged_replacement(&fixture.root)
        .and_then(|writer| writer.commit(|_| true).map(|_| ()))
        .expect_err("generation publication unexpectedly succeeded on a full filesystem");
    assert!(
        index_error_has_enospc(&failure),
        "generation failure did not retain actual ENOSPC: {failure:?}\n{failure}"
    );

    fs::remove_file(fill_path).unwrap();
    assert_generation(
        &fixture.root,
        &fixture.baseline.generation_id,
        "previous",
        "candidate",
    );
    let receipt = staged_replacement(&fixture.root).commit(|_| true).unwrap();
    assert_generation(
        &fixture.root,
        &receipt.generation_id,
        "candidate",
        "previous",
    );
    eprintln!(
        "actual bounded-filesystem ENOSPC after {filled_bytes} fill bytes; prior generation preserved and retry published {}",
        receipt.generation_id
    );
}

#[test]
#[ignore = "requires scripts/source-backed-recovery/run-linux-fault-tests.sh"]
fn writer_reopen_reclaims_abandoned_atomic_write_files() {
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
    assert!(
        after.is_empty(),
        "writer reopen left abandoned atomic-write files: {after:?}"
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
fn retry_republishes_a_reclaimed_manifest_before_pointer_publication() {
    let shim = required_fault_shim();
    let fixture = RecoveryFixture::new();
    let first_crash = FaultCase::stop("rename", "manifest_final", "after", None, Visibility::Old);
    let mut child = fixture.spawn_stopped_child("commit", Some((&shim, first_crash)));
    fixture.kill_at_marker(&mut child);
    let baseline_manifest = format!("{}.json", fixture.baseline.generation_id);
    let manifest_directory = fixture.root.join("ctx-generations");
    let candidate_manifests = canonical_generation_manifests(&manifest_directory)
        .into_iter()
        .filter(|entry| entry.file_name() != OsStr::new(&baseline_manifest))
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    assert!(
        candidate_manifests.len() == 1,
        "first crash did not leave exactly one candidate manifest: {candidate_manifests:?}"
    );
    let candidate_manifest = &candidate_manifests[0];
    let candidate_manifest_bytes = fs::read(candidate_manifest).unwrap();
    assert_generation(
        &fixture.root,
        &fixture.baseline.generation_id,
        "previous",
        "candidate",
    );

    let retry_temp_fence = FaultCase::stop("sync", "manifest_temp", "after", None, Visibility::Old);
    let mut temp_retry = fixture.spawn_stopped_child("commit", Some((&shim, retry_temp_fence)));
    fixture.kill_at_marker(&mut temp_retry);
    assert!(
        !candidate_manifest.exists(),
        "writer preflight did not reclaim the unreferenced candidate manifest"
    );
    assert!(
        !atomic_temporary_files(&manifest_directory).is_empty(),
        "retry did not leave its synchronized manifest staging witness"
    );
    assert_generation(
        &fixture.root,
        &fixture.baseline.generation_id,
        "previous",
        "candidate",
    );

    let retry_publish_fence =
        FaultCase::stop("rename", "manifest_final", "after", None, Visibility::Old);
    let mut publish_retry =
        fixture.spawn_stopped_child("commit", Some((&shim, retry_publish_fence)));
    fixture.kill_at_marker(&mut publish_retry);
    assert_eq!(
        fs::read(candidate_manifest).unwrap(),
        candidate_manifest_bytes
    );
    assert_generation(
        &fixture.root,
        &fixture.baseline.generation_id,
        "previous",
        "candidate",
    );

    let receipt = staged_replacement(&fixture.root).commit(|_| true).unwrap();
    assert_generation(
        &fixture.root,
        &receipt.generation_id,
        "candidate",
        "previous",
    );
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
    writer.add_core_record(document(&source, body)).unwrap();
    writer
        .certify_source(certificate(&source, revision))
        .unwrap();
    writer.commit(|_| true).unwrap()
}

fn staged_replacement(root: &Path) -> GenerationWriter {
    try_staged_replacement(root).unwrap()
}

fn stage_exact_replay(writer: &mut GenerationWriter) {
    let base = writer.begin_source_append(source()).unwrap().clone();
    let frontier = base.frontier().unwrap();
    let replay = CertifiedSourceAppend::certify(
        &base,
        base.clone(),
        frontier.certified_prefix_bytes(),
        *frontier.certified_prefix_digest(),
    )
    .unwrap();
    writer.certify_source_append(replay).unwrap();
}

fn try_staged_replacement(root: &Path) -> std::result::Result<GenerationWriter, IndexError> {
    let source = source();
    let mut writer = GenerationWriter::open(root, writer_options())?;
    writer.begin_source(source.clone())?;
    writer.add_core_record(document(&source, CANDIDATE_BODY))?;
    writer.certify_source(certificate(&source, 2))?;
    Ok(writer)
}

fn complete_inventory(
    authority_source: &SourceKey,
    revision: u8,
    sources: Vec<SourceKey>,
) -> CertifiedSourceInventory {
    let observation = SourceInventoryObservation::new(
        authority_source.provider(),
        "provider-root",
        TypedKey::utf8("root-lineage").unwrap(),
        "tree-inventory-v1",
        vec![revision],
    )
    .unwrap();
    CertifiedSourceInventory::certify(observation.clone(), observation, "discovery-v1", sources)
        .unwrap()
}

fn inactive_generation_directories(root: &Path) -> Vec<PathBuf> {
    let pointer: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("active-generation.json")).unwrap()).unwrap();
    let retained = ["active", "previous"]
        .into_iter()
        .filter_map(|slot| pointer.get(slot))
        .filter_map(|slot| slot.get("directory"))
        .filter_map(serde_json::Value::as_str)
        .collect::<HashSet<_>>();
    let generations = root.join("index-generations");
    let mut inactive = fs::read_dir(generations)
        .unwrap()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|file_type| file_type.is_dir()))
        .filter(|entry| !retained.contains(entry.file_name().to_string_lossy().as_ref()))
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    inactive.sort();
    inactive
}

fn canonical_generation_manifests(directory: &Path) -> Vec<fs::DirEntry> {
    let mut manifests = fs::read_dir(directory)
        .unwrap()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|file_type| file_type.is_file()))
        .filter(|entry| {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                return false;
            };
            let Some(generation_id) = name.strip_suffix(".json") else {
                return false;
            };
            generation_id.len() == 64
                && generation_id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .collect::<Vec<_>>();
    manifests.sort_by_key(fs::DirEntry::file_name);
    manifests
}

#[test]
fn recovery_manifest_selector_excludes_integrity_receipts_and_temporaries() {
    let directory = tempdir().unwrap();
    let generation_id = "ab".repeat(32);
    let canonical = directory.path().join(format!("{generation_id}.json"));
    fs::write(&canonical, b"manifest").unwrap();
    fs::write(
        directory
            .path()
            .join(format!("generation-{generation_id}.integrity.json")),
        b"receipt",
    )
    .unwrap();
    fs::write(
        directory
            .path()
            .join(format!(".{generation_id}.json.temporary")),
        b"temporary",
    )
    .unwrap();
    fs::write(
        directory.path().join(format!("{generation_id}.JSON")),
        b"wrong case",
    )
    .unwrap();

    let manifests = canonical_generation_manifests(directory.path())
        .into_iter()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    assert_eq!(manifests, vec![canonical]);
}

fn directory_file_bytes(directory: &Path) -> u64 {
    fs::read_dir(directory)
        .unwrap()
        .filter_map(std::result::Result::ok)
        .map(|entry| {
            if entry.file_type().unwrap().is_dir() {
                directory_file_bytes(&entry.path())
            } else {
                entry.metadata().unwrap().len()
            }
        })
        .sum()
}

fn index_error_has_enospc(error: &IndexError) -> bool {
    let debug = format!("{error:?}");
    error_chain_has_enospc(error) || (debug.contains("code: 28") && debug.contains("StorageFull"))
}

fn error_chain_has_enospc(error: &(dyn StdError + 'static)) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.raw_os_error() == Some(28))
        || error.source().is_some_and(error_chain_has_enospc)
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
    CertifiedSource::certify_with_frontier(
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
        Some(
            SourceFrontier::new("jsonl-byte-offset", TypedKey::U64(100), 100, [revision; 32])
                .unwrap(),
        ),
    )
    .unwrap()
}

fn document(source: &SourceKey, body: &str) -> CoreRecord {
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
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source.clone(),
        1,
        "message",
        "primary",
        true,
        "codex-parser-v1",
        body,
    )
    .unwrap();
    record.provider_session_id = Some("session".to_owned());
    record.native_event_id = Some(TypedKey::U64(1));
    record.branch = Some("main".to_owned());
    record.occurred_at_unix_ms = Some(1_700_000_000_001);
    record.role = Some("user".to_owned());
    record.workspace = Some("ctx".to_owned());
    record.cwd = Some("/work/ctx".to_owned());
    record
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
    let active = active_generation_path(root);
    let path = fs::read_dir(&active)
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

fn active_generation_path(root: &Path) -> PathBuf {
    let pointer: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("active-generation.json")).unwrap()).unwrap();
    let directory = pointer
        .get("active")
        .and_then(|active| active.get("directory"))
        .and_then(serde_json::Value::as_str)
        .expect("active generation pointer has no directory");
    root.join("index-generations").join(directory)
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
