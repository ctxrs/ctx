use super::*;

impl SegmentMaterializer {
    pub fn open(root: impl Into<std::path::PathBuf>) -> Result<Self, SegmentMaterializerError> {
        Self::open_inner(root, None, None)
    }

    pub fn open_for_revision(
        root: impl Into<std::path::PathBuf>,

        materializer_revision: &str,
    ) -> Result<Self, SegmentMaterializerError> {
        Self::open_inner(root, Some(materializer_revision), None)
    }

    pub(crate) fn open_cancellable(
        root: impl Into<std::path::PathBuf>,
        materializer_revision: &str,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<Self, SegmentMaterializerError> {
        Self::open_inner(root, Some(materializer_revision), Some(cancelled))
    }

    fn open_inner(
        root: impl Into<std::path::PathBuf>,

        expected_materializer_revision: Option<&str>,
        cancelled: Option<&(dyn Fn() -> bool + Sync)>,
    ) -> Result<Self, SegmentMaterializerError> {
        let root = root.into();
        super::super::storage::prepare_root(&root)?;
        let mut store = Self {
            root,

            expected_materializer_revision: expected_materializer_revision.map(str::to_owned),
            active: None,
            force_next_rebuild: false,
            metrics: Default::default(),
            writer_lease: None,
            rollback_cleanup_failed: false,
        };
        let lock = match cancelled {
            Some(cancelled) => {
                super::super::locking::OperationLock::acquire_cancellable(&store.root, cancelled)?
            }
            None => super::super::locking::OperationLock::acquire(&store.root)?,
        };
        store.refresh_locked(expected_materializer_revision)?;
        lock.verify_identity()?;
        store.writer_lease = Some(lock);
        Ok(store)
    }

    /// Returns whether the active generation uses the exact serving schema.
    /// Checked predecessor controls remain active materializer authority,
    /// but they are unavailable to strict query readers until a clean current
    /// generation is published.
    pub fn has_queryable_active(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| !active.requires_clean_rebuild)
    }

    pub fn flat_store(&self) -> crate::graph::segment::FlatStore<'_> {
        crate::graph::segment::FlatStore::new(&self.root)
    }

    /// Reads projection status without creating, locking, repairing, cleaning,
    /// publishing, or collecting any graph-store path.
    pub(crate) fn read_only_projection_status(
        root: impl AsRef<std::path::Path>,

        request: &StatusRequest,
    ) -> Result<CoreProjectionStatus, SegmentMaterializerError> {
        validate_status_request(request)?;
        let root = root.as_ref();
        match std::fs::symlink_metadata(root) {
            Ok(_) => super::super::locking::verify_private_root(root)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return projection_status(
                    &super::super::model::initial_completed_control(),
                    false,
                    false,
                    request,
                );
            }
            Err(source) => {
                return Err(SegmentMaterializerError::Io {
                    operation: "stat read-only materializer root",
                    path: root.to_owned(),
                    source,
                });
            }
        }

        // A publication can replace the active manifest between reads. Retry
        // until the checked active generation is stable.
        for _attempt in 0..4 {
            let before = super::super::publication::load_active_control(root)?;
            let after = super::super::publication::load_active_control(root)?;
            let before_generation = before
                .as_ref()
                .map(|active| active.manifest_generation.as_str());
            let after_generation = after
                .as_ref()
                .map(|active| active.manifest_generation.as_str());
            if before_generation != after_generation {
                continue;
            }

            let active_control = after
                .as_ref()
                .map(|active| active.completed.clone())
                .unwrap_or_else(super::super::model::initial_completed_control);
            return projection_status(
                &active_control,
                false,
                after
                    .as_ref()
                    .is_some_and(|active| active.requires_clean_rebuild),
                request,
            );
        }
        Err(SegmentMaterializerError::Busy)
    }

    pub(super) fn acquire_writer_lease(&mut self) -> Result<(), SegmentMaterializerError> {
        if self.writer_lease.is_some() {
            self.verify_writer_lease()?;
            return Ok(());
        }
        let lock = super::super::locking::OperationLock::acquire(&self.root)?;
        let expected_revision = self.expected_materializer_revision.clone();
        self.refresh_locked(expected_revision.as_deref())?;
        lock.verify_identity()?;
        self.writer_lease = Some(lock);
        Ok(())
    }

    pub(super) fn verify_writer_lease(&self) -> Result<(), SegmentMaterializerError> {
        self.writer_lease
            .as_ref()
            .ok_or(SegmentMaterializerError::Corrupt(
                "writer operation has no materializer lease",
            ))?
            .verify_identity()
    }

    #[cfg(test)]
    pub(crate) fn projection_status(
        &mut self,
        request: &StatusRequest,
    ) -> Result<CoreProjectionStatus, SegmentMaterializerError> {
        projection_status(
            &self
                .active
                .as_ref()
                .map(|active| active.completed.clone())
                .unwrap_or_else(super::super::model::initial_completed_control),
            false,
            self.force_next_rebuild
                || self
                    .active
                    .as_ref()
                    .is_some_and(|active| active.requires_clean_rebuild),
            request,
        )
    }
}
