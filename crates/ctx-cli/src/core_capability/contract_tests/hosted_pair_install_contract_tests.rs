use std::{ffi::OsString, fs, path::PathBuf};

use ctx_upgrade_engine::{
    ManagedPairComponentIdentity, ManagedPairTarget, ManagedPairVerifier,
    VerifiedManagedPairIdentity, try_acquire_managed_installation_mutation_at_root,
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::super::{hosted_pair_install as hosted, managed_pair_apply};

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

struct Verifier(VerifiedManagedPairIdentity);
impl ManagedPairVerifier for Verifier {
    fn verify_signed_envelope(
        &self,
        envelope: &[u8],
    ) -> anyhow::Result<VerifiedManagedPairIdentity> {
        anyhow::ensure!(envelope == b"fixture-envelope", "invalid fixture envelope");
        Ok(self.0.clone())
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    core: PathBuf,
    candidate: PathBuf,
    companion: PathBuf,
    marker: PathBuf,
    current_marker: PathBuf,
    envelope: PathBuf,
    verifier: Verifier,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let install = root.join("install");
        let download = root.join("download");
        fs::create_dir_all(install.join("bin")).unwrap();
        fs::create_dir_all(&download).unwrap();
        let core = managed_pair_apply::managed_core_destination(&install);
        let candidate = download.join("candidate-core");
        let companion = download.join("candidate-companion");
        let envelope = download.join("envelope.json");
        let marker = download.join("marker.json");
        let mut current_marker = core.as_os_str().to_owned();
        current_marker.push(".install.json");
        let current_marker = PathBuf::from(current_marker);
        let (target, platform) = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("windows", "x86_64") => (ManagedPairTarget::WindowsX64, "windows-x64"),
            ("linux", "x86_64") => (ManagedPairTarget::LinuxX64, "linux-x64"),
            ("linux", "aarch64") => (ManagedPairTarget::LinuxArm64, "linux-aarch64"),
            ("macos", "x86_64") => (ManagedPairTarget::MacosX64, "macos-x64"),
            ("macos", "aarch64") => (ManagedPairTarget::MacosArm64, "macos-arm64"),
            other => panic!("unsupported fixture target {other:?}"),
        };
        for path in [&core, &candidate] {
            fs::write(path, b"signed-core").unwrap();
        }
        fs::write(&companion, b"signed-companion").unwrap();
        fs::write(&envelope, b"fixture-envelope").unwrap();
        // Released Windows installer markers lack managed_pair. That omission
        // must remain confined to the released entry point.
        let value = json!({"schema_version":1,"manager":"ctx-hosted-installer",
            "install_path":core,"platform":platform,"channel":"stable",
            "version":env!("CARGO_PKG_VERSION"),"sha256":hash(b"signed-core"),
            "man_pages":null,"extension":{"retained":true}});
        fs::write(&marker, serde_json::to_vec(&value).unwrap()).unwrap();
        fs::write(&current_marker, serde_json::to_vec(&value).unwrap()).unwrap();
        let identity = VerifiedManagedPairIdentity::new(
            "bridge",
            target,
            4,
            hash(b"manifest"),
            ManagedPairComponentIdentity::new(hash(b"signed-core"), 11).unwrap(),
            ManagedPairComponentIdentity::new(hash(b"signed-companion"), 16).unwrap(),
        )
        .unwrap();
        Self {
            _temp: temp,
            core,
            candidate,
            companion,
            marker,
            current_marker,
            envelope,
            verifier: Verifier(identity),
        }
    }
    fn arguments(&self) -> Vec<OsString> {
        [
            OsString::from("ctx"),
            OsString::from("--ctx-core-hosted-pair-install-v1"),
            self.envelope.as_os_str().to_owned(),
            self.candidate.as_os_str().to_owned(),
            self.companion.as_os_str().to_owned(),
            self.marker.as_os_str().to_owned(),
        ]
        .into()
    }
    fn complete(&self) -> anyhow::Result<VerifiedManagedPairIdentity> {
        hosted::complete(
            &hosted::parse(&self.arguments(), &self.core)?,
            &self.core,
            &self.verifier,
        )
    }
    fn change_marker(&self, key: &str, value: Value) {
        let mut marker: Value = serde_json::from_slice(&fs::read(&self.marker).unwrap()).unwrap();
        marker[key] = value;
        fs::write(&self.marker, serde_json::to_vec(&marker).unwrap()).unwrap();
    }
}

#[test]
fn released_argv_requires_installed_core_slot_and_exact_six_arguments() {
    let f = Fixture::new();
    assert!(hosted::parse(&f.arguments(), &f.core).is_ok());
    assert!(hosted::parse(&f.arguments()[..5], &f.core).is_err());
    assert!(hosted::parse(&f.arguments(), &f.candidate).is_err());
    let mut arguments = f.arguments();
    arguments[2] = "relative-envelope".into();
    assert!(hosted::parse(&arguments, &f.core).is_err());
}

#[test]
fn released_missing_pair_field_does_not_weaken_current_candidate_marker() {
    let f = Fixture::new();
    assert!(managed_pair_apply::read_install_marker(&f.marker).is_err());
    assert!(managed_pair_apply::read_released_install_marker(&f.marker).is_ok());
    f.change_marker("managed_pair", Value::Bool(false));
    assert!(managed_pair_apply::read_released_install_marker(&f.marker).is_err());
    f.change_marker("managed_pair", Value::Bool(true));
    assert!(managed_pair_apply::read_install_marker(&f.marker).is_ok());
}

#[test]
fn marker_preserves_released_null_man_page_contract_and_extensions() {
    let f = Fixture::new();
    let mut installed: Value =
        serde_json::from_slice(&fs::read(&f.current_marker).unwrap()).unwrap();
    installed["man_pages"] = json!({"schema_version":1,"status":"installed","owned":true});
    fs::write(&f.current_marker, serde_json::to_vec(&installed).unwrap()).unwrap();
    let normalized: Value =
        serde_json::from_slice(&hosted::normalized_marker(&f.marker, &f.current_marker).unwrap())
            .unwrap();
    assert_eq!(normalized["man_pages"], installed["man_pages"]);
    assert_eq!(normalized["extension"], json!({"retained":true}));
    assert_eq!(normalized["managed_pair"], true);
    f.change_marker("man_pages", json!({"schema_version":1,"status":"disabled"}));
    let disabled: Value =
        serde_json::from_slice(&hosted::normalized_marker(&f.marker, &f.current_marker).unwrap())
            .unwrap();
    assert_eq!(disabled["man_pages"]["status"], "disabled");
}

#[test]
fn installed_core_completion_and_reinstall_use_existing_kernel() {
    let f = Fixture::new();
    assert_eq!(f.complete().unwrap(), f.verifier.0);
    assert_eq!(f.complete().unwrap(), f.verifier.0);
    assert_eq!(fs::read(&f.core).unwrap(), b"signed-core");
    let marker: Value = serde_json::from_slice(&fs::read(&f.current_marker).unwrap()).unwrap();
    assert_eq!(marker["managed_pair"], true);
    let receipt: Value =
        serde_json::from_slice(&hosted::success_receipt(&f.verifier.0).unwrap()).unwrap();
    assert_eq!(
        receipt,
        json!({"schema_version":1,"command":"hosted_managed_pair_install",
        "status":"committed","release_name":"bridge","rollback_generation":4})
    );
    assert!(
        !fs::read_dir(f.marker.parent().unwrap())
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".ctx-hosted-marker-"))
    );
}

#[test]
fn installed_identity_channel_signature_and_marker_fail_closed_before_publication() {
    for fault in [
        "installed",
        "candidate",
        "companion",
        "envelope",
        "channel",
        "path",
        "hash",
        "version",
    ] {
        let f = Fixture::new();
        match fault {
            "installed" => fs::write(&f.core, b"wrong-core").unwrap(),
            "candidate" => fs::write(&f.candidate, b"wrong-core").unwrap(),
            "companion" => fs::write(&f.companion, b"wrong-companion").unwrap(),
            "envelope" => fs::write(&f.envelope, b"not-signed").unwrap(),
            "channel" => f.change_marker("channel", json!("staging")),
            "path" => f.change_marker("install_path", json!(f.candidate)),
            "hash" => f.change_marker("sha256", json!(hash(b"wrong"))),
            "version" => f.change_marker("version", json!("0.0.0")),
            _ => unreachable!(),
        }
        assert!(f.complete().is_err(), "{fault}");
        assert!(
            !f.core
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("libexec/ctx-pro")
                .exists()
        );
        assert!(
            !f.core
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("libexec/ctx-pro.exe")
                .exists()
        );
    }
}

#[test]
fn installed_entry_respects_live_installation_lock() {
    let f = Fixture::new();
    let root = f.core.parent().unwrap().parent().unwrap();
    let _guard = try_acquire_managed_installation_mutation_at_root(root)
        .unwrap()
        .unwrap();
    assert!(f.complete().is_err());
}

#[test]
fn installed_entry_cannot_downgrade_existing_pair() {
    let mut f = Fixture::new();
    f.complete().unwrap();
    f.verifier.0 = VerifiedManagedPairIdentity::new(
        "older",
        f.verifier.0.target(),
        3,
        hash(b"older"),
        f.verifier.0.core().clone(),
        f.verifier.0.companion().clone(),
    )
    .unwrap();
    // The fixture verifier rejects retained envelope identity changes via the
    // committed state witness even though it returns the lower new identity.
    assert!(f.complete().is_err());
}
