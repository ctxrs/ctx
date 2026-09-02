use std::{
    cell::Cell,
    fs::{self, OpenOptions},
    panic::{catch_unwind, AssertUnwindSafe},
    path::PathBuf,
};

use fs2::FileExt as _;

use super::*;

#[test]
fn fresh_apply_uses_minimal_active_record_under_canonical_lock() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate(
        "fresh",
        1,
        b"fresh-core",
        b"fresh-companion",
        br#"{"kind":"managed","version":1}"#,
    );
    let verifier = TestVerifier::new([(candidate.envelope.clone(), candidate.identity.clone())]);
    let pending_checked = Cell::new(false);

    let outcome = apply_with_fault(&fixture, &candidate, &verifier, &|point| {
        if point != "pending" {
            return;
        }
        pending_checked.set(true);
        let pending: serde_json::Value = serde_json::from_slice(
            &fs::read(
                fixture
                    .install
                    .join(MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH),
            )
            .unwrap(),
        )
        .unwrap();
        let mut keys: Vec<_> = pending.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "attempt_id",
                "candidate_envelope_identity",
                "candidate_marker_identity",
                "schema",
            ]
        );
        let contender = OpenOptions::new()
            .read(true)
            .write(true)
            .open(
                fixture
                    .install
                    .join(MANAGED_PAIR_INSTALLATION_LOCK_RELATIVE_PATH),
            )
            .unwrap();
        assert!(contender.try_lock_exclusive().is_err());
    })
    .unwrap();

    assert!(pending_checked.get());
    assert!(matches!(outcome, ManagedPairApplyOutcome::Applied { .. }));
    assert_active(&fixture, &candidate, &verifier);
    assert_cleanup(&fixture, outcome.attempt_id().unwrap());
}

#[test]
fn already_current_is_a_no_op_and_marker_change_is_repaired() {
    let fixture = Fixture::new();
    let first = fixture.candidate("same", 1, b"core", b"companion", b"marker-one");
    let verifier = TestVerifier::new([(first.envelope.clone(), first.identity.clone())]);
    apply(&fixture, &first, &verifier);
    let calls_before = verifier.calls();
    let outcome = apply(&fixture, &first, &verifier);
    assert!(matches!(
        outcome,
        ManagedPairApplyOutcome::AlreadyCurrent { .. }
    ));
    assert!(verifier.calls() > calls_before);

    let marker_update = fixture.candidate("same-copy", 1, b"core", b"companion", b"marker-two");
    fs::write(
        marker_update.root.join(MANAGED_PAIR_ENVELOPE_RELATIVE_PATH),
        &first.envelope,
    )
    .unwrap();
    ctx_history_platform::platform_security::restrict_private_file(
        &marker_update.root.join(MANAGED_PAIR_ENVELOPE_RELATIVE_PATH),
    )
    .unwrap();
    let marker_update = Candidate {
        envelope: first.envelope.clone(),
        identity: first.identity.clone(),
        ..marker_update
    };
    let repaired = apply(&fixture, &marker_update, &verifier);
    assert!(matches!(repaired, ManagedPairApplyOutcome::Applied { .. }));
    assert_active(&fixture, &marker_update, &verifier);
}

#[test]
fn update_publishes_envelope_companion_marker_core_then_state() {
    let fixture = Fixture::new();
    let old = fixture.candidate("old", 1, b"old-core", b"old-pro", b"old-marker");
    let new = fixture.candidate("new", 2, b"new-core", b"new-pro", b"new-marker");
    let verifier = TestVerifier::new([
        (old.envelope.clone(), old.identity.clone()),
        (new.envelope.clone(), new.identity.clone()),
    ]);
    apply(&fixture, &old, &verifier);
    let observed = std::cell::RefCell::new(Vec::new());

    apply_with_fault(&fixture, &new, &verifier, &|point| {
        if point.starts_with("publish_") {
            observed.borrow_mut().push(point.to_owned());
        }
    })
    .unwrap();
    assert_eq!(
        *observed.borrow(),
        [
            "publish_envelope",
            "publish_companion",
            "publish_marker",
            "publish_core",
            "publish_state",
        ]
    );
    assert_active(&fixture, &new, &verifier);
}

#[test]
fn crash_after_each_publication_boundary_resumes_retained_candidate() {
    for boundary in [
        "pending",
        "publish_envelope",
        "publish_companion",
        "publish_marker",
        "publish_core",
        "publish_state",
    ] {
        let fixture = Fixture::new();
        let old = fixture.candidate(
            &format!("old-{boundary}"),
            1,
            b"old-core",
            b"old-pro",
            b"old-marker",
        );
        let new = fixture.candidate(
            &format!("new-{boundary}"),
            2,
            b"new-core",
            b"new-pro",
            b"new-marker",
        );
        let unrelated = fixture.candidate(
            &format!("unrelated-{boundary}"),
            3,
            b"other-core",
            b"other-pro",
            b"other-marker",
        );
        let verifier = TestVerifier::new([
            (old.envelope.clone(), old.identity.clone()),
            (new.envelope.clone(), new.identity.clone()),
            (unrelated.envelope.clone(), unrelated.identity.clone()),
        ]);
        apply(&fixture, &old, &verifier);

        let crashed = catch_unwind(AssertUnwindSafe(|| {
            apply_with_fault(&fixture, &new, &verifier, &|point| {
                if point == boundary {
                    panic!("simulated crash after {boundary}");
                }
            })
            .unwrap();
        }));
        assert!(crashed.is_err(), "fault did not fire at {boundary}");
        let pending: serde_json::Value = serde_json::from_slice(
            &fs::read(
                fixture
                    .install
                    .join(MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH),
            )
            .unwrap(),
        )
        .unwrap();
        let attempt_id = pending["attempt_id"].as_str().unwrap().to_owned();

        // A resume ignores newly supplied paths and uses the retained attempt.
        let outcome = apply(&fixture, &unrelated, &verifier);
        assert!(matches!(outcome, ManagedPairApplyOutcome::Resumed { .. }));
        assert_eq!(outcome.attempt_id(), Some(attempt_id.as_str()));
        assert_active(&fixture, &new, &verifier);
        assert_cleanup(&fixture, &attempt_id);
    }
}

#[test]
fn identical_signed_identity_repairs_each_fixed_slot_and_state() {
    for damaged in filesystem::Slot::ALL {
        let fixture = Fixture::new();
        let candidate = fixture.candidate(
            &format!("repair-{}", damaged.label().replace(' ', "-")),
            7,
            b"repair-core",
            b"repair-pro",
            b"repair-marker",
        );
        let verifier =
            TestVerifier::new([(candidate.envelope.clone(), candidate.identity.clone())]);
        apply(&fixture, &candidate, &verifier);
        fs::remove_file(
            filesystem::Layout::open(&fixture.install, false)
                .unwrap()
                .target(damaged),
        )
        .unwrap();

        let outcome = apply(&fixture, &candidate, &verifier);
        assert!(matches!(outcome, ManagedPairApplyOutcome::Applied { .. }));
        assert_active(&fixture, &candidate, &verifier);
    }

    let fixture = Fixture::new();
    let candidate = fixture.candidate("repair-bad-state", 7, b"core", b"pro", b"marker");
    let verifier = TestVerifier::new([(candidate.envelope.clone(), candidate.identity.clone())]);
    apply(&fixture, &candidate, &verifier);
    fs::write(
        fixture.install.join(MANAGED_PAIR_STATE_RELATIVE_PATH),
        b"not-json",
    )
    .unwrap();
    ctx_history_platform::platform_security::restrict_private_file(
        &fixture.install.join(MANAGED_PAIR_STATE_RELATIVE_PATH),
    )
    .unwrap();
    apply(&fixture, &candidate, &verifier);
    assert_active(&fixture, &candidate, &verifier);
}

#[test]
fn rollback_and_changed_same_generation_are_rejected() {
    let fixture = Fixture::new();
    let current = fixture.candidate("current", 5, b"current-core", b"current-pro", b"marker");
    let older = fixture.candidate("older", 4, b"older-core", b"older-pro", b"marker");
    let rebound = fixture.candidate("rebound", 5, b"other-core", b"other-pro", b"marker");
    let verifier = TestVerifier::new([
        (current.envelope.clone(), current.identity.clone()),
        (older.envelope.clone(), older.identity.clone()),
        (rebound.envelope.clone(), rebound.identity.clone()),
    ]);
    apply(&fixture, &current, &verifier);

    for rejected in [&older, &rebound] {
        let error = under_installation_lock(&fixture.install, || {
            apply_or_resume_managed_pair_under_installation_lock(
                &fixture.install,
                &input(rejected),
                &verifier,
            )
            .unwrap_err()
        });
        assert!(
            error.to_string().contains("rollback generation")
                || error.to_string().contains("without advancing")
        );
    }
    assert_active(&fixture, &current, &verifier);
}

#[cfg(unix)]
#[test]
fn rejects_unsafe_input_and_fixed_paths() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let candidate = fixture.candidate("unsafe", 1, b"core", b"pro", b"marker");
    let verifier = TestVerifier::new([(candidate.envelope.clone(), candidate.identity.clone())]);
    let valid = input(&candidate);
    let relative = ManagedPairApplyInput::new(
        valid.signed_envelope(),
        PathBuf::from("relative-core"),
        valid.companion(),
        valid.core_install_marker(),
    );
    assert!(under_installation_lock(&fixture.install, || {
        apply_or_resume_managed_pair_under_installation_lock(&fixture.install, &relative, &verifier)
    })
    .is_err());
    let traversal = ManagedPairApplyInput::new(
        valid.signed_envelope(),
        candidate.root.join("bin/../bin/ctx"),
        valid.companion(),
        valid.core_install_marker(),
    );
    assert!(under_installation_lock(&fixture.install, || {
        apply_or_resume_managed_pair_under_installation_lock(
            &fixture.install,
            &traversal,
            &verifier,
        )
    })
    .is_err());

    let core = valid.core().to_path_buf();
    let real = candidate.root.join("real-core");
    fs::rename(&core, &real).unwrap();
    symlink(&real, &core).unwrap();
    assert!(under_installation_lock(&fixture.install, || {
        apply_or_resume_managed_pair_under_installation_lock(&fixture.install, &valid, &verifier)
    })
    .is_err());

    fs::remove_file(&core).unwrap();
    fs::rename(&real, &core).unwrap();
    fs::hard_link(&core, candidate.root.join("core-alias")).unwrap();
    assert!(under_installation_lock(&fixture.install, || {
        apply_or_resume_managed_pair_under_installation_lock(&fixture.install, &valid, &verifier)
    })
    .is_err());

    fs::remove_file(candidate.root.join("core-alias")).unwrap();
    apply(&fixture, &candidate, &verifier);
    let installed_core = fixture.install.join("bin/ctx");
    let displaced = fixture.install.join("bin/displaced-ctx");
    fs::rename(&installed_core, &displaced).unwrap();
    symlink(&displaced, &installed_core).unwrap();
    assert!(under_installation_lock(&fixture.install, || {
        apply_or_resume_managed_pair_under_installation_lock(&fixture.install, &valid, &verifier)
    })
    .is_err());
}

#[test]
fn orphan_candidate_is_cleaned_before_a_new_attempt() {
    let fixture = Fixture::new();
    filesystem::Layout::open(&fixture.install, true).unwrap();
    let orphan = filesystem::create_apply_candidate(&fixture.install).unwrap();
    assert!(orphan.is_dir());
    let candidate = fixture.candidate("cleanup", 1, b"core", b"pro", b"marker");
    let verifier = TestVerifier::new([(candidate.envelope.clone(), candidate.identity.clone())]);

    let outcome = apply(&fixture, &candidate, &verifier);
    assert_active(&fixture, &candidate, &verifier);
    assert_cleanup(&fixture, outcome.attempt_id().unwrap());
}

#[test]
fn narrow_routing_apis_preserve_generic_pending_and_stage_without_publication() {
    let fixture = Fixture::new();
    filesystem::Layout::open(&fixture.install, true).unwrap();
    let active = fixture
        .install
        .join(MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH);
    fs::write(&active, br#"{"schema_version":1,"kind":"generic-upgrade"}"#).unwrap();
    ctx_history_platform::platform_security::restrict_private_file(&active).unwrap();
    let candidate = fixture.candidate("routing", 1, b"core", b"pro", b"marker");
    let verifier = TestVerifier::new([(candidate.envelope.clone(), candidate.identity.clone())]);

    assert!(under_installation_lock(&fixture.install, || {
        resume_pending_managed_pair_under_installation_lock(&fixture.install, &verifier).unwrap()
    })
    .is_none());
    assert!(active.is_file());
    fs::remove_file(&active).unwrap();
    assert_eq!(
        under_installation_lock(&fixture.install, || {
            inspect_managed_pair_under_installation_lock(&fixture.install, &verifier).unwrap()
        }),
        ManagedPairInstallationStatus::Absent
    );

    let staged = under_installation_lock(&fixture.install, || {
        stage_managed_pair_under_installation_lock(&fixture.install, &input(&candidate), &verifier)
            .unwrap()
    });
    let ManagedPairStageOutcome::Staged {
        attempt_id,
        identity,
        retained_core,
    } = staged
    else {
        panic!("candidate was not staged")
    };
    assert_eq!(identity, candidate.identity);
    assert_eq!(fs::read(retained_core).unwrap(), candidate.core);
    assert!(active.is_file());
    assert!(!fixture
        .install
        .join(if cfg!(windows) {
            "bin/ctx.exe"
        } else {
            "bin/ctx"
        })
        .exists());

    let resumed = under_installation_lock(&fixture.install, || {
        resume_pending_managed_pair_under_installation_lock(&fixture.install, &verifier)
            .unwrap()
            .unwrap()
    });
    assert!(matches!(resumed, ManagedPairApplyOutcome::Resumed { .. }));
    assert_eq!(resumed.attempt_id(), Some(attempt_id.as_str()));
    assert!(matches!(
        under_installation_lock(&fixture.install, || {
            inspect_managed_pair_under_installation_lock(&fixture.install, &verifier).unwrap()
        }),
        ManagedPairInstallationStatus::Healthy { .. }
    ));

    fs::write(
        fixture.install.join(MANAGED_PAIR_STATE_RELATIVE_PATH),
        b"damaged",
    )
    .unwrap();
    assert_eq!(
        under_installation_lock(&fixture.install, || {
            inspect_managed_pair_under_installation_lock(&fixture.install, &verifier).unwrap()
        }),
        ManagedPairInstallationStatus::RepairRequired
    );
}
