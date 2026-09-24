use std::path::PathBuf;

use crate::{
    retention::{
        acquire_candidate_generation_directory_read_authority,
        ExistingGenerationDirectoryReadAuthority,
    },
    Result,
};

use super::{
    clone_checkpoint, discard_bound_directory, validate_child_binding, validate_path_binding,
    validate_single_component, BoundDirectory, CloneStage,
};
use crate::INDEX_GENERATIONS_DIRECTORY;

pub(in crate::clone) struct CandidateGuard {
    pub(super) _alias_authority: ExistingGenerationDirectoryReadAuthority,
    pub(super) root_path: PathBuf,
    pub(super) root: BoundDirectory,
    pub(super) generations_name: PathBuf,
    pub(super) generations_path: PathBuf,
    pub(super) generations: BoundDirectory,
    pub(super) destination_name: PathBuf,
    pub(super) destination: BoundDirectory,
}

impl CandidateGuard {
    pub(in crate::clone) fn bind(
        root_path: &std::path::Path,
        destination_name: &std::path::Path,
    ) -> Result<Self> {
        validate_single_component(destination_name)?;
        let root = BoundDirectory::open_path(root_path)?;
        validate_path_binding(root_path, root.identity)?;
        let generations_name = PathBuf::from(INDEX_GENERATIONS_DIRECTORY);
        let generations_path = root_path.join(&generations_name);
        let generations = BoundDirectory::open_at(&root.file, &generations_name)?;
        validate_child_binding(&root.file, &generations_name, generations.identity)?;
        validate_path_binding(&generations_path, generations.identity)?;
        let destination = BoundDirectory::open_at(&generations.file, destination_name)?;
        validate_child_binding(&generations.file, destination_name, destination.identity)?;
        let _alias_authority = acquire_candidate_generation_directory_read_authority(
            root_path,
            destination_name
                .to_str()
                .ok_or(crate::GenerationError::InvalidActiveGenerationPointer)?,
        )?;
        Ok(Self {
            _alias_authority,
            root_path: root_path.to_path_buf(),
            root,
            generations_name,
            generations_path,
            generations,
            destination_name: destination_name.to_path_buf(),
            destination,
        })
    }

    pub(in crate::clone) fn validate_binding(&self) -> Result<()> {
        validate_path_binding(&self.root_path, self.root.identity)?;
        validate_child_binding(
            &self.root.file,
            &self.generations_name,
            self.generations.identity,
        )?;
        validate_path_binding(&self.generations_path, self.generations.identity)?;
        validate_child_binding(
            &self.generations.file,
            &self.destination_name,
            self.destination.identity,
        )
    }

    pub(in crate::clone) fn discard(self) {
        if clone_checkpoint(CloneStage::BeforeCleanup, &self.destination_name).is_err()
            || self.validate_binding().is_err()
        {
            return;
        }
        if discard_bound_directory(&self.generations, &self.destination_name, &self.destination)
            .is_ok()
        {
            let _ = self.generations.file.sync_all();
        }
    }
}
