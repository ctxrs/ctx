use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, Metadata, Permissions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};

use sha2::{Digest, Sha256};
use tantivy::Index;
use uuid::Uuid;

use crate::{
    analyzer::register_body_analyzer, durable_directory::DurableMmapDirectory,
    physical_integrity_digest, IndexError, Result,
};

use super::super::super::{
    generation::CandidateGeneration, lexical_index_settings, ActiveGenerationPointer,
    INDEX_GENERATIONS_DIRECTORY,
};
use super::{
    admit_clone_resource, validate_single_component, MANAGED_FILE, MAX_MANAGED_METADATA_BYTES,
    MAX_MIGRATION_CLONE_BYTES, MAX_MIGRATION_CLONE_FILES, MAX_MIGRATION_DIRECTORY_ENTRIES,
    MIGRATION_HEADROOM_RESERVE_BYTES, TANTIVY_LOCK_FILES,
};

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
            return Err(IndexError::PredecessorMigrationSourceTopology(
                "migration directory changed after authentication",
            ));
        }
        Ok(())
    }

    fn validate_path_binding(&self) -> Result<()> {
        let named = Self::open_path(&self.path)?;
        if named.identity != self.identity {
            return Err(IndexError::PredecessorMigrationSourceTopology(
                "migration directory path changed after authentication",
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

#[derive(Debug, Clone)]
struct PlannedFile {
    path: PathBuf,
    identity: FileIdentity,
    permissions: Permissions,
}

struct ClonePlan {
    files: Vec<PlannedFile>,
    logical_bytes: u64,
    required_headroom: u64,
}

pub(super) fn create_authenticated_migration_candidate(
    root: &Path,
    predecessor_pointer: &ActiveGenerationPointer,
    predecessor_index: &Index,
) -> Result<(CandidateGeneration, CandidateGuard)> {
    let base = predecessor_pointer.active();
    let root_directory = BoundDirectory::open_path(root)?;
    let generations_name = Path::new(INDEX_GENERATIONS_DIRECTORY);
    let generations = BoundDirectory::open_at(&root_directory, generations_name)?;
    let source_name = Path::new(base.directory());
    validate_single_component(source_name)?;
    let source = BoundDirectory::open_at(&generations, source_name)?;

    let plan = authenticated_clone_plan(&generations, source_name, &source, predecessor_index)?;
    let available = available_bytes(&generations)?;
    record_plan_metrics(&plan, available);
    if available < plan.required_headroom {
        return Err(IndexError::PredecessorMigrationInsufficientHeadroom {
            available,
            required: plan.required_headroom,
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
    let destination_path = guard.destination.path.clone();
    let clone_result = (|| {
        source.validate_child_binding(&guard.generations, source_name)?;
        guard.validate_binding()?;
        clone_files(
            &guard.generations,
            source_name,
            &source,
            &guard.destination_name,
            &guard.destination,
            &plan,
        )?;
        platform::sync_directory(&guard.destination.file)?;
        platform::sync_directory(&guard.generations.file)?;
        source.validate_child_binding(&guard.generations, source_name)?;
        guard.validate_binding()?;

        let directory =
            DurableMmapDirectory::open(&destination_path).map_err(tantivy::TantivyError::from)?;
        let index = Index::open(directory)?;
        if index.settings() != &lexical_index_settings() {
            return Err(IndexError::IndexSettingsMismatch(
                crate::LEXICAL_SCHEMA_VERSION,
            ));
        }
        let cloned_digest =
            physical_integrity_digest(&index, &destination_path, Some(predecessor_pointer))?;
        if cloned_digest != base.physical_integrity_digest() {
            return Err(IndexError::ChecksumMismatch);
        }
        register_body_analyzer(&index);
        Ok(CandidateGeneration {
            directory_name,
            index,
        })
    })();
    match clone_result {
        Ok(candidate) => Ok((candidate, guard)),
        Err(error) => {
            guard.discard();
            Err(error)
        }
    }
}

fn authenticated_clone_plan(
    generations: &BoundDirectory,
    source_name: &Path,
    source: &BoundDirectory,
    index: &Index,
) -> Result<ClonePlan> {
    let mut active = super::super::super::verification::active_index_files(index)?;
    active.insert(PathBuf::from("meta.json"));
    for path in &active {
        validate_single_component(path)?;
    }

    let mut seen_active = BTreeSet::new();
    let mut managed_seen = false;
    let mut planned = BTreeMap::new();
    let mut total_files = 0_usize;
    let mut total_bytes = 0_u64;
    source.validate_child_binding(generations, source_name)?;
    for name in
        platform::directory_entries(&source.file, &source.path, MAX_MIGRATION_DIRECTORY_ENTRIES)?
    {
        let name_text = name
            .to_str()
            .ok_or(IndexError::PredecessorMigrationSourceTopology(
                "non-UTF-8 directory entry",
            ))?;
        let relative = PathBuf::from(&name);
        validate_single_component(&relative)?;
        let opened = open_bound_file(source, &relative)?;
        if active.contains(&relative) {
            seen_active.insert(relative.clone());
            admit_clone_resource(
                &mut total_files,
                &mut total_bytes,
                opened.identity.bytes,
                MAX_MIGRATION_CLONE_FILES,
                MAX_MIGRATION_CLONE_BYTES,
            )?;
            planned.insert(
                relative.clone(),
                PlannedFile {
                    path: relative,
                    identity: opened.identity,
                    permissions: opened.permissions,
                },
            );
        } else if name_text == MANAGED_FILE {
            if opened.identity.bytes > MAX_MANAGED_METADATA_BYTES {
                return Err(IndexError::PredecessorMigrationByteLimit {
                    actual: opened.identity.bytes,
                    maximum: MAX_MANAGED_METADATA_BYTES,
                });
            }
            managed_seen = true;
            admit_clone_resource(
                &mut total_files,
                &mut total_bytes,
                opened.identity.bytes,
                MAX_MIGRATION_CLONE_FILES,
                MAX_MIGRATION_CLONE_BYTES,
            )?;
            planned.insert(
                relative.clone(),
                PlannedFile {
                    path: relative,
                    identity: opened.identity,
                    permissions: opened.permissions,
                },
            );
        } else if TANTIVY_LOCK_FILES.contains(&name_text) && opened.identity.bytes == 0 {
            continue;
        } else {
            return Err(IndexError::PredecessorMigrationSourceTopology(
                "unexpected directory entry",
            ));
        }
    }
    source.validate_child_binding(generations, source_name)?;
    if seen_active != active || !managed_seen {
        return Err(IndexError::PredecessorMigrationSourceTopology(
            "active or managed file missing",
        ));
    }

    let managed = planned.get(Path::new(MANAGED_FILE)).ok_or(
        IndexError::PredecessorMigrationSourceTopology("managed file missing"),
    )?;
    let managed_bytes = read_planned_file(source, managed, MAX_MANAGED_METADATA_BYTES)?;
    let managed_paths: Vec<PathBuf> = serde_json::from_slice(&managed_bytes)
        .map_err(|_| IndexError::PredecessorMigrationSourceTopology("invalid managed metadata"))?;
    for path in &managed_paths {
        validate_single_component(path)?;
    }
    let managed_set = managed_paths.iter().cloned().collect::<BTreeSet<_>>();
    if managed_set.len() != managed_paths.len() || managed_set != active {
        return Err(IndexError::PredecessorMigrationSourceTopology(
            "managed metadata does not match active files",
        ));
    }

    let required_headroom = total_bytes
        .checked_add(MIGRATION_HEADROOM_RESERVE_BYTES)
        .ok_or(IndexError::CountOverflow)?;
    Ok(ClonePlan {
        files: planned.into_values().collect(),
        logical_bytes: total_bytes,
        required_headroom,
    })
}

struct OpenedFile {
    file: File,
    identity: FileIdentity,
    permissions: Permissions,
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

fn open_planned_file(directory: &BoundDirectory, planned: &PlannedFile) -> Result<File> {
    let opened = open_bound_file(directory, &planned.path)?;
    if opened.identity != planned.identity {
        return Err(IndexError::PredecessorMigrationSourceTopology(
            "source file changed after authentication",
        ));
    }
    Ok(opened.file)
}

fn read_planned_file(
    directory: &BoundDirectory,
    planned: &PlannedFile,
    maximum: u64,
) -> Result<Vec<u8>> {
    if planned.identity.bytes > maximum {
        return Err(IndexError::PredecessorMigrationByteLimit {
            actual: planned.identity.bytes,
            maximum,
        });
    }
    let mut file = open_planned_file(directory, planned)?;
    let allocation = usize::try_from(planned.identity.bytes).map_err(|_| {
        IndexError::PredecessorMigrationByteLimit {
            actual: planned.identity.bytes,
            maximum,
        }
    })?;
    let mut bytes = Vec::with_capacity(allocation);
    Read::by_ref(&mut file)
        .take(planned.identity.bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != planned.identity.bytes {
        return Err(IndexError::PredecessorMigrationSourceTopology(
            "source file size changed while reading",
        ));
    }
    validate_open_and_named_file(directory, planned, &file)?;
    Ok(bytes)
}

fn clone_files(
    generations: &BoundDirectory,
    source_name: &Path,
    source: &BoundDirectory,
    destination_name: &Path,
    destination: &BoundDirectory,
    plan: &ClonePlan,
) -> Result<()> {
    let mut copied_bytes = 0_u64;
    for planned in &plan.files {
        source.validate_child_binding(generations, source_name)?;
        clone_checkpoint(PortableCloneStage::BeforeCopy, &planned.path)?;
        let mut source_file = open_planned_file(source, planned)?;
        clone_checkpoint(PortableCloneStage::AfterSourceOpen, &planned.path)?;
        destination.validate_child_binding(generations, destination_name)?;
        let mut destination_file =
            platform::create_regular_file_at(&destination.file, &destination.path, &planned.path)?;

        let remaining_allowance = plan.logical_bytes.checked_sub(copied_bytes).ok_or(
            IndexError::PredecessorMigrationByteLimit {
                actual: copied_bytes,
                maximum: plan.logical_bytes,
            },
        )?;
        let (copied, source_digest) = copy_with_digest(
            &mut source_file,
            &mut destination_file,
            planned.identity.bytes,
            remaining_allowance,
        )?;
        destination_file.flush()?;
        destination_file.set_permissions(planned.permissions.clone())?;
        destination_file.sync_all()?;
        if copied != planned.identity.bytes {
            return Err(IndexError::PredecessorMigrationSourceTopology(
                "copy byte count does not match authenticated source",
            ));
        }
        copied_bytes = copied_bytes
            .checked_add(copied)
            .ok_or(IndexError::CountOverflow)?;
        if copied_bytes > MAX_MIGRATION_CLONE_BYTES || copied_bytes > plan.logical_bytes {
            return Err(IndexError::PredecessorMigrationByteLimit {
                actual: copied_bytes,
                maximum: plan.logical_bytes.min(MAX_MIGRATION_CLONE_BYTES),
            });
        }

        validate_open_and_named_file(source, planned, &source_file)?;
        let destination_opened = open_bound_file(destination, &planned.path)?;
        if destination_opened.identity.bytes != planned.identity.bytes
            || destination_opened.identity.permissions != planned.identity.permissions
        {
            return Err(IndexError::PredecessorMigrationSourceTopology(
                "copied file metadata does not match authenticated source",
            ));
        }
        let destination_digest = digest_exact_file(
            destination,
            &planned.path,
            &destination_opened.identity,
            destination_opened.file,
        )?;
        if destination_digest != source_digest {
            return Err(IndexError::ChecksumMismatch);
        }
        clone_checkpoint(PortableCloneStage::AfterCopy, &planned.path)?;
    }
    record_clone_metrics(copied_bytes, plan.files.len());
    Ok(())
}

fn copy_with_digest<R: Read, W: Write>(
    source: &mut R,
    destination: &mut W,
    expected_bytes: u64,
    aggregate_allowance: u64,
) -> Result<(u64, [u8; 32])> {
    if expected_bytes > aggregate_allowance {
        return Err(IndexError::PredecessorMigrationByteLimit {
            actual: expected_bytes,
            maximum: aggregate_allowance,
        });
    }
    let mut digest = Sha256::new();
    let mut copied = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    while copied < expected_bytes {
        let remaining = expected_bytes - copied;
        let read_limit = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| IndexError::CountOverflow)?;
        let read = source.read(&mut buffer[..read_limit])?;
        if read == 0 {
            return Err(IndexError::PredecessorMigrationSourceTopology(
                "source file truncated while cloning",
            ));
        }
        digest.update(&buffer[..read]);
        destination.write_all(&buffer[..read])?;
        copied = copied
            .checked_add(read as u64)
            .ok_or(IndexError::CountOverflow)?;
    }
    let mut growth_probe = [0_u8; 1];
    if source.read(&mut growth_probe)? != 0 {
        return Err(IndexError::PredecessorMigrationSourceTopology(
            "source file grew while cloning",
        ));
    }
    Ok((copied, digest.finalize().into()))
}

fn digest_exact_file(
    directory: &BoundDirectory,
    relative: &Path,
    expected: &FileIdentity,
    mut file: File,
) -> Result<[u8; 32]> {
    let mut digest = Sha256::new();
    let mut read_bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    while read_bytes < expected.bytes {
        let remaining = expected.bytes - read_bytes;
        let read_limit = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| IndexError::CountOverflow)?;
        let read = file.read(&mut buffer[..read_limit])?;
        if read == 0 {
            return Err(IndexError::PredecessorMigrationSourceTopology(
                "copied file truncated during verification",
            ));
        }
        digest.update(&buffer[..read]);
        read_bytes = read_bytes
            .checked_add(read as u64)
            .ok_or(IndexError::CountOverflow)?;
    }
    let mut growth_probe = [0_u8; 1];
    if file.read(&mut growth_probe)? != 0 {
        return Err(IndexError::PredecessorMigrationSourceTopology(
            "copied file grew during verification",
        ));
    }
    let actual = FileIdentity::from_file(&file)?;
    if &actual != expected {
        return Err(IndexError::PredecessorMigrationSourceTopology(
            "copied file changed during verification",
        ));
    }
    validate_named_file(directory, relative, expected)?;
    Ok(digest.finalize().into())
}

fn validate_open_and_named_file(
    directory: &BoundDirectory,
    planned: &PlannedFile,
    file: &File,
) -> Result<()> {
    if FileIdentity::from_file(file)? != planned.identity {
        return Err(IndexError::PredecessorMigrationSourceTopology(
            "source file changed while cloning",
        ));
    }
    validate_named_file(directory, &planned.path, &planned.identity)
}

fn validate_named_file(
    directory: &BoundDirectory,
    relative: &Path,
    expected: &FileIdentity,
) -> Result<()> {
    let named = platform::open_regular_file_at(&directory.file, &directory.path, relative)
        .map_err(source_topology_open_error)?;
    if FileIdentity::from_file(&named)? != *expected {
        return Err(IndexError::PredecessorMigrationSourceTopology(
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
        EntryKind::LinkOrReparse => Err(IndexError::PredecessorMigrationSourceTopology(
            "symlink, reparse point, or remote-provider file in migration source",
        )),
        EntryKind::Directory | EntryKind::Special => Err(
            IndexError::PredecessorMigrationSourceTopology("non-regular directory entry"),
        ),
    }
}

fn require_directory(kind: EntryKind) -> Result<()> {
    match kind {
        EntryKind::Directory => Ok(()),
        EntryKind::LinkOrReparse => Err(IndexError::PredecessorMigrationSourceTopology(
            "symlinked, reparse-point, or remote-provider migration directory",
        )),
        EntryKind::Regular | EntryKind::Special => Err(
            IndexError::PredecessorMigrationSourceTopology("migration path is not a directory"),
        ),
    }
}

fn source_topology_open_error(error: io::Error) -> IndexError {
    if platform::is_nofollow_rejection(&error) {
        IndexError::PredecessorMigrationSourceTopology(
            "symlink, reparse point, or remote-provider file in migration source",
        )
    } else {
        IndexError::Io(error)
    }
}

fn available_bytes(directory: &BoundDirectory) -> Result<u64> {
    #[cfg(test)]
    if let Some(available) = TEST_OPTIONS.with(|options| options.borrow().available_bytes) {
        return Ok(available);
    }
    platform::available_bytes(&directory.file, &directory.path).map_err(IndexError::Io)
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PortableCloneStage {
    BeforeCopy,
    AfterSourceOpen,
    AfterCopy,
    BeforeCleanup,
}

#[cfg(not(test))]
#[derive(Debug, Clone, Copy)]
enum PortableCloneStage {
    BeforeCopy,
    AfterSourceOpen,
    AfterCopy,
    BeforeCleanup,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PortableCloneTestOptions {
    pub(crate) available_bytes: Option<u64>,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PortableCloneMetrics {
    pub(crate) planned_files: usize,
    pub(crate) logical_bytes: u64,
    pub(crate) required_headroom: u64,
    pub(crate) available_bytes: u64,
    pub(crate) copied_bytes: u64,
    pub(crate) copied_files: usize,
}

#[cfg(test)]
type PortableCloneTestHook = Box<dyn for<'a> FnMut(PortableCloneStage, &'a Path) -> Result<()>>;

#[cfg(test)]
thread_local! {
    static FORCE_PORTABLE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static TEST_OPTIONS: std::cell::RefCell<PortableCloneTestOptions> = const {
        std::cell::RefCell::new(PortableCloneTestOptions { available_bytes: None })
    };
    static TEST_HOOK: std::cell::RefCell<Option<PortableCloneTestHook>> =
        std::cell::RefCell::new(None);
    static TEST_METRICS: std::cell::Cell<PortableCloneMetrics> = const {
        std::cell::Cell::new(PortableCloneMetrics {
            planned_files: 0,
            logical_bytes: 0,
            required_headroom: 0,
            available_bytes: 0,
            copied_bytes: 0,
            copied_files: 0,
        })
    };
}

#[cfg(test)]
pub(crate) struct PortableCloneTestGuard {
    previous_force: bool,
    previous_options: PortableCloneTestOptions,
    previous_hook: Option<PortableCloneTestHook>,
    previous_metrics: PortableCloneMetrics,
}

#[cfg(test)]
impl PortableCloneTestGuard {
    pub(crate) fn set<F>(options: PortableCloneTestOptions, hook: F) -> Self
    where
        F: for<'a> FnMut(PortableCloneStage, &'a Path) -> Result<()> + 'static,
    {
        let previous_force = FORCE_PORTABLE.with(|force| force.replace(true));
        let previous_options = TEST_OPTIONS.with(|slot| slot.replace(options));
        let previous_hook = TEST_HOOK.with(|slot| slot.replace(Some(Box::new(hook))));
        let previous_metrics =
            TEST_METRICS.with(|slot| slot.replace(PortableCloneMetrics::default()));
        Self {
            previous_force,
            previous_options,
            previous_hook,
            previous_metrics,
        }
    }

    pub(crate) fn metrics(&self) -> PortableCloneMetrics {
        TEST_METRICS.with(std::cell::Cell::get)
    }
}

#[cfg(test)]
impl Drop for PortableCloneTestGuard {
    fn drop(&mut self) {
        FORCE_PORTABLE.with(|slot| slot.set(self.previous_force));
        TEST_OPTIONS.with(|slot| slot.replace(self.previous_options));
        TEST_HOOK.with(|slot| slot.replace(self.previous_hook.take()));
        TEST_METRICS.with(|slot| slot.set(self.previous_metrics));
    }
}

#[cfg(test)]
pub(super) fn forced_for_test() -> bool {
    FORCE_PORTABLE.with(std::cell::Cell::get)
}

#[cfg(test)]
fn clone_checkpoint(stage: PortableCloneStage, path: &Path) -> Result<()> {
    TEST_HOOK.with(|hook| match hook.borrow_mut().as_mut() {
        Some(hook) => hook(stage, path),
        None => Ok(()),
    })
}

#[cfg(not(test))]
fn clone_checkpoint(_stage: PortableCloneStage, _path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
fn record_plan_metrics(plan: &ClonePlan, available: u64) {
    TEST_METRICS.with(|metrics| {
        metrics.set(PortableCloneMetrics {
            planned_files: plan.files.len(),
            logical_bytes: plan.logical_bytes,
            required_headroom: plan.required_headroom,
            available_bytes: available,
            ..metrics.get()
        });
    });
}

#[cfg(not(test))]
fn record_plan_metrics(_plan: &ClonePlan, _available: u64) {}

#[cfg(test)]
fn record_clone_metrics(copied_bytes: u64, copied_files: usize) {
    TEST_METRICS.with(|metrics| {
        metrics.set(PortableCloneMetrics {
            copied_bytes,
            copied_files,
            ..metrics.get()
        });
    });
}

#[cfg(not(test))]
fn record_clone_metrics(_copied_bytes: u64, _copied_files: usize) {}

#[cfg(unix)]
#[path = "portable/unix.rs"]
mod platform;

#[cfg(windows)]
#[path = "portable/windows.rs"]
mod platform;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_entry_contract_rejects_links_directories_and_special_files() {
        assert!(require_regular(EntryKind::Regular).is_ok());
        for kind in [
            EntryKind::Directory,
            EntryKind::LinkOrReparse,
            EntryKind::Special,
        ] {
            assert!(matches!(
                require_regular(kind),
                Err(IndexError::PredecessorMigrationSourceTopology(_))
            ));
        }
    }

    #[test]
    fn portable_authenticated_growth_probe_never_writes_the_extra_byte() {
        let mut source = io::Cursor::new(b"abcde".to_vec());
        let mut destination = Vec::new();
        assert!(matches!(
            copy_with_digest(&mut source, &mut destination, 4, 4),
            Err(IndexError::PredecessorMigrationSourceTopology(
                "source file grew while cloning"
            ))
        ));
        assert_eq!(destination, b"abcd");

        let mut source = io::Cursor::new(b"abcde".to_vec());
        let mut destination = Vec::new();
        assert!(matches!(
            copy_with_digest(&mut source, &mut destination, 5, 4),
            Err(IndexError::PredecessorMigrationByteLimit {
                actual: 5,
                maximum: 4
            })
        ));
        assert!(destination.is_empty());
    }
}
