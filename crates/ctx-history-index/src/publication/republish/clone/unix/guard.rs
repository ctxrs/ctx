use std::path::PathBuf;

use crate::Result;

use super::{
    clone_checkpoint, discard_bound_directory, validate_child_binding, validate_path_binding,
    BoundDirectory, CloneStage,
};

pub(in crate::publication::republish) struct CandidateGuard {
    pub(super) root_path: PathBuf,
    pub(super) root: BoundDirectory,
    pub(super) generations_name: PathBuf,
    pub(super) generations_path: PathBuf,
    pub(super) generations: BoundDirectory,
    pub(super) destination_name: PathBuf,
    pub(super) destination: BoundDirectory,
}

impl CandidateGuard {
    pub(in crate::publication::republish) fn validate_binding(&self) -> Result<()> {
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

    pub(in crate::publication::republish) fn discard(self) {
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
