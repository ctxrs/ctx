//! Compatibility for installer scripts served before the 1.3.2 bridge.
//! This preserves their installed-Core argv/receipt, not a second swap engine.

use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, anyhow, bail};
use ctx_history_platform::platform_security::{
    create_private_directory_all, create_private_file_new,
};
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
    let temporary = PreparedInputs::create(request, &envelope, &bytes, &identity)?;
    let marker = read_install_marker(&temporary.request.install_marker)?;
    apply_under_installation_lock(&temporary.request, &marker, channel, verifier, Some(core))
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

/// Released PowerShell downloads inherit their temporary directory's ACL.
/// Copy into protected inputs without changing those caller-owned files or
/// weakening the kernel's private-file contract. The existing owner verifies
/// the copied envelope and both component identities again before publication.
struct PreparedInputs {
    directory: PathBuf,
    request: ApplyRequest,
}
impl PreparedInputs {
    fn create(
        request: &ApplyRequest,
        envelope: &[u8],
        marker: &[u8],
        identity: &VerifiedManagedPairIdentity,
    ) -> Result<Self> {
        let parent = request
            .install_marker
            .parent()
            .ok_or_else(|| anyhow!("candidate marker has no parent"))?;
        let directory = parent.join(format!(
            ".ctx-hosted-inputs-{}",
            uuid::Uuid::new_v4().simple()
        ));
        create_private_directory_all(&directory).context("create protected hosted inputs")?;
        let temporary = Self {
            request: ApplyRequest {
                install_root: request.install_root.clone(),
                signed_envelope: directory.join("envelope.json"),
                core: directory.join("core"),
                companion: directory.join("companion"),
                install_marker: directory.join("marker.json"),
            },
            directory,
        };
        for (path, bytes) in [
            (&temporary.request.signed_envelope, envelope),
            (&temporary.request.install_marker, marker),
        ] {
            let mut file = create_private_file_new(path)?;
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        for (source, target, size) in [
            (
                &request.core,
                &temporary.request.core,
                identity.core().size_bytes(),
            ),
            (
                &request.companion,
                &temporary.request.companion,
                identity.companion().size_bytes(),
            ),
        ] {
            copy_bounded_component(source, target, size)?;
        }
        Ok(temporary)
    }
}
impl Drop for PreparedInputs {
    fn drop(&mut self) {
        for path in [
            &self.request.signed_envelope,
            &self.request.core,
            &self.request.companion,
            &self.request.install_marker,
        ] {
            let _ = fs::remove_file(path);
        }
        let _ = fs::remove_dir(&self.directory);
    }
}

fn copy_bounded_component(source: &Path, target: &Path, size: u64) -> Result<()> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let source = options
        .open(source)
        .context("open released hosted component")?;
    let metadata = source.metadata()?;
    if !metadata.is_file() || metadata.len() != size {
        bail!("released hosted component does not match its signed size");
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            bail!("released hosted component is a reparse point");
        }
    }
    let maximum = size
        .checked_add(1)
        .ok_or_else(|| anyhow!("hosted component size overflow"))?;
    let mut target = create_private_file_new(target)?;
    if std::io::copy(&mut source.take(maximum), &mut target)? != size {
        bail!("released hosted component changed size while copying");
    }
    target.sync_all().context("sync protected hosted component")
}

pub(super) fn success_receipt(identity: &VerifiedManagedPairIdentity) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(
        &json!({ "schema_version": 1, "command": "hosted_managed_pair_install",
        "status": "committed", "release_name": identity.release_name(),
        "rollback_generation": identity.rollback_generation() }),
    )?)
}
