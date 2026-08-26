use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::Read as _,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt, OpenOptionsExt};
#[cfg(windows)]
use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};

#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::HANDLE,
    Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_GENERIC_READ, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    },
};

use serde::{Deserialize, Serialize};
use tantivy::{schema::Schema, store::Compressor, Index, IndexSettings};
use uuid::Uuid;

use ctx_history_platform::platform_security::{
    ensure_private_directory, restrict_private_directory,
};

use crate::clone::{bind_candidate_activation_fence, create_authenticated_candidate_generation};
use crate::is_generation_id;
use crate::retention::{
    ensure_generation_read_lease_coordinator, try_generation_directory_reclaim_authority,
};
use crate::{
    CandidateActivationFence, CandidatePhysicalProof, DurableAtomicWriteOutcome,
    DurableMmapDirectory, GenerationError as IndexError, GenerationRetentionLease, Result,
    ACTIVE_GENERATION_POINTER_FILE, INDEX_GENERATIONS_DIRECTORY,
};
const ACTIVE_GENERATION_POINTER_VERSION: u32 = 2;
const GENERATION_DIRECTORY_PREFIX: &str = "generation-";
const GENERATION_RECLAIM_REMOVE_ATTEMPTS: usize = 4;

pub fn lexical_index_settings() -> IndexSettings {
    IndexSettings {
        docstore_compression: Compressor::Lz4,
        docstore_compress_dedicated_thread: true,
        docstore_blocksize: 32 * 1024,
    }
}

fn validate_lexical_index_settings(index: &Index) -> Result<()> {
    if index.settings() != &lexical_index_settings() {
        return Err(IndexError::IndexSettingsMismatch);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationSlot {
    generation_id: String,
    directory: String,
    physical_integrity_digest: String,
}

impl GenerationSlot {
    pub fn new(
        generation_id: String,
        directory: String,
        physical_integrity_digest: String,
    ) -> Result<Self> {
        let slot = Self {
            generation_id,
            directory,
            physical_integrity_digest,
        };
        slot.validate()?;
        Ok(slot)
    }

    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub fn directory(&self) -> &str {
        &self.directory
    }

    pub fn physical_integrity_digest(&self) -> &str {
        &self.physical_integrity_digest
    }

    pub fn names_are_valid(generation_id: &str, directory: &str) -> bool {
        is_generation_id(generation_id) && is_generation_directory_name(directory)
    }

    pub fn validate(&self) -> Result<()> {
        if !Self::names_are_valid(&self.generation_id, &self.directory)
            || !is_generation_id(&self.physical_integrity_digest)
        {
            return Err(IndexError::InvalidActiveGenerationPointer);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActiveGenerationPointer {
    version: u32,
    active: GenerationSlot,
    previous: Option<GenerationSlot>,
}

impl ActiveGenerationPointer {
    pub fn new(active: GenerationSlot, previous: Option<GenerationSlot>) -> Result<Self> {
        let pointer = Self {
            version: ACTIVE_GENERATION_POINTER_VERSION,
            active,
            previous,
        };
        pointer.validate()?;
        Ok(pointer)
    }

    pub fn active(&self) -> &GenerationSlot {
        &self.active
    }

    pub fn previous(&self) -> Option<&GenerationSlot> {
        self.previous.as_ref()
    }

    fn validate(&self) -> Result<()> {
        if self.version != ACTIVE_GENERATION_POINTER_VERSION {
            return Err(IndexError::UnsupportedActiveGenerationPointer(self.version));
        }
        self.active.validate()?;
        if let Some(previous) = &self.previous {
            previous.validate()?;
            if previous.directory == self.active.directory {
                return Err(IndexError::InvalidActiveGenerationPointer);
            }
        }
        Ok(())
    }
}

pub struct CandidateGeneration {
    pub directory_name: String,
    pub index: Index,
    pub physical_proof: CandidatePhysicalProof,
    pub activation_fence: CandidateActivationFence,
}

#[derive(Debug)]
pub enum PointerPublicationOutcome {
    Durable,
    CommittedVisible { detail: String },
}

pub fn load_active_generation_pointer(root: &Path) -> Result<Option<ActiveGenerationPointer>> {
    let bytes = match crate::read_root::read_registered_file(
        root,
        Path::new(ACTIVE_GENERATION_POINTER_FILE),
    ) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => match fs::read(root.join(ACTIVE_GENERATION_POINTER_FILE)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    parse_active_generation_pointer(bytes).map(Some)
}

pub(crate) fn load_active_generation_pointer_from_read_root(
    root: &crate::GenerationReadRoot,
) -> Result<Option<ActiveGenerationPointer>> {
    let mut file = match root.open_file(Path::new(ACTIVE_GENERATION_POINTER_FILE)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    parse_active_generation_pointer(bytes).map(Some)
}

fn parse_active_generation_pointer(bytes: Vec<u8>) -> Result<ActiveGenerationPointer> {
    #[derive(Deserialize)]
    struct PointerVersion {
        version: u32,
    }
    let version: PointerVersion = serde_json::from_slice(&bytes)?;
    if version.version != ACTIVE_GENERATION_POINTER_VERSION {
        return Err(IndexError::UnsupportedActiveGenerationPointer(
            version.version,
        ));
    }
    let pointer: ActiveGenerationPointer = serde_json::from_slice(&bytes)?;
    if serde_json::to_vec(&pointer)? != bytes {
        return Err(IndexError::InvalidActiveGenerationPointer);
    }
    pointer.validate()?;
    Ok(pointer)
}

pub fn slot_path(root: &Path, slot: &GenerationSlot) -> PathBuf {
    root.join(INDEX_GENERATIONS_DIRECTORY)
        .join(slot.directory())
}

pub fn open_slot_index(root: &Path, slot: &GenerationSlot) -> Result<Index> {
    let directory =
        DurableMmapDirectory::open(slot_path(root, slot)).map_err(tantivy::TantivyError::from)?;
    let index = Index::open(directory)?;
    validate_lexical_index_settings(&index)?;
    Ok(index)
}

pub fn create_candidate_generation(
    root: &Path,
    base: Option<&GenerationSlot>,
    schema: Schema,
    writer_memory_bytes: u64,
) -> Result<CandidateGeneration> {
    if let Some(base) = base {
        let base_index = open_slot_index(root, base)?;
        let pointer = load_active_generation_pointer(root)?
            .ok_or(IndexError::MissingActiveGenerationPointer)?;
        if pointer.active() != base {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        return create_authenticated_candidate_generation(
            root,
            &pointer,
            &base_index,
            writer_memory_bytes,
        );
    }

    let generations = root.join(INDEX_GENERATIONS_DIRECTORY);
    ensure_private_directory(&generations)?;
    let directory_name = format!("{GENERATION_DIRECTORY_PREFIX}{}", Uuid::now_v7().simple());
    let path = generations.join(&directory_name);
    create_private_candidate_directory(&path)?;
    sync_directory(&generations)?;
    let directory = DurableMmapDirectory::open(&path).map_err(tantivy::TantivyError::from)?;
    let index = Index::create(directory, schema, lexical_index_settings())?;
    validate_lexical_index_settings(&index)?;
    let activation_fence = bind_candidate_activation_fence(root, Path::new(&directory_name))?;
    Ok(CandidateGeneration {
        directory_name,
        index,
        physical_proof: CandidatePhysicalProof::default(),
        activation_fence,
    })
}

pub fn publish_active_generation_pointer(
    root: &Path,
    pointer: &ActiveGenerationPointer,
) -> Result<PointerPublicationOutcome> {
    publish_active_generation_pointer_validated(root, pointer, || Ok(()))
}

pub fn publish_active_generation_pointer_validated<F, T>(
    root: &Path,
    pointer: &ActiveGenerationPointer,
    validate_before_replace: F,
) -> Result<PointerPublicationOutcome>
where
    F: FnOnce() -> Result<T>,
{
    pointer.validate()?;
    ensure_generation_read_lease_coordinator(root)?;
    let bytes = serde_json::to_vec(pointer)?;
    let directory = DurableMmapDirectory::open(root).map_err(tantivy::TantivyError::from)?;
    let mut retained_validation = None;
    let outcome = directory.atomic_write_with_outcome_validated(
        Path::new(ACTIVE_GENERATION_POINTER_FILE),
        &bytes,
        || {
            retained_validation = Some(validate_before_replace()?);
            Ok(())
        },
    )?;
    drop(retained_validation);
    match outcome {
        DurableAtomicWriteOutcome::Durable => Ok(PointerPublicationOutcome::Durable),
        DurableAtomicWriteOutcome::VisibleButDurabilityUncertain(error) => {
            Ok(PointerPublicationOutcome::CommittedVisible {
                detail: error.to_string(),
            })
        }
    }
}

pub fn sync_generation(path: &Path) -> Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            sync_generation_file(&entry.path())?;
        }
    }
    sync_directory(path)?;
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn sync_generation_file(path: &Path) -> std::io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(windows)]
fn sync_generation_file(path: &Path) -> std::io::Result<()> {
    OpenOptions::new()
        .write(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(path)?
        .sync_all()
}

#[cfg(not(windows))]
pub fn sync_directory(path: &Path) -> std::io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(windows)]
pub fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

pub fn reclaim_inactive_generation_directories(
    root: &Path,
    pointer: Option<&ActiveGenerationPointer>,
    lease: Option<&GenerationRetentionLease>,
) -> Result<()> {
    ensure_generation_read_lease_coordinator(root)?;
    let generations = root.join(INDEX_GENERATIONS_DIRECTORY);
    ensure_private_directory(&generations)?;
    let retained = pointer
        .into_iter()
        .flat_map(|pointer| std::iter::once(pointer.active()).chain(pointer.previous()))
        .map(|slot| slot.directory().to_owned())
        .chain(lease.map(|lease| lease.target().directory().to_owned()))
        .collect::<HashSet<_>>();
    let mut removed = false;
    for entry in fs::read_dir(&generations)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if is_generation_directory_name(&name) && !retained.contains(&name) {
            let Some(_reclaim_authority) = try_generation_directory_reclaim_authority(root, &name)?
            else {
                continue;
            };
            let candidate = RetainedGenerationDirectory::open(entry.path())?;
            reclamation_checkpoint(ReclamationStage::AfterCandidateRetained, candidate.path())?;
            remove_reclaimed_generation_directory(&candidate)?;
            removed = true;
        }
    }
    if removed {
        sync_directory(&generations)?;
    }
    Ok(())
}

fn remove_reclaimed_generation_directory(candidate: &RetainedGenerationDirectory) -> Result<()> {
    for attempt in 0..GENERATION_RECLAIM_REMOVE_ATTEMPTS {
        candidate.validate_binding()?;
        match fs::remove_dir_all(candidate.path()) {
            Ok(()) => return Ok(()),
            Err(error)
                if error.kind() == std::io::ErrorKind::DirectoryNotEmpty
                    && attempt + 1 < GENERATION_RECLAIM_REMOVE_ATTEMPTS =>
            {
                std::thread::yield_now();
            }
            Err(error) => return Err(error.into()),
        }
    }
    Err(IndexError::ConcurrentGenerationChange)
}

fn create_private_candidate_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700).create(path)?;
    }
    #[cfg(not(unix))]
    fs::create_dir(path)?;
    restrict_private_directory(path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GenerationDirectoryIdentity {
    first: u64,
    second: u64,
}

struct RetainedGenerationDirectory {
    path: PathBuf,
    _file: File,
    identity: GenerationDirectoryIdentity,
}

impl RetainedGenerationDirectory {
    fn open(path: PathBuf) -> Result<Self> {
        let file = open_generation_directory(&path)?;
        let identity = generation_directory_identity(&file)?;
        Ok(Self {
            path,
            _file: file,
            identity,
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn validate_binding(&self) -> Result<()> {
        let named = open_generation_directory(&self.path)
            .map_err(|_| IndexError::ConcurrentGenerationChange)?;
        if generation_directory_identity(&named)? != self.identity {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        Ok(())
    }
}

#[cfg(unix)]
fn open_generation_directory(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(windows)]
fn open_generation_directory(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .access_mode(FILE_GENERIC_READ)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(unix)]
fn generation_directory_identity(file: &File) -> Result<GenerationDirectoryIdentity> {
    let metadata = file.metadata()?;
    if !metadata.is_dir() {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    Ok(GenerationDirectoryIdentity {
        first: metadata.dev(),
        second: metadata.ino(),
    })
}

#[cfg(windows)]
fn generation_directory_identity(file: &File) -> Result<GenerationDirectoryIdentity> {
    let mut information = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    // SAFETY: `file` retains a live directory handle and `information` is a
    // correctly sized writable out pointer for the duration of the call.
    if unsafe {
        GetFileInformationByHandle(file.as_raw_handle() as HANDLE, information.as_mut_ptr())
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: the successful system call initialized the entire structure.
    let information = unsafe { information.assume_init() };
    if information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
        || information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    Ok(GenerationDirectoryIdentity {
        first: u64::from(information.dwVolumeSerialNumber),
        second: (u64::from(information.nFileIndexHigh) << 32)
            | u64::from(information.nFileIndexLow),
    })
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReclamationStage {
    AfterCandidateRetained,
}

#[cfg(not(any(test, feature = "test-support")))]
#[derive(Debug, Clone, Copy)]
enum ReclamationStage {
    AfterCandidateRetained,
}

#[cfg(any(test, feature = "test-support"))]
type ReclamationTestHook = Box<dyn FnMut(ReclamationStage, &Path) -> Result<()>>;

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static RECLAMATION_TEST_HOOK: std::cell::RefCell<Option<ReclamationTestHook>> =
        std::cell::RefCell::new(None);
}

#[cfg(any(test, feature = "test-support"))]
pub struct ReclamationTestHookGuard(Option<ReclamationTestHook>);

#[cfg(any(test, feature = "test-support"))]
impl ReclamationTestHookGuard {
    pub fn set<F>(hook: F) -> Self
    where
        F: FnMut(ReclamationStage, &Path) -> Result<()> + 'static,
    {
        let previous = RECLAMATION_TEST_HOOK.with(|active| active.replace(Some(Box::new(hook))));
        Self(previous)
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for ReclamationTestHookGuard {
    fn drop(&mut self) {
        RECLAMATION_TEST_HOOK.with(|active| active.replace(self.0.take()));
    }
}

#[cfg(any(test, feature = "test-support"))]
fn reclamation_checkpoint(stage: ReclamationStage, path: &Path) -> Result<()> {
    RECLAMATION_TEST_HOOK.with(|active| match active.borrow_mut().as_mut() {
        Some(hook) => hook(stage, path),
        None => Ok(()),
    })
}

#[cfg(not(any(test, feature = "test-support")))]
fn reclamation_checkpoint(_stage: ReclamationStage, _path: &Path) -> Result<()> {
    Ok(())
}

fn is_generation_directory_name(name: &str) -> bool {
    name.strip_prefix(GENERATION_DIRECTORY_PREFIX)
        .is_some_and(|suffix| {
            suffix.len() == 32
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}
#[cfg(test)]
mod tests {
    use super::*;

    fn create_mismatched_slot(root: &Path) -> GenerationSlot {
        let directory_name = "generation-00000000000000000000000000000001";
        let path = root.join(INDEX_GENERATIONS_DIRECTORY).join(directory_name);
        fs::create_dir_all(&path).unwrap();
        let mismatched_settings = IndexSettings {
            docstore_compression: Compressor::Zstd(tantivy::store::ZstdCompressor {
                compression_level: Some(1),
            }),
            ..lexical_index_settings()
        };
        Index::builder()
            .schema(Schema::builder().build())
            .settings(mismatched_settings)
            .create_in_dir(&path)
            .unwrap();
        GenerationSlot::new("0".repeat(64), directory_name.to_owned(), "0".repeat(64)).unwrap()
    }

    #[test]
    fn lexical_index_settings_are_exact() {
        let settings = lexical_index_settings();

        assert_eq!(settings.docstore_compression, Compressor::Lz4);
        assert_eq!(settings.docstore_blocksize, 32 * 1024);
        assert!(settings.docstore_compress_dedicated_thread);
        assert_ne!(settings, IndexSettings::default());
    }

    #[test]
    fn fresh_candidate_activation_fence_binds_and_settings_roundtrip_exactly() {
        let root = tempfile::tempdir().unwrap();
        let candidate =
            create_candidate_generation(root.path(), None, Schema::builder().build(), 0).unwrap();
        candidate.activation_fence.validate_binding().unwrap();
        let slot = GenerationSlot::new(
            "0".repeat(64),
            candidate.directory_name.clone(),
            "0".repeat(64),
        )
        .unwrap();
        assert_eq!(candidate.index.settings(), &lexical_index_settings());
        drop(candidate.index);

        let reopened = open_slot_index(root.path(), &slot).unwrap();
        assert_eq!(reopened.settings(), &lexical_index_settings());
    }

    #[test]
    fn completed_generation_syncs_before_validated_pointer_publication() {
        let root = tempfile::tempdir().unwrap();
        ensure_private_directory(root.path()).unwrap();
        let candidate =
            create_candidate_generation(root.path(), None, Schema::builder().build(), 0).unwrap();
        let slot = GenerationSlot::new(
            "0".repeat(64),
            candidate.directory_name.clone(),
            "0".repeat(64),
        )
        .unwrap();
        let pointer = ActiveGenerationPointer::new(slot.clone(), None).unwrap();

        sync_generation(&slot_path(root.path(), &slot)).unwrap();
        assert!(matches!(
            publish_active_generation_pointer_validated(root.path(), &pointer, || {
                candidate.activation_fence.validate_binding()
            })
            .unwrap(),
            PointerPublicationOutcome::Durable
        ));
        assert_eq!(
            load_active_generation_pointer(root.path()).unwrap(),
            Some(pointer)
        );
    }

    #[test]
    fn active_pointer_publication_preserves_exact_v2_bytes() {
        let root = tempfile::tempdir().unwrap();
        let active = GenerationSlot::new(
            "1".repeat(64),
            format!("generation-{}", "1".repeat(32)),
            "2".repeat(64),
        )
        .unwrap();
        let previous = GenerationSlot::new(
            "3".repeat(64),
            format!("generation-{}", "3".repeat(32)),
            "4".repeat(64),
        )
        .unwrap();
        let pointer = ActiveGenerationPointer::new(active, Some(previous)).unwrap();
        let expected = format!(
            "{{\"version\":2,\"active\":{{\"generation_id\":\"{}\",\"directory\":\"generation-{}\",\"physical_integrity_digest\":\"{}\"}},\"previous\":{{\"generation_id\":\"{}\",\"directory\":\"generation-{}\",\"physical_integrity_digest\":\"{}\"}}}}",
            "1".repeat(64),
            "1".repeat(32),
            "2".repeat(64),
            "3".repeat(64),
            "3".repeat(32),
            "4".repeat(64),
        );

        assert!(matches!(
            publish_active_generation_pointer(root.path(), &pointer).unwrap(),
            PointerPublicationOutcome::Durable
        ));
        assert_eq!(
            fs::read(root.path().join(ACTIVE_GENERATION_POINTER_FILE)).unwrap(),
            expected.as_bytes()
        );
        assert_eq!(
            load_active_generation_pointer(root.path()).unwrap(),
            Some(pointer)
        );
    }

    #[test]
    fn opened_index_with_mismatched_settings_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        let slot = create_mismatched_slot(root.path());

        assert!(matches!(
            open_slot_index(root.path(), &slot),
            Err(IndexError::IndexSettingsMismatch)
        ));
    }

    #[test]
    fn cloned_candidate_with_mismatched_settings_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        let slot = create_mismatched_slot(root.path());

        assert!(matches!(
            create_candidate_generation(root.path(), Some(&slot), Schema::builder().build(), 0),
            Err(IndexError::IndexSettingsMismatch)
        ));
    }

    #[test]
    fn version_one_pointer_requires_rebuild_instead_of_compatibility() {
        let root = tempfile::tempdir().unwrap();
        let bytes = format!(
            "{{\"version\":1,\"active\":{{\"generation_id\":\"{}\",\"directory\":\"generation-00000000000000000000000000000001\"}},\"previous\":null}}",
            "0".repeat(64)
        );
        fs::write(root.path().join(ACTIVE_GENERATION_POINTER_FILE), bytes).unwrap();

        assert!(matches!(
            load_active_generation_pointer(root.path()),
            Err(IndexError::UnsupportedActiveGenerationPointer(1))
        ));
    }
}
