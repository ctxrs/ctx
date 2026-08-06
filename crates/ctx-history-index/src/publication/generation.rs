use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
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
use tantivy::{store::Compressor, Index, IndexSettings};
use uuid::Uuid;

use crate::{
    analyzer::register_body_analyzer,
    durable_directory::{DurableAtomicWriteOutcome, DurableMmapDirectory},
    identity::is_generation_id,
    lexical_schema, sync_directory, IndexError, Result,
};

pub(crate) const ACTIVE_GENERATION_POINTER_FILE: &str = "active-generation.json";
pub(crate) const INDEX_GENERATIONS_DIRECTORY: &str = "index-generations";
const ACTIVE_GENERATION_POINTER_VERSION: u32 = 2;
const GENERATION_DIRECTORY_PREFIX: &str = "generation-";

pub(crate) fn lexical_index_settings() -> IndexSettings {
    IndexSettings {
        docstore_compression: Compressor::Lz4,
        docstore_compress_dedicated_thread: true,
        docstore_blocksize: 32 * 1024,
    }
}

fn validate_lexical_index_settings(index: &Index) -> Result<()> {
    if index.settings() != &lexical_index_settings() {
        return Err(IndexError::IndexSettingsMismatch(
            crate::LEXICAL_SCHEMA_VERSION,
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GenerationSlot {
    generation_id: String,
    directory: String,
    physical_integrity_digest: String,
}

impl GenerationSlot {
    pub(crate) fn new(
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

    pub(crate) fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub(crate) fn directory(&self) -> &str {
        &self.directory
    }

    pub(crate) fn physical_integrity_digest(&self) -> &str {
        &self.physical_integrity_digest
    }

    pub(crate) fn names_are_valid(generation_id: &str, directory: &str) -> bool {
        is_generation_id(generation_id) && is_generation_directory_name(directory)
    }

    pub(crate) fn validate(&self) -> Result<()> {
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
pub(crate) struct ActiveGenerationPointer {
    version: u32,
    active: GenerationSlot,
    previous: Option<GenerationSlot>,
}

impl ActiveGenerationPointer {
    pub(crate) fn new(active: GenerationSlot, previous: Option<GenerationSlot>) -> Result<Self> {
        let pointer = Self {
            version: ACTIVE_GENERATION_POINTER_VERSION,
            active,
            previous,
        };
        pointer.validate()?;
        Ok(pointer)
    }

    pub(crate) fn active(&self) -> &GenerationSlot {
        &self.active
    }

    pub(crate) fn previous(&self) -> Option<&GenerationSlot> {
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

pub(crate) struct CandidateGeneration {
    pub(crate) directory_name: String,
    pub(crate) index: Index,
}

#[derive(Debug)]
pub(crate) enum PointerPublicationOutcome {
    Durable,
    CommittedVisible { detail: String },
}

pub(crate) fn load_active_generation_pointer(
    root: &Path,
) -> Result<Option<ActiveGenerationPointer>> {
    let path = root.join(ACTIVE_GENERATION_POINTER_FILE);
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
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
    Ok(Some(pointer))
}

pub(crate) fn slot_path(root: &Path, slot: &GenerationSlot) -> PathBuf {
    root.join(INDEX_GENERATIONS_DIRECTORY)
        .join(slot.directory())
}

pub(crate) fn open_slot_index(root: &Path, slot: &GenerationSlot) -> Result<Index> {
    let directory =
        DurableMmapDirectory::open(slot_path(root, slot)).map_err(tantivy::TantivyError::from)?;
    let index = Index::open(directory)?;
    validate_lexical_index_settings(&index)?;
    register_body_analyzer(&index);
    Ok(index)
}

pub(crate) fn create_candidate_generation(
    root: &Path,
    base: Option<&GenerationSlot>,
) -> Result<CandidateGeneration> {
    let generations = root.join(INDEX_GENERATIONS_DIRECTORY);
    fs::create_dir_all(&generations)?;
    let directory_name = format!("{GENERATION_DIRECTORY_PREFIX}{}", Uuid::now_v7().simple());
    let path = generations.join(&directory_name);
    fs::create_dir(&path)?;
    if let Some(base) = base {
        clone_index_files(&slot_path(root, base), &path)?;
    }
    sync_directory(&generations)?;
    let directory = DurableMmapDirectory::open(&path).map_err(tantivy::TantivyError::from)?;
    let index = if base.is_some() {
        Index::open(directory)?
    } else {
        Index::create(directory, lexical_schema(), lexical_index_settings())?
    };
    validate_lexical_index_settings(&index)?;
    register_body_analyzer(&index);
    Ok(CandidateGeneration {
        directory_name,
        index,
    })
}

pub(crate) fn publish_active_generation_pointer(
    root: &Path,
    pointer: &ActiveGenerationPointer,
) -> Result<PointerPublicationOutcome> {
    pointer.validate()?;
    let bytes = serde_json::to_vec(pointer)?;
    let directory = DurableMmapDirectory::open(root).map_err(tantivy::TantivyError::from)?;
    match directory.atomic_write_with_outcome(Path::new(ACTIVE_GENERATION_POINTER_FILE), &bytes)? {
        DurableAtomicWriteOutcome::Durable => Ok(PointerPublicationOutcome::Durable),
        DurableAtomicWriteOutcome::VisibleButDurabilityUncertain(error) => {
            Ok(PointerPublicationOutcome::CommittedVisible {
                detail: error.to_string(),
            })
        }
    }
}

pub(crate) fn sync_generation(path: &Path) -> Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            File::open(entry.path())?.sync_all()?;
        }
    }
    sync_directory(path)?;
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

pub(crate) fn reclaim_inactive_generation_directories(
    root: &Path,
    pointer: Option<&ActiveGenerationPointer>,
    lease: Option<&super::GenerationRetentionLease>,
) -> Result<()> {
    let generations = root.join(INDEX_GENERATIONS_DIRECTORY);
    fs::create_dir_all(&generations)?;
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
            let candidate = RetainedGenerationDirectory::open(entry.path())?;
            reclamation_checkpoint(ReclamationStage::AfterCandidateRetained, candidate.path())?;
            candidate.validate_binding()?;
            fs::remove_dir_all(candidate.path())?;
            removed = true;
        }
    }
    if removed {
        sync_directory(&generations)?;
    }
    Ok(())
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

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReclamationStage {
    AfterCandidateRetained,
}

#[cfg(not(test))]
#[derive(Debug, Clone, Copy)]
enum ReclamationStage {
    AfterCandidateRetained,
}

#[cfg(test)]
type ReclamationTestHook = Box<dyn FnMut(ReclamationStage, &Path) -> Result<()>>;

#[cfg(test)]
thread_local! {
    static RECLAMATION_TEST_HOOK: std::cell::RefCell<Option<ReclamationTestHook>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
pub(crate) struct ReclamationTestHookGuard(Option<ReclamationTestHook>);

#[cfg(test)]
impl ReclamationTestHookGuard {
    pub(crate) fn set<F>(hook: F) -> Self
    where
        F: FnMut(ReclamationStage, &Path) -> Result<()> + 'static,
    {
        let previous = RECLAMATION_TEST_HOOK.with(|active| active.replace(Some(Box::new(hook))));
        Self(previous)
    }
}

#[cfg(test)]
impl Drop for ReclamationTestHookGuard {
    fn drop(&mut self) {
        RECLAMATION_TEST_HOOK.with(|active| active.replace(self.0.take()));
    }
}

#[cfg(test)]
fn reclamation_checkpoint(stage: ReclamationStage, path: &Path) -> Result<()> {
    RECLAMATION_TEST_HOOK.with(|active| match active.borrow_mut().as_mut() {
        Some(hook) => hook(stage, path),
        None => Ok(()),
    })
}

#[cfg(not(test))]
fn reclamation_checkpoint(_stage: ReclamationStage, _path: &Path) -> Result<()> {
    Ok(())
}

fn clone_index_files(source: &Path, destination: &Path) -> Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() || should_skip_index_file(&entry.file_name()) {
            continue;
        }
        let target = destination.join(entry.file_name());
        let copy_required = matches!(
            entry.file_name().to_str(),
            Some("meta.json" | ".managed.json")
        );
        if copy_required || fs::hard_link(entry.path(), &target).is_err() {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn should_skip_index_file(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return true;
    };
    name.ends_with(".lock") || name.starts_with(".ctx-tantivy-atomic-")
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
            .schema(lexical_schema())
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
    fn candidate_settings_roundtrip_exactly() {
        let root = tempfile::tempdir().unwrap();
        let candidate = create_candidate_generation(root.path(), None).unwrap();
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
    fn opened_index_with_mismatched_settings_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        let slot = create_mismatched_slot(root.path());

        assert!(matches!(
            open_slot_index(root.path(), &slot),
            Err(IndexError::IndexSettingsMismatch(
                crate::LEXICAL_SCHEMA_VERSION
            ))
        ));
    }

    #[test]
    fn cloned_candidate_with_mismatched_settings_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        let slot = create_mismatched_slot(root.path());

        assert!(matches!(
            create_candidate_generation(root.path(), Some(&slot)),
            Err(IndexError::IndexSettingsMismatch(
                crate::LEXICAL_SCHEMA_VERSION
            ))
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
