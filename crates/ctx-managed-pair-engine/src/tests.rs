use std::{
    collections::BTreeMap,
    fs,
    panic::{catch_unwind, AssertUnwindSafe},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use anyhow::{anyhow, Result};
use sha2::{Digest as _, Sha256};

use super::*;

struct TestVerifier {
    identities: Mutex<BTreeMap<Vec<u8>, VerifiedManagedPairIdentity>>,
    calls: AtomicUsize,
}

impl TestVerifier {
    fn new(entries: impl IntoIterator<Item = (Vec<u8>, VerifiedManagedPairIdentity)>) -> Self {
        Self {
            identities: Mutex::new(entries.into_iter().collect()),
            calls: AtomicUsize::new(0),
        }
    }

    fn add(&self, envelope: Vec<u8>, identity: VerifiedManagedPairIdentity) {
        self.identities.lock().unwrap().insert(envelope, identity);
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl ManagedPairVerifier for TestVerifier {
    fn verify_signed_envelope(
        &self,
        signed_envelope: &[u8],
    ) -> Result<VerifiedManagedPairIdentity> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.identities
            .lock()
            .unwrap()
            .get(signed_envelope)
            .cloned()
            .ok_or_else(|| anyhow!("test signature rejected"))
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    install: PathBuf,
    candidates: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        ctx_history_platform::platform_security::restrict_private_directory(temp.path()).unwrap();
        let install = temp.path().join("install");
        let candidates = temp.path().join("candidates");
        fs::create_dir(&candidates).unwrap();
        ctx_history_platform::platform_security::restrict_private_directory(&candidates).unwrap();
        Self {
            _temp: temp,
            install,
            candidates,
        }
    }

    fn candidate(
        &self,
        name: &str,
        generation: u64,
        core: &[u8],
        companion: &[u8],
    ) -> (PathBuf, Vec<u8>, VerifiedManagedPairIdentity) {
        let root = self.candidates.join(name);
        fs::create_dir(&root).unwrap();
        ctx_history_platform::platform_security::restrict_private_directory(&root).unwrap();
        for relative in ["bin", "libexec", "share", "share/ctx"] {
            let directory = root.join(relative);
            fs::create_dir(&directory).unwrap();
            ctx_history_platform::platform_security::restrict_private_directory(&directory)
                .unwrap();
        }
        let layout = Layout::open_candidate(&root).unwrap();
        fs::write(layout.target(Slot::Core), core).unwrap();
        fs::write(layout.target(Slot::Companion), companion).unwrap();
        let envelope = format!("signed-envelope:{name}:{generation}\n").into_bytes();
        fs::write(layout.target(Slot::Envelope), &envelope).unwrap();
        for slot in [Slot::Core, Slot::Companion, Slot::Envelope] {
            ctx_history_platform::platform_security::restrict_private_file(&layout.target(slot))
                .unwrap();
        }
        let identity = identity(name, generation, core, companion);
        (root, envelope, identity)
    }

    fn engine(&self) -> ManagedPairEngine {
        ManagedPairEngine::new(&self.install).unwrap()
    }
}

fn identity(
    name: &str,
    generation: u64,
    core: &[u8],
    companion: &[u8],
) -> VerifiedManagedPairIdentity {
    VerifiedManagedPairIdentity::new(
        name,
        current_target().unwrap(),
        generation,
        digest(format!("manifest:{name}:{generation}").as_bytes()),
        ManagedPairComponentIdentity::new(digest(core), core.len() as u64).unwrap(),
        ManagedPairComponentIdentity::new(digest(companion), companion.len() as u64).unwrap(),
    )
    .unwrap()
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn activate(
    engine: &ManagedPairEngine,
    root: &Path,
    verifier: &TestVerifier,
) -> ManagedPairPrepared {
    let prepared = engine.stage(root, verifier).unwrap();
    #[cfg(windows)]
    {
        assert!(matches!(
            engine.activate(&prepared, verifier).unwrap(),
            ManagedPairActivation::PostExitRequired { .. }
        ));
        // Native Windows tests use a dedicated child for the waiting contract.
        // The platform-neutral tests commit through the internal transaction.
        let layout = Layout::open(engine.install_root(), false).unwrap();
        let journal = journal::read(&layout).unwrap().unwrap();
        commit_transaction(engine, &layout, journal, verifier, &|_| {}).unwrap();
    }
    #[cfg(not(windows))]
    assert_eq!(
        engine.activate(&prepared, verifier).unwrap(),
        ManagedPairActivation::Activated
    );
    prepared
}

fn commit_transaction(
    engine: &ManagedPairEngine,
    layout: &Layout,
    journal: Journal,
    verifier: &TestVerifier,
    fault: &dyn Fn(&str),
) -> Result<()> {
    let attempt_id = journal.attempt_id.clone();
    engine.commit(layout, &attempt_id, verifier, fault)
}

fn assert_active_bytes(engine: &ManagedPairEngine, core: &[u8], companion: &[u8]) {
    let layout = Layout::open(engine.install_root(), false).unwrap();
    assert_eq!(fs::read(layout.target(Slot::Core)).unwrap(), core);
    assert_eq!(fs::read(layout.target(Slot::Companion)).unwrap(), companion);
    assert!(layout.target(Slot::Envelope).is_file());
    assert!(layout.target(Slot::State).is_file());
    assert!(!layout.journal().exists());
}

#[test]
fn stages_and_activates_the_complete_fixed_pair() {
    let fixture = Fixture::new();
    let core = b"core-v1";
    let companion = b"companion-v1";
    let (candidate, envelope, identity) = fixture.candidate("release-1", 1, core, companion);
    let verifier = TestVerifier::new([(envelope, identity.clone())]);
    let engine = fixture.engine();

    let prepared = engine.stage(&candidate, &verifier).unwrap();
    assert_eq!(prepared.identity(), &identity);
    let layout = Layout::open(engine.install_root(), false).unwrap();
    assert!(!layout.target(Slot::Core).exists());
    assert!(!layout.target(Slot::State).exists());
    assert!(layout.journal().is_file());

    #[cfg(not(windows))]
    assert_eq!(
        engine.activate(&prepared, &verifier).unwrap(),
        ManagedPairActivation::Activated
    );
    #[cfg(windows)]
    {
        let journal = journal::read(&layout).unwrap().unwrap();
        commit_transaction(&engine, &layout, journal, &verifier, &|_| {}).unwrap();
    }
    assert_active_bytes(&engine, core, companion);
    let state: serde_json::Value = serde_json::from_slice(
        &fs::read(
            Layout::open(engine.install_root(), false)
                .unwrap()
                .target(Slot::State),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(state.get("rollback_generation").is_none());
    assert_eq!(state["identity"]["rollback_generation"], 1);
    assert!(state["identity"]["core"]["sha256"].is_string());
    assert!(state["identity"]["companion"]["sha256"].is_string());
    assert_eq!(engine.validate_active(&verifier).unwrap(), identity);
    assert!(verifier.calls() >= 3, "stage and commit must reverify");
}

#[test]
fn post_exit_swapper_reopens_and_reverifies_before_commit() {
    let fixture = Fixture::new();
    let (candidate, envelope, identity) =
        fixture.candidate("release-swap", 1, b"core", b"companion");
    let verifier = TestVerifier::new([(envelope, identity)]);
    let engine = fixture.engine();
    engine.stage(&candidate, &verifier).unwrap();
    let calls_after_stage = verifier.calls();

    #[cfg(not(windows))]
    engine.run_post_exit_swapper(&verifier).unwrap();
    #[cfg(windows)]
    {
        let layout = Layout::open(engine.install_root(), false).unwrap();
        let mut journal = journal::read(&layout).unwrap().unwrap();
        journal.phase = Phase::Activating;
        journal.parent_pid = Some(u32::MAX);
        journal::write(&layout, &mut journal).unwrap();
        commit_transaction(&engine, &layout, journal, &verifier, &|_| {}).unwrap();
    }
    assert!(verifier.calls() > calls_after_stage);
    assert_active_bytes(&engine, b"core", b"companion");
}

#[test]
fn missing_component_or_trust_envelope_never_stages() {
    for missing in Slot::ALL.into_iter().filter(|slot| *slot != Slot::State) {
        let fixture = Fixture::new();
        let (candidate, envelope, identity) =
            fixture.candidate("release-missing", 1, b"core", b"companion");
        let verifier = TestVerifier::new([(envelope, identity)]);
        let layout = Layout::open_candidate(&candidate).unwrap();
        fs::remove_file(layout.target(missing)).unwrap();
        assert!(fixture.engine().stage(&candidate, &verifier).is_err());
        assert!(!fixture.install.join("bin/ctx").exists());
    }
}

#[test]
fn verifier_rejection_has_no_unsigned_bypass() {
    let fixture = Fixture::new();
    let (candidate, _, _) = fixture.candidate("release-reject", 1, b"core", b"companion");
    let verifier = TestVerifier::new([]);
    assert!(fixture.engine().stage(&candidate, &verifier).is_err());
    let layout = Layout::open(&fixture.install, false).unwrap();
    assert!(!layout.journal().exists());
    assert!(!layout.target(Slot::Core).exists());
}

#[test]
fn begin_is_idempotent_and_terminal_receipts_distinguish_outcomes() {
    let fixture = Fixture::new();
    let verifier = TestVerifier::new([]);
    let engine = fixture.engine();
    let absent = "0".repeat(32);
    assert_eq!(
        engine.status(&absent).unwrap(),
        ManagedPairTransactionStatus::Absent
    );

    let failed = engine.begin(&verifier).unwrap();
    let repeated = engine.begin(&verifier).unwrap();
    assert_eq!(repeated, failed);
    assert_eq!(
        engine.status(failed.attempt_id()).unwrap(),
        ManagedPairTransactionStatus::Begun
    );
    assert!(engine
        .stage_attempt(failed.attempt_id(), &verifier)
        .is_err());
    assert_eq!(
        engine.status(failed.attempt_id()).unwrap(),
        ManagedPairTransactionStatus::Failed
    );

    let aborted = engine.begin(&verifier).unwrap();
    assert_ne!(aborted.attempt_id(), failed.attempt_id());
    assert!(engine.abort(aborted.attempt_id()).unwrap());
    assert_eq!(
        engine.status(aborted.attempt_id()).unwrap(),
        ManagedPairTransactionStatus::Aborted
    );
    assert!(!engine.abort(aborted.attempt_id()).unwrap());
}

#[test]
fn size_and_digest_mismatch_fail_before_publication() {
    let fixture = Fixture::new();
    let (candidate, envelope, mut identity) =
        fixture.candidate("release-bad", 1, b"core", b"companion");
    identity.core = ManagedPairComponentIdentity::new(digest(b"other"), 5).unwrap();
    let verifier = TestVerifier::new([(envelope, identity)]);
    assert!(fixture.engine().stage(&candidate, &verifier).is_err());
    assert!(!fixture.install.join("bin/ctx").exists());
}

#[test]
fn downgrade_and_same_generation_rebinding_are_rejected() {
    let fixture = Fixture::new();
    let (newer_root, newer_envelope, newer_identity) =
        fixture.candidate("release-2", 2, b"core-2", b"companion-2");
    let verifier = TestVerifier::new([(newer_envelope, newer_identity)]);
    let engine = fixture.engine();
    activate(&engine, &newer_root, &verifier);

    let (older_root, older_envelope, older_identity) =
        fixture.candidate("release-1", 1, b"core-1", b"companion-1");
    verifier.add(older_envelope, older_identity);
    assert!(engine.stage(&older_root, &verifier).is_err());

    let (rebind_root, rebind_envelope, rebind_identity) =
        fixture.candidate("release-2b", 2, b"core-2b", b"companion-2b");
    verifier.add(rebind_envelope, rebind_identity);
    assert!(engine.stage(&rebind_root, &verifier).is_err());
    assert_active_bytes(&engine, b"core-2", b"companion-2");
}

#[test]
fn partial_active_pair_is_not_adopted_or_overwritten() {
    let fixture = Fixture::new();
    let (candidate, envelope, identity) =
        fixture.candidate("release-partial", 1, b"core", b"companion");
    let verifier = TestVerifier::new([(envelope, identity)]);
    let layout = Layout::open(&fixture.install, true).unwrap();
    fs::write(layout.target(Slot::Companion), b"unmanaged").unwrap();
    assert!(fixture.engine().stage(&candidate, &verifier).is_err());
    assert_eq!(
        fs::read(layout.target(Slot::Companion)).unwrap(),
        b"unmanaged"
    );
}

#[test]
fn wrong_candidate_geometry_cannot_redirect_a_slot() {
    let fixture = Fixture::new();
    let (candidate, envelope, identity) =
        fixture.candidate("release-path", 1, b"core", b"companion");
    let verifier = TestVerifier::new([(envelope, identity)]);
    let layout = Layout::open_candidate(&candidate).unwrap();
    let custom = candidate.join("custom-core");
    fs::rename(layout.target(Slot::Core), &custom).unwrap();
    assert!(fixture.engine().stage(&candidate, &verifier).is_err());
    assert!(custom.is_file());
}

#[cfg(unix)]
#[test]
fn symlink_and_hardlink_substitution_are_rejected() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let (symlink_root, symlink_envelope, symlink_identity) =
        fixture.candidate("release-symlink", 1, b"core", b"companion");
    let verifier = TestVerifier::new([(symlink_envelope, symlink_identity)]);
    let layout = Layout::open_candidate(&symlink_root).unwrap();
    let real = symlink_root.join("real-core");
    fs::rename(layout.target(Slot::Core), &real).unwrap();
    symlink(&real, layout.target(Slot::Core)).unwrap();
    assert!(fixture.engine().stage(&symlink_root, &verifier).is_err());

    let (hard_root, hard_envelope, hard_identity) =
        fixture.candidate("release-hardlink", 1, b"core", b"companion");
    verifier.add(hard_envelope, hard_identity);
    let hard_layout = Layout::open_candidate(&hard_root).unwrap();
    fs::hard_link(
        hard_layout.target(Slot::Companion),
        hard_root.join("companion-alias"),
    )
    .unwrap();
    assert!(fixture.engine().stage(&hard_root, &verifier).is_err());
}

#[test]
fn active_component_tamper_invalidates_the_pair() {
    let fixture = Fixture::new();
    let (candidate, envelope, identity) =
        fixture.candidate("release-tamper", 1, b"core", b"companion");
    let verifier = TestVerifier::new([(envelope, identity)]);
    let engine = fixture.engine();
    activate(&engine, &candidate, &verifier);
    let layout = Layout::open(engine.install_root(), false).unwrap();
    fs::write(layout.target(Slot::Companion), b"tampered!!").unwrap();
    assert!(engine.validate_active(&verifier).is_err());
}

#[test]
fn staging_faults_resume_to_a_clean_absent_transaction() {
    for point in [
        "journal",
        "stage_core",
        "stage_companion",
        "stage_envelope",
        "stage_state",
    ] {
        let fixture = Fixture::new();
        let (candidate, envelope, identity) = fixture.candidate(point, 1, b"core", b"companion");
        let verifier = TestVerifier::new([(envelope, identity)]);
        let engine = fixture.engine();
        let crashed = catch_unwind(AssertUnwindSafe(|| {
            let _ = engine.stage_with_fault(&candidate, &verifier, None, &|observed| {
                if observed == point {
                    panic!("injected staging crash at {point}");
                }
            });
        }));
        assert!(crashed.is_err());
        assert_eq!(
            engine.resume(&verifier).unwrap(),
            ManagedPairRecovery::RolledBack
        );
        assert_eq!(engine.resume(&verifier).unwrap(), ManagedPairRecovery::None);
        let layout = Layout::open(engine.install_root(), false).unwrap();
        assert!(!layout.target(Slot::State).exists());
    }
}

#[test]
fn unrecorded_staged_file_and_interrupted_journal_update_resume_idempotently() {
    let fixture = Fixture::new();
    let (candidate, envelope, identity) =
        fixture.candidate("release-journal-crash", 1, b"core", b"companion");
    let verifier = TestVerifier::new([(envelope, identity)]);
    let engine = fixture.engine();
    let crashed = catch_unwind(AssertUnwindSafe(|| {
        let _ = engine.stage_with_fault(&candidate, &verifier, None, &|point| {
            if point == "journal" {
                panic!("injected crash after initial journal");
            }
        });
    }));
    assert!(crashed.is_err());
    let layout = Layout::open(engine.install_root(), false).unwrap();
    let journal = journal::read(&layout).unwrap().unwrap();
    assert_eq!(journal.phase, Phase::Staging);
    filesystem::copy_verified(
        &Layout::open_candidate(&candidate)
            .unwrap()
            .target(Slot::Core),
        &layout.staged(Slot::Core, &journal.attempt_id),
        journal.identity.core(),
        true,
        Slot::Core.label(),
    )
    .unwrap();
    assert_eq!(
        engine.resume(&verifier).unwrap(),
        ManagedPairRecovery::RolledBack
    );
    assert_eq!(engine.resume(&verifier).unwrap(), ManagedPairRecovery::None);

    let prepared = engine.stage(&candidate, &verifier).unwrap();
    let mut journal = journal::read(&layout).unwrap().unwrap();
    journal.phase = Phase::Activating;
    journal::write_temporary_for_test(&layout, &mut journal).unwrap();
    let promoted = journal::read(&layout).unwrap().unwrap();
    assert_eq!(promoted.phase, Phase::Activating);
    assert!(!layout.journal_temporary().exists());
    assert_eq!(
        engine.resume(&verifier).unwrap(),
        ManagedPairRecovery::RolledBack
    );
    assert_eq!(prepared.identity(), &promoted.identity);

    let prepared = engine.stage(&candidate, &verifier).unwrap();
    fs::write(layout.journal_temporary(), b"").unwrap();
    assert_eq!(
        engine.resume(&verifier).unwrap(),
        ManagedPairRecovery::Staged {
            prepared: prepared.clone()
        }
    );
    assert!(!layout.journal_temporary().exists());
}

#[cfg(unix)]
#[test]
fn directory_substitution_during_activation_cannot_redirect_fixed_slots() {
    use std::{
        os::unix::fs::symlink,
        sync::atomic::{AtomicBool, Ordering},
    };

    let fixture = Fixture::new();
    let (old_root, old_envelope, old_identity) =
        fixture.candidate("old-path-race", 1, b"old-core", b"old-companion");
    let verifier = TestVerifier::new([(old_envelope, old_identity)]);
    let engine = fixture.engine();
    activate(&engine, &old_root, &verifier);

    let (new_root, new_envelope, new_identity) =
        fixture.candidate("new-path-race", 2, b"new-core", b"new-companion");
    verifier.add(new_envelope, new_identity);
    engine.stage(&new_root, &verifier).unwrap();
    let layout = Layout::open(engine.install_root(), false).unwrap();
    let journal = journal::read(&layout).unwrap().unwrap();
    let alternate = fixture._temp.path().join("alternate-libexec");
    fs::create_dir(&alternate).unwrap();
    ctx_history_platform::platform_security::restrict_private_directory(&alternate).unwrap();
    fs::write(alternate.join("sentinel"), b"outside").unwrap();
    let held = fixture.install.join("libexec-held");
    let replaced = AtomicBool::new(false);

    let result = commit_transaction(&engine, &layout, journal, &verifier, &|point| {
        if point == "backup_core" && !replaced.swap(true, Ordering::SeqCst) {
            fs::rename(fixture.install.join("libexec"), &held).unwrap();
            symlink(&alternate, fixture.install.join("libexec")).unwrap();
        }
    });
    assert!(result.is_err());
    assert_eq!(fs::read(alternate.join("sentinel")).unwrap(), b"outside");
    assert!(!alternate.join("ctx-pro").exists());
    assert_eq!(fs::read(held.join("ctx-pro")).unwrap(), b"old-companion");

    fs::remove_file(fixture.install.join("libexec")).unwrap();
    fs::rename(&held, fixture.install.join("libexec")).unwrap();
    assert_eq!(
        engine.resume(&verifier).unwrap(),
        ManagedPairRecovery::RolledBack
    );
    assert_active_bytes(&engine, b"old-core", b"old-companion");
}

#[cfg(unix)]
#[test]
fn installation_root_substitution_after_lock_fails_closed() {
    use std::{
        os::unix::fs::symlink,
        sync::atomic::{AtomicBool, Ordering},
    };

    let fixture = Fixture::new();
    let (old_root, old_envelope, old_identity) =
        fixture.candidate("old-root-race", 1, b"old-core", b"old-companion");
    let verifier = TestVerifier::new([(old_envelope, old_identity)]);
    let engine = fixture.engine();
    activate(&engine, &old_root, &verifier);

    let (new_root, new_envelope, new_identity) =
        fixture.candidate("new-root-race", 2, b"new-core", b"new-companion");
    verifier.add(new_envelope, new_identity);
    engine.stage(&new_root, &verifier).unwrap();
    let layout = Layout::open(engine.install_root(), false).unwrap();
    let journal = journal::read(&layout).unwrap().unwrap();
    let alternate = fixture._temp.path().join("alternate-install");
    Layout::open(&alternate, true).unwrap();
    fs::write(alternate.join("sentinel"), b"outside").unwrap();
    let held = fixture._temp.path().join("install-held");
    let replaced = AtomicBool::new(false);

    let result = commit_transaction(&engine, &layout, journal, &verifier, &|point| {
        if point == "activating" && !replaced.swap(true, Ordering::SeqCst) {
            fs::rename(&fixture.install, &held).unwrap();
            symlink(&alternate, &fixture.install).unwrap();
        }
    });
    assert!(result.is_err());
    assert_eq!(fs::read(alternate.join("sentinel")).unwrap(), b"outside");
    assert!(!alternate.join("share/ctx/managed-pair-state.json").exists());
    assert_eq!(fs::read(held.join("bin/ctx")).unwrap(), b"old-core");

    fs::remove_file(&fixture.install).unwrap();
    fs::rename(&held, &fixture.install).unwrap();
    assert_eq!(
        engine.resume(&verifier).unwrap(),
        ManagedPairRecovery::RolledBack
    );
    assert_active_bytes(&engine, b"old-core", b"old-companion");
}

#[test]
fn activation_faults_roll_back_before_state_and_fix_forward_after_state() {
    let points = [
        "activating",
        "backup_state",
        "backup_core",
        "backup_companion",
        "backup_envelope",
        "publish_envelope",
        "publish_companion",
        "publish_core",
        "publish_state",
    ];
    for point in points {
        let fixture = Fixture::new();
        let (old_root, old_envelope, old_identity) =
            fixture.candidate(&format!("old-{point}"), 1, b"old-core", b"old-companion");
        let verifier = TestVerifier::new([(old_envelope, old_identity)]);
        let engine = fixture.engine();
        activate(&engine, &old_root, &verifier);

        let (new_root, new_envelope, new_identity) =
            fixture.candidate(&format!("new-{point}"), 2, b"new-core", b"new-companion");
        verifier.add(new_envelope, new_identity);
        engine.stage(&new_root, &verifier).unwrap();
        let layout = Layout::open(engine.install_root(), false).unwrap();
        let journal = journal::read(&layout).unwrap().unwrap();
        let crashed = catch_unwind(AssertUnwindSafe(|| {
            let _ = commit_transaction(&engine, &layout, journal, &verifier, &|observed| {
                if observed == point {
                    panic!("injected activation crash at {point}");
                }
            });
        }));
        assert!(crashed.is_err());
        assert!(
            fixture.install.join("bin/ctx").is_file(),
            "Core disappeared after crash at {point}"
        );

        let recovery = engine.resume(&verifier).unwrap();
        if point == "publish_state" {
            assert_eq!(recovery, ManagedPairRecovery::Activated);
            assert_active_bytes(&engine, b"new-core", b"new-companion");
        } else {
            assert_eq!(recovery, ManagedPairRecovery::RolledBack);
            assert_active_bytes(&engine, b"old-core", b"old-companion");
        }
        assert_eq!(engine.resume(&verifier).unwrap(), ManagedPairRecovery::None);
    }
}

#[test]
fn activation_publishes_envelope_then_companion_then_core_then_state_and_receipt() {
    let fixture = Fixture::new();
    let (old_root, old_envelope, old_identity) =
        fixture.candidate("old-state-order", 1, b"old-core", b"old-companion");
    let verifier = TestVerifier::new([(old_envelope.clone(), old_identity)]);
    let engine = fixture.engine();
    activate(&engine, &old_root, &verifier);
    let old_state = fs::read(fixture.install.join(MANAGED_PAIR_STATE_RELATIVE_PATH)).unwrap();

    let (new_root, new_envelope, new_identity) =
        fixture.candidate("new-state-order", 2, b"new-core", b"new-companion");
    verifier.add(new_envelope.clone(), new_identity);
    engine.stage(&new_root, &verifier).unwrap();
    let layout = Layout::open(engine.install_root(), false).unwrap();
    let journal = journal::read(&layout).unwrap().unwrap();
    let new_state = fs::read(layout.staged(Slot::State, &journal.attempt_id)).unwrap();
    let old_receipt_attempt_id = attempt::read_terminal(&layout).unwrap().unwrap().attempt_id;
    let new_attempt_id = journal.attempt_id.clone();
    let observed = Mutex::new(Vec::new());

    commit_transaction(&engine, &layout, journal, &verifier, &|point| {
        observed.lock().unwrap().push(point.to_owned());
        let published = match point {
            "publish_envelope" => 1,
            "publish_companion" => 2,
            "publish_core" => 3,
            "publish_state" => 4,
            _ => 0,
        };
        let expected_envelope: &[u8] = if published >= 1 {
            &new_envelope
        } else {
            &old_envelope
        };
        let expected_state: &[u8] = if published >= 4 {
            &new_state
        } else {
            &old_state
        };
        assert!(
            layout.target(Slot::Core).is_file(),
            "Core was absent at {point}"
        );
        assert_eq!(
            fs::read(layout.target(Slot::Envelope)).unwrap(),
            expected_envelope
        );
        assert_eq!(
            fs::read(layout.target(Slot::Companion)).unwrap(),
            if published >= 2 {
                b"new-companion"
            } else {
                b"old-companion"
            }
        );
        assert_eq!(
            fs::read(layout.target(Slot::Core)).unwrap(),
            if published >= 3 {
                b"new-core"
            } else {
                b"old-core"
            }
        );
        assert_eq!(
            fs::read(layout.target(Slot::State)).unwrap(),
            expected_state
        );
        let receipt = attempt::read_terminal(&layout).unwrap().unwrap();
        assert_eq!(receipt.attempt_id, old_receipt_attempt_id);
        assert_eq!(receipt.outcome, TerminalOutcome::Committed);
    })
    .unwrap();

    assert_eq!(
        observed.into_inner().unwrap(),
        [
            "activating",
            "backup_state",
            "backup_core",
            "backup_companion",
            "backup_envelope",
            "publish_envelope",
            "publish_companion",
            "publish_core",
            "publish_state",
        ]
    );
    let receipt = attempt::read_terminal(&layout).unwrap().unwrap();
    assert_eq!(receipt.attempt_id, new_attempt_id);
    assert_eq!(receipt.outcome, TerminalOutcome::Committed);
    assert_active_bytes(&engine, b"new-core", b"new-companion");
}

#[test]
fn rollback_crashes_keep_state_hidden_until_old_pair_is_complete() {
    for rollback_point in [
        "rollback_hide_state",
        "rollback_core",
        "rollback_companion",
        "rollback_envelope",
        "rollback_restore_state",
    ] {
        let fixture = Fixture::new();
        let (old_root, old_envelope, old_identity) = fixture.candidate(
            &format!("old-{rollback_point}"),
            1,
            b"old-core",
            b"old-companion",
        );
        let verifier = TestVerifier::new([(old_envelope, old_identity)]);
        let engine = fixture.engine();
        activate(&engine, &old_root, &verifier);
        let old_state = fs::read(fixture.install.join(MANAGED_PAIR_STATE_RELATIVE_PATH)).unwrap();

        let (new_root, new_envelope, new_identity) = fixture.candidate(
            &format!("new-{rollback_point}"),
            2,
            b"new-core",
            b"new-companion",
        );
        verifier.add(new_envelope, new_identity);
        engine.stage(&new_root, &verifier).unwrap();
        let layout = Layout::open(engine.install_root(), false).unwrap();
        let journal = journal::read(&layout).unwrap().unwrap();
        let activation_crash = catch_unwind(AssertUnwindSafe(|| {
            let _ = commit_transaction(&engine, &layout, journal, &verifier, &|point| {
                if point == "publish_envelope" {
                    panic!("leave a pre-state activation for rollback");
                }
            });
        }));
        assert!(activation_crash.is_err());
        assert_eq!(
            fs::read(fixture.install.join(MANAGED_PAIR_STATE_RELATIVE_PATH)).unwrap(),
            old_state
        );
        assert!(fixture.install.join("bin/ctx").is_file());

        let mut journal = journal::read(&layout).unwrap().unwrap();
        let rollback_crash = catch_unwind(AssertUnwindSafe(|| {
            let _ = rollback_with_fault(&layout, &mut journal, &|point| {
                let state = fixture.install.join(MANAGED_PAIR_STATE_RELATIVE_PATH);
                if point == "rollback_restore_state" {
                    assert!(state.is_file());
                    assert_eq!(
                        fs::read(fixture.install.join("bin/ctx")).unwrap(),
                        b"old-core"
                    );
                    assert_eq!(
                        fs::read(fixture.install.join("libexec/ctx-pro")).unwrap(),
                        b"old-companion"
                    );
                } else {
                    assert!(!state.exists(), "state was visible at {point}");
                }
                if point == rollback_point {
                    panic!("injected rollback crash at {rollback_point}");
                }
            });
        }));
        assert!(rollback_crash.is_err());

        assert_eq!(
            engine.resume(&verifier).unwrap(),
            ManagedPairRecovery::RolledBack
        );
        assert_active_bytes(&engine, b"old-core", b"old-companion");
        assert_eq!(engine.resume(&verifier).unwrap(), ManagedPairRecovery::None);
    }
}

#[test]
fn public_identity_constructors_reject_unbounded_or_noncanonical_values() {
    assert!(ManagedPairComponentIdentity::new("A".repeat(64), 1).is_err());
    assert!(ManagedPairComponentIdentity::new("a".repeat(64), 0).is_err());
    assert!(ManagedPairComponentIdentity::new("a".repeat(64), MAX_COMPONENT_BYTES).is_ok());
    assert!(ManagedPairComponentIdentity::new("a".repeat(64), MAX_COMPONENT_BYTES + 1).is_err());
    let component = ManagedPairComponentIdentity::new("a".repeat(64), 1).unwrap();
    assert!(VerifiedManagedPairIdentity::new(
        "bad/name",
        current_target().unwrap(),
        1,
        "b".repeat(64),
        component.clone(),
        component,
    )
    .is_err());
}

#[test]
fn staged_recovery_reconstructs_a_usable_prepared_handle() {
    let fixture = Fixture::new();
    let (candidate, envelope, identity) =
        fixture.candidate("release-resumed-staged", 1, b"core", b"companion");
    let verifier = TestVerifier::new([(envelope, identity.clone())]);
    let engine = fixture.engine();
    let original = engine.stage(&candidate, &verifier).unwrap();

    let recovered = match engine.resume(&verifier).unwrap() {
        ManagedPairRecovery::Staged { prepared } => prepared,
        other => panic!("unexpected staged recovery: {other:?}"),
    };
    assert_eq!(recovered, original);
    assert_eq!(recovered.identity(), &identity);

    #[cfg(not(windows))]
    {
        assert_eq!(
            engine.activate(&recovered, &verifier).unwrap(),
            ManagedPairActivation::Activated
        );
        assert_active_bytes(&engine, b"core", b"companion");
    }
    #[cfg(windows)]
    assert!(matches!(
        engine.activate(&recovered, &verifier).unwrap(),
        ManagedPairActivation::PostExitRequired { .. }
    ));
}

#[test]
fn concurrent_stagers_are_serialized_by_one_stable_installation_lock() {
    let fixture = Fixture::new();
    let (candidate, envelope, identity) =
        fixture.candidate("release-concurrent", 1, b"core", b"companion");
    let verifier = Arc::new(TestVerifier::new([(envelope, identity)]));
    let engine = fixture.engine();
    let first_engine = engine.clone();
    let second_engine = engine.clone();
    let first_candidate = candidate.clone();
    let second_candidate = candidate;
    let first_verifier = Arc::clone(&verifier);
    let second_verifier = Arc::clone(&verifier);
    let first =
        std::thread::spawn(move || first_engine.stage(&first_candidate, first_verifier.as_ref()));
    let second = std::thread::spawn(move || {
        second_engine.stage(&second_candidate, second_verifier.as_ref())
    });
    let outcomes = [first.join().unwrap(), second.join().unwrap()];
    assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
    assert_eq!(
        outcomes.iter().filter(|outcome| outcome.is_err()).count(),
        1
    );
    assert!(matches!(
        engine.resume(verifier.as_ref()).unwrap(),
        ManagedPairRecovery::Staged { .. }
    ));
}

#[test]
fn upgrade_and_uninstall_are_reciprocally_exclusive() {
    let fixture = Fixture::new();
    let (candidate, envelope, identity) =
        fixture.candidate("release-exclusive", 1, b"core", b"companion");
    let verifier = TestVerifier::new([(envelope, identity)]);
    let engine = fixture.engine();
    activate(&engine, &candidate, &verifier);

    let uninstall = engine.prepare_uninstall(&verifier).unwrap();
    assert!(engine.begin(&verifier).is_err());
    let layout = Layout::open(engine.install_root(), false).unwrap();
    uninstall::execute(&layout, uninstall.attempt_id()).unwrap();

    let fixture = Fixture::new();
    let (candidate, envelope, identity) =
        fixture.candidate("release-begun", 1, b"core", b"companion");
    let verifier = TestVerifier::new([(envelope, identity)]);
    let engine = fixture.engine();
    activate(&engine, &candidate, &verifier);
    let begun = engine.begin(&verifier).unwrap();
    assert!(engine.prepare_uninstall(&verifier).is_err());
    assert!(engine.abort(begun.attempt_id()).unwrap());
}

#[cfg(unix)]
#[test]
fn interrupted_uninstall_retains_authority_and_retries_after_core_delete_failure() {
    use std::os::unix::fs::PermissionsExt as _;

    let fixture = Fixture::new();
    let (candidate, envelope, identity) =
        fixture.candidate("release-uninstall-retry", 1, b"core", b"companion");
    let verifier = TestVerifier::new([(envelope, identity)]);
    let engine = fixture.engine();
    activate(&engine, &candidate, &verifier);
    let uninstall = engine.prepare_uninstall(&verifier).unwrap();
    let layout = Layout::open(engine.install_root(), false).unwrap();

    let bin = fixture.install.join("bin");
    let mut permissions = fs::metadata(&bin).unwrap().permissions();
    permissions.set_mode(0o500);
    fs::set_permissions(&bin, permissions).unwrap();
    let failure = uninstall::execute(&layout, uninstall.attempt_id()).unwrap_err();
    assert!(
        failure
            .to_string()
            .contains("managed_pair_core_delete_retry_required"),
        "{failure:#}"
    );
    assert!(layout.uninstall_journal().exists());
    assert!(layout.target(Slot::Core).exists());
    for slot in [Slot::State, Slot::Envelope, Slot::Companion] {
        assert!(!layout.target(slot).exists(), "{} remained", slot.label());
    }

    let mut permissions = fs::metadata(&bin).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&bin, permissions).unwrap();
    assert!(!uninstall::execute(&layout, uninstall.attempt_id()).unwrap());
    assert!(!layout.uninstall_journal().exists());
    for slot in Slot::ALL {
        assert!(!layout.target(slot).exists(), "{} remained", slot.label());
    }
}

#[cfg(windows)]
#[test]
fn windows_layout_revalidation_detects_directory_substitution() {
    let fixture = Fixture::new();
    let layout = Layout::open(&fixture.install, true).unwrap();
    let held = fixture._temp.path().join("libexec-held");
    fs::rename(fixture.install.join("libexec"), &held).unwrap();
    fs::create_dir(fixture.install.join("libexec")).unwrap();
    ctx_history_platform::platform_security::restrict_private_directory(
        &fixture.install.join("libexec"),
    )
    .unwrap();
    assert!(layout.revalidate().is_err());
    fs::remove_dir(fixture.install.join("libexec")).unwrap();
    fs::rename(&held, fixture.install.join("libexec")).unwrap();
    layout.revalidate().unwrap();
}

#[cfg(windows)]
#[test]
fn windows_swap_and_restore_cannot_redirect_handle_relative_file_operations() {
    let fixture = Fixture::new();
    let layout = Layout::open(&fixture.install, true).unwrap();
    let companion = layout.target(Slot::Companion);
    fs::write(&companion, b"trusted-companion").unwrap();
    ctx_history_platform::platform_security::restrict_private_file(&companion).unwrap();

    let alternate = fixture._temp.path().join("alternate-libexec");
    fs::create_dir(&alternate).unwrap();
    ctx_history_platform::platform_security::restrict_private_directory(&alternate).unwrap();
    let redirected = alternate.join("ctx-pro.exe");
    fs::write(&redirected, b"redirected-companion").unwrap();
    ctx_history_platform::platform_security::restrict_private_file(&redirected).unwrap();
    let held = fixture._temp.path().join("held-libexec");

    fs::rename(fixture.install.join("libexec"), &held).unwrap();
    fs::rename(&alternate, fixture.install.join("libexec")).unwrap();

    let observed = filesystem::read_regular(&companion, 64, "bound companion").unwrap();
    assert_eq!(observed.bytes, b"trusted-companion");
    let staged = layout.staged(Slot::Companion, "swap-and-restore");
    let staged_stamp = filesystem::write_new(&staged, b"bound-stage", true, "bound stage").unwrap();
    let staged_name = staged.file_name().unwrap();
    assert_eq!(fs::read(held.join(staged_name)).unwrap(), b"bound-stage");
    assert!(!fixture.install.join("libexec").join(staged_name).exists());
    assert_eq!(
        fs::read(fixture.install.join("libexec/ctx-pro.exe")).unwrap(),
        b"redirected-companion"
    );

    fs::rename(fixture.install.join("libexec"), &alternate).unwrap();
    fs::rename(&held, fixture.install.join("libexec")).unwrap();
    layout.revalidate().unwrap();
    filesystem::remove_if_exact(&staged, &staged_stamp, 64, "bound stage").unwrap();
}

#[cfg(windows)]
#[test]
fn windows_transaction_lock_and_installation_root_cannot_be_replaced_while_held() {
    let fixture = Fixture::new();
    let layout = Layout::open(&fixture.install, true).unwrap();
    let transaction = filesystem::acquire_lock(&layout).unwrap();
    let lock_path = layout.lock().to_path_buf();
    let retired_lock = fixture.install.join("retired-managed-pair-lock");
    let held_root = fixture._temp.path().join("held-installation-root");
    drop(layout);

    let install = fixture.install.clone();
    let racer = std::thread::spawn(move || {
        let lock_rename = fs::rename(&lock_path, &retired_lock);
        let lock_remove = fs::remove_file(&lock_path);
        let root_rename = fs::rename(&install, &held_root);
        (lock_rename, lock_remove, root_rename, lock_path, held_root)
    });
    let (lock_rename, lock_remove, root_rename, lock_path, held_root) = racer.join().unwrap();
    assert!(lock_rename.is_err(), "held lock was renamed");
    assert!(lock_remove.is_err(), "held lock was removed");
    assert!(
        root_rename.is_err(),
        "guarded installation root was renamed"
    );
    assert!(lock_path.is_file());
    assert!(fixture.install.is_dir());

    drop(transaction);
    fs::rename(
        &lock_path,
        fixture.install.join("retired-managed-pair-lock"),
    )
    .unwrap();
    fs::rename(
        fixture.install.join("retired-managed-pair-lock"),
        &lock_path,
    )
    .unwrap();
    fs::rename(&fixture.install, &held_root).unwrap();
    fs::rename(&held_root, &fixture.install).unwrap();
}

#[cfg(windows)]
#[test]
fn windows_deferred_path_commits_after_the_recorded_parent_exits() {
    const CHILD_ROOT: &str = "CTX_MANAGED_PAIR_DEFERRED_TEST_ROOT";
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        let engine = ManagedPairEngine::new(PathBuf::from(root)).unwrap();
        let layout = Layout::open(engine.install_root(), false).unwrap();
        let journal = journal::read(&layout).unwrap().unwrap();
        let prepared = ManagedPairPrepared {
            attempt_id: journal.attempt_id,
            identity: journal.identity,
        };
        let verifier = TestVerifier::new([]);
        assert!(matches!(
            engine.activate(&prepared, &verifier).unwrap(),
            ManagedPairActivation::PostExitRequired { .. }
        ));
        return;
    }

    let fixture = Fixture::new();
    let (candidate, envelope, identity) =
        fixture.candidate("windows-deferred", 1, b"core", b"companion");
    let verifier = TestVerifier::new([(envelope, identity)]);
    let engine = fixture.engine();
    engine.stage(&candidate, &verifier).unwrap();

    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("managed_pair::tests::windows_deferred_path_commits_after_the_recorded_parent_exits")
        .arg("--nocapture")
        .env(CHILD_ROOT, &fixture.install)
        .status()
        .unwrap();
    assert!(status.success());

    let layout = Layout::open(engine.install_root(), false).unwrap();
    let deferred = journal::read(&layout).unwrap().unwrap();
    assert_eq!(deferred.phase, Phase::Deferred);
    assert!(deferred.parent_pid.is_some());
    assert!(deferred.parent_creation_time.is_some());
    engine.run_post_exit_swapper(&verifier).unwrap();
    assert_active_bytes(&engine, b"core", b"companion");
    assert!(verifier.calls() >= 2);
}

#[cfg(windows)]
#[test]
fn windows_parent_wait_is_bounded_and_does_not_wait_on_a_reused_pid() {
    const SLEEP_CHILD: &str = "CTX_MANAGED_PAIR_PARENT_WAIT_TEST_CHILD";
    if std::env::var_os(SLEEP_CHILD).is_some() {
        std::thread::sleep(std::time::Duration::from_secs(30));
        return;
    }

    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg(
            "managed_pair::tests::windows_parent_wait_is_bounded_and_does_not_wait_on_a_reused_pid",
        )
        .arg("--nocapture")
        .env(SLEEP_CHILD, "1")
        .spawn()
        .unwrap();
    let pid = child.id();
    let creation = filesystem::process_creation_identity_for_test(pid).unwrap();
    let timeout = filesystem::wait_for_parent_exit_for_test(pid, creation, 0).unwrap_err();
    assert!(timeout.to_string().contains("timed out"));
    let reused_identity = if creation == u64::MAX {
        creation - 1
    } else {
        creation + 1
    };
    filesystem::wait_for_parent_exit_for_test(pid, reused_identity, 0).unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
}
