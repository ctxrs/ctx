use std::{
    fs::{File, Metadata, Permissions},
    io,
    path::{Path, PathBuf},
    time::SystemTime,
};

use tantivy::Index;
use uuid::Uuid;

use super::{
    record_candidate_clone_metrics, validate_single_component, CandidateActivationFence,
    CandidateCloneMetrics, MAX_REPUBLISH_DIRECTORY_ENTRIES,
};
use crate::{
    lexical_index_settings, verify_or_certify_physical_integrity, ActiveGenerationPointer,
    CandidateGeneration, CandidatePhysicalProof, DurableMmapDirectory,
    GenerationError as IndexError, Result, INDEX_GENERATIONS_DIRECTORY,
};

pub(super) fn candidate_available_bytes(root: &Path) -> Result<u64> {
    let directory = BoundDirectory::open_path(root)?;
    available_bytes(&directory, false)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Regular,
    Directory,
    LinkOrReparse,
    Special,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ObjectIdentity {
    first: u64,
    second: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileIdentity {
    object: ObjectIdentity,
    bytes: u64,
    modified: Option<SystemTime>,
    permissions: PermissionIdentity,
}

impl FileIdentity {
    fn from_file(file: &File) -> Result<Self> {
        let metadata = file.metadata()?;
        require_regular(entry_kind(&metadata)?)?;
        Ok(Self {
            object: platform::object_identity(file)?,
            bytes: metadata.len(),
            modified: metadata.modified().ok(),
            permissions: platform::permission_identity(&metadata),
        })
    }
}

#[cfg(unix)]
type PermissionIdentity = u32;

#[cfg(windows)]
type PermissionIdentity = bool;

struct BoundDirectory {
    path: PathBuf,
    file: File,
    identity: ObjectIdentity,
}

impl BoundDirectory {
    fn open_path(path: &Path) -> Result<Self> {
        let file = platform::open_directory_path(path).map_err(source_topology_open_error)?;
        require_directory(entry_kind(&file.metadata()?)?)?;
        let identity = platform::object_identity(&file)?;
        Ok(Self {
            path: path.to_path_buf(),
            file,
            identity,
        })
    }

    fn open_at(parent: &Self, name: &Path) -> Result<Self> {
        validate_single_component(name)?;
        let file = platform::open_directory_at(&parent.file, &parent.path, name)
            .map_err(source_topology_open_error)?;
        Self::from_child(parent, name, file)
    }

    fn open_discardable_at(parent: &Self, name: &Path) -> Result<Self> {
        validate_single_component(name)?;
        let file = platform::open_discardable_directory_at(&parent.file, &parent.path, name)
            .map_err(source_topology_open_error)?;
        Self::from_child(parent, name, file)
    }

    fn create_at(parent: &Self, name: &Path) -> Result<Self> {
        validate_single_component(name)?;
        let file = platform::create_directory_at(&parent.file, &parent.path, name)?;
        Self::from_child(parent, name, file)
    }

    fn from_child(parent: &Self, name: &Path, file: File) -> Result<Self> {
        require_directory(entry_kind(&file.metadata()?)?)?;
        let identity = platform::object_identity(&file)?;
        let directory = Self {
            path: parent.path.join(name),
            file,
            identity,
        };
        directory.validate_child_binding(parent, name)?;
        Ok(directory)
    }

    fn validate_child_binding(&self, parent: &Self, name: &Path) -> Result<()> {
        let named = platform::open_directory_at(&parent.file, &parent.path, name)
            .map_err(source_topology_open_error)?;
        require_directory(entry_kind(&named.metadata()?)?)?;
        if platform::object_identity(&named)? != self.identity {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "republish directory changed after authentication",
            ));
        }
        Ok(())
    }

    fn validate_path_binding(&self) -> Result<()> {
        let named = Self::open_path(&self.path)?;
        if named.identity != self.identity {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "republish directory path changed after authentication",
            ));
        }
        Ok(())
    }
}

pub(super) struct CandidateGuard {
    _root: BoundDirectory,
    generations: BoundDirectory,
    destination_name: PathBuf,
    destination: BoundDirectory,
}

impl CandidateGuard {
    pub(super) fn bind(root: &Path, destination_name: &Path) -> Result<Self> {
        validate_single_component(destination_name)?;
        let root = BoundDirectory::open_path(root)?;
        let generations = BoundDirectory::open_at(&root, Path::new(INDEX_GENERATIONS_DIRECTORY))?;
        let destination = BoundDirectory::open_discardable_at(&generations, destination_name)?;
        Ok(Self {
            _root: root,
            generations,
            destination_name: destination_name.to_path_buf(),
            destination,
        })
    }

    pub(super) fn validate_binding(&self) -> Result<()> {
        self._root.validate_path_binding()?;
        self.generations
            .validate_child_binding(&self._root, Path::new(INDEX_GENERATIONS_DIRECTORY))?;
        self.destination
            .validate_child_binding(&self.generations, &self.destination_name)
    }

    pub(super) fn discard(self) {
        if clone_checkpoint(PortableCloneStage::BeforeCleanup, &self.destination_name).is_err()
            || self.validate_binding().is_err()
        {
            return;
        }
        if platform::discard_destination(
            &self.generations.file,
            &self.generations.path,
            &self.destination_name,
            &self.destination.file,
            &self.destination.path,
        )
        .is_ok()
        {
            let _ = platform::sync_directory(&self.generations.file);
        }
    }
}

pub(super) fn create_authenticated_candidate_generation(
    root: &Path,
    predecessor_pointer: &ActiveGenerationPointer,
    predecessor_index: &Index,
    writer_memory_bytes: u64,
) -> Result<CandidateGeneration> {
    let base = predecessor_pointer.active();
    let certified =
        verify_or_certify_physical_integrity(root, predecessor_pointer, base, predecessor_index)?;
    let root_directory = BoundDirectory::open_path(root)?;
    let generations_name = Path::new(INDEX_GENERATIONS_DIRECTORY);
    let generations = BoundDirectory::open_at(&root_directory, generations_name)?;
    let source_name = Path::new(base.directory());
    validate_single_component(source_name)?;
    let source = BoundDirectory::open_at(&generations, source_name)?;

    let plan =
        planning::authenticated_clone_plan(&generations, source_name, &source, predecessor_index)?;
    let required_headroom = plan.full_copy_candidate_headroom(writer_memory_bytes)?;
    let writer_output_headroom = plan.writer_output_headroom(writer_memory_bytes)?;
    let available = available_bytes(&generations, false)?;
    record_plan_metrics_with_required(&plan, available, required_headroom);
    if available < required_headroom {
        return Err(IndexError::CurrentRepublishInsufficientHeadroom {
            available,
            required: required_headroom,
        });
    }

    let directory_name = format!("generation-{}", Uuid::now_v7().simple());
    let destination_name = PathBuf::from(&directory_name);
    let destination = BoundDirectory::create_at(&generations, &destination_name)?;
    platform::restrict_destination_directory(&destination.file)?;
    let guard = CandidateGuard {
        _root: root_directory,
        generations,
        destination_name,
        destination,
    };
    let source_path = guard.generations.path.join(source_name);
    let destination_path = guard.destination.path.clone();
    let clone_result = (|| {
        source.validate_child_binding(&guard.generations, source_name)?;
        guard.validate_binding()?;
        let mut physical_proof = CandidatePhysicalProof::default();
        let mut metrics = CandidateCloneMetrics::default();
        transfer::clone_candidate_files(
            root,
            &source_path,
            predecessor_pointer,
            &certified,
            &guard.generations,
            source_name,
            &source,
            &guard.destination_name,
            &guard.destination,
            &plan,
            writer_output_headroom,
            &mut physical_proof,
            &mut metrics,
        )?;
        platform::sync_directory(&guard.destination.file)?;
        platform::sync_directory(&guard.generations.file)?;
        source.validate_child_binding(&guard.generations, source_name)?;
        guard.validate_binding()?;

        let directory =
            DurableMmapDirectory::open(&destination_path).map_err(tantivy::TantivyError::from)?;
        let index = Index::open(directory)?;
        if index.settings() != &lexical_index_settings() {
            return Err(IndexError::IndexSettingsMismatch);
        }
        record_candidate_clone_metrics(metrics);
        Ok((directory_name, index, physical_proof))
    })();
    match clone_result {
        Ok((directory_name, index, physical_proof)) => Ok(CandidateGeneration {
            directory_name,
            index,
            physical_proof,
            activation_fence: CandidateActivationFence::portable(guard),
        }),
        Err(error) => {
            guard.discard();
            Err(error)
        }
    }
}

struct OpenedFile {
    file: File,
    identity: FileIdentity,
    permissions: Permissions,
}

#[cfg(all(test, windows))]
mod windows_tests {
    use ctx_history_platform::platform_security::ensure_private_directory;
    use tempfile::tempdir;

    use super::*;
    use crate::GenerationReadRoot;

    #[test]
    fn source_topology_guard_coexists_with_generation_read_root_descent() {
        let temp = tempdir().unwrap();
        ensure_private_directory(temp.path()).unwrap();
        let generations = temp.path().join(INDEX_GENERATIONS_DIRECTORY);
        ensure_private_directory(&generations).unwrap();
        let source_name = Path::new("generation-00000000000000000000000000000000");
        ensure_private_directory(&generations.join(source_name)).unwrap();

        let root_guard = BoundDirectory::open_path(temp.path()).unwrap();
        let generations_guard =
            BoundDirectory::open_at(&root_guard, Path::new(INDEX_GENERATIONS_DIRECTORY)).unwrap();
        let _source_guard = BoundDirectory::open_at(&generations_guard, source_name).unwrap();

        let read_root = GenerationReadRoot::open_index_root(temp.path()).unwrap();
        read_root
            .opened()
            .open_directory(&Path::new(INDEX_GENERATIONS_DIRECTORY).join(source_name))
            .unwrap();
    }
}

fn open_bound_file(directory: &BoundDirectory, relative: &Path) -> Result<OpenedFile> {
    validate_single_component(relative)?;
    let file = platform::open_regular_file_at(&directory.file, &directory.path, relative)
        .map_err(source_topology_open_error)?;
    let metadata = file.metadata()?;
    require_regular(entry_kind(&metadata)?)?;
    let identity = FileIdentity::from_file(&file)?;
    let permissions = metadata.permissions();
    validate_named_file(directory, relative, &identity)?;
    Ok(OpenedFile {
        file,
        identity,
        permissions,
    })
}

fn validate_named_file(
    directory: &BoundDirectory,
    relative: &Path,
    expected: &FileIdentity,
) -> Result<()> {
    let named = platform::open_regular_file_at(&directory.file, &directory.path, relative)
        .map_err(source_topology_open_error)?;
    if FileIdentity::from_file(&named)? != *expected {
        return Err(IndexError::CurrentRepublishSourceTopology(
            "named file changed after authentication",
        ));
    }
    Ok(())
}

fn entry_kind(metadata: &Metadata) -> Result<EntryKind> {
    if metadata.file_type().is_symlink() || platform::is_unsafe_link_or_provider(metadata) {
        Ok(EntryKind::LinkOrReparse)
    } else if metadata.is_file() {
        Ok(EntryKind::Regular)
    } else if metadata.is_dir() {
        Ok(EntryKind::Directory)
    } else {
        Ok(EntryKind::Special)
    }
}

fn require_regular(kind: EntryKind) -> Result<()> {
    match kind {
        EntryKind::Regular => Ok(()),
        EntryKind::LinkOrReparse => Err(IndexError::CurrentRepublishSourceTopology(
            "symlink, reparse point, or remote-provider file in republish source",
        )),
        EntryKind::Directory | EntryKind::Special => Err(
            IndexError::CurrentRepublishSourceTopology("non-regular directory entry"),
        ),
    }
}

fn require_directory(kind: EntryKind) -> Result<()> {
    match kind {
        EntryKind::Directory => Ok(()),
        EntryKind::LinkOrReparse => Err(IndexError::CurrentRepublishSourceTopology(
            "symlinked, reparse-point, or remote-provider republish directory",
        )),
        EntryKind::Regular | EntryKind::Special => Err(IndexError::CurrentRepublishSourceTopology(
            "republish path is not a directory",
        )),
    }
}

mod planning;
use planning::ValidatedClonePlan;

mod resource;
use resource::{available_bytes, source_topology_open_error};

mod support;

#[cfg(any(test, feature = "test-support"))]
pub(super) use support::forced_for_test;
#[cfg(not(any(test, feature = "test-support")))]
use support::PortableCloneStage;
use support::{clone_checkpoint, record_plan_metrics_with_required};
#[cfg(any(test, feature = "test-support"))]
pub use support::{
    PortableCloneMetrics, PortableCloneStage, PortableCloneTestGuard, PortableCloneTestOptions,
};

mod transfer;

#[cfg(unix)]
#[path = "portable/unix.rs"]
mod platform;

#[cfg(windows)]
#[path = "portable/windows.rs"]
mod platform;

#[cfg(test)]
mod tests;
