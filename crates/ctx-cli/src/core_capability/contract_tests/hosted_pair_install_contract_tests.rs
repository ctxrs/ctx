use std::{ffi::OsString, fs, path::PathBuf};

use ctx_upgrade_engine::{
    try_acquire_managed_installation_mutation_at_root, ManagedPairComponentIdentity,
    ManagedPairTarget, ManagedPairVerifier, VerifiedManagedPairIdentity,
};
use serde_json::{json, Value};
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
        #[cfg(windows)]
        {
            use ctx_history_platform::platform_security::{
                restrict_private_directory, restrict_private_file,
            };
            // Match the released script: only installed bin/Core/marker are
            // protected. Download inputs and the root above bin are inherited.
            restrict_private_directory(core.parent().unwrap()).unwrap();
            restrict_private_file(&core).unwrap();
            restrict_private_file(&current_marker).unwrap();
        }
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
    #[cfg(windows)]
    fn create_released_pair(&self) -> [PathBuf; 3] {
        let root = self.core.parent().unwrap().parent().unwrap();
        fs::create_dir_all(root.join("libexec")).unwrap();
        fs::create_dir_all(root.join("share/ctx")).unwrap();
        let paths = [
            root.join("libexec/ctx-pro.exe"),
            root.join(ctx_upgrade_engine::MANAGED_PAIR_ENVELOPE_RELATIVE_PATH),
            root.join(ctx_upgrade_engine::MANAGED_PAIR_STATE_RELATIVE_PATH),
        ];
        // Match 1.3.1 hosted_pair_receipt/stage_hosted_bytes: ordinary directory
        // and file creation, schema-1 pair state, no DACL protection.
        fs::write(&paths[0], b"signed-companion").unwrap();
        fs::write(&paths[1], b"fixture-envelope").unwrap();
        fs::write(
            &paths[2],
            serde_json::to_vec(&json!({
                "contract":"ctx-managed-pair-state", "schema_version":1,
                "identity":self.verifier.0, "envelope_sha256":hash(b"fixture-envelope"),
                "envelope_size_bytes":b"fixture-envelope".len(),
            }))
            .unwrap(),
        )
        .unwrap();
        paths
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
    assert!(!fs::read_dir(f.marker.parent().unwrap())
        .unwrap()
        .any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".ctx-hosted-inputs-")));
}

#[cfg(windows)]
#[test]
fn released_windows_downloads_remain_inherited_while_kernel_inputs_are_private() {
    use ctx_history_platform::platform_security::{verify_private_directory, verify_private_file};
    let f = Fixture::new();
    let root = f.core.parent().unwrap().parent().unwrap();
    assert!(verify_private_directory(root).is_err());
    assert!(verify_private_directory(f.marker.parent().unwrap()).is_err());
    for input in [&f.envelope, &f.candidate, &f.companion, &f.marker] {
        assert!(verify_private_file(input).is_err());
    }
    assert_eq!(f.complete().unwrap(), f.verifier.0);
    assert!(verify_private_directory(root).is_ok());
    assert!(verify_private_directory(f.marker.parent().unwrap()).is_err());
    for input in [&f.envelope, &f.candidate, &f.companion, &f.marker] {
        assert!(verify_private_file(input).is_err());
    }
}

#[cfg(windows)]
#[test]
fn released_windows_existing_pair_permissions_are_adapted_without_losing_state() {
    use ctx_history_platform::platform_security::{verify_private_directory, verify_private_file};
    let f = Fixture::new();
    let paths = f.create_released_pair();
    let root = f.core.parent().unwrap().parent().unwrap();
    for relative in ["libexec", "share", "share/ctx"] {
        assert!(verify_private_directory(&root.join(relative)).is_err());
    }
    for path in &paths {
        assert!(verify_private_file(path).is_err());
    }
    let unrelated = root.join("share/ctx/unrelated.json");
    fs::write(&unrelated, b"not managed").unwrap();
    assert_eq!(f.complete().unwrap(), f.verifier.0);
    assert_eq!(f.complete().unwrap(), f.verifier.0);
    for relative in ["libexec", "share", "share/ctx"] {
        assert!(verify_private_directory(&root.join(relative)).is_ok());
    }
    for path in &paths {
        assert!(verify_private_file(path).is_ok());
    }
    assert_eq!(fs::read(&paths[0]).unwrap(), b"signed-companion");
    assert_eq!(fs::read(&paths[1]).unwrap(), b"fixture-envelope");
    let state: Value = serde_json::from_slice(&fs::read(&paths[2]).unwrap()).unwrap();
    assert_eq!(
        state["identity"],
        serde_json::to_value(&f.verifier.0).unwrap()
    );
    assert!(verify_private_file(&unrelated).is_err());
    assert_eq!(fs::read(&unrelated).unwrap(), b"not managed");
}

#[cfg(windows)]
#[test]
fn released_windows_existing_pair_keeps_rollback_witness() {
    let mut f = Fixture::new();
    let paths = f.create_released_pair();
    let previous: Vec<_> = paths.iter().map(|path| fs::read(path).unwrap()).collect();
    f.verifier.0 = VerifiedManagedPairIdentity::new(
        "older",
        f.verifier.0.target(),
        3,
        hash(b"older"),
        f.verifier.0.core().clone(),
        f.verifier.0.companion().clone(),
    )
    .unwrap();
    let error = f.complete().unwrap_err();
    assert!(format!("{error:#}").contains("rollback generation"));
    for (path, expected) in paths.iter().zip(previous) {
        assert_eq!(fs::read(path).unwrap(), expected);
    }
}

#[cfg(windows)]
#[test]
fn released_windows_existing_pair_rejects_hard_links_before_acl_changes() {
    use ctx_history_platform::platform_security::verify_private_file;
    for index in 0..3 {
        let f = Fixture::new();
        let paths = f.create_released_pair();
        let outside = f.marker.parent().unwrap().join("outside-linked-file");
        fs::hard_link(&paths[index], &outside).unwrap();
        let original = fs::read(&outside).unwrap();
        assert!(verify_private_file(&outside).is_err());
        let error = f.complete().unwrap_err();
        assert!(format!("{error:#}").contains("unique no-follow Windows file"));
        assert!(verify_private_file(&outside).is_err());
        assert_eq!(fs::read(&outside).unwrap(), original);
    }
}

#[cfg(unix)]
#[test]
fn released_completion_stages_inputs_without_relaxing_kernel_source_checks() {
    use ctx_upgrade_engine::{
        apply_or_resume_managed_pair_under_installation_lock, ManagedPairApplyInput,
    };
    use std::os::unix::fs::PermissionsExt as _;
    let f = Fixture::new();
    let root = f.core.parent().unwrap().parent().unwrap();
    let download = f.marker.parent().unwrap();
    fs::set_permissions(download, fs::Permissions::from_mode(0o777)).unwrap();
    {
        let _guard = try_acquire_managed_installation_mutation_at_root(root)
            .unwrap()
            .unwrap();
        let input = ManagedPairApplyInput::new(
            f.envelope.clone(),
            f.candidate.clone(),
            f.companion.clone(),
            f.marker.clone(),
        );
        assert!(
            apply_or_resume_managed_pair_under_installation_lock(root, &input, &f.verifier)
                .is_err()
        );
    }
    assert_eq!(f.complete().unwrap(), f.verifier.0);
    assert_eq!(
        fs::metadata(download).unwrap().permissions().mode() & 0o777,
        0o777
    );
    assert_eq!(fs::read(&f.envelope).unwrap(), b"fixture-envelope");
    assert_eq!(fs::read(&f.candidate).unwrap(), b"signed-core");
    assert_eq!(fs::read(&f.companion).unwrap(), b"signed-companion");
    assert_eq!(fs::read_dir(download).unwrap().count(), 4);
}

#[test]
fn installed_identity_channel_signature_and_marker_fail_closed_before_publication() {
    for fault in [
        "installed",
        "candidate",
        "companion",
        "candidate-digest",
        "companion-digest",
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
            "candidate-digest" => fs::write(&f.candidate, b"wrong--core").unwrap(),
            "companion-digest" => fs::write(&f.companion, b"wrong--companion").unwrap(),
            "envelope" => fs::write(&f.envelope, b"not-signed").unwrap(),
            "channel" => f.change_marker("channel", json!("staging")),
            "path" => f.change_marker("install_path", json!(f.candidate)),
            "hash" => f.change_marker("sha256", json!(hash(b"wrong"))),
            "version" => f.change_marker("version", json!("0.0.0")),
            _ => unreachable!(),
        }
        assert!(f.complete().is_err(), "{fault}");
        assert_eq!(
            fs::read_dir(f.marker.parent().unwrap()).unwrap().count(),
            4,
            "{fault} cleanup"
        );
        assert!(!f
            .core
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("libexec/ctx-pro")
            .exists());
        assert!(!f
            .core
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("libexec/ctx-pro.exe")
            .exists());
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
