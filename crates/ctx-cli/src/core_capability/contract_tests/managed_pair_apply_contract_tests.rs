use std::{ffi::OsString, fs, path::PathBuf};

use ctx_upgrade_engine::{
    try_acquire_managed_installation_mutation_at_root, ManagedPairComponentIdentity,
    ManagedPairTarget, VerifiedManagedPairIdentity, MANAGED_PAIR_INSTALLATION_LOCK_RELATIVE_PATH,
};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};

use super::super::managed_pair_apply::{
    managed_core_destination, marker_channel, read_install_marker, success_receipt,
    validate_install_marker, ApplyRequest, MAX_PATH_BYTES,
};
use super::super::*;

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn current_target() -> ManagedPairTarget {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "aarch64") => ManagedPairTarget::LinuxArm64,
        ("linux", "x86_64") => ManagedPairTarget::LinuxX64,
        ("macos", "aarch64") => ManagedPairTarget::MacosArm64,
        ("macos", "x86_64") => ManagedPairTarget::MacosX64,
        ("windows", "x86_64") => ManagedPairTarget::WindowsX64,
        (os, arch) => panic!("unsupported managed-pair test target {os}-{arch}"),
    }
}

fn marker_platform() -> &'static str {
    match current_target() {
        ManagedPairTarget::LinuxArm64 => "linux-aarch64",
        ManagedPairTarget::LinuxX64 => "linux-x64",
        ManagedPairTarget::MacosArm64 => "macos-arm64",
        ManagedPairTarget::MacosX64 => "macos-x64",
        ManagedPairTarget::WindowsX64 => "windows-x64",
    }
}

fn core_name() -> &'static str {
    if cfg!(windows) {
        "ctx.exe"
    } else {
        "ctx"
    }
}

fn marker_bytes(install_root: &std::path::Path, core: &[u8]) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "schema_version": 1,
        "manager": "ctx-hosted-installer",
        "install_path": managed_core_destination(install_root),
        "platform": marker_platform(),
        "channel": "stable",
        "version": env!("CARGO_PKG_VERSION"),
        "sha256": sha256(core),
    }))
    .unwrap()
}

fn identity(
    release: &str,
    generation: u64,
    core: &[u8],
    companion: &[u8],
) -> VerifiedManagedPairIdentity {
    VerifiedManagedPairIdentity::new(
        release,
        current_target(),
        generation,
        sha256(format!("manifest-{release}").as_bytes()),
        ManagedPairComponentIdentity::new(sha256(core), core.len() as u64).unwrap(),
        ManagedPairComponentIdentity::new(sha256(companion), companion.len() as u64).unwrap(),
    )
    .unwrap()
}

struct ArgFixture {
    _temp: tempfile::TempDir,
    install: PathBuf,
    candidate: PathBuf,
    companion: PathBuf,
    envelope: PathBuf,
    marker: PathBuf,
}

impl ArgFixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let install = temp.path().join("install");
        let download = temp.path().join("download");
        fs::create_dir_all(install.join("bin")).unwrap();
        fs::create_dir_all(&download).unwrap();
        let candidate = download.join(core_name());
        let companion = download.join(if cfg!(windows) {
            "ctx-pro.exe"
        } else {
            "ctx-pro"
        });
        let envelope = download.join("managed-pair-envelope.json");
        let marker = download.join("candidate-marker.json");
        fs::write(&candidate, b"candidate-core").unwrap();
        fs::write(&companion, b"candidate-companion").unwrap();
        fs::write(&envelope, b"signed-envelope-placeholder").unwrap();
        fs::write(&marker, marker_bytes(&install, b"candidate-core")).unwrap();
        Self {
            _temp: temp,
            install,
            candidate,
            companion,
            envelope,
            marker,
        }
    }

    fn arguments(&self) -> Vec<OsString> {
        [
            OsString::from("ctx"),
            OsString::from(MANAGED_PAIR_APPLY_INVOCATION),
            self.install.as_os_str().to_owned(),
            OsString::from("-"),
            self.envelope.as_os_str().to_owned(),
            self.candidate.as_os_str().to_owned(),
            self.companion.as_os_str().to_owned(),
            self.marker.as_os_str().to_owned(),
        ]
        .into()
    }
}

#[test]
fn only_the_exact_apply_argv_is_intercepted() {
    assert!(intercept(&["ctx".into(), MANAGED_PAIR_APPLY_INVOCATION.into()]).is_some());
    for removed in [
        "--ctx-core-hosted-pair-install-v1",
        "--ctx-core-managed-pair-swap-v1",
        "--ctx-core-managed-pair-uninstall-v1",
    ] {
        assert!(intercept(&["ctx".into(), removed.into()]).is_none());
    }
    assert!(intercept(&["ctx".into(), "--ctx-core-managed-pair-apply-v1=x".into(),]).is_none());
    assert!(intercept(&["ctx".into(), INVOCATION.into()]).is_some());
}

#[test]
fn fresh_temp_candidate_is_distinct_from_the_install_destination() {
    let fixture = ArgFixture::new();
    let request = ApplyRequest::parse(&fixture.arguments()).unwrap();

    assert!(!managed_core_destination(&fixture.install).exists());
    request.require_running_core(&fixture.candidate).unwrap();
    let other = fixture.candidate.with_file_name("other-core");
    fs::write(&other, b"candidate-core").unwrap();
    assert!(request.require_running_core(&other).is_err());

    let marker = read_install_marker(&fixture.marker).unwrap();
    let candidate = identity(
        "fresh-temp-candidate",
        1,
        b"candidate-core",
        b"candidate-companion",
    );
    validate_install_marker(
        &request,
        &marker,
        marker_channel(&marker).unwrap(),
        &candidate,
    )
    .unwrap();
}

#[test]
fn apply_arguments_require_dash_and_normalized_absolute_bounded_files() {
    let fixture = ArgFixture::new();
    let arguments = fixture.arguments();
    assert!(ApplyRequest::parse(&arguments).is_ok());

    let mut wrong_count = arguments.clone();
    wrong_count.pop();
    assert!(ApplyRequest::parse(&wrong_count).is_err());

    let mut data_root = arguments.clone();
    data_root[3] = fixture.install.as_os_str().to_owned();
    assert!(ApplyRequest::parse(&data_root).is_err());

    for index in [2, 4, 5, 6, 7] {
        let mut relative = arguments.clone();
        relative[index] = OsString::from("relative");
        assert!(
            ApplyRequest::parse(&relative).is_err(),
            "argv index {index}"
        );
    }

    let mut oversized = arguments.clone();
    oversized[5] = OsString::from(format!("/{}", "x".repeat(MAX_PATH_BYTES)));
    assert!(ApplyRequest::parse(&oversized).is_err());

    let mut traversal = arguments;
    traversal[4] = fixture
        .envelope
        .parent()
        .unwrap()
        .join("..")
        .join("download")
        .join("managed-pair-envelope.json")
        .into_os_string();
    assert!(ApplyRequest::parse(&traversal).is_err());
}

#[test]
fn marker_binds_the_destination_channel_platform_digest_and_candidate_version() {
    let fixture = ArgFixture::new();
    let request = ApplyRequest::parse(&fixture.arguments()).unwrap();
    let candidate = identity(
        "marker-binding",
        1,
        b"candidate-core",
        b"candidate-companion",
    );
    let marker = read_install_marker(&fixture.marker).unwrap();
    let channel = marker_channel(&marker).unwrap();
    validate_install_marker(&request, &marker, channel, &candidate).unwrap();

    for field in ["install_path", "platform", "channel", "version", "sha256"] {
        let mut value: Value =
            serde_json::from_slice(&marker_bytes(&fixture.install, b"candidate-core")).unwrap();
        value[field] = Value::String("mismatch".to_owned());
        fs::write(&fixture.marker, serde_json::to_vec(&value).unwrap()).unwrap();
        let marker = read_install_marker(&fixture.marker).unwrap();
        let result = marker_channel(&marker)
            .and_then(|observed| validate_install_marker(&request, &marker, observed, &candidate));
        assert!(result.is_err(), "accepted mismatched marker field {field}");
    }

    let mut contradictory: Value =
        serde_json::from_slice(&marker_bytes(&fixture.install, b"candidate-core")).unwrap();
    contradictory["staging_dogfood"] = Value::Bool(true);
    fs::write(&fixture.marker, serde_json::to_vec(&contradictory).unwrap()).unwrap();
    assert!(marker_channel(&read_install_marker(&fixture.marker).unwrap()).is_err());

    contradictory["channel"] = Value::String("staging".to_owned());
    fs::write(&fixture.marker, serde_json::to_vec(&contradictory).unwrap()).unwrap();
    assert_eq!(
        marker_channel(&read_install_marker(&fixture.marker).unwrap()).unwrap(),
        ctx_companion_bridge::ReleaseChannel::Staging
    );
}

#[test]
fn canonical_root_lock_contends_and_persists() {
    let fixture = ArgFixture::new();
    let first = try_acquire_managed_installation_mutation_at_root(&fixture.install)
        .unwrap()
        .unwrap();
    assert!(
        try_acquire_managed_installation_mutation_at_root(&fixture.install)
            .unwrap()
            .is_none()
    );
    drop(first);
    assert!(
        try_acquire_managed_installation_mutation_at_root(&fixture.install)
            .unwrap()
            .is_some()
    );
    assert!(fixture
        .install
        .join(MANAGED_PAIR_INSTALLATION_LOCK_RELATIVE_PATH)
        .is_file());
}

#[test]
fn success_receipt_is_the_exact_bounded_typed_json_object() {
    let expected =
        br#"{"schema_version":1,"command":"managed_pair_apply","ok":true,"status":"committed"}"#;
    assert_eq!(success_receipt(), expected);
    assert!(success_receipt().len() < MAX_RESPONSE_BYTES);
    let mut stdout = Vec::new();
    write_response_frame(&mut stdout, success_receipt()).unwrap();
    assert_eq!(stdout, [expected.as_slice(), b"\n"].concat());
    let value: Value = serde_json::from_slice(success_receipt()).unwrap();
    assert_eq!(value.as_object().unwrap().len(), 4);
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["command"], "managed_pair_apply");
    assert_eq!(value["ok"], true);
    assert_eq!(value["status"], "committed");
}

#[test]
fn removed_managed_pair_operations_are_not_in_the_capability_protocol() {
    for operation in [
        "ManagedPairBegin",
        "ManagedPairStage",
        "ManagedPairAbort",
        "ManagedPairStatus",
        "ManagedPairUninstall",
    ] {
        assert!(!API_INVENTORY.contains(operation));
        let frame = json!({
            "data_root": std::env::temp_dir(),
            "operation": operation,
            "options": {},
            "protocol_version": CORE_PRO_PROTOCOL_VERSION.get(),
            "schema_version": 1,
        });
        assert!(parse_frame(canonical(&frame).unwrap()).is_err());
    }
}
