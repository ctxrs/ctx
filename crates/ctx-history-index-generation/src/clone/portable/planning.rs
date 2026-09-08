//! Immutable source enumeration and certification for portable clone transfer.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, Permissions},
    io::Read,
    path::{Path, PathBuf},
};

use tantivy::Index;

use super::{open_bound_file, validate_named_file, BoundDirectory, FileIdentity};
use crate::{
    active_index_files,
    clone::{
        admit_clone_resource, MANAGED_FILE, MAX_REPUBLISH_CLONE_BYTES, MAX_REPUBLISH_CLONE_FILES,
        MAX_REPUBLISH_DIRECTORY_ENTRIES, REPUBLISH_HEADROOM_RESERVE_BYTES, TANTIVY_LOCK_FILES,
    },
    physical::{canonical_active_managed_bytes, managed_file_topology, MAX_MANAGED_METADATA_BYTES},
    GenerationError as IndexError, Result,
};

#[derive(Debug, Clone)]
pub(super) struct PlannedFile {
    path: PathBuf,
    identity: FileIdentity,
    permissions: Permissions,
}

impl PlannedFile {
    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn identity(&self) -> &FileIdentity {
        &self.identity
    }

    pub(super) fn permissions(&self) -> &Permissions {
        &self.permissions
    }

    pub(super) fn open(&self, directory: &BoundDirectory) -> Result<File> {
        let opened = open_bound_file(directory, &self.path)?;
        if opened.identity != self.identity {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "source file changed after authentication",
            ));
        }
        Ok(opened.file)
    }

    pub(super) fn validate_open_and_named(
        &self,
        directory: &BoundDirectory,
        file: &File,
    ) -> Result<()> {
        if FileIdentity::from_file(file)? != self.identity {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "source file changed while cloning",
            ));
        }
        validate_named_file(directory, &self.path, &self.identity)
    }
}

pub(super) struct ValidatedClonePlan {
    files: Vec<PlannedFile>,
    logical_bytes: u64,
    managed_bytes: Vec<u8>,
}

impl ValidatedClonePlan {
    pub(super) fn files(&self) -> &[PlannedFile] {
        &self.files
    }

    pub(super) fn logical_bytes(&self) -> u64 {
        self.logical_bytes
    }

    pub(super) fn managed_bytes(&self) -> &[u8] {
        &self.managed_bytes
    }

    pub(super) fn writer_output_headroom(&self, writer_memory_bytes: u64) -> Result<u64> {
        self.logical_bytes
            .checked_add(writer_memory_bytes)
            .and_then(|bytes| bytes.checked_add(REPUBLISH_HEADROOM_RESERVE_BYTES))
            .ok_or(IndexError::CountOverflow)
    }

    pub(super) fn full_copy_candidate_headroom(&self, writer_memory_bytes: u64) -> Result<u64> {
        self.logical_bytes
            .checked_add(self.writer_output_headroom(writer_memory_bytes)?)
            .ok_or(IndexError::CountOverflow)
    }
}

pub(super) fn authenticated_clone_plan(
    generations: &BoundDirectory,
    source_name: &Path,
    source: &BoundDirectory,
    index: &Index,
) -> Result<ValidatedClonePlan> {
    let mut active = active_index_files(index)?;
    active.insert(PathBuf::from("meta.json"));
    for path in &active {
        crate::clone::validate_single_component(path)?;
    }

    let mut observed = BTreeMap::new();
    source.validate_child_binding(generations, source_name)?;
    for name in super::platform::directory_entries(
        &source.file,
        &source.path,
        MAX_REPUBLISH_DIRECTORY_ENTRIES,
    )? {
        if name.to_str().is_none() {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "non-UTF-8 directory entry",
            ));
        }
        let relative = PathBuf::from(&name);
        crate::clone::validate_single_component(&relative)?;
        let opened = open_bound_file(source, &relative)?;
        if observed
            .insert(
                relative.clone(),
                PlannedFile {
                    path: relative,
                    identity: opened.identity,
                    permissions: opened.permissions,
                },
            )
            .is_some()
        {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "duplicate directory entry",
            ));
        }
    }
    source.validate_child_binding(generations, source_name)?;
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
    let managed_bytes = read_planned_file(source, managed, MAX_MANAGED_METADATA_BYTES)?;
    let managed_paths: Vec<PathBuf> = serde_json::from_slice(&managed_bytes)
        .map_err(|_| IndexError::CurrentRepublishSourceTopology("invalid managed metadata"))?;
    for path in &managed_paths {
        crate::clone::validate_single_component(path)?;
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

    Ok(ValidatedClonePlan {
        files: planned.into_values().collect(),
        logical_bytes: total_bytes,
        managed_bytes,
    })
}

fn read_planned_file(
    directory: &BoundDirectory,
    planned: &PlannedFile,
    maximum: u64,
) -> Result<Vec<u8>> {
    if planned.identity.bytes > maximum {
        return Err(IndexError::CurrentRepublishByteLimit {
            actual: planned.identity.bytes,
            maximum,
        });
    }
    let mut file = planned.open(directory)?;
    let allocation = usize::try_from(planned.identity.bytes).map_err(|_| {
        IndexError::CurrentRepublishByteLimit {
            actual: planned.identity.bytes,
            maximum,
        }
    })?;
    let mut bytes = Vec::with_capacity(allocation);
    Read::by_ref(&mut file)
        .take(planned.identity.bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != planned.identity.bytes {
        return Err(IndexError::CurrentRepublishSourceTopology(
            "source file size changed while reading",
        ));
    }
    planned.validate_open_and_named(directory, &file)?;
    Ok(bytes)
}
