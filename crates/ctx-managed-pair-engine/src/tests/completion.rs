use super::*;

fn completion_fixture() -> (Fixture, Candidate, TestVerifier) {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("completion", 1, b"new-core", b"new-companion", b"marker");
    let verifier = TestVerifier::new([(candidate.envelope.clone(), candidate.identity.clone())]);
    (fixture, candidate, verifier)
}

#[test]
fn committed_pair_survives_candidate_directory_cleanup_io_failure() {
    let (fixture, candidate, verifier) = completion_fixture();
    let obstruction =
        filesystem::apply_candidate_root(&fixture.install).join("share/ctx/preserved-file");
    let result = apply_with_fault(&fixture, &candidate, &verifier, &|point| {
        if point == "publish_state" {
            fs::write(&obstruction, b"unrelated retained bytes").unwrap();
        }
    });

    let outcome = result.expect("verified committed pair must survive disposable cleanup failure");
    assert!(matches!(outcome, ManagedPairApplyOutcome::Applied { .. }));
    assert_eq!(outcome.identity(), &candidate.identity);
    assert_active(&fixture, &candidate, &verifier);
    assert_eq!(fs::read(&obstruction).unwrap(), b"unrelated retained bytes");
    assert!(!fixture
        .install
        .join(MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH)
        .exists());

    fs::remove_file(&obstruction).unwrap();
    let retried = apply(&fixture, &candidate, &verifier);
    assert!(matches!(
        retried,
        ManagedPairApplyOutcome::AlreadyCurrent { .. }
    ));
    assert_cleanup(&fixture, outcome.attempt_id().unwrap());
}

#[cfg(unix)]
#[test]
fn committed_pair_survives_pending_unlink_failure_and_resumes_same_attempt() {
    use std::os::unix::fs::PermissionsExt as _;

    let (fixture, candidate, verifier) = completion_fixture();
    let bin = fixture.install.join("bin");
    let result = apply_with_fault(&fixture, &candidate, &verifier, &|point| {
        if point == "publish_state" {
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o500)).unwrap();
        }
    });
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();

    let outcome = result.expect("verified committed pair must survive pending unlink denial");
    assert!(matches!(outcome, ManagedPairApplyOutcome::Applied { .. }));
    assert_active(&fixture, &candidate, &verifier);
    let pending_path = fixture
        .install
        .join(MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH);
    assert!(
        pending_path.is_file(),
        "run this permissions test as an unprivileged user"
    );
    let pending: serde_json::Value =
        serde_json::from_slice(&fs::read(&pending_path).unwrap()).unwrap();
    assert_eq!(pending["attempt_id"].as_str(), outcome.attempt_id());

    let resumed = under_installation_lock(&fixture.install, || {
        resume_pending_managed_pair_under_installation_lock(&fixture.install, &verifier)
            .unwrap()
            .unwrap()
    });
    assert!(matches!(resumed, ManagedPairApplyOutcome::Resumed { .. }));
    assert_eq!(resumed.attempt_id(), outcome.attempt_id());
    assert_active(&fixture, &candidate, &verifier);
    assert_cleanup(&fixture, outcome.attempt_id().unwrap());
}

#[test]
fn committed_pair_slot_corruption_is_never_a_cleanup_warning() {
    for missing in [false, true] {
        for slot in [
            filesystem::Slot::Core,
            filesystem::Slot::Companion,
            filesystem::Slot::Envelope,
            filesystem::Slot::Marker,
            filesystem::Slot::State,
        ] {
            let (fixture, candidate, verifier) = completion_fixture();
            let obstruction =
                filesystem::apply_candidate_root(&fixture.install).join("share/ctx/preserved-file");
            let result = apply_with_fault(&fixture, &candidate, &verifier, &|point| {
                if point == "publish_state" {
                    fs::write(&obstruction, b"unrelated retained bytes").unwrap();
                    let layout = filesystem::Layout::open(&fixture.install, false).unwrap();
                    if missing {
                        fs::remove_file(layout.target(slot)).unwrap();
                    } else {
                        fs::write(layout.target(slot), b"substituted").unwrap();
                    }
                }
            });
            assert!(
                result.is_err(),
                "{} integrity failure must remain an error",
                slot.label()
            );
            assert!(fixture
                .install
                .join(MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH)
                .is_file());
            let retained = filesystem::Layout::open_candidate(&filesystem::apply_candidate_root(
                &fixture.install,
            ))
            .unwrap();
            assert_eq!(
                fs::read(retained.target(filesystem::Slot::Core)).unwrap(),
                candidate.core
            );
            assert_eq!(fs::read(&obstruction).unwrap(), b"unrelated retained bytes");
        }
    }
}

#[test]
fn replaced_pending_record_is_not_disposable_cleanup() {
    let (fixture, candidate, verifier) = completion_fixture();
    let pending_path = fixture
        .install
        .join(MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH);
    let result = apply_with_fault(&fixture, &candidate, &verifier, &|point| {
        if point == "publish_state" {
            let mut pending: serde_json::Value =
                serde_json::from_slice(&fs::read(&pending_path).unwrap()).unwrap();
            pending["attempt_id"] = serde_json::json!("00000000000000000000000000000000");
            fs::write(&pending_path, serde_json::to_vec(&pending).unwrap()).unwrap();
        }
    });
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("refusing to remove a replaced"));
    assert_active(&fixture, &candidate, &verifier);
    let pending: serde_json::Value =
        serde_json::from_slice(&fs::read(&pending_path).unwrap()).unwrap();
    assert_eq!(pending["attempt_id"], "00000000000000000000000000000000");
}

#[test]
fn unsafe_retained_candidate_is_not_a_cleanup_warning() {
    let (fixture, candidate, verifier) = completion_fixture();
    let result = apply_with_fault(&fixture, &candidate, &verifier, &|point| {
        if point == "publish_state" {
            let retained = filesystem::Layout::open_candidate(&filesystem::apply_candidate_root(
                &fixture.install,
            ))
            .unwrap();
            let path = retained.target(filesystem::Slot::Core);
            fs::remove_file(&path).unwrap();
            fs::create_dir(&path).unwrap();
        }
    });
    assert!(
        result.is_err(),
        "native slot inspection/type failures must remain errors"
    );
    assert_active(&fixture, &candidate, &verifier);
}

#[test]
fn late_cleanup_obstruction_preserves_installed_pair_but_can_leave_remnants() {
    let (fixture, candidate, verifier) = completion_fixture();
    let retained = filesystem::apply_candidate_root(&fixture.install);
    let obstruction = retained.join("bin/preserved-file");
    let result = apply_with_fault(&fixture, &candidate, &verifier, &|point| {
        if point == "publish_state" {
            fs::write(&obstruction, b"keep").unwrap();
        }
    });
    assert!(matches!(
        result.unwrap(),
        ManagedPairApplyOutcome::Applied { .. }
    ));
    assert_active(&fixture, &candidate, &verifier);
    assert!(!retained.join("share").exists());
    assert!(!retained.join("libexec").exists());
    assert_eq!(fs::read(&obstruction).unwrap(), b"keep");
    // Existing orphan discovery treats a missing child directory as absence.
    // Installed success does not promise convergence of every cleanup remnant.
    assert!(matches!(
        apply(&fixture, &candidate, &verifier),
        ManagedPairApplyOutcome::AlreadyCurrent { .. }
    ));
    assert_eq!(fs::read(&obstruction).unwrap(), b"keep");
}

#[cfg(unix)]
#[test]
fn substituted_installation_directory_is_not_a_cleanup_warning() {
    let (fixture, candidate, verifier) = completion_fixture();
    let result = apply_with_fault(&fixture, &candidate, &verifier, &|point| {
        if point == "publish_state" {
            fs::rename(fixture.install.join("bin"), fixture.install.join("old-bin")).unwrap();
            fs::create_dir(fixture.install.join("bin")).unwrap();
        }
    });
    assert!(result.is_err());
    assert_eq!(
        fs::read(fixture.install.join("old-bin/ctx")).unwrap(),
        candidate.core
    );
    assert!(!fixture.install.join("bin/ctx").exists());
}

#[test]
fn active_verification_io_failure_is_not_disposable_cleanup() {
    use std::sync::atomic::AtomicBool;

    struct FailingVerifier<'a> {
        verifier: &'a TestVerifier,
        fail: AtomicBool,
    }
    impl ManagedPairVerifier for FailingVerifier<'_> {
        fn verify_signed_envelope(&self, bytes: &[u8]) -> Result<VerifiedManagedPairIdentity> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "fixture verification I/O failure",
                )
                .into());
            }
            self.verifier.verify_signed_envelope(bytes)
        }
    }
    let (fixture, candidate, verifier) = completion_fixture();
    let failing = FailingVerifier {
        verifier: &verifier,
        fail: AtomicBool::new(false),
    };
    let result = under_installation_lock(&fixture.install, || {
        crate::fix_forward::apply_or_resume_with_fault(
            &fixture.install,
            &input(&candidate),
            &failing,
            &|point| {
                if point == "publish_state" {
                    failing.fail.store(true, Ordering::SeqCst);
                }
            },
        )
    });
    assert!(format!("{:#}", result.unwrap_err()).contains("fixture verification I/O failure"));
    assert!(fixture
        .install
        .join(MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH)
        .is_file());
}
