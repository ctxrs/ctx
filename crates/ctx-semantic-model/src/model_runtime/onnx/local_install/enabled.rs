use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, Read, Seek, Write},
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};

use super::{SemanticRuntimeBackend, SemanticRuntimeInstall, SemanticRuntimeReport};
use crate::model_runtime::onnx::{
    verified_runtime::{
        expected_runtime_files, is_safe_relative_file, is_sha256, read_runtime_install_manifest,
        sha256_runtime_file, validate_runtime_candidate, RuntimeFile, RuntimeInstallManifest,
        HOSTED_INSTALLER_MANAGER, HOSTED_INSTALLER_METADATA_TRUST, LOCAL_OPERATOR_MANAGER,
        LOCAL_OPERATOR_METADATA_TRUST, RUNTIME_INSTALL_MANIFEST,
    },
    OnnxRuntimeFlavor, SEMANTIC_ONNXRUNTIME_DYLIB,
};

/// The loader searches `<root>/onnxruntime/<version>/<platform>` for every
/// runtime flavor, including Windows ML whose runtime name differs.
/// An install that does not use this literal layout is invisible to it.
const RUNTIME_LAYOUT_DIR: &str = "onnxruntime";

const MAX_ARCHIVE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_ENTRY_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_EXPANDED_BYTES: u64 = 6 * 1024 * 1024 * 1024;

const ZSTD_FRAME_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

impl SemanticRuntimeBackend {
    fn flavor(self) -> OnnxRuntimeFlavor {
        match self {
            Self::Cpu => OnnxRuntimeFlavor::Cpu,
            Self::Cuda => OnnxRuntimeFlavor::Cuda,
            Self::WindowsMl => OnnxRuntimeFlavor::WindowsMl,
        }
    }
}

pub fn install_operator_runtime(
    request: &SemanticRuntimeInstall<'_>,
) -> Result<SemanticRuntimeReport> {
    // Only the sidecars published as a self-contained zstd-framed tar can be
    // installed here. Windows ML always ships as a zip, and so does the Windows
    // CPU sidecar, so accepting either would mean carrying a second archive
    // reader in this trust boundary for a path this command cannot verify.
    // Inference can never reach one either: the identifiable set is exactly the
    // locally installable set.
    if request.backend == Some(SemanticRuntimeBackend::WindowsMl) {
        return Err(anyhow!(
            "local runtime install cannot provision the Windows ML runtime, which ships as a zip; provision it with the hosted installer"
        ));
    }
    if cfg!(target_os = "windows") && request.backend == Some(SemanticRuntimeBackend::Cpu) {
        return Err(anyhow!(
            "local runtime install cannot provision the Windows CPU runtime, whose sidecar ships as a zip; provision it with the hosted installer"
        ));
    }
    let digest = pinned_digest(request.expected_archive_sha256)?;
    if !request.runtime_root.is_absolute() {
        return Err(anyhow!(
            "runtime root {} is not absolute; the runtime loader only searches absolute runtime roots",
            request.runtime_root.display()
        ));
    }

    let archive = absolute_archive_path(request.archive)?;
    let artifact_url = file_url(&archive)?;
    // The digest settles first: nothing about the contents is trusted, or even
    // read to identify the runtime, until the operator's pin matches.
    let mut file = open_verified_archive(&archive, &digest)?;
    verify_tar_zstd_container(&mut file)?;
    let backend = resolve_archive_backend(&mut file, request.backend)?;
    file.rewind()
        .with_context(|| format!("rewind sidecar archive {}", archive.display()))?;
    let flavor = backend.flavor();
    let platform = backend_platform(backend, flavor)?;

    let version_dir = request
        .runtime_root
        .join(RUNTIME_LAYOUT_DIR)
        .join(flavor.version());
    let root = version_dir.join(platform);
    let library = root.join("lib").join(SEMANTIC_ONNXRUNTIME_DYLIB);
    reject_unusable_target(&root, request.replace_existing)?;
    ensure_private_directory(&version_dir)?;

    let unique = uuid::Uuid::new_v4().simple().to_string();
    let staging = version_dir.join(format!(".{platform}.ctx-install-{unique}"));
    let backup = version_dir.join(format!(".{platform}.ctx-replaced-{unique}"));
    reject_existing_path(&staging)?;
    reject_existing_path(&backup)?;

    let expected_files = contract_files(flavor);
    let manifest = match stage_runtime_tree(
        &mut file,
        &staging,
        &expected_files,
        flavor,
        platform,
        &digest,
        &artifact_url,
    ) {
        Ok(manifest) => manifest,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
    };

    publish_staged_tree(&staging, &root, &backup)?;
    let identity = match validate_runtime_candidate(&library, flavor) {
        Ok(identity) => identity,
        Err(error) => {
            // The loader refused the tree we just published, so unpublish it:
            // leaving it in place would fail every later load and would shadow
            // whatever install it replaced.
            let _ = fs::remove_dir_all(&root);
            restore_backup(&backup, &root);
            let error = error.context(format!(
                "validate the installed {} runtime",
                backend.as_str()
            ));
            if backup.exists() {
                return Err(error.context(format!(
                    "the rejected install could not be rolled back; remove {} and restore {} by hand",
                    root.display(),
                    backup.display()
                )));
            }
            return Err(error);
        }
    };
    let _ = fs::remove_dir_all(&backup);

    Ok(SemanticRuntimeReport {
        backend,
        platform: platform.to_owned(),
        version: flavor.version().to_owned(),
        root,
        library,
        archive_sha256: digest,
        manager: LOCAL_OPERATOR_MANAGER,
        metadata_trust: LOCAL_OPERATOR_METADATA_TRUST,
        files: manifest.files.len(),
        identity,
    })
}

/// Reads which runtime a sidecar archive carries out of its own member set.
///
/// Every flavor's contract names a different set of files, so the contents are
/// self-describing. Nothing is extracted: entries are walked through the same
/// safety rejections extraction applies, and their bodies are skipped.
pub fn identify_runtime_archive(archive: &Path) -> Result<SemanticRuntimeBackend> {
    let archive = absolute_archive_path(archive)?;
    let mut file = open_bounded_archive(&archive)?;
    verify_tar_zstd_container(&mut file)?;
    identify_archive_contents(&mut file)
        .with_context(|| format!("identify sidecar archive {}", archive.display()))
}

/// The archive decides which runtime is installed. An asserted backend is a
/// cross-check an operator can opt into, so a disagreement fails naming both
/// sides instead of installing something other than what they meant.
fn resolve_archive_backend(
    file: &mut fs::File,
    asserted: Option<SemanticRuntimeBackend>,
) -> Result<SemanticRuntimeBackend> {
    let identified = identify_archive_contents(file)?;
    match asserted {
        Some(asserted) if asserted != identified => Err(anyhow!(
            "the requested {requested} runtime does not match this archive, which carries the {identified} runtime; install it without requesting a backend, or request {identified}",
            requested = asserted.as_str(),
            identified = identified.as_str(),
        )),
        _ => Ok(identified),
    }
}

/// Matches an archive's member set against the in-binary contract of every
/// backend this build can install on this target. Set equality is exact, so an
/// archive that is truncated, repacked, or simply not a ctx sidecar matches
/// nothing rather than being installed as the closest flavor.
fn identify_archive_contents(file: &mut fs::File) -> Result<SemanticRuntimeBackend> {
    let candidates = super::supported_local_runtime_backends();
    if candidates.is_empty() {
        return Err(anyhow!(
            "this build installs no semantic runtime from a local archive on {} {}",
            std::env::consts::OS,
            std::env::consts::ARCH,
        ));
    }
    let mut contracts = Vec::with_capacity(candidates.len());
    let mut directories = BTreeSet::new();
    for backend in candidates {
        let files = contract_files(backend.flavor());
        directories.extend(contract_directories(&files));
        contracts.push((*backend, files));
    }
    let members = probe_archive_members(file, &directories)?;
    let mut matched = contracts
        .iter()
        .filter(|(_, contract)| {
            members.len() == contract.len() && contract.iter().all(|file| members.contains(*file))
        })
        .map(|(backend, _)| *backend);
    let Some(identified) = matched.next() else {
        return Err(unmatched_archive_error(&members, &contracts));
    };
    if let Some(ambiguous) = matched.next() {
        // Two flavors sharing a member set would make the archive unreadable as
        // a selection, so ctx refuses rather than guessing between them.
        return Err(anyhow!(
            "sidecar archive contents match both the {} and {} runtime contracts, so ctx cannot tell which runtime it carries",
            identified.as_str(),
            ambiguous.as_str()
        ));
    }
    Ok(identified)
}

/// Names the closest contract and a few offending entries, because "matches no
/// contract" alone leaves an operator with a verified archive and no idea what
/// is wrong with it.
fn unmatched_archive_error(
    members: &BTreeSet<String>,
    contracts: &[(SemanticRuntimeBackend, BTreeSet<&'static str>)],
) -> anyhow::Error {
    let closest = contracts
        .iter()
        .map(|(backend, contract)| {
            let missing = contract
                .iter()
                .filter(|file| !members.contains(**file))
                .map(|file| (*file).to_owned())
                .collect::<Vec<_>>();
            let unexpected = members
                .iter()
                .filter(|file| !contract.contains(file.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            (missing.len() + unexpected.len(), *backend, missing, unexpected)
        })
        .min_by_key(|(distance, ..)| *distance);
    let Some((_, backend, missing, unexpected)) = closest else {
        return anyhow!("sidecar archive matches no semantic runtime contract");
    };
    anyhow!(
        "sidecar archive contents match no semantic runtime contract: it carries {count} files, and against the closest contract ({backend}) it is missing {missing} and has {unexpected} unexpected",
        count = members.len(),
        backend = backend.as_str(),
        missing = sampled_entries(&missing),
        unexpected = sampled_entries(&unexpected),
    )
}

/// A bounded sample: a mismatched archive can differ by every file in the
/// contract, and an error listing eighteen paths is not read.
fn sampled_entries(entries: &[String]) -> String {
    const SAMPLE: usize = 3;
    if entries.is_empty() {
        return "nothing".to_owned();
    }
    let shown = entries
        .iter()
        .take(SAMPLE)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    match entries.len().checked_sub(SAMPLE) {
        Some(0) | None => format!("[{shown}]"),
        Some(rest) => format!("[{shown}, +{rest} more]"),
    }
}

/// Walks an archive for its member names only. Directory entries are held to
/// the union of the candidate contracts because the flavor is not known yet;
/// extraction still holds them to the one contract it resolved.
fn probe_archive_members(
    file: &mut fs::File,
    expected_directories: &BTreeSet<String>,
) -> Result<BTreeSet<String>> {
    let mut sink = ProbedEntries {
        expected_directories,
        seen: BTreeSet::new(),
        files: BTreeSet::new(),
        expanded: 0,
    };
    read_tar_zstd(file, &mut sink)?;
    Ok(sink.files)
}

pub fn installed_runtime_report(
    runtime_root: &Path,
    backend: SemanticRuntimeBackend,
) -> Result<Option<SemanticRuntimeReport>> {
    let flavor = backend.flavor();
    let platform = backend_platform(backend, flavor)?;
    let root = runtime_root
        .join(RUNTIME_LAYOUT_DIR)
        .join(flavor.version())
        .join(platform);
    match fs::symlink_metadata(&root) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(anyhow!(
                "inspect runtime directory {}: {error}",
                root.display()
            ))
        }
    }
    let library = root.join("lib").join(SEMANTIC_ONNXRUNTIME_DYLIB);
    let identity = validate_runtime_candidate(&library, flavor)?;
    let manifest = read_runtime_install_manifest(&root)?;
    let (manager, metadata_trust) =
        static_trust_tier(&manifest.manager, &manifest.metadata_trust)?;
    Ok(Some(SemanticRuntimeReport {
        backend,
        platform: platform.to_owned(),
        version: flavor.version().to_owned(),
        root,
        library,
        archive_sha256: manifest.sha256,
        manager,
        metadata_trust,
        files: manifest.files.len(),
        identity,
    }))
}

fn backend_platform(
    backend: SemanticRuntimeBackend,
    flavor: OnnxRuntimeFlavor,
) -> Result<&'static str> {
    flavor.platform_dir().with_context(|| {
        format!(
            "the {} runtime has no sidecar for {} {}",
            backend.as_str(),
            std::env::consts::OS,
            std::env::consts::ARCH,
        )
    })
}

/// The loader compares lowercase hex, so an uppercase or truncated pin is
/// rejected here rather than silently never matching.
fn pinned_digest(expected: &str) -> Result<String> {
    if !is_sha256(expected) {
        return Err(anyhow!(
            "expected archive SHA-256 {expected:?} is not 64 lowercase hexadecimal characters"
        ));
    }
    Ok(expected.to_owned())
}

fn absolute_archive_path(archive: &Path) -> Result<PathBuf> {
    if archive.is_absolute() {
        return Ok(archive.to_path_buf());
    }
    let current = std::env::current_dir().context("resolve the current directory")?;
    Ok(current.join(archive))
}

fn file_url(archive: &Path) -> Result<String> {
    url::Url::from_file_path(archive)
        .map(|url| url.to_string())
        .map_err(|()| {
            anyhow!(
                "archive path {} cannot be expressed as a file URL",
                archive.display()
            )
        })
}

/// Opens a sidecar archive under the size bound every read of it inherits.
/// Identification and installation share it so a probe cannot be pointed at a
/// device node or an unbounded file the installer would have refused.
fn open_bounded_archive(archive: &Path) -> Result<fs::File> {
    let file = fs::File::open(archive)
        .with_context(|| format!("open sidecar archive {}", archive.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("inspect sidecar archive {}", archive.display()))?;
    if !metadata.is_file() {
        return Err(anyhow!(
            "sidecar archive {} is not a regular file",
            archive.display()
        ));
    }
    if metadata.len() > MAX_ARCHIVE_BYTES {
        return Err(anyhow!(
            "sidecar archive {} exceeds the {MAX_ARCHIVE_BYTES} byte limit",
            archive.display()
        ));
    }
    Ok(file)
}

/// Hashes the archive and hands back the same open handle so identification and
/// extraction read exactly the bytes that were measured.
fn open_verified_archive(archive: &Path, expected: &str) -> Result<fs::File> {
    let mut file = open_bounded_archive(archive)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("hash sidecar archive {}", archive.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual = format!("{:x}", hasher.finalize());
    if actual != expected {
        return Err(anyhow!(
            "sidecar archive {} has SHA-256 {actual}, not the pinned {expected}",
            archive.display()
        ));
    }
    file.rewind()
        .with_context(|| format!("rewind sidecar archive {}", archive.display()))?;
    Ok(file)
}

/// Every sidecar ctx installs locally is a zstd-framed tar. Settling the
/// container before any runtime directory exists keeps a rejected archive from
/// touching the filesystem.
fn verify_tar_zstd_container(file: &mut fs::File) -> Result<()> {
    let mut magic = [0_u8; 4];
    file.read_exact(&mut magic)
        .context("read the sidecar archive signature")?;
    file.rewind().context("rewind the sidecar archive")?;
    if magic != ZSTD_FRAME_MAGIC {
        return Err(anyhow!(
            "unsupported sidecar archive format (leading bytes {magic:02x?}); ctx installs the .tar.zst sidecar"
        ));
    }
    Ok(())
}

fn contract_files(flavor: OnnxRuntimeFlavor) -> BTreeSet<&'static str> {
    expected_runtime_files(flavor).into_iter().collect()
}

fn contract_directories(files: &BTreeSet<&'static str>) -> BTreeSet<String> {
    let mut directories = BTreeSet::new();
    for file in files {
        let mut prefix = String::new();
        let mut components = file.split('/').peekable();
        while let Some(component) = components.next() {
            if components.peek().is_none() {
                break;
            }
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(component);
            directories.insert(prefix.clone());
        }
    }
    directories
}

fn stage_runtime_tree(
    file: &mut fs::File,
    staging: &Path,
    expected_files: &BTreeSet<&'static str>,
    flavor: OnnxRuntimeFlavor,
    platform: &str,
    digest: &str,
    artifact_url: &str,
) -> Result<RuntimeInstallManifest> {
    ensure_private_directory(staging)?;
    extract_sidecar_archive(file, staging, expected_files)?;
    let files = audit_staged_tree(staging, expected_files)?;
    let manifest = RuntimeInstallManifest {
        schema_version: 1,
        manager: LOCAL_OPERATOR_MANAGER.to_owned(),
        metadata_trust: LOCAL_OPERATOR_METADATA_TRUST.to_owned(),
        runtime: flavor.runtime_name().to_owned(),
        platform: platform.to_owned(),
        version: flavor.version().to_owned(),
        sha256: digest.to_owned(),
        artifact_url: artifact_url.to_owned(),
        installed_at: rfc3339_utc_now(),
        files,
    };
    let manifest_path = staging.join(RUNTIME_INSTALL_MANIFEST);
    let body = serde_json::to_vec(&manifest).context("encode the runtime install manifest")?;
    let mut handle = create_staged_file(&manifest_path)?;
    handle
        .write_all(&body)
        .with_context(|| format!("write {}", manifest_path.display()))?;
    handle
        .sync_all()
        .with_context(|| format!("flush {}", manifest_path.display()))?;
    Ok(manifest)
}

/// Walks the staged tree and measures it. Stray directories are rejected
/// here, not only stray files: the loader's own file audit ignores empty
/// directories, so an archive that unpacks one must fail the install.
fn audit_staged_tree(
    staging: &Path,
    expected_files: &BTreeSet<&'static str>,
) -> Result<Vec<RuntimeFile>> {
    let expected_directories = contract_directories(expected_files);
    let mut found = BTreeMap::new();
    walk_staged_tree(
        staging,
        staging,
        &expected_directories,
        expected_files,
        &mut found,
    )?;
    let staged = found.keys().cloned().collect::<BTreeSet<_>>();
    let expected = expected_files
        .iter()
        .map(|file| (*file).to_owned())
        .collect::<BTreeSet<_>>();
    if staged != expected {
        let missing = expected.difference(&staged).cloned().collect::<Vec<_>>();
        let extra = staged.difference(&expected).cloned().collect::<Vec<_>>();
        return Err(anyhow!(
            "sidecar archive does not match the runtime file contract (missing: [{}], unexpected: [{}])",
            missing.join(", "),
            extra.join(", ")
        ));
    }
    Ok(found.into_values().collect())
}

fn walk_staged_tree(
    staging: &Path,
    directory: &Path,
    expected_directories: &BTreeSet<String>,
    expected_files: &BTreeSet<&'static str>,
    found: &mut BTreeMap<String, RuntimeFile>,
) -> Result<()> {
    for entry in fs::read_dir(directory)
        .with_context(|| format!("read staged directory {}", directory.display()))?
    {
        let entry = entry.with_context(|| {
            format!("read staged directory entry in {}", directory.display())
        })?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("inspect staged entry {}", path.display()))?;
        let relative = path
            .strip_prefix(staging)
            .map_err(|_| anyhow!("staged entry escaped the staging directory"))?
            .to_str()
            .ok_or_else(|| anyhow!("staged entry {} is not UTF-8", path.display()))?
            .replace('\\', "/");
        if metadata.file_type().is_symlink() {
            return Err(anyhow!("staged tree contains symbolic link {relative}"));
        }
        if metadata.is_dir() {
            if !expected_directories.contains(&relative) {
                return Err(anyhow!(
                    "staged tree contains unexpected directory {relative}"
                ));
            }
            walk_staged_tree(staging, &path, expected_directories, expected_files, found)?;
            continue;
        }
        if !metadata.is_file() {
            return Err(anyhow!("staged tree contains unsupported entry {relative}"));
        }
        if relative == RUNTIME_INSTALL_MANIFEST {
            return Err(anyhow!(
                "sidecar archive carries its own {RUNTIME_INSTALL_MANIFEST}"
            ));
        }
        if !expected_files.contains(relative.as_str()) {
            return Err(anyhow!("staged tree contains unexpected file {relative}"));
        }
        let size = metadata.len();
        if size == 0 {
            return Err(anyhow!("staged runtime file {relative} is empty"));
        }
        found.insert(
            relative.clone(),
            RuntimeFile {
                path: relative,
                size,
                sha256: sha256_runtime_file(&path)?,
            },
        );
    }
    Ok(())
}

fn reject_unusable_target(root: &Path, replace_existing: bool) -> Result<()> {
    match fs::symlink_metadata(root) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(anyhow!(
            "inspect runtime directory {}: {error}",
            root.display()
        )),
        Ok(metadata) if metadata.file_type().is_symlink() => Err(anyhow!(
            "runtime directory {} is a symbolic link; refusing to install through it",
            root.display()
        )),
        Ok(metadata) if !metadata.is_dir() => Err(anyhow!(
            "runtime path {} exists and is not a directory",
            root.display()
        )),
        Ok(_) if !replace_existing => Err(anyhow!(
            "a semantic runtime is already installed at {}; pass the replace option to overwrite it",
            root.display()
        )),
        Ok(_) => Ok(()),
    }
}

fn reject_existing_path(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(anyhow!("inspect {}: {error}", path.display())),
        Ok(_) => Err(anyhow!(
            "install scratch path {} already exists",
            path.display()
        )),
    }
}

fn publish_staged_tree(staging: &Path, root: &Path, backup: &Path) -> Result<()> {
    let displaced = match fs::rename(root, backup) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => {
            let _ = fs::remove_dir_all(staging);
            return Err(anyhow!(
                "move the existing runtime {} aside: {error}",
                root.display()
            ));
        }
    };
    if let Err(error) = fs::rename(staging, root) {
        if displaced {
            restore_backup(backup, root);
        }
        let _ = fs::remove_dir_all(staging);
        return Err(anyhow!(
            "publish the staged runtime to {}: {error}",
            root.display()
        ));
    }
    Ok(())
}

fn restore_backup(backup: &Path, root: &Path) {
    if backup.exists() {
        let _ = fs::rename(backup, root);
    }
}

fn static_trust_tier(
    manager: &str,
    metadata_trust: &str,
) -> Result<(&'static str, &'static str)> {
    match (manager, metadata_trust) {
        (HOSTED_INSTALLER_MANAGER, HOSTED_INSTALLER_METADATA_TRUST) => {
            Ok((HOSTED_INSTALLER_MANAGER, HOSTED_INSTALLER_METADATA_TRUST))
        }
        (LOCAL_OPERATOR_MANAGER, LOCAL_OPERATOR_METADATA_TRUST) => {
            Ok((LOCAL_OPERATOR_MANAGER, LOCAL_OPERATOR_METADATA_TRUST))
        }
        _ => Err(anyhow!(
            "installed runtime manifest declares an unrecognized provenance tier {manager:?}/{metadata_trust:?}"
        )),
    }
}

fn ensure_private_directory(path: &Path) -> Result<()> {
    ctx_history_platform::platform_security::ensure_private_directory(path)
        .with_context(|| format!("create private directory {}", path.display()))
}

fn create_staged_file(path: &Path) -> Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o644);
    }
    options
        .open(path)
        .with_context(|| format!("create staged file {}", path.display()))
}

/// Accepts exactly the entries the contract names, in whatever order the
/// archive presents them, and writes nothing else to disk.
fn extract_sidecar_archive(
    file: &mut fs::File,
    staging: &Path,
    expected_files: &BTreeSet<&'static str>,
) -> Result<()> {
    let expected_directories = contract_directories(expected_files);
    for directory in &expected_directories {
        ensure_private_directory(&staging.join(directory))?;
    }
    let mut sink = StagedFiles {
        staging,
        expected_files,
        expected_directories: &expected_directories,
        seen: BTreeSet::new(),
        expanded: 0,
    };
    read_tar_zstd(file, &mut sink)
}

/// The per-entry policy a sidecar archive walk enforces. Identification and
/// extraction implement it over one walk, so an archive ctx would refuse to
/// install is refused just as early when it is only being identified.
trait SidecarEntries {
    fn accept_name(&mut self, raw: &str) -> Result<(String, bool)>;
    fn accept_directory(&mut self, name: &str) -> Result<()>;
    fn accept_file(&mut self, name: &str, declared: u64, reader: &mut dyn Read) -> Result<()>;
}

/// Normalizes an archive name and rejects everything that could point outside
/// the staging root: absolute paths, drive-qualified paths, backslash
/// separators, `.`/`..` components, and duplicates.
fn accept_entry_name(seen: &mut BTreeSet<String>, raw: &str) -> Result<(String, bool)> {
    let is_directory = raw.ends_with('/');
    let name = raw.strip_suffix('/').unwrap_or(raw);
    if !is_safe_relative_file(name) || name.contains(':') {
        return Err(anyhow!("unsafe sidecar archive entry {raw:?}"));
    }
    if !seen.insert(name.to_ascii_lowercase()) {
        return Err(anyhow!(
            "duplicate or case-colliding sidecar archive entry {raw:?}"
        ));
    }
    Ok((name.to_owned(), is_directory))
}

/// The size bounds an entry has to satisfy before its body is read at all,
/// applied to the declared header size so a lying header cannot be used to
/// stream past the limits.
fn accept_entry_size(expanded: &mut u64, name: &str, declared: u64) -> Result<()> {
    if declared == 0 {
        return Err(anyhow!("sidecar archive file {name:?} is empty"));
    }
    if declared > MAX_ENTRY_BYTES {
        return Err(anyhow!(
            "sidecar archive file {name:?} exceeds the {MAX_ENTRY_BYTES} byte per-file limit"
        ));
    }
    *expanded = expanded
        .checked_add(declared)
        .ok_or_else(|| anyhow!("sidecar archive expanded size overflow"))?;
    if *expanded > MAX_EXPANDED_BYTES {
        return Err(anyhow!(
            "sidecar archive expands beyond the {MAX_EXPANDED_BYTES} byte limit"
        ));
    }
    Ok(())
}

struct StagedFiles<'a> {
    staging: &'a Path,
    expected_files: &'a BTreeSet<&'static str>,
    expected_directories: &'a BTreeSet<String>,
    seen: BTreeSet<String>,
    expanded: u64,
}

impl SidecarEntries for StagedFiles<'_> {
    fn accept_name(&mut self, raw: &str) -> Result<(String, bool)> {
        accept_entry_name(&mut self.seen, raw)
    }

    fn accept_directory(&mut self, name: &str) -> Result<()> {
        if !self.expected_directories.contains(name) {
            return Err(anyhow!("unexpected sidecar archive directory {name:?}"));
        }
        Ok(())
    }

    fn accept_file(&mut self, name: &str, declared: u64, reader: &mut dyn Read) -> Result<()> {
        let expected: &'static str = *self
            .expected_files
            .get(name)
            .ok_or_else(|| anyhow!("unexpected sidecar archive file {name:?}"))?;
        accept_entry_size(&mut self.expanded, name, declared)?;
        let target = self.staging.join(expected);
        let mut output = create_staged_file(&target)?;
        let mut bounded = reader.take(declared.saturating_add(1));
        let written = io::copy(&mut bounded, &mut output)
            .with_context(|| format!("extract sidecar archive file {name:?}"))?;
        if written != declared {
            return Err(anyhow!(
                "sidecar archive file {name:?} carries {written} bytes, not the declared {declared}"
            ));
        }
        output
            .sync_all()
            .with_context(|| format!("flush {}", target.display()))?;
        Ok(())
    }
}

/// Collects member names and writes nothing. Entry bodies are never read here:
/// the tar reader skips to the next header, so identification costs one
/// decompression pass and no filesystem writes at all.
struct ProbedEntries<'a> {
    expected_directories: &'a BTreeSet<String>,
    seen: BTreeSet<String>,
    files: BTreeSet<String>,
    expanded: u64,
}

impl SidecarEntries for ProbedEntries<'_> {
    fn accept_name(&mut self, raw: &str) -> Result<(String, bool)> {
        accept_entry_name(&mut self.seen, raw)
    }

    fn accept_directory(&mut self, name: &str) -> Result<()> {
        if !self.expected_directories.contains(name) {
            return Err(anyhow!("unexpected sidecar archive directory {name:?}"));
        }
        Ok(())
    }

    fn accept_file(&mut self, name: &str, declared: u64, _reader: &mut dyn Read) -> Result<()> {
        accept_entry_size(&mut self.expanded, name, declared)?;
        self.files.insert(name.to_owned());
        Ok(())
    }
}

fn read_tar_zstd(file: &mut fs::File, sink: &mut dyn SidecarEntries) -> Result<()> {
    let decoder = zstd::stream::read::Decoder::new(&mut *file)
        .context("open the Zstandard sidecar archive")?;
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries().context("read the sidecar archive")? {
        let mut entry = entry.context("read a sidecar archive entry")?;
        let raw = std::str::from_utf8(entry.path_bytes().as_ref())
            .context("sidecar archive path is not UTF-8")?
            .to_owned();
        let (name, directory_name) = sink.accept_name(&raw)?;
        let mode = entry
            .header()
            .mode()
            .context("read a sidecar archive entry mode")?;
        if mode & 0o7000 != 0 {
            return Err(anyhow!(
                "unsafe permission bits on sidecar archive entry {raw:?}"
            ));
        }
        let entry_type = entry.header().entry_type();
        if entry_type.is_dir() {
            sink.accept_directory(&name)?;
            continue;
        }
        if directory_name || !entry_type.is_file() {
            return Err(anyhow!(
                "sidecar archive entry {raw:?} is a link, device, or other non-regular entry"
            ));
        }
        let declared = entry
            .header()
            .size()
            .context("read a sidecar archive entry size")?;
        sink.accept_file(&name, declared, &mut entry)?;
    }
    Ok(())
}

fn rfc3339_utc_now() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default();
    format_rfc3339_utc(seconds)
}

/// ctx-semantic-model deliberately carries no calendar dependency and the
/// manifest only needs a UTC second stamp, so the civil date is derived
/// from the epoch here.
fn format_rfc3339_utc(unix_seconds: i64) -> String {
    let days = unix_seconds.div_euclid(86_400);
    let time = unix_seconds.rem_euclid(86_400);
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_position = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_position + 2) / 5 + 1;
    let month = if month_position < 10 {
        month_position + 3
    } else {
        month_position - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time / 3_600,
        (time % 3_600) / 60,
        time % 60
    )
}

#[cfg(test)]
mod tests;
