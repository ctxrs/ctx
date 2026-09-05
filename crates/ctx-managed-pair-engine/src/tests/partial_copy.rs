use super::*;

fn pending_after_publication(
    fixture: &Fixture,
    candidate: &Candidate,
    verifier: &TestVerifier,
) -> String {
    let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        apply_with_fault(fixture, candidate, verifier, &|point| {
            if point == "publish_state" {
                panic!("interrupt before pending cleanup");
            }
        })
    }));
    assert!(interrupted.is_err());
    let pending: serde_json::Value = serde_json::from_slice(
        &fs::read(
            fixture
                .install
                .join(MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH),
        )
        .unwrap(),
    )
    .unwrap();
    pending["attempt_id"].as_str().unwrap().to_owned()
}

fn write_private(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    ctx_history_platform::platform_security::restrict_private_file(path).unwrap();
}

#[test]
fn partial_publication_copies_resume_each_slot_from_retained_candidate() {
    for slot in filesystem::Slot::ALL {
        // The last case exercises reuse/cleanup of a complete temporary too.
        for length in [0, 1, 5, usize::MAX] {
            for target_current in [false, true] {
                let fixture = Fixture::new();
                let candidate = fixture.candidate(
                    "partial",
                    1,
                    b"new-core-bytes",
                    b"new-pro-bytes",
                    b"new-marker",
                );
                let verifier =
                    TestVerifier::new([(candidate.envelope.clone(), candidate.identity.clone())]);
                let attempt = pending_after_publication(&fixture, &candidate, &verifier);
                let layout = filesystem::Layout::open(&fixture.install, false).unwrap();
                let target = layout.target(slot);
                let bytes = fs::read(&target).unwrap();
                let length = length.min(bytes.len());
                let temporary = layout.staged(slot, &attempt);
                write_private(&temporary, &bytes[..length]);
                if !target_current {
                    fs::remove_file(&target).unwrap();
                }
                let unrelated = layout.staged(slot, "00000000000000000000000000000000");
                write_private(&unrelated, b"unrelated attempt");
                // Recovery must use retained inputs, with no new download/source access.
                fs::remove_dir_all(&fixture.candidates).unwrap();
                let resumed = under_installation_lock(&fixture.install, || {
                    resume_pending_managed_pair_under_installation_lock(&fixture.install, &verifier)
                });
                assert!(
                    resumed.is_ok(),
                    "slot={slot:?}, length={length}, target_current={target_current}: {resumed:?}"
                );
                let resumed = resumed.unwrap().unwrap();
                assert_eq!(resumed.attempt_id(), Some(attempt.as_str()));
                assert_eq!(fs::read(&target).unwrap(), bytes);
                assert_eq!(fs::read(&unrelated).unwrap(), b"unrelated attempt");
                assert_active(&fixture, &candidate, &verifier);
                assert_cleanup(&fixture, &attempt);
            }
        }
    }
}

#[test]
fn partial_publication_preserves_unrelated_or_unsafe_temporaries() {
    for kind in [
        "unrelated",
        "wrong-full",
        "oversized",
        "directory",
        "hardlink",
        "symlink",
    ] {
        for target_current in [false, true] {
            if kind == "symlink" && !cfg!(unix) {
                continue;
            }
            let fixture = Fixture::new();
            let candidate = fixture.candidate(
                "protected",
                1,
                b"new-core-bytes",
                b"new-pro-bytes",
                b"new-marker",
            );
            let verifier =
                TestVerifier::new([(candidate.envelope.clone(), candidate.identity.clone())]);
            let attempt = pending_after_publication(&fixture, &candidate, &verifier);
            let layout = filesystem::Layout::open(&fixture.install, false).unwrap();
            let slot = filesystem::Slot::Companion;
            let target = layout.target(slot);
            let temporary = layout.staged(slot, &attempt);
            let victim = fixture._temp.path().join("unrelated-file");
            write_private(&victim, &candidate.companion[..5]);
            match kind {
                "unrelated" => write_private(&temporary, b"other"),
                "wrong-full" => write_private(&temporary, &vec![b'x'; candidate.companion.len()]),
                "oversized" => write_private(
                    &temporary,
                    &[candidate.companion.as_slice(), b"extra"].concat(),
                ),
                "directory" => fs::create_dir(&temporary).unwrap(),
                "hardlink" => fs::hard_link(&victim, &temporary).unwrap(),
                #[cfg(unix)]
                "symlink" => std::os::unix::fs::symlink(&victim, &temporary).unwrap(),
                _ => unreachable!(),
            }
            if !target_current {
                fs::remove_file(&target).unwrap();
            }
            let pending_path = fixture
                .install
                .join(MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH);
            let pending = fs::read(&pending_path).unwrap();
            let before = fs::symlink_metadata(&temporary).unwrap();
            let before_bytes = (!before.is_dir()).then(|| fs::read(&temporary).unwrap());
            let victim_metadata = fs::metadata(&victim).unwrap();
            let result = under_installation_lock(&fixture.install, || {
                resume_pending_managed_pair_under_installation_lock(&fixture.install, &verifier)
            });
            assert!(
                result.is_err(),
                "kind={kind}, target_current={target_current}"
            );
            assert_eq!(fs::read(&victim).unwrap(), &candidate.companion[..5]);
            let after = fs::symlink_metadata(&temporary).unwrap();
            assert_eq!(before.file_type(), after.file_type());
            assert_eq!(before.len(), after.len());
            if let Some(bytes) = before_bytes {
                assert_eq!(fs::read(&temporary).unwrap(), bytes);
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt as _;
                assert_eq!(
                    (before.ino(), before.mode(), before.nlink()),
                    (after.ino(), after.mode(), after.nlink())
                );
                let after_victim = fs::metadata(&victim).unwrap();
                assert_eq!(
                    (
                        victim_metadata.ino(),
                        victim_metadata.mode(),
                        victim_metadata.nlink()
                    ),
                    (
                        after_victim.ino(),
                        after_victim.mode(),
                        after_victim.nlink()
                    )
                );
            }
            #[cfg(not(unix))]
            assert_eq!(
                victim_metadata.permissions().readonly(),
                fs::metadata(&victim).unwrap().permissions().readonly()
            );
            assert_eq!(fs::read(&pending_path).unwrap(), pending);
            assert_eq!(target.exists(), target_current);
            if target_current {
                assert_eq!(fs::read(&target).unwrap(), candidate.companion);
            }
        }
    }
}

#[test]
fn partial_publication_requires_verified_retained_source_before_removal() {
    for corrupt in [
        filesystem::Slot::Envelope,
        filesystem::Slot::Core,
        filesystem::Slot::Companion,
        filesystem::Slot::Marker,
    ] {
        let fixture = Fixture::new();
        let candidate = fixture.candidate(
            "corrupt",
            1,
            b"new-core-bytes",
            b"new-pro-bytes",
            b"new-marker",
        );
        let verifier =
            TestVerifier::new([(candidate.envelope.clone(), candidate.identity.clone())]);
        let attempt = pending_after_publication(&fixture, &candidate, &verifier);
        let layout = filesystem::Layout::open(&fixture.install, false).unwrap();
        let temporary = layout.staged(filesystem::Slot::Companion, &attempt);
        write_private(&temporary, &candidate.companion[..5]);
        let retained = layout.open_apply_candidate().unwrap();
        write_private(&retained.target(corrupt), b"corrupt-retained-source");
        let result = under_installation_lock(&fixture.install, || {
            resume_pending_managed_pair_under_installation_lock(&fixture.install, &verifier)
        });
        assert!(result.is_err());
        assert_eq!(fs::read(&temporary).unwrap(), &candidate.companion[..5]);
        assert!(layout.active_transaction().exists());
    }
}

#[cfg(target_os = "linux")]
#[test]
fn partial_publication_copy_process_death_resumes_retained_candidate() {
    use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
    const COPY_BUFFER: usize = 128 * 1024;
    const CHILD_ROOT: &str = "CTX_PAIR_COPY_DEATH_ROOT";
    let core = vec![b'c'; COPY_BUFFER * 3];
    let companion = vec![b'p'; COPY_BUFFER * 3];
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        let root = PathBuf::from(root);
        let verifier = TestVerifier::new([(
            b"signed-envelope:killed-copy:1\n".to_vec(),
            identity("killed-copy", 1, &core, &companion),
        )]);
        under_installation_lock(&root, || {
            resume_pending_managed_pair_under_installation_lock(&root, &verifier).unwrap();
        });
        panic!("copy should have been terminated by the file-size limit");
    }

    let fixture = Fixture::new();
    let candidate = fixture.candidate("killed-copy", 1, &core, &companion, b"marker");
    let verifier = TestVerifier::new([(candidate.envelope.clone(), candidate.identity.clone())]);
    let staged = under_installation_lock(&fixture.install, || {
        stage_managed_pair_under_installation_lock(&fixture.install, &input(&candidate), &verifier)
            .unwrap()
    });
    let ManagedPairStageOutcome::Staged { attempt_id, .. } = staged else {
        panic!("candidate should be staged");
    };
    fs::remove_dir_all(&fixture.candidates).unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "tests::partial_copy::partial_publication_copy_process_death_resumes_retained_candidate",
            "--nocapture",
        ])
        .env(CHILD_ROOT, &fixture.install);
    // The kernel terminates the child during copy_exact's second write. This
    // bypasses Rust error cleanup without adding a production fault hook.
    unsafe {
        child.pre_exec(|| {
            let limit = libc::rlimit {
                rlim_cur: COPY_BUFFER as libc::rlim_t,
                rlim_max: COPY_BUFFER as libc::rlim_t,
            };
            if libc::setrlimit(libc::RLIMIT_FSIZE, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            libc::signal(libc::SIGXFSZ, libc::SIG_DFL);
            let no_core = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::setrlimit(libc::RLIMIT_CORE, &no_core) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let output = child.output().unwrap();
    assert_eq!(output.status.signal(), Some(libc::SIGXFSZ), "{output:?}");
    let layout = filesystem::Layout::open(&fixture.install, false).unwrap();
    let temporary = layout.staged(filesystem::Slot::Companion, &attempt_id);
    let partial = fs::read(&temporary).unwrap();
    assert_eq!(partial.len(), COPY_BUFFER);
    assert_eq!(partial, companion[..COPY_BUFFER]);
    assert!(layout.active_transaction().exists());
    assert!(!layout.target(filesystem::Slot::Companion).exists());
    assert_eq!(
        fs::read(layout.target(filesystem::Slot::Envelope)).unwrap(),
        candidate.envelope
    );
    eprintln!(
        "kernel SIGXFSZ interrupted production copy: {} of {} bytes; retained pending attempt {}",
        partial.len(),
        companion.len(),
        attempt_id
    );

    let resumed = under_installation_lock(&fixture.install, || {
        resume_pending_managed_pair_under_installation_lock(&fixture.install, &verifier)
    })
    .unwrap()
    .unwrap();
    assert_eq!(resumed.attempt_id(), Some(attempt_id.as_str()));
    assert_active(&fixture, &candidate, &verifier);
    assert_cleanup(&fixture, &attempt_id);
}
