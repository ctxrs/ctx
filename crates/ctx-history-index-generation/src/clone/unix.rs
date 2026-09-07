mod guard;

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{CStr, CString, OsString},
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::{ffi::OsStringExt, fs::MetadataExt},
    },
    path::{Path, PathBuf},
};

use tantivy::Index;
use uuid::Uuid;

use super::exact_copy::copy_and_hash_exact_authenticated_file;
use super::{
    admit_clone_resource, record_candidate_clone_metrics, validate_single_component,
    CandidateActivationFence, CandidateCloneMetrics, MANAGED_FILE, MAX_REPUBLISH_CLONE_BYTES,
    MAX_REPUBLISH_CLONE_FILES, MAX_REPUBLISH_DIRECTORY_ENTRIES, REPUBLISH_HEADROOM_RESERVE_BYTES,
    TANTIVY_LOCK_FILES,
};
use crate::{
    active_index_files,
    certification::{
        capture_artifact_identity, open_authenticated_artifact, recapture_authenticated_artifact,
    },
    lexical_index_settings,
    physical::{
        canonical_active_managed_bytes, managed_file_topology, PhysicalFileDigest,
        MAX_MANAGED_METADATA_BYTES,
    },
    verify_or_certify_physical_integrity, ActiveGenerationPointer, CandidateGeneration,
    CandidatePhysicalProof, CertifiedPhysicalIntegrity, DurableMmapDirectory,
    GenerationError as IndexError, Result, INDEX_GENERATIONS_DIRECTORY,
};
pub(super) use guard::CandidateGuard;

pub(super) fn candidate_available_bytes(root: &Path) -> Result<u64> {
    let directory = BoundDirectory::open_path(root)?;
    available_bytes(&directory.file, false)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    bytes: u64,
    mode: u64,
}

#[cfg(target_os = "linux")]
const fn normalized_stat_device(device: libc::dev_t) -> u64 {
    device
}

#[cfg(target_os = "macos")]
const fn normalized_stat_device(device: libc::dev_t) -> u64 {
    device as u64
}

impl FileIdentity {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            bytes: metadata.len(),
            mode: u64::from(metadata.mode()),
        }
    }

    fn from_stat(stat: &libc::stat) -> Self {
        Self {
            device: normalized_stat_device(stat.st_dev),
            inode: stat.st_ino,
            bytes: u64::try_from(stat.st_size).unwrap_or(u64::MAX),
            mode: u64::from(stat.st_mode),
        }
    }

    fn is_regular(self) -> bool {
        self.mode & u64::from(libc::S_IFMT) == u64::from(libc::S_IFREG)
    }

    fn is_directory(self) -> bool {
        self.mode & u64::from(libc::S_IFMT) == u64::from(libc::S_IFDIR)
    }

    fn is_same_object(self, other: Self) -> bool {
        self.device == other.device
            && self.inode == other.inode
            && (self.mode & u64::from(libc::S_IFMT)) == (other.mode & u64::from(libc::S_IFMT))
    }
}

#[cfg(all(test, target_os = "macos"))]
#[test]
fn signed_darwin_device_id_normalization_preserves_distinct_values() {
    assert_ne!(normalized_stat_device(-1), normalized_stat_device(-2));
}

#[derive(Debug, Clone)]
struct PlannedFile {
    path: PathBuf,
    identity: FileIdentity,
    copy_required: bool,
}

struct ClonePlan {
    files: Vec<PlannedFile>,
    logical_bytes: u64,
    control_copy_bytes: u64,
    managed_bytes: Vec<u8>,
}

impl ClonePlan {
    fn writer_output_headroom(&self, writer_memory_bytes: u64) -> Result<u64> {
        self.logical_bytes
            .checked_add(writer_memory_bytes)
            .and_then(|bytes| bytes.checked_add(REPUBLISH_HEADROOM_RESERVE_BYTES))
            .ok_or(IndexError::CountOverflow)
    }

    fn initial_candidate_headroom(&self, writer_memory_bytes: u64) -> Result<u64> {
        self.control_copy_bytes
            .checked_add(self.writer_output_headroom(writer_memory_bytes)?)
            .ok_or(IndexError::CountOverflow)
    }

    fn full_copy_candidate_headroom(&self, writer_memory_bytes: u64) -> Result<u64> {
        self.logical_bytes
            .checked_add(self.writer_output_headroom(writer_memory_bytes)?)
            .ok_or(IndexError::CountOverflow)
    }
}

struct BoundDirectory {
    file: File,
    identity: FileIdentity,
}

impl BoundDirectory {
    fn open_path(path: &Path) -> Result<Self> {
        let file = open_path_nofollow(path, libc::O_RDONLY | libc::O_DIRECTORY)
            .map_err(source_topology_open_error)?;
        Self::from_file(file)
    }

    fn open_at(parent: &File, name: &Path) -> Result<Self> {
        let file = open_at_nofollow(parent.as_raw_fd(), name, libc::O_RDONLY | libc::O_DIRECTORY)
            .map_err(source_topology_open_error)?;
        Self::from_file(file)
    }

    fn from_file(file: File) -> Result<Self> {
        let identity = FileIdentity::from_metadata(&file.metadata()?);
        if !identity.is_directory() {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "generation path is not a directory",
            ));
        }
        Ok(Self { file, identity })
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
    let root_path = root.to_path_buf();
    let root_directory = BoundDirectory::open_path(root)?;
    validate_path_binding(root, root_directory.identity)?;
    let generations_name = PathBuf::from(INDEX_GENERATIONS_DIRECTORY);
    let generations_path = root.join(INDEX_GENERATIONS_DIRECTORY);
    let generations = BoundDirectory::open_at(&root_directory.file, &generations_name)?;
    validate_child_binding(
        &root_directory.file,
        &generations_name,
        generations.identity,
    )?;
    validate_path_binding(&generations_path, generations.identity)?;
    let source_name = Path::new(base.directory());
    validate_single_component(source_name)?;
    let source_path = generations_path.join(source_name);
    let source = BoundDirectory::open_at(&generations.file, source_name)?;
    validate_child_binding(&generations.file, source_name, source.identity)?;

    let plan = authenticated_clone_plan(&source, predecessor_index)?;
    let required_headroom = plan.initial_candidate_headroom(writer_memory_bytes)?;
    let full_copy_headroom = plan.full_copy_candidate_headroom(writer_memory_bytes)?;
    let writer_output_headroom = plan.writer_output_headroom(writer_memory_bytes)?;
    let available = available_bytes(&generations.file, false)?;
    record_plan_metrics_with_required(&plan, available, full_copy_headroom);
    if available < required_headroom {
        return Err(IndexError::CurrentRepublishInsufficientHeadroom {
            available,
            required: required_headroom,
        });
    }

    let directory_name = format!("generation-{}", Uuid::now_v7().simple());
    let destination_name = PathBuf::from(&directory_name);
    create_directory_at(&generations.file, &destination_name)?;
    let destination_path = generations_path.join(&directory_name);
    let destination = BoundDirectory::open_at(&generations.file, &destination_name)?;
    validate_child_binding(&generations.file, &destination_name, destination.identity)?;
    let guard = CandidateGuard {
        root_path,
        root: root_directory,
        generations_name,
        generations_path,
        generations,
        destination_name,
        destination,
    };
    let clone_result = (|| {
        let mut physical_proof = CandidatePhysicalProof::default();
        let mut metrics = CandidateCloneMetrics::default();
        clone_candidate_files(
            root,
            &source_path,
            &destination_path,
            predecessor_pointer,
            &certified,
            &guard.generations,
            source_name,
            &source,
            &guard.destination,
            &plan,
            writer_output_headroom,
            &mut physical_proof,
            &mut metrics,
        )?;
        guard.generations.file.sync_all()?;
        validate_child_binding(&guard.generations.file, source_name, source.identity)?;
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
            activation_fence: CandidateActivationFence::descriptor_clone(guard),
        }),
        Err(error) => {
            guard.discard();
            Err(error)
        }
    }
}

fn authenticated_clone_plan(source: &BoundDirectory, index: &Index) -> Result<ClonePlan> {
    let mut active = active_index_files(index)?;
    active.insert(PathBuf::from("meta.json"));
    for path in &active {
        validate_single_component(path)?;
    }

    let mut observed = BTreeMap::new();
    for name in directory_entries(&source.file, MAX_REPUBLISH_DIRECTORY_ENTRIES)? {
        if name.to_str().is_none() {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "non-UTF-8 directory entry",
            ));
        }
        let relative = PathBuf::from(&name);
        validate_single_component(&relative)?;
        let file = open_regular_file_at(&source.file, &relative)?;
        let identity = FileIdentity::from_metadata(&file.metadata()?);
        validate_file_binding(&source.file, &relative, identity)?;
        let copy_required =
            relative == Path::new("meta.json") || relative == Path::new(MANAGED_FILE);
        if observed
            .insert(
                relative.clone(),
                PlannedFile {
                    path: relative,
                    identity,
                    copy_required,
                },
            )
            .is_some()
        {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "duplicate directory entry",
            ));
        }
    }
    let managed =
        observed
            .get(Path::new(MANAGED_FILE))
            .ok_or(IndexError::CurrentRepublishSourceTopology(
                "managed file missing",
            ))?;
    if managed.identity.bytes > MAX_MANAGED_METADATA_BYTES {
        return Err(IndexError::CurrentRepublishByteLimit {
            actual: managed.identity.bytes,
            maximum: MAX_MANAGED_METADATA_BYTES,
        });
    }
    let managed_bytes = read_bound_file(source, managed, MAX_MANAGED_METADATA_BYTES)?;
    let managed_paths: Vec<PathBuf> = serde_json::from_slice(&managed_bytes)
        .map_err(|_| IndexError::CurrentRepublishSourceTopology("invalid managed metadata"))?;
    for path in &managed_paths {
        validate_single_component(path)?;
    }
    let topology = managed_file_topology(&managed_paths, &active).ok_or(
        IndexError::CurrentRepublishSourceTopology(
            "managed metadata is not a safe active superset",
        ),
    )?;
    let managed_bytes = if topology.retired().is_empty() {
        managed_bytes
    } else {
        canonical_active_managed_bytes(&active)?
    };

    let mut seen_active = BTreeSet::new();
    let mut planned = BTreeMap::new();
    let mut admitted_files = 0_usize;
    let mut admitted_bytes = 0_u64;
    let mut total_files = 0_usize;
    let mut total_bytes = 0_u64;
    for (relative, file) in observed {
        let name_text = relative
            .to_str()
            .ok_or(IndexError::CurrentRepublishSourceTopology(
                "non-UTF-8 directory entry",
            ))?;
        let clone_file = active.contains(&relative) || relative == Path::new(MANAGED_FILE);
        if clone_file || topology.retired().contains(&relative) {
            admit_clone_resource(
                &mut admitted_files,
                &mut admitted_bytes,
                file.identity.bytes,
                MAX_REPUBLISH_CLONE_FILES,
                MAX_REPUBLISH_CLONE_BYTES,
            )?;
        }
        if clone_file {
            if active.contains(&relative) {
                seen_active.insert(relative.clone());
            }
            admit_clone_resource(
                &mut total_files,
                &mut total_bytes,
                file.identity.bytes,
                MAX_REPUBLISH_CLONE_FILES,
                MAX_REPUBLISH_CLONE_BYTES,
            )?;
            planned.insert(relative, file);
        } else if topology.retired().contains(&relative)
            || (TANTIVY_LOCK_FILES.contains(&name_text) && file.identity.bytes == 0)
        {
            continue;
        } else {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "unexpected directory entry",
            ));
        }
    }
    if seen_active != active {
        return Err(IndexError::CurrentRepublishSourceTopology(
            "active or managed file missing",
        ));
    }

    let control_copy_bytes = planned
        .values()
        .filter(|file| file.copy_required)
        .try_fold(0_u64, |bytes, file| {
            bytes
                .checked_add(file.identity.bytes)
                .ok_or(IndexError::CountOverflow)
        })?;
    Ok(ClonePlan {
        files: planned.into_values().collect(),
        logical_bytes: total_bytes,
        control_copy_bytes,
        managed_bytes,
    })
}

fn read_bound_file(
    directory: &BoundDirectory,
    planned: &PlannedFile,
    maximum: u64,
) -> Result<Vec<u8>> {
    let mut file = open_regular_file_at(&directory.file, &planned.path)?;
    let before = FileIdentity::from_metadata(&file.metadata()?);
    if before != planned.identity {
        return Err(IndexError::CurrentRepublishSourceTopology(
            "source file changed after authentication",
        ));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != planned.identity.bytes {
        return Err(IndexError::CurrentRepublishSourceTopology(
            "source file size changed while reading",
        ));
    }
    validate_file_binding(&directory.file, &planned.path, planned.identity)?;
    Ok(bytes)
}

#[allow(clippy::too_many_arguments)]
fn clone_candidate_files(
    root: &Path,
    source_path: &Path,
    destination_path: &Path,
    predecessor_pointer: &ActiveGenerationPointer,
    certified: &CertifiedPhysicalIntegrity,
    generations: &BoundDirectory,
    source_name: &Path,
    source: &BoundDirectory,
    destination: &BoundDirectory,
    plan: &ClonePlan,
    writer_output_headroom: u64,
    physical_proof: &mut CandidatePhysicalProof,
    metrics: &mut CandidateCloneMetrics,
) -> Result<()> {
    let mut actual_copied_bytes = 0_u64;
    for planned in &plan.files {
        validate_child_binding(&generations.file, source_name, source.identity)?;
        clone_checkpoint(CloneStage::BeforeFile, &planned.path)?;
        validate_child_binding(&generations.file, source_name, source.identity)?;
        if planned.path == Path::new(MANAGED_FILE) {
            clone_checkpoint(CloneStage::BeforeCopy, &planned.path)?;
            let copied = write_authenticated_plan_bytes(
                &destination.file,
                &planned.path,
                &plan.managed_bytes,
            )?;
            actual_copied_bytes = actual_copied_bytes
                .checked_add(copied)
                .ok_or(IndexError::CountOverflow)?;
            if actual_copied_bytes > MAX_REPUBLISH_CLONE_BYTES
                || actual_copied_bytes > plan.logical_bytes
            {
                return Err(IndexError::CurrentRepublishByteLimit {
                    actual: actual_copied_bytes,
                    maximum: plan.logical_bytes.min(MAX_REPUBLISH_CLONE_BYTES),
                });
            }
            validate_child_binding(&generations.file, source_name, source.identity)?;
            clone_checkpoint(CloneStage::AfterFile, &planned.path)?;
            continue;
        }

        let (expected_artifact, expected_sha256, sealed) = certified
            .certified_artifact(&planned.path)
            .ok_or(IndexError::ChecksumMismatch)?;
        let (mut source_file, source_before) = open_authenticated_artifact(
            root,
            source_path,
            &planned.path,
            Some(predecessor_pointer),
        )?;
        if source_before != expected_artifact {
            return if expected_artifact.same_payload_identity_changed(&source_before) {
                Err(IndexError::ConcurrentGenerationChange)
            } else {
                Err(IndexError::ChecksumMismatch)
            };
        }
        clone_checkpoint(CloneStage::AfterSourceOpen, &planned.path)?;
        if sealed
            && !planned.copy_required
            && !force_reflink_fallback()
            && try_clone_reflink_at(&source_file, &destination.file, &planned.path)?
        {
            metrics.retained_reflinked_files = metrics
                .retained_reflinked_files
                .checked_add(1)
                .ok_or(IndexError::CountOverflow)?;
            let source_after = recapture_authenticated_artifact(
                root,
                source_path,
                &planned.path,
                &source_file,
                Some(predecessor_pointer),
            )?;
            if source_after != expected_artifact {
                return if expected_artifact.same_payload_identity_changed(&source_after) {
                    Err(IndexError::ConcurrentGenerationChange)
                } else {
                    Err(IndexError::ChecksumMismatch)
                };
            }
            let destination_artifact =
                capture_artifact_identity(root, destination_path, &planned.path, None)?;
            physical_proof.insert(PhysicalFileDigest {
                artifact: destination_artifact,
                sha256: expected_sha256,
            });
            validate_child_binding(&generations.file, source_name, source.identity)?;
            clone_checkpoint(CloneStage::AfterFile, &planned.path)?;
            continue;
        }

        clone_checkpoint(CloneStage::BeforeHardlink, &planned.path)?;
        let source_prelink = recapture_authenticated_artifact(
            root,
            source_path,
            &planned.path,
            &source_file,
            Some(predecessor_pointer),
        )?;
        if source_prelink != expected_artifact {
            return if expected_artifact.same_payload_identity_changed(&source_prelink) {
                Err(IndexError::ConcurrentGenerationChange)
            } else {
                Err(IndexError::ChecksumMismatch)
            };
        }
        let before = FileIdentity::from_metadata(&source_file.metadata()?);
        let linked = sealed
            && !planned.copy_required
            && !force_hardlink_fallback()
            && !force_copy_fallback()
            && match hard_link_authenticated_source(&source.file, &planned.path, &destination.file)
            {
                Ok(()) => true,
                Err(error) if hardlink_copy_fallback_error(&error) => false,
                Err(error) => return Err(error.into()),
            };
        if linked {
            let linked_file = open_regular_file_at(&destination.file, &planned.path)?;
            let linked_identity = FileIdentity::from_metadata(&linked_file.metadata()?);
            if linked_identity != before {
                return Err(IndexError::CurrentRepublishSourceTopology(
                    "hardlink target identity does not match authenticated source",
                ));
            }
            validate_file_binding(&source.file, &planned.path, before)?;
            let destination_artifact =
                capture_artifact_identity(root, destination_path, &planned.path, None)?;
            physical_proof.insert(PhysicalFileDigest {
                artifact: destination_artifact,
                sha256: expected_sha256,
            });
            metrics.retained_hardlinked_files = metrics
                .retained_hardlinked_files
                .checked_add(1)
                .ok_or(IndexError::CountOverflow)?;
            validate_child_binding(&generations.file, source_name, source.identity)?;
            clone_checkpoint(CloneStage::AfterFile, &planned.path)?;
            continue;
        }

        let admitted_copy_bytes = if planned.copy_required {
            source_before.identity.length()
        } else {
            plan.logical_bytes
                .checked_sub(actual_copied_bytes)
                .ok_or(IndexError::CountOverflow)?
        };
        let required = admitted_copy_bytes
            .checked_add(writer_output_headroom)
            .ok_or(IndexError::CountOverflow)?;
        admit_available_bytes(&generations.file, required, true)?;
        clone_checkpoint(CloneStage::BeforeCopy, &planned.path)?;
        source_file.seek(SeekFrom::Start(0))?;
        let remaining_allowance = plan.logical_bytes.checked_sub(actual_copied_bytes).ok_or(
            IndexError::CurrentRepublishByteLimit {
                actual: actual_copied_bytes,
                maximum: plan.logical_bytes,
            },
        )?;
        let mut destination_file = create_regular_file_at(&destination.file, &planned.path)?;
        let (copied, copied_sha256) = copy_and_hash_exact_authenticated_file(
            &mut source_file,
            &mut destination_file,
            source_before.identity.length(),
            remaining_allowance,
        )?;
        if copied_sha256 != expected_sha256 {
            return Err(IndexError::ChecksumMismatch);
        }
        destination_file.flush()?;
        let destination_identity = FileIdentity::from_metadata(&destination_file.metadata()?);
        if copied != source_before.identity.length()
            || destination_identity.bytes != source_before.identity.length()
        {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "copy byte count does not match authenticated source",
            ));
        }
        actual_copied_bytes = actual_copied_bytes
            .checked_add(copied)
            .ok_or(IndexError::CountOverflow)?;
        if actual_copied_bytes > MAX_REPUBLISH_CLONE_BYTES
            || actual_copied_bytes > plan.logical_bytes
        {
            return Err(IndexError::CurrentRepublishByteLimit {
                actual: actual_copied_bytes,
                maximum: plan.logical_bytes.min(MAX_REPUBLISH_CLONE_BYTES),
            });
        }
        let source_after = recapture_authenticated_artifact(
            root,
            source_path,
            &planned.path,
            &source_file,
            Some(predecessor_pointer),
        )?;
        if source_after != expected_artifact {
            return if expected_artifact.same_payload_identity_changed(&source_after) {
                Err(IndexError::ConcurrentGenerationChange)
            } else {
                Err(IndexError::ChecksumMismatch)
            };
        }
        let destination_artifact =
            capture_artifact_identity(root, destination_path, &planned.path, None)?;
        physical_proof.insert(PhysicalFileDigest {
            artifact: destination_artifact,
            sha256: expected_sha256,
        });
        if !planned.copy_required {
            metrics.retained_copied_files = metrics
                .retained_copied_files
                .checked_add(1)
                .ok_or(IndexError::CountOverflow)?;
            metrics.retained_copied_bytes = metrics
                .retained_copied_bytes
                .checked_add(copied)
                .ok_or(IndexError::CountOverflow)?;
        }
        validate_child_binding(&generations.file, source_name, source.identity)?;
        clone_checkpoint(CloneStage::AfterFile, &planned.path)?;
    }
    admit_available_bytes(&generations.file, writer_output_headroom, true)?;
    Ok(())
}

mod fs_ops;
use fs_ops::*;
mod support;

#[cfg(not(any(test, feature = "test-support")))]
use support::CloneStage;
use support::{
    clone_checkpoint, force_copy_fallback, force_hardlink_fallback, force_reflink_fallback,
    record_plan_metrics_with_required,
};
#[cfg(any(test, feature = "test-support"))]
pub use support::{CloneMetrics, CloneStage, CloneTestHookGuard, CloneTestOptions};
