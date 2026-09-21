use std::{
    ffi::OsStr,
    fs::{self, File},
    io::Read as _,
    path::{Component, Path, PathBuf},
};

use anyhow::{anyhow, bail, Context as _, Result};
use ctx_companion_bridge::ReleaseChannel;
use ctx_upgrade_engine::{
    apply_or_resume_managed_pair_under_installation_lock,
    ensure_hosted_transaction_inactive_under_installation_lock,
    inspect_managed_pair_under_installation_lock, managed_install_path_identity_matches,
    pending_managed_pair_hint, preflight_pending_managed_pair_under_installation_lock,
    resume_pending_managed_pair_under_installation_lock,
    try_acquire_managed_installation_mutation_at_root, InstallMarker, ManagedPairApplyInput,
    ManagedPairInstallationStatus, ManagedPairTarget, ManagedPairVerifier,
    VerifiedManagedPairIdentity, MANAGED_CORE_INSTALL_MARKER_RELATIVE_PATH,
};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use super::{write_response_frame, CoreManagedPairVerifier};

const ARGUMENT_COUNT: usize = 8;
pub(super) const MAX_PATH_BYTES: usize = 16 * 1024;
const MAX_ENVELOPE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_MARKER_BYTES: u64 = 64 * 1024;
const SUCCESS_RECEIPT: &[u8] =
    br#"{"schema_version":1,"command":"managed_pair_apply","ok":true,"status":"committed"}"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ApplyRequest {
    install_root: PathBuf,
    signed_envelope: PathBuf,
    core: PathBuf,
    companion: PathBuf,
    install_marker: PathBuf,
}

impl ApplyRequest {
    pub(super) fn parse(arguments: &[std::ffi::OsString]) -> Result<Self> {
        if arguments.len() != ARGUMENT_COUNT {
            bail!("invalid managed-pair apply invocation");
        }
        if arguments[3] != OsStr::new("-") {
            bail!("managed-pair apply V1 requires data root '-'");
        }
        let install_root = normalized_absolute_path(&arguments[2], "install root")?;
        require_directory(&install_root, "install root")?;
        let signed_envelope = normalized_absolute_path(&arguments[4], "signed envelope")?;
        let core = normalized_absolute_path(&arguments[5], "Core")?;
        let companion = normalized_absolute_path(&arguments[6], "companion")?;
        let install_marker = normalized_absolute_path(&arguments[7], "install marker")?;
        for (path, label) in [
            (&signed_envelope, "signed envelope"),
            (&core, "Core"),
            (&companion, "companion"),
            (&install_marker, "install marker"),
        ] {
            require_regular_file(path, label)?;
        }
        Ok(Self {
            install_root,
            signed_envelope,
            core,
            companion,
            install_marker,
        })
    }

    pub(super) fn require_running_core(&self, running_core: &Path) -> Result<()> {
        let running_core = fs::canonicalize(running_core)
            .context("canonicalize running managed-pair candidate Core")?;
        if !managed_install_path_identity_matches(&running_core, &self.core) {
            bail!("managed-pair apply must run from the supplied candidate Core");
        }
        Ok(())
    }

    fn destination_core(&self) -> PathBuf {
        managed_core_destination(&self.install_root)
    }

    fn kernel_input(&self) -> ManagedPairApplyInput {
        ManagedPairApplyInput::new(
            self.signed_envelope.clone(),
            self.core.clone(),
            self.companion.clone(),
            self.install_marker.clone(),
        )
    }
}

pub(super) fn managed_core_destination(install_root: &Path) -> PathBuf {
    let executable = MANAGED_CORE_INSTALL_MARKER_RELATIVE_PATH
        .strip_suffix(".install.json")
        .expect("managed Core marker slot must suffix the executable path");
    install_root.join(executable)
}

pub(super) fn run(arguments: &[std::ffi::OsString], writer: impl std::io::Write) -> Result<()> {
    let request = ApplyRequest::parse(arguments)?;
    request.require_running_core(
        &std::env::current_exe().context("resolve running managed-pair candidate Core")?,
    )?;
    apply(&request)?;
    write_response_frame(writer, SUCCESS_RECEIPT)
}

fn apply(request: &ApplyRequest) -> Result<()> {
    let _guard = try_acquire_managed_installation_mutation_at_root(&request.install_root)?
        .ok_or_else(|| anyhow!("managed-pair installation is busy"))?;
    ensure_hosted_transaction_inactive_under_installation_lock(&request.destination_core())?;
    let marker = read_install_marker(&request.install_marker)?;
    let channel = marker_channel(&marker)?;
    let verifier = CoreManagedPairVerifier::for_channel(channel);
    apply_with_verifier(request, &marker, channel, &verifier)
}

// Caller owns the canonical installation lock and has excluded hosted transactions.
fn apply_with_verifier(
    request: &ApplyRequest,
    marker: &InstallMarker,
    channel: ReleaseChannel,
    verifier: &dyn ManagedPairVerifier,
) -> Result<()> {
    let envelope = read_bounded_regular_file(
        &request.signed_envelope,
        MAX_ENVELOPE_BYTES,
        "signed envelope",
    )?;
    let envelope_sha256 = format!("{:x}", Sha256::digest(&envelope));
    let identity = verifier.verify_signed_envelope(&envelope)?;
    if is_retained_recovery_request(request) {
        // The installer authenticated the downloaded running executable from
        // ordinary release metadata. It is recovery code, not the old component.
        // Only exact retained input slots may select this existing resume path.
        if !pending_managed_pair_hint(&request.install_root)? {
            bail!("retained managed-pair recovery has no pending transaction");
        }
        validate_install_marker_identity(
            request,
            marker,
            channel,
            &identity,
            identity
                .release_name()
                .strip_prefix('v')
                .unwrap_or(identity.release_name()),
        )?;
        preflight_pending_managed_pair_under_installation_lock(
            &request.install_root,
            identity.core().sha256(),
            &envelope_sha256,
            verifier,
        )?;
        resume_pending_managed_pair_under_installation_lock(&request.install_root, verifier)?
            .ok_or_else(|| anyhow!("pending managed pair disappeared during recovery"))?;
    } else {
        verify_component(&request.core, identity.core(), "Core")?;
        verify_component(&request.companion, identity.companion(), "companion")?;
        validate_install_marker(request, marker, channel, &identity)?;
        // The generic kernel resumes retained work first. This CLI request
        // instead owns only its verified candidate, including same-identity retry.
        preflight_pending_managed_pair_under_installation_lock(
            &request.install_root,
            identity.core().sha256(),
            &envelope_sha256,
            verifier,
        )?;
        apply_or_resume_managed_pair_under_installation_lock(
            &request.install_root,
            &request.kernel_input(),
            verifier,
        )?;
    }
    match inspect_managed_pair_under_installation_lock(&request.install_root, verifier)? {
        ManagedPairInstallationStatus::Healthy {
            identity: active,
            envelope_sha256: active_envelope,
        } if active == identity && active_envelope.eq_ignore_ascii_case(&envelope_sha256) => Ok(()),
        _ => bail!("published managed pair does not match the requested signed candidate"),
    }
}

fn is_retained_recovery_request(request: &ApplyRequest) -> bool {
    let retained = request
        .install_root
        .join("share/ctx/.managed-pair-apply-v1");
    let companion = retained.join(if cfg!(windows) {
        "libexec/ctx-pro.exe"
    } else {
        "libexec/ctx-pro"
    });
    [
        (
            &request.signed_envelope,
            retained.join("share/ctx/managed-pair-envelope.json"),
        ),
        (&request.companion, companion),
        (
            &request.install_marker,
            retained.join(MANAGED_CORE_INSTALL_MARKER_RELATIVE_PATH),
        ),
    ]
    .into_iter()
    .all(|(supplied, expected)| managed_install_path_identity_matches(supplied, &expected))
}

pub(super) fn read_install_marker(path: &Path) -> Result<InstallMarker> {
    let bytes = read_bounded_regular_file(path, MAX_MARKER_BYTES, "install marker")?;
    let value: Value =
        serde_json::from_slice(&bytes).context("parse managed Core install marker")?;
    if value.get("schema_version").and_then(Value::as_u64) != Some(1)
        || value.get("manager").and_then(Value::as_str) != Some("ctx-hosted-installer")
        || value.get("managed_pair").and_then(Value::as_bool) != Some(true)
    {
        bail!("managed Core install marker schema is invalid");
    }
    Ok(InstallMarker {
        install_path: PathBuf::from(marker_string(&value, "install_path")?),
        platform: marker_string(&value, "platform")?.to_owned(),
        channel: marker_string(&value, "channel")?.to_owned(),
        version: marker_string(&value, "version")?.to_owned(),
        sha256: marker_string(&value, "sha256")?.to_owned(),
        staging_dogfood: value.get("staging_dogfood").and_then(Value::as_bool) == Some(true),
    })
}

fn marker_string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("managed Core install marker is missing {key}"))
}

pub(super) fn marker_channel(marker: &InstallMarker) -> Result<ReleaseChannel> {
    match (marker.channel.as_str(), marker.staging_dogfood) {
        ("stable", false) => Ok(ReleaseChannel::Stable),
        ("staging", true) => Ok(ReleaseChannel::Staging),
        _ => bail!("managed Core install marker channel is inconsistent"),
    }
}

pub(super) fn validate_install_marker(
    request: &ApplyRequest,
    marker: &InstallMarker,
    channel: ReleaseChannel,
    identity: &VerifiedManagedPairIdentity,
) -> Result<()> {
    validate_install_marker_identity(
        request,
        marker,
        channel,
        identity,
        env!("CARGO_PKG_VERSION"),
    )
}

fn validate_install_marker_identity(
    request: &ApplyRequest,
    marker: &InstallMarker,
    channel: ReleaseChannel,
    identity: &VerifiedManagedPairIdentity,
    expected_version: &str,
) -> Result<()> {
    if !managed_install_path_identity_matches(&request.destination_core(), &marker.install_path)
        || marker_platform(identity.target()) != marker.platform
        || marker_channel(marker)? != channel
        || !marker.sha256.eq_ignore_ascii_case(identity.core().sha256())
        || marker.version != expected_version
    {
        bail!("managed Core install marker does not match the signed candidate")
    }
    Ok(())
}

const fn marker_platform(target: ManagedPairTarget) -> &'static str {
    match target {
        ManagedPairTarget::LinuxArm64 => "linux-aarch64",
        ManagedPairTarget::LinuxX64 => "linux-x64",
        ManagedPairTarget::MacosArm64 => "macos-arm64",
        ManagedPairTarget::MacosX64 => "macos-x64",
        ManagedPairTarget::WindowsX64 => "windows-x64",
    }
}

fn verify_component(
    path: &Path,
    expected: &ctx_upgrade_engine::ManagedPairComponentIdentity,
    label: &str,
) -> Result<()> {
    let metadata = fs::symlink_metadata(path).with_context(|| format!("inspect {label}"))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() != expected.size_bytes()
        || sha256_file(path)? != expected.sha256()
    {
        bail!("managed-pair {label} does not match its signed identity");
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn read_bounded_regular_file(path: &Path, maximum: u64, label: &str) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path).with_context(|| format!("inspect {label}"))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > maximum
    {
        bail!("managed-pair {label} is not a bounded regular file");
    }
    fs::read(path).with_context(|| format!("read {label}"))
}

pub(super) fn normalized_absolute_path(value: &OsStr, label: &str) -> Result<PathBuf> {
    if value.as_encoded_bytes().len() > MAX_PATH_BYTES || value.as_encoded_bytes().contains(&0) {
        bail!("managed-pair {label} path exceeds its bound");
    }
    let path = PathBuf::from(value);
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        bail!("managed-pair {label} path must be normalized and absolute");
    }
    let canonical = fs::canonicalize(&path)
        .with_context(|| format!("canonicalize managed-pair {label} path"))?;
    if !managed_install_path_identity_matches(&canonical, &path) {
        bail!("managed-pair {label} path must already be normalized");
    }
    Ok(path)
}

pub(super) fn require_directory(path: &Path, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path).with_context(|| format!("inspect {label}"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        bail!("managed-pair {label} must be a directory");
    }
    Ok(())
}

pub(super) fn require_regular_file(path: &Path, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path).with_context(|| format!("inspect {label}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("managed-pair {label} must be a regular file");
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn success_receipt() -> &'static [u8] {
    SUCCESS_RECEIPT
}

#[cfg(test)]
mod retained_recovery_tests {
    use super::*;
    use ctx_history_platform::platform_security::{
        create_private_directory_all, restrict_private_file,
    };
    use ctx_upgrade_engine::{
        stage_managed_pair_under_installation_lock, ManagedPairComponentIdentity,
        MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH, MANAGED_PAIR_ENVELOPE_RELATIVE_PATH,
    };
    use serde_json::json;

    const OLD_CORE: &[u8] = b"authored retained 1.4.12 Core";
    const OLD_COMPANION: &[u8] = b"authored retained 1.4.12 companion";
    const ENVELOPE: &[u8] = b"authored signed-envelope fixture";

    struct Fixture {
        _temp: tempfile::TempDir,
        request: ApplyRequest,
        identity: VerifiedManagedPairIdentity,
        data: PathBuf,
    }

    impl ManagedPairVerifier for Fixture {
        fn verify_signed_envelope(&self, bytes: &[u8]) -> Result<VerifiedManagedPairIdentity> {
            if bytes != ENVELOPE {
                bail!("fixture signature rejected");
            }
            Ok(self.identity.clone())
        }
    }

    fn digest(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn write(path: &Path, bytes: &[u8]) -> Result<()> {
        fs::write(path, bytes)?;
        restrict_private_file(path)?;
        Ok(())
    }

    impl Fixture {
        fn new() -> Result<Self> {
            let temp = tempfile::tempdir()?;
            let root = fs::canonicalize(temp.path())?;
            let install = root.join("install");
            let inputs = root.join("inputs");
            let data = root.join("data/pro");
            for path in [install.join("bin"), inputs.clone(), data.clone()] {
                create_private_directory_all(&path)?;
            }
            write(&data.join("graph"), b"authored user graph")?;
            write(&data.join("key"), b"authored user key")?;
            let executor = inputs.join("authenticated-ordinary-1.5-candidate");
            write(&executor, b"authored ordinary 1.5 executable fixture")?;
            let target = match (std::env::consts::OS, std::env::consts::ARCH) {
                ("linux", "aarch64") => ManagedPairTarget::LinuxArm64,
                ("linux", "x86_64") => ManagedPairTarget::LinuxX64,
                ("macos", "aarch64") => ManagedPairTarget::MacosArm64,
                ("macos", "x86_64") => ManagedPairTarget::MacosX64,
                ("windows", "x86_64") => ManagedPairTarget::WindowsX64,
                _ => bail!("unsupported test target"),
            };
            let identity = VerifiedManagedPairIdentity::new(
                "v1.4.12",
                target,
                1,
                digest(b"authored old manifest"),
                ManagedPairComponentIdentity::new(digest(OLD_CORE), OLD_CORE.len() as u64)?,
                ManagedPairComponentIdentity::new(
                    digest(OLD_COMPANION),
                    OLD_COMPANION.len() as u64,
                )?,
            )?;
            let retained = install.join("share/ctx/.managed-pair-apply-v1");
            let companion_name = if cfg!(windows) {
                "libexec/ctx-pro.exe"
            } else {
                "libexec/ctx-pro"
            };
            let f = Self {
                _temp: temp,
                data,
                identity,
                request: ApplyRequest {
                    install_root: install.clone(),
                    signed_envelope: retained.join(MANAGED_PAIR_ENVELOPE_RELATIVE_PATH),
                    core: executor,
                    companion: retained.join(companion_name),
                    install_marker: retained.join(MANAGED_CORE_INSTALL_MARKER_RELATIVE_PATH),
                },
            };
            let destination = managed_core_destination(&install);
            write(&destination, b"authored previous Core")?;
            for (name, bytes) in [
                ("core", OLD_CORE),
                ("companion", OLD_COMPANION),
                ("envelope", ENVELOPE),
            ] {
                write(&inputs.join(name), bytes)?;
            }
            let marker = json!({"schema_version":1,"manager":"ctx-hosted-installer","managed_pair":true,
                "install_path":destination,"platform":marker_platform(target),"channel":"stable",
                "version":"1.4.12","sha256":digest(OLD_CORE)});
            write(&inputs.join("marker"), &serde_json::to_vec(&marker)?)?;
            let _lock = try_acquire_managed_installation_mutation_at_root(&install)?.unwrap();
            stage_managed_pair_under_installation_lock(
                &install,
                &ManagedPairApplyInput::new(
                    inputs.join("envelope"),
                    inputs.join("core"),
                    inputs.join("companion"),
                    inputs.join("marker"),
                ),
                &f,
            )?;
            // Reproduce the actual publication order: envelope, companion and
            // marker published, with Core still old (or missing in the test).
            write(&install.join(MANAGED_PAIR_ENVELOPE_RELATIVE_PATH), ENVELOPE)?;
            write(&install.join(companion_name), OLD_COMPANION)?;
            write(
                &install.join(MANAGED_CORE_INSTALL_MARKER_RELATIVE_PATH),
                &serde_json::to_vec(&marker)?,
            )?;
            drop(_lock);
            Ok(f)
        }

        fn recover(&self) -> Result<()> {
            let args: Vec<_> = [
                self.request.core.as_os_str().to_owned(),
                "--ctx-core-managed-pair-apply-v1".into(),
                self.request.install_root.as_os_str().to_owned(),
                "-".into(),
                self.request.signed_envelope.as_os_str().to_owned(),
                self.request.core.as_os_str().to_owned(),
                self.request.companion.as_os_str().to_owned(),
                self.request.install_marker.as_os_str().to_owned(),
            ]
            .into();
            let request = ApplyRequest::parse(&args)?;
            request.require_running_core(&self.request.core)?;
            let _lock =
                try_acquire_managed_installation_mutation_at_root(&request.install_root)?.unwrap();
            ensure_hosted_transaction_inactive_under_installation_lock(
                &request.destination_core(),
            )?;
            let marker = read_install_marker(&request.install_marker)?;
            apply_with_verifier(&request, &marker, marker_channel(&marker)?, self)
        }
    }

    #[test]
    fn ordinary_1_5_candidate_recovers_retained_1_4_before_installed_core_validation() -> Result<()>
    {
        for missing in [false, true] {
            let f = Fixture::new()?;
            if missing {
                fs::remove_file(f.request.destination_core())?;
            }
            let executable_before = fs::read(&f.request.core)?;
            f.recover()?;
            assert_eq!(fs::read(f.request.destination_core())?, OLD_CORE);
            assert_eq!(fs::read(&f.request.core)?, executable_before);
            assert!(!f
                .request
                .install_root
                .join(MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH)
                .exists());
            // The old pair remains for its owner to finish any scheduler/handoff.
            assert!(f
                .request
                .install_root
                .join(MANAGED_PAIR_ENVELOPE_RELATIVE_PATH)
                .exists());
            assert_eq!(fs::read(f.data.join("graph"))?, b"authored user graph");
            assert_eq!(fs::read(f.data.join("key"))?, b"authored user key");
        }
        Ok(())
    }

    #[test]
    fn retained_recovery_never_bypasses_pending_identity_or_input_geometry() -> Result<()> {
        for case in ["envelope", "component", "outside", "absent", "marker"] {
            let mut f = Fixture::new()?;
            match case {
                "envelope" => write(&f.request.signed_envelope, b"tampered envelope")?,
                "component" => write(
                    &managed_core_destination(
                        &f.request
                            .install_root
                            .join("share/ctx/.managed-pair-apply-v1"),
                    ),
                    b"tampered old Core",
                )?,
                "outside" => {
                    let other = f._temp.path().join("outside-envelope");
                    write(&other, ENVELOPE)?;
                    f.request.signed_envelope = other;
                }
                "absent" => fs::remove_file(
                    f.request
                        .install_root
                        .join(MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH),
                )?,
                "marker" => {
                    let mut marker: Value =
                        serde_json::from_slice(&fs::read(&f.request.install_marker)?)?;
                    marker["version"] = json!("1.5.0");
                    write(&f.request.install_marker, &serde_json::to_vec(&marker)?)?;
                }
                _ => unreachable!(),
            }
            let before = fs::read(f.request.destination_core())?;
            let pending = f
                .request
                .install_root
                .join(MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH);
            let pending_before = fs::read(&pending).ok();
            assert!(f.recover().is_err(), "accepted {case}");
            assert_eq!(fs::read(f.request.destination_core())?, before);
            assert_eq!(fs::read(pending).ok(), pending_before);
        }
        Ok(())
    }

    const FRESH_ENVELOPE: &[u8] = b"authored signed current-release envelope";

    struct FreshVerifier<'a> {
        old: &'a Fixture,
        fresh: VerifiedManagedPairIdentity,
    }

    impl ManagedPairVerifier for FreshVerifier<'_> {
        fn verify_signed_envelope(&self, bytes: &[u8]) -> Result<VerifiedManagedPairIdentity> {
            if bytes == FRESH_ENVELOPE {
                Ok(self.fresh.clone())
            } else {
                self.old.verify_signed_envelope(bytes)
            }
        }
    }

    // Inspect every fixture file, including all retained slots, the published
    // Core/marker, pending journal and user-data witnesses, without changing it.
    fn tree_bytes(root: &Path) -> Result<std::collections::BTreeMap<PathBuf, Option<Vec<u8>>>> {
        let mut result = std::collections::BTreeMap::new();
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                result.insert(path.clone(), None);
                result.extend(tree_bytes(&path)?);
            } else {
                result.insert(path.clone(), Some(fs::read(path)?));
            }
        }
        Ok(result)
    }

    #[test]
    fn ordinary_apply_binds_valid_candidate_before_any_pending_publication() -> Result<()> {
        for case in ["unrelated", "absent", "same"] {
            let old = Fixture::new()?;
            let install = if case == "unrelated" {
                old.request.install_root.clone()
            } else {
                fs::canonicalize(old._temp.path())?.join("fresh-install")
            };
            create_private_directory_all(&install.join("bin"))?;
            let inputs = fs::canonicalize(old._temp.path())?.join("fresh-inputs");
            create_private_directory_all(&inputs)?;
            let core = b"authored valid current-release Core";
            let companion = b"authored valid current-release companion";
            let identity = VerifiedManagedPairIdentity::new(
                format!("v{}", env!("CARGO_PKG_VERSION")),
                old.identity.target(),
                2,
                digest(b"authored current-release manifest"),
                ManagedPairComponentIdentity::new(digest(core), core.len() as u64)?,
                ManagedPairComponentIdentity::new(digest(companion), companion.len() as u64)?,
            )?;
            let request = ApplyRequest {
                install_root: install.clone(),
                signed_envelope: inputs.join("envelope"),
                core: inputs.join("core"),
                companion: inputs.join("companion"),
                install_marker: inputs.join("marker"),
            };
            write(&request.core, core)?;
            write(&request.companion, companion)?;
            write(&request.signed_envelope, FRESH_ENVELOPE)?;
            write(
                &request.install_marker,
                &serde_json::to_vec(&json!({
                    "schema_version":1,"manager":"ctx-hosted-installer","managed_pair":true,
                    "install_path":request.destination_core(),"platform":marker_platform(identity.target()),
                    "channel":"stable","version":env!("CARGO_PKG_VERSION"),"sha256":digest(core)
                }))?,
            )?;
            let verifier = FreshVerifier {
                old: &old,
                fresh: identity.clone(),
            };
            let marker = read_install_marker(&request.install_marker)?;
            let channel = marker_channel(&marker)?;
            let _lock = try_acquire_managed_installation_mutation_at_root(&install)?.unwrap();
            if case == "same" {
                stage_managed_pair_under_installation_lock(
                    &install,
                    &request.kernel_input(),
                    &verifier,
                )?;
            }
            // Prove this is a fully valid ordinary request, unlike the outside-
            // path negative fixture whose executing-Core digest is invalid.
            assert!(!is_retained_recovery_request(&request));
            request.require_running_core(&request.core)?;
            assert_eq!(verifier.verify_signed_envelope(FRESH_ENVELOPE)?, identity);
            verify_component(&request.core, identity.core(), "Core")?;
            verify_component(&request.companion, identity.companion(), "companion")?;
            validate_install_marker(&request, &marker, channel, &identity)?;
            assert_eq!(pending_managed_pair_hint(&install)?, case != "absent");
            let before = tree_bytes(old._temp.path())?;
            let result = apply_with_verifier(&request, &marker, channel, &verifier);
            if case == "unrelated" {
                assert!(format!("{:#}", result.unwrap_err())
                    .contains("expected Core/envelope identity"));
                assert_eq!(
                    tree_bytes(old._temp.path())?,
                    before,
                    "a different valid pending pair must remain completely untouched"
                );
            } else {
                result?;
                assert_eq!(fs::read(request.destination_core())?, core);
                assert!(!pending_managed_pair_hint(&install)?);
                assert!(
                    matches!(inspect_managed_pair_under_installation_lock(&install, &verifier)?,
                    ManagedPairInstallationStatus::Healthy { identity: active, .. } if active == identity)
                );
            }
        }
        Ok(())
    }
}
