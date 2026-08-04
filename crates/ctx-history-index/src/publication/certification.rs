use std::{
    fmt,
    fs::{self, File, Metadata, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
};

use serde::{
    de::{Error as _, SeqAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};
use tantivy::directory::Directory as _;

use super::{
    manifest::manifest_path,
    verification::{active_index_files, physical_integrity_audit, PhysicalIntegrityAudit},
    ActiveGenerationPointer, GenerationSlot, INDEX_GENERATIONS_DIRECTORY,
};
use crate::{durable_directory::DurableMmapDirectory, IndexError, Result, MANIFEST_DIRECTORY};

const CERTIFICATION_VERSION: u32 = 1;
const CERTIFICATION_SUFFIX: &str = ".physical-certification.json";
const CERTIFICATION_DIRECTORY: &str = "integrity-certifications";
const TANTIVY_META_FILE: &str = "meta.json";
const ARTIFACT_STABLE_SNAPSHOT_ATTEMPTS: usize = 4;
pub(crate) const MAX_CERTIFICATION_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_CERTIFIED_ARTIFACTS: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerationIntegrityCertification {
    version: u32,
    pointer: ActiveGenerationPointer,
    pointer_identity: FileIdentity,
    manifest_identity: FileIdentity,
    slot: GenerationSlot,
    #[serde(deserialize_with = "deserialize_artifacts")]
    artifacts: Vec<ArtifactIdentity>,
}

fn deserialize_artifacts<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<ArtifactIdentity>, D::Error>
where
    D: Deserializer<'de>,
{
    struct BoundedArtifacts;

    impl<'de> Visitor<'de> for BoundedArtifacts {
        type Value = Vec<ArtifactIdentity>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(
                formatter,
                "at most {MAX_CERTIFIED_ARTIFACTS} certified artifacts"
            )
        }

        fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let hinted = sequence.size_hint().unwrap_or(0);
            if hinted > MAX_CERTIFIED_ARTIFACTS {
                return Err(A::Error::custom(
                    "certification artifact count exceeds bound",
                ));
            }
            let mut artifacts = Vec::with_capacity(hinted.min(MAX_CERTIFIED_ARTIFACTS));
            while let Some(artifact) = sequence.next_element()? {
                if artifacts.len() == MAX_CERTIFIED_ARTIFACTS {
                    return Err(A::Error::custom(
                        "certification artifact count exceeds bound",
                    ));
                }
                artifacts.push(artifact);
            }
            Ok(artifacts)
        }
    }

    deserializer.deserialize_seq(BoundedArtifacts)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ArtifactIdentity {
    pub(super) path: String,
    pub(super) identity: FileIdentity,
}

impl ArtifactIdentity {
    /// Returns whether both observations still bind the same native file and
    /// immutable-content metadata, but some stronger identity metadata changed.
    ///
    /// This is a fail-closed concurrency classification, not proof that a hard
    /// link operation was the cause: ctime is excluded because managed
    /// link/unlink operations change it, and a same-size mutation with restored
    /// mtime must likewise force the caller to retry rather than accept bytes.
    pub(super) fn same_payload_identity_changed(&self, other: &Self) -> bool {
        self.path == other.path
            && self.identity != other.identity
            && self.identity.same_payload_identity(&other.identity)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FileIdentity {
    length: u64,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    mode: u32,
    #[cfg(unix)]
    modified_seconds: i64,
    #[cfg(unix)]
    modified_nanoseconds: i64,
    #[cfg(unix)]
    changed_seconds: i64,
    #[cfg(unix)]
    changed_nanoseconds: i64,
    #[cfg(unix)]
    links: u64,
    #[cfg(windows)]
    volume_serial_number: u64,
    #[cfg(windows)]
    file_id: [u8; 16],
    #[cfg(windows)]
    creation_time: i64,
    #[cfg(windows)]
    last_write_time: i64,
    #[cfg(windows)]
    change_time: i64,
    #[cfg(windows)]
    attributes: u32,
    #[cfg(windows)]
    links: u32,
}

impl FileIdentity {
    pub(super) fn length(&self) -> u64 {
        self.length
    }

    fn link_count(&self) -> u64 {
        #[cfg(unix)]
        {
            self.links
        }
        #[cfg(windows)]
        {
            u64::from(self.links)
        }
        #[cfg(not(any(unix, windows)))]
        {
            0
        }
    }

    fn same_native_file(&self, other: &Self) -> bool {
        #[cfg(unix)]
        {
            self.device == other.device && self.inode == other.inode
        }
        #[cfg(windows)]
        {
            self.volume_serial_number == other.volume_serial_number && self.file_id == other.file_id
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = other;
            false
        }
    }

    /// Identity fields that cannot be changed by creating or removing a hard
    /// link to the same immutable payload. Link operations may change ctime
    /// and link count, so those fields are deliberately excluded here.
    fn same_payload_identity(&self, other: &Self) -> bool {
        if !self.same_native_file(other) {
            return false;
        }
        #[cfg(unix)]
        {
            self.length == other.length
                && self.mode == other.mode
                && self.modified_seconds == other.modified_seconds
                && self.modified_nanoseconds == other.modified_nanoseconds
        }
        #[cfg(windows)]
        {
            self.length == other.length
                && self.creation_time == other.creation_time
                && self.last_write_time == other.last_write_time
                && self.attributes == other.attributes
        }
        #[cfg(not(any(unix, windows)))]
        {
            false
        }
    }

    fn follows_link_reclamation(&self, prior: &Self) -> bool {
        if !self.same_native_file(prior) || self.link_count() >= prior.link_count() {
            return false;
        }
        #[cfg(unix)]
        {
            self.length == prior.length
                && self.mode == prior.mode
                && self.modified_seconds == prior.modified_seconds
                && self.modified_nanoseconds == prior.modified_nanoseconds
        }
        #[cfg(windows)]
        {
            self.length == prior.length
                && self.creation_time == prior.creation_time
                && self.last_write_time == prior.last_write_time
                && self.attributes == prior.attributes
        }
        #[cfg(not(any(unix, windows)))]
        {
            false
        }
    }
}

pub(crate) fn verify_or_certify_physical_integrity(
    root: &Path,
    pointer: &ActiveGenerationPointer,
    slot: &GenerationSlot,
    index: &tantivy::Index,
) -> Result<()> {
    if certification_matches(root, pointer, slot, index)? {
        return Ok(());
    }

    let generation_path = super::slot_path(root, slot);
    let audit = physical_integrity_audit(index, &generation_path)?;
    if audit.digest() != slot.physical_integrity_digest() {
        return Err(IndexError::ChecksumMismatch);
    }
    install_certification(root, pointer, slot, index, &audit, false)
}

pub(crate) fn scrub_and_certify_physical_integrity(
    root: &Path,
    pointer: &ActiveGenerationPointer,
    slot: &GenerationSlot,
    index: &tantivy::Index,
) -> Result<()> {
    let generation_path = super::slot_path(root, slot);
    let audit = physical_integrity_audit(index, &generation_path)?;
    if audit.digest() != slot.physical_integrity_digest() {
        return Err(IndexError::ChecksumMismatch);
    }
    install_certification(root, pointer, slot, index, &audit, false)
}

/// Installs the certification for a candidate that was fully hashed before
/// pointer publication. Reclaiming the formerly retained previous generation
/// can only reduce hard-link counts; every other identity field remains bound
/// to the file that supplied the candidate hash.
pub(crate) fn certify_activated_generation(
    root: &Path,
    pointer: &ActiveGenerationPointer,
    slot: &GenerationSlot,
    index: &tantivy::Index,
    audit: &PhysicalIntegrityAudit,
) -> Result<()> {
    install_certification(root, pointer, slot, index, audit, true)
}

fn install_certification(
    root: &Path,
    pointer: &ActiveGenerationPointer,
    slot: &GenerationSlot,
    index: &tantivy::Index,
    audit: &PhysicalIntegrityAudit,
    allow_link_reclamation: bool,
) -> Result<()> {
    if audit.artifacts().len() > MAX_CERTIFIED_ARTIFACTS {
        return Ok(());
    }
    if load_current_pointer(root)? != *pointer {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    let expected_paths = expected_artifact_paths(index)?;
    if audit.artifact_paths() != expected_paths {
        return Err(IndexError::ChecksumMismatch);
    }

    let generation_path = super::slot_path(root, slot);
    ensure_real_directory(root)?;
    ensure_real_directory(&root.join(MANIFEST_DIRECTORY))?;
    ensure_real_directory(&root.join(INDEX_GENERATIONS_DIRECTORY))?;
    ensure_real_directory(&generation_path)?;

    let pointer_identity = capture_pointer_bound_single_link_control(
        root,
        pointer,
        &root.join("active-generation.json"),
    )?;
    let manifest_identity = capture_pointer_bound_single_link_control(
        root,
        pointer,
        &manifest_path(root, slot.generation_id()),
    )?;
    let mut artifacts = Vec::with_capacity(audit.artifacts().len());
    for prior in audit.artifacts() {
        let current = capture_artifact(
            root,
            &generation_path,
            Path::new(&prior.path),
            Some(pointer),
        )?;
        if current.identity != prior.identity {
            if allow_link_reclamation && current.identity.follows_link_reclamation(&prior.identity)
            {
                artifacts.push(current);
                continue;
            }
            return if prior.same_payload_identity_changed(&current) {
                Err(IndexError::ConcurrentGenerationChange)
            } else {
                Err(IndexError::ChecksumMismatch)
            };
        }
        artifacts.push(current);
    }
    let certification = GenerationIntegrityCertification {
        version: CERTIFICATION_VERSION,
        pointer: pointer.clone(),
        pointer_identity,
        manifest_identity,
        slot: slot.clone(),
        artifacts,
    };
    let bytes = serde_json::to_vec(&certification)?;
    if bytes.len() > MAX_CERTIFICATION_BYTES {
        return Ok(());
    }
    let certification_directory = root.join(CERTIFICATION_DIRECTORY);
    if fs::create_dir_all(&certification_directory).is_err()
        || ensure_real_directory(&certification_directory).is_err()
    {
        return Ok(());
    }
    let directory = match DurableMmapDirectory::open(root) {
        Ok(directory) => directory,
        Err(_) => return Ok(()),
    };
    let relative_path = Path::new(CERTIFICATION_DIRECTORY).join(certification_file_name(slot));
    if directory.atomic_write(&relative_path, &bytes).is_err() {
        // Certification is an optimization, never publication authority. A
        // read-only or full filesystem simply causes the next open to hash.
        return Ok(());
    }
    if !certification_matches(root, pointer, slot, index)? {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    Ok(())
}

fn certification_matches(
    root: &Path,
    pointer: &ActiveGenerationPointer,
    slot: &GenerationSlot,
    index: &tantivy::Index,
) -> Result<bool> {
    if ensure_real_directory(&root.join(CERTIFICATION_DIRECTORY)).is_err() {
        return Ok(false);
    }
    let Some(bytes) = read_certification(&certification_path(root, slot)) else {
        return Ok(false);
    };
    let Ok(certification) = serde_json::from_slice::<GenerationIntegrityCertification>(&bytes)
    else {
        return Ok(false);
    };
    if serde_json::to_vec(&certification)? != bytes
        || certification.version != CERTIFICATION_VERSION
        || certification.pointer != *pointer
        || certification.slot != *slot
    {
        return Ok(false);
    }
    if load_current_pointer(root)? != *pointer {
        return Err(IndexError::ConcurrentGenerationChange);
    }

    ensure_real_directory(root)?;
    ensure_real_directory(&root.join(MANIFEST_DIRECTORY))?;
    ensure_real_directory(&root.join(INDEX_GENERATIONS_DIRECTORY))?;
    let generation_path = super::slot_path(root, slot);
    ensure_real_directory(&generation_path)?;
    if capture_pointer_bound_single_link_control(
        root,
        pointer,
        &root.join("active-generation.json"),
    )? != certification.pointer_identity
        || capture_pointer_bound_single_link_control(
            root,
            pointer,
            &manifest_path(root, slot.generation_id()),
        )? != certification.manifest_identity
    {
        return Ok(false);
    }

    let expected_paths = expected_artifact_paths(index)?;
    if certification
        .artifacts
        .iter()
        .map(|artifact| artifact.path.clone())
        .collect::<Vec<_>>()
        != expected_paths
    {
        return Ok(false);
    }
    for expected in &certification.artifacts {
        let current = capture_artifact(
            root,
            &generation_path,
            Path::new(&expected.path),
            Some(pointer),
        )?;
        if &current != expected {
            return Ok(false);
        }
    }
    if load_current_pointer(root)? != *pointer {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    Ok(true)
}

fn load_current_pointer(root: &Path) -> Result<ActiveGenerationPointer> {
    super::load_active_generation_pointer(root)?.ok_or(IndexError::MissingActiveGenerationPointer)
}

fn expected_artifact_paths(index: &tantivy::Index) -> Result<Vec<String>> {
    let mut paths = active_index_files(index)?;
    paths.insert(PathBuf::from(TANTIVY_META_FILE));
    paths
        .into_iter()
        .map(|path| {
            path.to_str()
                .map(str::to_owned)
                .ok_or(IndexError::ChecksumMismatch)
        })
        .collect()
}

fn read_certification(path: &Path) -> Option<Vec<u8>> {
    let (file, identity) = open_regular_file(path).ok()?;
    let length = usize::try_from(identity.length()).ok()?;
    if length > MAX_CERTIFICATION_BYTES {
        return None;
    }
    let mut bytes = Vec::with_capacity(length);
    file.take(
        u64::try_from(MAX_CERTIFICATION_BYTES)
            .ok()?
            .saturating_add(1),
    )
    .read_to_end(&mut bytes)
    .ok()?;
    if bytes.len() > MAX_CERTIFICATION_BYTES || bytes.len() != length {
        return None;
    }
    Some(bytes)
}

pub(super) fn open_artifact(
    root: &Path,
    generation_path: &Path,
    relative_path: &Path,
    pointer: Option<&ActiveGenerationPointer>,
) -> Result<(File, ArtifactIdentity)> {
    if relative_path.components().count() != 1 {
        return Err(IndexError::ChecksumMismatch);
    }
    let path = relative_path
        .to_str()
        .ok_or(IndexError::ChecksumMismatch)?
        .to_owned();
    let artifact_path = generation_path.join(relative_path);
    let mut unaccounted_observation: Option<(FileIdentity, u64)> = None;
    let mut stable_unaccounted_attempts = 0_usize;
    for _ in 0..ARTIFACT_STABLE_SNAPSHOT_ATTEMPTS {
        let Some((file, identity)) = open_artifact_file_snapshot(&artifact_path)? else {
            unaccounted_observation = None;
            stable_unaccounted_attempts = 0;
            std::thread::yield_now();
            continue;
        };
        match stable_artifact_link_snapshot(
            root,
            &artifact_path,
            relative_path,
            &file,
            &identity,
            pointer,
        )? {
            ArtifactLinkSnapshot::Stable(identity) => {
                return Ok((file, ArtifactIdentity { path, identity }));
            }
            ArtifactLinkSnapshot::Retry => {
                unaccounted_observation = None;
                stable_unaccounted_attempts = 0;
            }
            ArtifactLinkSnapshot::Unaccounted { identity, aliases } => {
                let observation = (identity, aliases);
                if unaccounted_observation.as_ref() == Some(&observation) {
                    stable_unaccounted_attempts = stable_unaccounted_attempts
                        .checked_add(1)
                        .ok_or(IndexError::CountOverflow)?;
                } else {
                    unaccounted_observation = Some(observation);
                    stable_unaccounted_attempts = 1;
                }
                if stable_unaccounted_attempts == ARTIFACT_STABLE_SNAPSHOT_ATTEMPTS {
                    return Err(IndexError::ChecksumMismatch);
                }
            }
        }
        std::thread::yield_now();
    }
    Err(IndexError::ConcurrentGenerationChange)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ArtifactLinkSnapshot {
    Stable(FileIdentity),
    Retry,
    Unaccounted {
        identity: FileIdentity,
        aliases: u64,
    },
}

/// Opens a named artifact while distinguishing hard-link topology churn from
/// replacement or payload mutation. `None` asks the bounded caller to retry a
/// snapshot changed only by a link/unlink operation.
fn open_artifact_file_snapshot(path: &Path) -> Result<Option<(File, FileIdentity)>> {
    validate_named_regular_file(path)?;
    let file = open_nofollow(path).map_err(|_| IndexError::ChecksumMismatch)?;
    let opened = file_identity(&file).map_err(|_| IndexError::ChecksumMismatch)?;
    validate_named_regular_file(path)?;
    let named = open_nofollow(path).map_err(|_| IndexError::ChecksumMismatch)?;
    let named_identity = file_identity(&named).map_err(|_| IndexError::ChecksumMismatch)?;
    drop(named);
    let held = file_identity(&file).map_err(|_| IndexError::ChecksumMismatch)?;
    if opened == named_identity && named_identity == held {
        return Ok(Some((file, held)));
    }
    if opened.same_payload_identity(&named_identity) && named_identity.same_payload_identity(&held)
    {
        return Ok(None);
    }
    Err(IndexError::ChecksumMismatch)
}

/// Proves one stable managed-alias snapshot for an already-bound artifact.
/// Stable unaccounted hardlinks remain corruption; only an observation that
/// changed during the bounded snapshot is retryable.
fn stable_artifact_link_snapshot(
    root: &Path,
    artifact_path: &Path,
    relative_path: &Path,
    file: &File,
    identity: &FileIdentity,
    pointer: Option<&ActiveGenerationPointer>,
) -> Result<ArtifactLinkSnapshot> {
    let before = file_identity(file).map_err(|_| IndexError::ChecksumMismatch)?;
    if before != *identity {
        return if before.same_payload_identity(identity) {
            Ok(ArtifactLinkSnapshot::Retry)
        } else {
            Err(IndexError::ChecksumMismatch)
        };
    }
    let Some(alias_snapshot) = managed_artifact_alias_count(root, relative_path, &before, pointer)?
    else {
        return Ok(ArtifactLinkSnapshot::Retry);
    };
    let after_scan = file_identity(file).map_err(|_| IndexError::ChecksumMismatch)?;
    validate_named_regular_file(artifact_path)?;
    let named = open_nofollow(artifact_path).map_err(|_| IndexError::ChecksumMismatch)?;
    let named_identity = file_identity(&named).map_err(|_| IndexError::ChecksumMismatch)?;
    drop(named);
    let final_identity = file_identity(file).map_err(|_| IndexError::ChecksumMismatch)?;

    if before == after_scan && after_scan == named_identity && named_identity == final_identity {
        if alias_snapshot.aliases == 0 || alias_snapshot.aliases != final_identity.link_count() {
            if alias_snapshot.saw_unpublished_generation {
                return Ok(ArtifactLinkSnapshot::Retry);
            }
            return Ok(ArtifactLinkSnapshot::Unaccounted {
                identity: final_identity,
                aliases: alias_snapshot.aliases,
            });
        }
        return Ok(ArtifactLinkSnapshot::Stable(final_identity));
    }
    if before.same_payload_identity(&after_scan)
        && after_scan.same_payload_identity(&named_identity)
        && named_identity.same_payload_identity(&final_identity)
    {
        return Ok(ArtifactLinkSnapshot::Retry);
    }
    Err(IndexError::ChecksumMismatch)
}

pub(super) fn recapture_artifact(
    root: &Path,
    generation_path: &Path,
    relative_path: &Path,
    pointer: Option<&ActiveGenerationPointer>,
) -> Result<ArtifactIdentity> {
    capture_artifact(root, generation_path, relative_path, pointer)
}

fn capture_artifact(
    root: &Path,
    generation_path: &Path,
    relative_path: &Path,
    pointer: Option<&ActiveGenerationPointer>,
) -> Result<ArtifactIdentity> {
    let (file, artifact) = open_artifact(root, generation_path, relative_path, pointer)?;
    drop(file);
    Ok(artifact)
}

fn capture_single_link_control(path: &Path) -> Result<FileIdentity> {
    let (file, identity) = open_regular_file(path)?;
    drop(file);
    if identity.link_count() != 1 {
        return Err(IndexError::ChecksumMismatch);
    }
    Ok(identity)
}

fn capture_pointer_bound_single_link_control(
    root: &Path,
    pointer: &ActiveGenerationPointer,
    path: &Path,
) -> Result<FileIdentity> {
    for attempt in 0..ARTIFACT_STABLE_SNAPSHOT_ATTEMPTS {
        match capture_single_link_control(path) {
            Ok(identity) => {
                if load_current_pointer(root)? != *pointer {
                    return Err(IndexError::ConcurrentGenerationChange);
                }
                return Ok(identity);
            }
            Err(error) => {
                if load_current_pointer(root)? != *pointer {
                    return Err(IndexError::ConcurrentGenerationChange);
                }
                if attempt + 1 == ARTIFACT_STABLE_SNAPSHOT_ATTEMPTS {
                    return Err(error);
                }
                std::thread::yield_now();
            }
        }
    }
    Err(IndexError::ConcurrentGenerationChange)
}

fn open_regular_file(path: &Path) -> Result<(File, FileIdentity)> {
    validate_named_regular_file(path)?;
    let file = open_nofollow(path).map_err(|_| IndexError::ChecksumMismatch)?;
    let identity = file_identity(&file).map_err(|_| IndexError::ChecksumMismatch)?;
    #[cfg(test)]
    run_regular_file_identity_test_hook(path);
    validate_named_regular_file(path)?;
    let named = open_nofollow(path).map_err(|_| IndexError::ChecksumMismatch)?;
    let named_identity = file_identity(&named).map_err(|_| IndexError::ChecksumMismatch)?;
    if identity != named_identity {
        return Err(IndexError::ChecksumMismatch);
    }
    Ok((file, identity))
}

#[cfg(test)]
type PathTestHook = Box<dyn FnMut(&Path)>;

#[cfg(test)]
thread_local! {
    static REGULAR_FILE_IDENTITY_TEST_HOOK: std::cell::RefCell<Option<PathTestHook>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
struct RegularFileIdentityTestHookGuard(Option<PathTestHook>);

#[cfg(test)]
impl RegularFileIdentityTestHookGuard {
    fn install(hook: impl FnMut(&Path) + 'static) -> Self {
        let previous =
            REGULAR_FILE_IDENTITY_TEST_HOOK.with(|active| active.replace(Some(Box::new(hook))));
        Self(previous)
    }
}

#[cfg(test)]
impl Drop for RegularFileIdentityTestHookGuard {
    fn drop(&mut self) {
        REGULAR_FILE_IDENTITY_TEST_HOOK.with(|active| active.replace(self.0.take()));
    }
}

#[cfg(test)]
fn run_regular_file_identity_test_hook(path: &Path) {
    REGULAR_FILE_IDENTITY_TEST_HOOK.with(|active| {
        if let Some(hook) = active.borrow_mut().as_mut() {
            hook(path);
        }
    });
}

fn open_nofollow(path: &Path) -> std::io::Result<File> {
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
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        options
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

fn validate_named_regular_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|_| IndexError::ChecksumMismatch)?;
    if metadata.file_type().is_symlink()
        || metadata_is_reparse_point(&metadata)
        || !metadata.file_type().is_file()
    {
        return Err(IndexError::ChecksumMismatch);
    }
    Ok(())
}

fn ensure_real_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|_| IndexError::ChecksumMismatch)?;
    if metadata.file_type().is_symlink()
        || metadata_is_reparse_point(&metadata)
        || !metadata.file_type().is_dir()
    {
        return Err(IndexError::ChecksumMismatch);
    }
    Ok(())
}

#[cfg(unix)]
fn file_identity(file: &File) -> std::io::Result<FileIdentity> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(std::io::Error::other(
            "index artifact is not a regular file",
        ));
    }
    Ok(FileIdentity {
        length: metadata.len(),
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
        links: metadata.nlink(),
    })
}

#[cfg(windows)]
fn file_identity(file: &File) -> std::io::Result<FileIdentity> {
    use std::{mem::size_of, os::windows::io::AsRawHandle as _};
    use windows_sys::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{
            FileBasicInfo, FileIdInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
            BY_HANDLE_FILE_INFORMATION, FILE_BASIC_INFO, FILE_ID_INFO,
        },
    };

    let handle = file.as_raw_handle() as HANDLE;
    let mut basic = FILE_BASIC_INFO::default();
    if unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileBasicInfo,
            (&mut basic as *mut FILE_BASIC_INFO).cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    let mut id = FILE_ID_INFO::default();
    if unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            (&mut id as *mut FILE_ID_INFO).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(handle, &mut information) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(FileIdentity {
        length: file.metadata()?.len(),
        volume_serial_number: id.VolumeSerialNumber,
        file_id: id.FileId.Identifier,
        creation_time: basic.CreationTime,
        last_write_time: basic.LastWriteTime,
        change_time: basic.ChangeTime,
        attributes: basic.FileAttributes,
        links: information.nNumberOfLinks,
    })
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_file: &File) -> std::io::Result<FileIdentity> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "strong index artifact identity is unsupported on this platform",
    ))
}

#[cfg(windows)]
fn metadata_is_reparse_point(metadata: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn metadata_is_reparse_point(_metadata: &Metadata) -> bool {
    false
}

fn managed_artifact_alias_count(
    root: &Path,
    relative_path: &Path,
    identity: &FileIdentity,
    pointer: Option<&ActiveGenerationPointer>,
) -> Result<Option<ManagedAliasSnapshot>> {
    let generations = root.join(INDEX_GENERATIONS_DIRECTORY);
    let mut aliases = 0_u64;
    let mut saw_unpublished_generation = false;
    for entry in fs::read_dir(generations).map_err(|_| IndexError::ChecksumMismatch)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if retryable_alias_snapshot_error(&error) => return Ok(None),
            Err(_) => return Err(IndexError::ChecksumMismatch),
        };
        #[cfg(test)]
        run_alias_entry_test_hook(&entry.path());
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) if retryable_alias_snapshot_error(&error) => return Ok(None),
            Err(_) => return Err(IndexError::ChecksumMismatch),
        };
        let Some(directory_name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !file_type.is_dir() || !is_generation_directory_name(&directory_name) {
            continue;
        }
        if pointer.is_some_and(|pointer| {
            pointer.active().directory() != directory_name
                && pointer
                    .previous()
                    .is_none_or(|slot| slot.directory() != directory_name)
        }) {
            saw_unpublished_generation = true;
        }
        let candidate = entry.path().join(relative_path);
        let (file, candidate_identity) = match open_regular_file(&candidate) {
            Ok(opened) => opened,
            Err(_) => continue,
        };
        drop(file);
        if candidate_identity.same_native_file(identity) {
            aliases = aliases.checked_add(1).ok_or(IndexError::CountOverflow)?;
        }
    }
    Ok(Some(ManagedAliasSnapshot {
        aliases,
        saw_unpublished_generation,
    }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ManagedAliasSnapshot {
    aliases: u64,
    saw_unpublished_generation: bool,
}

fn retryable_alias_snapshot_error(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::NotFound {
        return true;
    }
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::ESTALE)
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(test)]
thread_local! {
    static ALIAS_ENTRY_TEST_HOOK: std::cell::RefCell<Option<PathTestHook>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
struct AliasEntryTestHookGuard(Option<PathTestHook>);

#[cfg(test)]
impl AliasEntryTestHookGuard {
    fn install(hook: impl FnMut(&Path) + 'static) -> Self {
        let previous = ALIAS_ENTRY_TEST_HOOK.with(|active| active.replace(Some(Box::new(hook))));
        Self(previous)
    }
}

#[cfg(test)]
impl Drop for AliasEntryTestHookGuard {
    fn drop(&mut self) {
        ALIAS_ENTRY_TEST_HOOK.with(|active| active.replace(self.0.take()));
    }
}

#[cfg(test)]
fn run_alias_entry_test_hook(path: &Path) {
    ALIAS_ENTRY_TEST_HOOK.with(|active| {
        if let Some(hook) = active.borrow_mut().as_mut() {
            hook(path);
        }
    });
}

fn is_generation_directory_name(name: &str) -> bool {
    name.strip_prefix("generation-").is_some_and(|suffix| {
        suffix.len() == 32
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn certification_file_name(slot: &GenerationSlot) -> String {
    format!("{}{CERTIFICATION_SUFFIX}", slot.directory())
}

pub(crate) fn certification_path(root: &Path, slot: &GenerationSlot) -> PathBuf {
    root.join(CERTIFICATION_DIRECTORY)
        .join(certification_file_name(slot))
}

pub(crate) fn reclaim_unreferenced_certifications(
    root: &Path,
    pointer: Option<&ActiveGenerationPointer>,
) -> Result<()> {
    let directory = root.join(CERTIFICATION_DIRECTORY);
    fs::create_dir_all(&directory)?;
    let retained = pointer
        .into_iter()
        .flat_map(|pointer| std::iter::once(pointer.active()).chain(pointer.previous()))
        .map(GenerationSlot::directory)
        .collect::<std::collections::HashSet<_>>();
    let mut removed = false;
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        let Some(file_name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(generation_directory) = file_name.strip_suffix(CERTIFICATION_SUFFIX) else {
            continue;
        };
        if is_generation_directory_name(generation_directory)
            && !retained.contains(generation_directory)
        {
            fs::remove_file(entry.path())?;
            removed = true;
        }
    }
    if removed {
        super::sync_directory(&directory)?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn certification_file_for_active(root: &Path) -> Result<PathBuf> {
    let pointer = load_current_pointer(root)?;
    Ok(certification_path(root, pointer.active()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generation(root: &Path, digit: char) -> PathBuf {
        root.join(INDEX_GENERATIONS_DIRECTORY)
            .join(format!("generation-{}", digit.to_string().repeat(32)))
    }

    fn pointer(digit: char) -> ActiveGenerationPointer {
        let digit = digit.to_string();
        ActiveGenerationPointer::new(
            GenerationSlot::new(
                digit.repeat(64),
                format!("generation-{}", digit.repeat(32)),
                digit.repeat(64),
            )
            .unwrap(),
            None,
        )
        .unwrap()
    }

    #[test]
    fn managed_link_creation_and_cleanup_are_retryable_stable_snapshots() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let active = generation(root, '1');
        let candidate = generation(root, '2');
        fs::create_dir_all(&active).unwrap();
        fs::create_dir_all(&candidate).unwrap();
        let relative = Path::new("payload.bin");
        let active_path = active.join(relative);
        let candidate_path = candidate.join(relative);
        fs::write(&active_path, b"immutable payload").unwrap();

        let (file, before_link) = open_artifact_file_snapshot(&active_path).unwrap().unwrap();
        fs::hard_link(&active_path, &candidate_path).unwrap();
        assert!(matches!(
            stable_artifact_link_snapshot(root, &active_path, relative, &file, &before_link, None,)
                .unwrap(),
            ArtifactLinkSnapshot::Retry
        ));
        drop(file);

        let (_, linked) = open_artifact(root, &active, relative, None).unwrap();
        assert_eq!(linked.identity.link_count(), 2);
        let (file, before_unlink) = open_artifact_file_snapshot(&active_path).unwrap().unwrap();
        fs::remove_file(&candidate_path).unwrap();
        assert!(matches!(
            stable_artifact_link_snapshot(
                root,
                &active_path,
                relative,
                &file,
                &before_unlink,
                None,
            )
            .unwrap(),
            ArtifactLinkSnapshot::Retry
        ));
        drop(file);

        let (_, unlinked) = open_artifact(root, &active, relative, None).unwrap();
        assert_eq!(unlinked.identity.link_count(), 1);
        assert!(linked.same_payload_identity_changed(&unlinked));
    }

    #[test]
    fn generation_disappearing_during_alias_scan_is_retryable() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let active = generation(root, '1');
        let candidate = generation(root, '2');
        fs::create_dir_all(&active).unwrap();
        fs::create_dir_all(&candidate).unwrap();
        let relative = Path::new("payload.bin");
        let active_path = active.join(relative);
        let candidate_path = candidate.join(relative);
        fs::write(&active_path, b"immutable payload").unwrap();
        fs::hard_link(&active_path, &candidate_path).unwrap();
        let (file, linked) = open_artifact_file_snapshot(&active_path).unwrap().unwrap();

        let candidate_for_hook = candidate.clone();
        let candidate_path_for_hook = candidate_path.clone();
        let _hook = AliasEntryTestHookGuard::install(move |entry_path| {
            if entry_path == candidate_for_hook {
                fs::remove_file(&candidate_path_for_hook).unwrap();
                fs::remove_dir(&candidate_for_hook).unwrap();
            }
        });

        assert!(matches!(
            stable_artifact_link_snapshot(root, &active_path, relative, &file, &linked, None,)
                .unwrap(),
            ArtifactLinkSnapshot::Retry
        ));
    }

    #[test]
    fn stale_directory_entry_errors_are_retryable_but_io_errors_are_not() {
        assert!(retryable_alias_snapshot_error(&std::io::Error::from(
            std::io::ErrorKind::NotFound,
        )));
        assert!(!retryable_alias_snapshot_error(&std::io::Error::from(
            std::io::ErrorKind::PermissionDenied,
        )));
        #[cfg(unix)]
        assert!(retryable_alias_snapshot_error(
            &std::io::Error::from_raw_os_error(libc::ESTALE)
        ));
    }

    #[test]
    fn pointer_replacement_during_control_capture_is_concurrent_not_corruption() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let first = pointer('1');
        let second = pointer('2');
        let target = root.join("active-generation.json");
        fs::write(&target, serde_json::to_vec(&first).unwrap()).unwrap();
        let directory = DurableMmapDirectory::open(root).unwrap();
        let target_for_hook = target.clone();
        let second_bytes = serde_json::to_vec(&second).unwrap();
        let mut replaced = false;
        let hook = RegularFileIdentityTestHookGuard::install(move |path| {
            if path == target_for_hook && !replaced {
                directory
                    .atomic_write(Path::new("active-generation.json"), &second_bytes)
                    .unwrap();
                replaced = true;
            }
        });

        assert!(matches!(
            capture_pointer_bound_single_link_control(root, &first, &target),
            Err(IndexError::ConcurrentGenerationChange)
        ));
        drop(hook);
        assert_eq!(load_current_pointer(root).unwrap(), second);

        let directory = DurableMmapDirectory::open(root).unwrap();
        let target_for_hook = target.clone();
        let second_bytes = serde_json::to_vec(&second).unwrap();
        let mut rewritten = false;
        let hook = RegularFileIdentityTestHookGuard::install(move |path| {
            if path == target_for_hook && !rewritten {
                directory
                    .atomic_write(Path::new("active-generation.json"), &second_bytes)
                    .unwrap();
                rewritten = true;
            }
        });
        assert!(capture_pointer_bound_single_link_control(root, &second, &target).is_ok());
        drop(hook);

        fs::hard_link(&target, root.join("unmanaged-pointer-hardlink")).unwrap();
        assert!(matches!(
            capture_pointer_bound_single_link_control(root, &second, &target),
            Err(IndexError::ChecksumMismatch)
        ));
    }

    #[test]
    fn stable_unmanaged_hardlink_remains_checksum_mismatch() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let active = generation(root, '1');
        fs::create_dir_all(&active).unwrap();
        let relative = Path::new("payload.bin");
        let active_path = active.join(relative);
        fs::write(&active_path, b"immutable payload").unwrap();
        fs::hard_link(&active_path, root.join("unmanaged-hardlink")).unwrap();

        assert!(matches!(
            open_artifact(root, &active, relative, None),
            Err(IndexError::ChecksumMismatch)
        ));
    }
}
