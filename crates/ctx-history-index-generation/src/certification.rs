use std::{
    collections::HashSet,
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

use ctx_history_platform::platform_security::ensure_private_directory;

#[cfg(all(windows, any(test, feature = "test-support")))]
use crate::publication_probe::{publication_io_checkpoint, PublicationIoEvent};
use crate::{
    active_index_files, load_active_generation_pointer, manifest_path,
    physical::physical_integrity_digest_from_parts,
    physical_integrity_audit,
    retention::{
        acquire_existing_generation_directory_read_authority,
        ExistingGenerationDirectoryReadAuthority,
    },
    slot_path, ActiveGenerationPointer, DurableMmapDirectory, GenerationError as IndexError,
    GenerationRetentionLease, GenerationSlot, PhysicalIntegrityAudit, Result,
    INDEX_GENERATIONS_DIRECTORY, MANIFEST_DIRECTORY,
};

// Version 5 deliberately drops the active-pointer identity. A certification
// names the immutable slot it authenticated, so it can be completed and
// reopened before that slot becomes active.
const CERTIFICATION_VERSION: u32 = 5;
const CERTIFICATION_SUFFIX: &str = ".physical-certification.json";
const CERTIFICATION_DIRECTORY: &str = "integrity-certifications";
const TANTIVY_META_FILE: &str = "meta.json";
#[cfg(windows)]
const MANAGED_FILE: &str = ".managed.json";
const ARTIFACT_STABLE_SNAPSHOT_ATTEMPTS: usize = 4;
pub const MAX_CERTIFICATION_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_CERTIFIED_ARTIFACTS: usize = crate::physical::MAX_MANAGED_FILE_ENTRIES + 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerationIntegrityCertification {
    version: u32,
    manifest_identity: FileIdentity,
    slot: GenerationSlot,
    #[serde(deserialize_with = "deserialize_artifacts")]
    artifacts: Vec<CertifiedArtifact>,
}

/// In-memory proof that every file in one pointer-bound generation matched
/// the slot's expected physical SHA. The proof retains per-file digests so an
/// already-required candidate audit can authenticate managed hard-link
/// transitions without another full read of the active generation.
pub struct CertifiedPhysicalIntegrity {
    certification: GenerationIntegrityCertification,
}

impl CertifiedPhysicalIntegrity {
    pub(crate) fn certified_artifact(
        &self,
        path: &Path,
    ) -> Option<(ArtifactIdentity, [u8; 32], bool)> {
        let path = path.to_str()?;
        self.certification
            .artifacts
            .iter()
            .find(|artifact| artifact.artifact.path == path)
            .map(|artifact| (artifact.artifact.clone(), artifact.sha256, artifact.sealed))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CertifiedArtifact {
    #[serde(flatten)]
    artifact: ArtifactIdentity,
    sha256: [u8; 32],
    #[serde(default)]
    sealed: bool,
}

fn deserialize_artifacts<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<CertifiedArtifact>, D::Error>
where
    D: Deserializer<'de>,
{
    struct BoundedArtifacts;

    impl<'de> Visitor<'de> for BoundedArtifacts {
        type Value = Vec<CertifiedArtifact>;

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

    fn is_readonly(&self) -> bool {
        #[cfg(unix)]
        {
            self.mode & 0o222 == 0
        }
        #[cfg(windows)]
        {
            self.attributes & 0x1 != 0
        }
        #[cfg(not(any(unix, windows)))]
        {
            false
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

    #[cfg(windows)]
    fn follows_readonly_seal(&self, prior: &Self) -> bool {
        self.same_native_file(prior)
            && !prior.is_readonly()
            && self.is_readonly()
            && self.length == prior.length
            && self.creation_time == prior.creation_time
            && self.last_write_time == prior.last_write_time
            && self.links == prior.links
            && (self.attributes & !0x1) == (prior.attributes & !0x1)
    }
}

pub fn verify_or_certify_physical_integrity(
    root: &Path,
    pointer: &ActiveGenerationPointer,
    slot: &GenerationSlot,
    index: &tantivy::Index,
) -> Result<CertifiedPhysicalIntegrity> {
    if let Some(certification) = matching_certification(root, pointer, slot, index)? {
        return Ok(CertifiedPhysicalIntegrity { certification });
    }

    let generation_path = slot_path(root, slot);
    let audit = physical_integrity_audit(index, &generation_path, Some(pointer))?;
    if audit.digest() != slot.physical_integrity_digest() {
        return Err(IndexError::ChecksumMismatch);
    }
    install_certification(
        root,
        Some(pointer),
        None,
        slot,
        index,
        &audit,
        CertificationInstallPolicy::ACTIVE_CACHE,
    )
}

/// Verifies one immutable generation from its existing publication-time
/// certification without hashing artifact bodies or changing durable state.
///
/// The certification remains bound to the exact slot, manifest file, artifact
/// path set, and exact native files after the active pointer moves on. Any
/// metadata transition invalidates the inherited SHA authority because a
/// later link/unlink can mask an intervening same-size, restored-mtime write.
/// Missing, malformed, stale, or otherwise unsupported certification fails
/// closed without hashing artifact bodies.
pub fn verify_physical_integrity_read_only(
    root: &Path,
    slot: &GenerationSlot,
    index: &tantivy::Index,
) -> Result<()> {
    ensure_real_directory(root)?;
    ensure_real_directory(&root.join(MANIFEST_DIRECTORY))?;
    ensure_real_directory(&root.join(INDEX_GENERATIONS_DIRECTORY))?;
    let generation_path = slot_path(root, slot);
    ensure_real_directory(&generation_path)?;
    if crate::read_root::has_retained_read_authority(root, slot.generation_id()) {
        return crate::verify_physical_integrity(
            index,
            &generation_path,
            None,
            slot.physical_integrity_digest(),
        );
    }
    ensure_real_directory(&root.join(CERTIFICATION_DIRECTORY))?;

    let bytes =
        read_certification(&certification_path(root, slot)).ok_or(IndexError::ChecksumMismatch)?;
    let certification = serde_json::from_slice::<GenerationIntegrityCertification>(&bytes)
        .map_err(|_| IndexError::ChecksumMismatch)?;
    if serde_json::to_vec(&certification)? != bytes
        || certification.version != CERTIFICATION_VERSION
        || certification.slot != *slot
        || !certification_digest_matches_slot(&certification)?
        || capture_single_link_control(&manifest_path(root, slot.generation_id()))?
            != certification.manifest_identity
    {
        return Err(IndexError::ChecksumMismatch);
    }

    let expected_paths = expected_artifact_paths(index)?;
    if certification
        .artifacts
        .iter()
        .map(|artifact| artifact.artifact.path.clone())
        .collect::<Vec<_>>()
        != expected_paths
    {
        return Err(IndexError::ChecksumMismatch);
    }
    let current_pointer = load_current_pointer(root)?;
    let retained_alias_directories = std::iter::once(current_pointer.active().directory())
        .chain(current_pointer.previous().map(GenerationSlot::directory))
        .chain(std::iter::once(slot.directory()))
        .map(str::to_owned)
        .collect::<HashSet<_>>();
    for expected in &certification.artifacts {
        let current = capture_artifact_with_retained_aliases(
            root,
            &generation_path,
            Path::new(&expected.artifact.path),
            &retained_alias_directories,
        )?;
        if current != expected.artifact {
            return Err(IndexError::ChecksumMismatch);
        }
    }
    if load_current_pointer(root)? != current_pointer {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    Ok(())
}

pub fn scrub_and_certify_physical_integrity(
    root: &Path,
    pointer: &ActiveGenerationPointer,
    slot: &GenerationSlot,
    index: &tantivy::Index,
) -> Result<CertifiedPhysicalIntegrity> {
    let generation_path = slot_path(root, slot);
    let audit = physical_integrity_audit(index, &generation_path, Some(pointer))?;
    if audit.digest() != slot.physical_integrity_digest() {
        return Err(IndexError::ChecksumMismatch);
    }
    install_certification(
        root,
        Some(pointer),
        None,
        slot,
        index,
        &audit,
        CertificationInstallPolicy::ACTIVE_CACHE,
    )
}

/// Backwards-compatible spelling for callers that certify an already-active
/// slot. New publication paths must use [`certify_candidate_physical_integrity`]
/// before replacing the active pointer.
pub fn certify_activated_generation(
    root: &Path,
    pointer: &ActiveGenerationPointer,
    slot: &GenerationSlot,
    index: &tantivy::Index,
    audit: &PhysicalIntegrityAudit,
) -> Result<()> {
    install_certification(
        root,
        Some(pointer),
        None,
        slot,
        index,
        audit,
        CertificationInstallPolicy::ACTIVATED_CACHE,
    )
    .map(|_| ())
}

fn seal_artifact(
    root: &Path,
    generation_path: &Path,
    relative_path: &Path,
    pointer: Option<&ActiveGenerationPointer>,
    expected: &ArtifactIdentity,
) -> Result<ArtifactIdentity> {
    let (file, observed) = open_artifact(root, generation_path, relative_path, pointer)?;
    #[cfg(windows)]
    let _ = &file;
    if observed != *expected {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    #[cfg(windows)]
    let expected_sealed_identity = if observed.identity.is_readonly() {
        observed.identity.clone()
    } else {
        seal_unsealed_artifact(&generation_path.join(relative_path), &observed)?
    };
    if !observed.identity.is_readonly() {
        #[cfg(not(windows))]
        let mut permissions = file.metadata()?.permissions();
        #[cfg(not(windows))]
        permissions.set_readonly(true);
        #[cfg(not(windows))]
        file.set_permissions(permissions)?;
        #[cfg(not(windows))]
        file.sync_all()?;
    }
    let sealed = recapture_artifact(root, generation_path, relative_path, pointer)?;
    #[cfg(windows)]
    if sealed.identity != expected_sealed_identity {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    if !sealed.identity.is_readonly()
        || !sealed.identity.same_native_file(&observed.identity)
        || sealed.identity.length() != observed.identity.length()
    {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    Ok(sealed)
}

#[cfg(windows)]
fn seal_unsealed_artifact(path: &Path, expected: &ArtifactIdentity) -> Result<FileIdentity> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_READ,
    };

    validate_named_regular_file(path)?;
    let file = OpenOptions::new()
        .access_mode(FILE_GENERIC_READ | FILE_GENERIC_WRITE)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    if file_identity(&file)? != expected.identity {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    validate_named_regular_file(path)?;
    let named = open_nofollow(path).map_err(|_| IndexError::ConcurrentGenerationChange)?;
    if file_identity(&named)? != expected.identity {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    let mut permissions = file.metadata()?.permissions();
    permissions.set_readonly(true);
    file.set_permissions(permissions)?;
    file.sync_all()?;
    let sealed = file_identity(&file)?;
    if !sealed.follows_readonly_seal(&expected.identity) {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    Ok(sealed)
}

#[cfg(windows)]
fn artifact_should_be_sealed(_path: &str) -> bool {
    true
}

#[cfg(not(windows))]
fn artifact_should_be_sealed(path: &str) -> bool {
    path != TANTIVY_META_FILE
}

#[cfg(windows)]
struct TerminalSealEntry {
    relative_path: PathBuf,
    file: File,
    before: ArtifactIdentity,
    sealed: ArtifactIdentity,
}

/// Keeps every Windows candidate artifact open from its terminal read-only
/// seal through the active-pointer replacement.
#[cfg(windows)]
pub struct TerminalPublicationGuard {
    root: PathBuf,
    generation_path: PathBuf,
    topology_authority: Option<ActiveGenerationPointer>,
    entries: Vec<TerminalSealEntry>,
}

#[cfg(windows)]
pub fn acquire_terminal_publication_guard(
    root: &Path,
    generation_path: &Path,
    index: &tantivy::Index,
    topology_authority: Option<&ActiveGenerationPointer>,
) -> Result<TerminalPublicationGuard> {
    ensure_real_directory(root)?;
    ensure_real_directory(&root.join(INDEX_GENERATIONS_DIRECTORY))?;
    ensure_real_directory(generation_path)?;

    let mut allowlist = active_index_files(index)?;
    allowlist.insert(PathBuf::from(TANTIVY_META_FILE));
    allowlist.insert(PathBuf::from(MANAGED_FILE));
    if allowlist.len() > MAX_CERTIFIED_ARTIFACTS.saturating_add(1) {
        return Err(IndexError::ChecksumMismatch);
    }

    #[cfg(any(test, feature = "test-support"))]
    publication_io_checkpoint(PublicationIoEvent::TerminalSealOpen)?;
    let mut entries = allowlist
        .into_iter()
        .map(|relative_path| {
            open_terminal_seal_entry(root, generation_path, relative_path, topology_authority)
        })
        .collect::<Result<Vec<_>>>()?;

    for entry in &mut entries {
        let mut permissions = entry.file.metadata()?.permissions();
        permissions.set_readonly(true);
        entry.file.set_permissions(permissions)?;
        entry.file.sync_all()?;
        let sealed_identity = file_identity(&entry.file)?;
        if !sealed_identity.follows_readonly_seal(&entry.before.identity) {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        let sealed = recapture_authenticated_artifact(
            root,
            generation_path,
            &entry.relative_path,
            &entry.file,
            topology_authority,
        )?;
        if sealed.identity != sealed_identity {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        entry.sealed = sealed;
    }

    Ok(TerminalPublicationGuard {
        root: root.to_owned(),
        generation_path: generation_path.to_owned(),
        topology_authority: topology_authority.cloned(),
        entries,
    })
}

#[cfg(windows)]
fn open_terminal_seal_entry(
    root: &Path,
    generation_path: &Path,
    relative_path: PathBuf,
    topology_authority: Option<&ActiveGenerationPointer>,
) -> Result<TerminalSealEntry> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_READ,
    };

    if relative_path.components().count() != 1 {
        return Err(IndexError::ChecksumMismatch);
    }
    let path = generation_path.join(&relative_path);
    validate_named_regular_file(&path)?;
    let file = OpenOptions::new()
        .access_mode(FILE_GENERIC_READ | FILE_GENERIC_WRITE)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&path)?;
    let opened = file_identity(&file)?;
    if opened.is_readonly() {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    validate_named_regular_file(&path)?;
    let named = open_nofollow(&path).map_err(|_| IndexError::ConcurrentGenerationChange)?;
    if file_identity(&named)? != opened || file_identity(&file)? != opened {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    let before = recapture_authenticated_artifact(
        root,
        generation_path,
        &relative_path,
        &file,
        topology_authority,
    )?;
    if before.identity != opened {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    Ok(TerminalSealEntry {
        relative_path,
        file,
        sealed: before.clone(),
        before,
    })
}

#[cfg(windows)]
impl TerminalPublicationGuard {
    pub fn verify_physical_fence(&self, expected: &PhysicalIntegrityAudit) -> Result<()> {
        if self.entries.len() != expected.files().len().saturating_add(1) {
            return Err(IndexError::ChecksumMismatch);
        }
        for expected_file in expected.files() {
            let Some(entry) = self
                .entries
                .iter()
                .find(|entry| entry.before.path == expected_file.artifact.path)
            else {
                return Err(IndexError::ChecksumMismatch);
            };
            if entry.before != expected_file.artifact
                || !entry
                    .sealed
                    .identity
                    .follows_readonly_seal(&entry.before.identity)
            {
                return Err(IndexError::ConcurrentGenerationChange);
            }
        }
        Ok(())
    }

    pub fn verify_identities(&self) -> Result<()> {
        for entry in &self.entries {
            let current = recapture_authenticated_artifact(
                &self.root,
                &self.generation_path,
                &entry.relative_path,
                &entry.file,
                self.topology_authority.as_ref(),
            )?;
            if current != entry.sealed {
                return Err(IndexError::ConcurrentGenerationChange);
            }
        }
        Ok(())
    }
}

fn matching_certification(
    root: &Path,
    pointer: &ActiveGenerationPointer,
    slot: &GenerationSlot,
    index: &tantivy::Index,
) -> Result<Option<GenerationIntegrityCertification>> {
    if ensure_real_directory(&root.join(CERTIFICATION_DIRECTORY)).is_err() {
        return Ok(None);
    }
    let Some(bytes) = read_certification(&certification_path(root, slot)) else {
        return Ok(None);
    };
    let Ok(certification) = serde_json::from_slice::<GenerationIntegrityCertification>(&bytes)
    else {
        return Ok(None);
    };
    if serde_json::to_vec(&certification)? != bytes
        || certification.version != CERTIFICATION_VERSION
        || certification.slot != *slot
        || !certification_digest_matches_slot(&certification)?
    {
        return Ok(None);
    }
    if load_current_pointer(root)? != *pointer {
        return Err(IndexError::ConcurrentGenerationChange);
    }

    ensure_real_directory(root)?;
    ensure_real_directory(&root.join(MANIFEST_DIRECTORY))?;
    ensure_real_directory(&root.join(INDEX_GENERATIONS_DIRECTORY))?;
    let generation_path = slot_path(root, slot);
    ensure_real_directory(&generation_path)?;
    if capture_single_link_control(&manifest_path(root, slot.generation_id()))?
        != certification.manifest_identity
    {
        return Ok(None);
    }

    let expected_paths = expected_artifact_paths(index)?;
    if certification
        .artifacts
        .iter()
        .map(|artifact| artifact.artifact.path.clone())
        .collect::<Vec<_>>()
        != expected_paths
    {
        return Ok(None);
    }
    for expected in &certification.artifacts {
        let current = capture_artifact(
            root,
            &generation_path,
            Path::new(&expected.artifact.path),
            Some(pointer),
        )?;
        if current != expected.artifact {
            return Ok(None);
        }
    }
    if load_current_pointer(root)? != *pointer {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    Ok(Some(certification))
}

/// Revalidates one in-memory expected-SHA proof immediately before a writer
/// relies on retained base artifacts. Exact identities take the metadata-only
/// fast path. A managed hard-link transition is accepted only when the
/// candidate's already-required physical audit observed the same native file
/// and the same per-file SHA that was authenticated for the active base.
pub fn verify_certified_physical_integrity(
    root: &Path,
    pointer: &ActiveGenerationPointer,
    slot: &GenerationSlot,
    certified: &CertifiedPhysicalIntegrity,
    candidate_audit: Option<&PhysicalIntegrityAudit>,
) -> Result<()> {
    let certification = &certified.certification;
    if certification.slot != *slot {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    if load_current_pointer(root)? != *pointer {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    ensure_real_directory(root)?;
    ensure_real_directory(&root.join(MANIFEST_DIRECTORY))?;
    ensure_real_directory(&root.join(INDEX_GENERATIONS_DIRECTORY))?;
    let generation_path = slot_path(root, slot);
    ensure_real_directory(&generation_path)?;
    if capture_single_link_control(&manifest_path(root, slot.generation_id()))?
        != certification.manifest_identity
    {
        return Err(IndexError::ConcurrentGenerationChange);
    }

    for expected in &certification.artifacts {
        let current = capture_artifact(
            root,
            &generation_path,
            Path::new(&expected.artifact.path),
            Some(pointer),
        )?;
        let candidate_file = candidate_audit.and_then(|audit| {
            audit
                .files()
                .iter()
                .find(|file| file.artifact.path == expected.artifact.path)
        });
        if candidate_audit.is_none() || expected.artifact.path == TANTIVY_META_FILE {
            if current != expected.artifact {
                return Err(IndexError::ChecksumMismatch);
            }
            continue;
        }
        let Some(candidate_file) = candidate_file else {
            // This segment is absent from the candidate and therefore cannot
            // be used as an exhaustive-verification exclusion.
            continue;
        };
        if candidate_file.sha256 != expected.sha256 {
            return Err(IndexError::ChecksumMismatch);
        }
        if current != expected.artifact
            && (!expected.artifact.same_payload_identity_changed(&current)
                || candidate_file.artifact != current)
        {
            return Err(IndexError::ChecksumMismatch);
        }
    }
    if load_current_pointer(root)? != *pointer {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    Ok(())
}

fn load_current_pointer(root: &Path) -> Result<ActiveGenerationPointer> {
    load_active_generation_pointer(root)?.ok_or(IndexError::MissingActiveGenerationPointer)
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

mod artifact_io;
use artifact_io::*;
pub(crate) use artifact_io::{
    capture_artifact_identity, open_artifact, open_authenticated_artifact, recapture_artifact,
    recapture_authenticated_artifact,
};
mod candidate;
mod install;
mod pointer_fence;
mod sidecar;
use candidate::certification_digest_matches_slot;
pub use candidate::{
    certify_candidate_physical_integrity, verify_candidate_physical_integrity_read_only,
};
use install::{install_certification, CertificationInstallPolicy};
pub use pointer_fence::ActiveGenerationPointerFence;
#[cfg(windows)]
pub(crate) use pointer_fence::ValidatedPredecessorPointer;

#[cfg(any(test, feature = "test-support"))]
pub use sidecar::certification_file_for_active;
pub(crate) use sidecar::certification_path;
pub use sidecar::reclaim_unreferenced_certifications;
use sidecar::{certification_file_name, is_generation_directory_name};

#[cfg(test)]
mod tests;
