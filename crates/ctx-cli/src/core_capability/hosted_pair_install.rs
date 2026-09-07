//! Compatibility for installer scripts served before the 1.3.2 bridge.
//! This preserves their installed-Core argv/receipt, not a second swap engine.

use std::{
    ffi::OsString,
    io::Write as _,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, anyhow, bail};
use ctx_history_platform::platform_security::create_private_file_new;
use ctx_upgrade_engine::{
    ManagedPairVerifier, VerifiedManagedPairIdentity, current_install_path,
    managed_install_path_identity_matches, try_acquire_managed_installation_mutation_at_root,
};
use serde_json::{Value, json};

use super::{
    CoreManagedPairVerifier,
    managed_pair_apply::{
        ApplyRequest, apply_under_installation_lock, managed_core_destination, marker_channel,
        normalized_absolute_path, read_bounded_regular_file, read_install_marker,
        read_released_install_marker, require_directory, require_regular_file,
        validate_install_marker,
    },
    write_response_frame,
};

const MAX_MARKER_BYTES: u64 = 64 * 1024;

pub(super) fn run(arguments: &[OsString], writer: impl std::io::Write) -> Result<()> {
    let core = current_install_path().context("certify running installed Core")?;
    let invoked = std::env::current_exe().context("resolve running installed Core")?;
    if !managed_install_path_identity_matches(&core, &invoked) {
        bail!("installed Core executable uses an unsupported or aliased path");
    }
    let request = parse(arguments, &core)?;
    let current_marker = installed_marker_path(&core);
    let channel = marker_channel(&read_released_install_marker(&current_marker)?)?;
    let verifier = CoreManagedPairVerifier::for_channel(channel);
    let identity = complete(&request, &core, &verifier)?;
    write_response_frame(writer, &success_receipt(&identity)?)
}

pub(super) fn parse(arguments: &[OsString], core: &Path) -> Result<ApplyRequest> {
    if arguments.len() != 6 {
        bail!("invalid hosted managed-pair install invocation");
    }
    let core = normalized_absolute_path(core.as_os_str(), "installed Core")?;
    require_regular_file(&core, "installed Core")?;
    let bin = core
        .parent()
        .filter(|path| path.file_name() == Some(std::ffi::OsStr::new("bin")))
        .ok_or_else(|| anyhow!("hosted pair installation requires the installed bin directory"))?;
    let root = bin
        .parent()
        .ok_or_else(|| anyhow!("installed Core has no installation root"))?;
    require_directory(root, "install root")?;
    if !managed_install_path_identity_matches(&core, &managed_core_destination(root)) {
        bail!("hosted pair installation requires the installed Core name");
    }
    let paths = arguments[2..]
        .iter()
        .map(|value| {
            let path = normalized_absolute_path(value, "hosted candidate")?;
            require_regular_file(&path, "hosted candidate")?;
            Ok(path)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(ApplyRequest {
        install_root: root.to_path_buf(),
        signed_envelope: paths[0].clone(),
        core: paths[1].clone(),
        companion: paths[2].clone(),
        install_marker: paths[3].clone(),
    })
}

fn installed_marker_path(core: &Path) -> PathBuf {
    let mut path = core.as_os_str().to_owned();
    path.push(".install.json");
    PathBuf::from(path)
}

pub(super) fn complete(
    request: &ApplyRequest,
    core: &Path,
    verifier: &dyn ManagedPairVerifier,
) -> Result<VerifiedManagedPairIdentity> {
    let _guard = try_acquire_managed_installation_mutation_at_root(&request.install_root)?
        .ok_or_else(|| anyhow!("managed-pair installation is busy"))?;
    let current_path = installed_marker_path(core);
    let installed = read_released_install_marker(&current_path)?;
    let channel = marker_channel(&installed)?;
    let candidate = read_released_install_marker(&request.install_marker)?;
    if marker_channel(&candidate)? != channel {
        bail!("hosted candidate and installed Core channels differ");
    }
    // Validate the installed marker against the same candidate identity while
    // holding the same lock used for publication. The shared owner repeats
    // envelope/component validation immediately before entering its kernel.
    let envelope =
        read_bounded_regular_file(&request.signed_envelope, 2 * 1024 * 1024, "signed envelope")?;
    let identity = verifier.verify_signed_envelope(&envelope)?;
    validate_install_marker(request, &installed, channel, &identity)?;
    validate_install_marker(request, &candidate, channel, &identity)?;

    let bytes = normalized_marker(&request.install_marker, &current_path)?;
    let temporary = MarkerTemporary::create(&request.install_marker, &bytes)?;
    let mut normalized = request.clone();
    normalized.install_marker = temporary.0.clone();
    let marker = read_install_marker(&normalized.install_marker)?;
    apply_under_installation_lock(&normalized, &marker, channel, verifier, Some(core))
}

pub(super) fn normalized_marker(candidate: &Path, current: &Path) -> Result<Vec<u8>> {
    let mut next: Value = serde_json::from_slice(&read_bounded_regular_file(
        candidate,
        MAX_MARKER_BYTES,
        "candidate install marker",
    )?)?;
    let previous: Value = serde_json::from_slice(&read_bounded_regular_file(
        current,
        MAX_MARKER_BYTES,
        "installed Core marker",
    )?)?;
    let next = next
        .as_object_mut()
        .ok_or_else(|| anyhow!("candidate install marker is not an object"))?;
    // The released Windows marker predates this field; the verified pair and
    // installed identity above establish it. All other extension fields survive.
    next.insert("managed_pair".to_owned(), Value::Bool(true));
    if next.get("man_pages").is_some_and(Value::is_null) {
        if let Some(receipt) = previous.get("man_pages") {
            next.insert("man_pages".to_owned(), receipt.clone());
        } else {
            next.remove("man_pages");
        }
    }
    let mut bytes = serde_json::to_vec_pretty(next)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_MARKER_BYTES {
        bail!("normalized hosted marker exceeds its bound");
    }
    Ok(bytes)
}

struct MarkerTemporary(PathBuf);
impl MarkerTemporary {
    fn create(candidate: &Path, bytes: &[u8]) -> Result<Self> {
        let parent = candidate
            .parent()
            .ok_or_else(|| anyhow!("candidate marker has no parent"))?;
        let path = parent.join(format!(
            ".ctx-hosted-marker-{}.json",
            uuid::Uuid::new_v4().simple()
        ));
        let mut file = create_private_file_new(&path).context("create normalized hosted marker")?;
        let temporary = Self(path);
        file.write_all(bytes)
            .context("write normalized hosted marker")?;
        file.sync_all().context("sync normalized hosted marker")?;
        Ok(temporary)
    }
}
impl Drop for MarkerTemporary {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub(super) fn success_receipt(identity: &VerifiedManagedPairIdentity) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(
        &json!({ "schema_version": 1, "command": "hosted_managed_pair_install",
        "status": "committed", "release_name": identity.release_name(),
        "rollback_generation": identity.rollback_generation() }),
    )?)
}
