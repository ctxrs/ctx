use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
};

use anyhow::{anyhow, Result};
use fs2::FileExt as _;
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

struct Candidate {
    root: PathBuf,
    envelope: Vec<u8>,
    identity: VerifiedManagedPairIdentity,
    core: Vec<u8>,
    companion: Vec<u8>,
    marker: Vec<u8>,
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
        marker: &[u8],
    ) -> Candidate {
        let root = self.candidates.join(name);
        fs::create_dir(&root).unwrap();
        ctx_history_platform::platform_security::restrict_private_directory(&root).unwrap();
        for relative in ["bin", "libexec", "share", "share/ctx"] {
            let directory = root.join(relative);
            fs::create_dir(&directory).unwrap();
            ctx_history_platform::platform_security::restrict_private_directory(&directory)
                .unwrap();
        }
        let layout = filesystem::Layout::open_candidate(&root).unwrap();
        fs::write(layout.target(filesystem::Slot::Core), core).unwrap();
        fs::write(layout.target(filesystem::Slot::Companion), companion).unwrap();
        fs::write(layout.target(filesystem::Slot::Marker), marker).unwrap();
        let envelope = format!("signed-envelope:{name}:{generation}\n").into_bytes();
        fs::write(layout.target(filesystem::Slot::Envelope), &envelope).unwrap();
        for slot in [
            filesystem::Slot::Core,
            filesystem::Slot::Companion,
            filesystem::Slot::Marker,
            filesystem::Slot::Envelope,
        ] {
            ctx_history_platform::platform_security::restrict_private_file(&layout.target(slot))
                .unwrap();
        }
        Candidate {
            root,
            envelope,
            identity: identity(name, generation, core, companion),
            core: core.to_vec(),
            companion: companion.to_vec(),
            marker: marker.to_vec(),
        }
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

fn input(candidate: &Candidate) -> ManagedPairApplyInput {
    let layout = filesystem::Layout::open_candidate(&candidate.root).unwrap();
    ManagedPairApplyInput::new(
        layout.target(filesystem::Slot::Envelope).as_ref(),
        layout.target(filesystem::Slot::Core).as_ref(),
        layout.target(filesystem::Slot::Companion).as_ref(),
        layout.target(filesystem::Slot::Marker).as_ref(),
    )
}

fn under_installation_lock<T>(install_root: &Path, operation: impl FnOnce() -> T) -> T {
    filesystem::Layout::open(install_root, true).unwrap();
    let lock_path = install_root.join(MANAGED_PAIR_INSTALLATION_LOCK_RELATIVE_PATH);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    ctx_history_platform::platform_security::restrict_private_file(&lock_path).unwrap();
    lock.lock_exclusive().unwrap();
    operation()
}

fn apply(
    fixture: &Fixture,
    candidate: &Candidate,
    verifier: &TestVerifier,
) -> ManagedPairApplyOutcome {
    under_installation_lock(&fixture.install, || {
        apply_or_resume_managed_pair_under_installation_lock(
            &fixture.install,
            &input(candidate),
            verifier,
        )
        .unwrap()
    })
}

fn apply_with_fault(
    fixture: &Fixture,
    candidate: &Candidate,
    verifier: &TestVerifier,
    fault: &dyn Fn(&str),
) -> Result<ManagedPairApplyOutcome> {
    under_installation_lock(&fixture.install, || {
        crate::fix_forward::apply_or_resume_with_fault(
            &fixture.install,
            &input(candidate),
            verifier,
            fault,
        )
    })
}

fn assert_active(fixture: &Fixture, candidate: &Candidate, verifier: &TestVerifier) {
    let layout = filesystem::Layout::open(&fixture.install, false).unwrap();
    assert_eq!(
        fs::read(layout.target(filesystem::Slot::Core)).unwrap(),
        candidate.core
    );
    assert_eq!(
        fs::read(layout.target(filesystem::Slot::Companion)).unwrap(),
        candidate.companion
    );
    assert_eq!(
        fs::read(layout.target(filesystem::Slot::Marker)).unwrap(),
        candidate.marker
    );
    assert_eq!(
        fs::read(layout.target(filesystem::Slot::Envelope)).unwrap(),
        candidate.envelope
    );
    assert_eq!(
        validate_active(&layout, verifier).unwrap().0,
        candidate.identity
    );
}

fn assert_cleanup(fixture: &Fixture, attempt_id: &str) {
    let layout = filesystem::Layout::open(&fixture.install, false).unwrap();
    assert!(!layout.active_transaction().exists());
    assert!(!layout.active_transaction_temporary().exists());
    assert!(!filesystem::apply_candidate_exists(&layout).unwrap());
    for slot in filesystem::Slot::ALL {
        assert!(!layout.staged(slot, attempt_id).exists());
    }
    assert!(fixture
        .install
        .join(MANAGED_PAIR_INSTALLATION_LOCK_RELATIVE_PATH)
        .is_file());
}

mod fix_forward;
#[cfg(unix)]
mod reconciliation;

#[test]
fn managed_core_marker_uses_the_platform_install_marker_name() {
    assert_eq!(
        MANAGED_CORE_INSTALL_MARKER_RELATIVE_PATH,
        if cfg!(windows) {
            "bin/ctx.exe.install.json"
        } else {
            "bin/ctx.install.json"
        }
    );
}
