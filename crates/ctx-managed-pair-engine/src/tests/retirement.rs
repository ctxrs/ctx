use super::*;

#[test]
fn same_byte_projection_finishes_pending_repair_before_retirement() {
    let fixture = Fixture::new();
    let old = fixture.candidate("old", 1, b"old-core", b"old-companion", b"old-marker");
    let next = fixture.candidate("unified", 2, b"unified", b"unified", b"new-marker");
    let verifier = TestVerifier::new([
        (old.envelope.clone(), old.identity.clone()),
        (next.envelope.clone(), next.identity.clone()),
    ]);
    apply(&fixture, &old, &verifier);
    under_installation_lock(&fixture.install, || {
        stage_managed_pair_under_installation_lock(&fixture.install, &input(&next), &verifier)
            .unwrap();
        assert!(retire_managed_pair_files_under_installation_lock(&fixture.install).is_err());
        let layout = filesystem::Layout::open(&fixture.install, false).unwrap();
        assert_eq!(
            fs::read(layout.target(filesystem::Slot::Core)).unwrap(),
            old.core
        );
        assert_eq!(
            fs::read(layout.target(filesystem::Slot::Companion)).unwrap(),
            old.companion
        );
        let result =
            resume_pending_managed_pair_under_installation_lock(&fixture.install, &verifier)
                .unwrap()
                .unwrap();
        assert_eq!(result.identity(), &next.identity);
        assert!(matches!(
            inspect_managed_pair_under_installation_lock(&fixture.install, &verifier).unwrap(),
            ManagedPairInstallationStatus::Healthy { .. }
        ));
        retire_managed_pair_files_under_installation_lock(&fixture.install).unwrap();
        assert_eq!(
            fs::read(layout.target(filesystem::Slot::Core)).unwrap(),
            next.core
        );
        assert_eq!(
            fs::read(layout.target(filesystem::Slot::Marker)).unwrap(),
            next.marker
        );
        for slot in [
            filesystem::Slot::Companion,
            filesystem::Slot::State,
            filesystem::Slot::Envelope,
        ] {
            assert!(!layout.target(slot).exists());
        }
    });
}

#[test]
fn retirement_retries_each_interruption_without_touching_other_files() {
    for stop in [
        "managed-pair companion component",
        "managed-pair state marker",
        "managed-pair signed envelope",
    ] {
        let fixture = Fixture::new();
        let candidate = fixture.candidate("unified", 1, b"unified", b"unified", b"marker");
        let verifier =
            TestVerifier::new([(candidate.envelope.clone(), candidate.identity.clone())]);
        apply(&fixture, &candidate, &verifier);
        let keep = fixture.install.join("libexec/keep");
        fs::write(&keep, b"unrelated program").unwrap();
        let data = fixture._temp.path().join("pro");
        fs::create_dir(&data).unwrap();
        fs::write(data.join("graph"), b"encrypted user graph").unwrap();
        fs::write(data.join("key"), b"synthetic legacy key state").unwrap();
        under_installation_lock(&fixture.install, || {
            let error = crate::retirement::retire_with_fault(&fixture.install, &mut |point| {
                if point == stop {
                    Err(anyhow!("interrupted after {point}"))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
            assert!(error.to_string().contains(stop));
            retire_managed_pair_files_under_installation_lock(&fixture.install).unwrap();
            retire_managed_pair_files_under_installation_lock(&fixture.install).unwrap();
        });
        assert_eq!(fs::read(keep).unwrap(), b"unrelated program");
        assert_eq!(
            fs::read(data.join("graph")).unwrap(),
            b"encrypted user graph"
        );
        assert_eq!(
            fs::read(data.join("key")).unwrap(),
            b"synthetic legacy key state"
        );
    }
}

#[cfg(unix)]
#[test]
fn retirement_rejects_substituted_link_before_deleting_any_slot() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("unified", 1, b"unified", b"unified", b"marker");
    let verifier = TestVerifier::new([(candidate.envelope.clone(), candidate.identity.clone())]);
    apply(&fixture, &candidate, &verifier);
    let state = fixture.install.join(MANAGED_PAIR_STATE_RELATIVE_PATH);
    fs::remove_file(&state).unwrap();
    std::os::unix::fs::symlink(
        candidate.root.join("share/ctx/managed-pair-envelope.json"),
        &state,
    )
    .unwrap();
    under_installation_lock(&fixture.install, || {
        assert!(retire_managed_pair_files_under_installation_lock(&fixture.install).is_err());
    });
    assert_eq!(
        fs::read(fixture.install.join("libexec/ctx-pro")).unwrap(),
        candidate.companion
    );
}
