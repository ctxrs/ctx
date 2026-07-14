impl Store {
    fn reclaim_orphaned_provider_staging(
        &self,
        scope: &ProviderFilePublicationScope,
    ) -> Result<()> {
        const MAX_RECLAIMED_PER_BEGIN: usize = 64;
        let lock_owner_id = provider_file_owner_lock_name(
            self.store_identity.digest(),
            scope.provider,
            &scope.material_source_format,
            &scope.material_source_root,
            &scope.source_path,
        );
        let prefix = format!("{STAGING_DIR_PREFIX}-{lock_owner_id}-");
        let current = format!("{prefix}{}", scope.staging_id);
        let root = self.store_identity.private_root();
        let mut reclaimed = 0usize;
        for entry in fs::read_dir(&root)? {
            if reclaimed >= MAX_RECLAIMED_PER_BEGIN {
                break;
            }
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !name.starts_with(&prefix) || name == current {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path())?;
            if !metadata.file_type().is_dir()
                || metadata.file_type().is_symlink()
                || metadata_is_reparse_point(&metadata)
            {
                return Err(StoreError::ProviderFileStaging);
            }
            validate_existing_private_lock_dir(&entry.path(), &metadata)
                .map_err(|_| StoreError::ProviderFileStaging)?;
            for child in [
                "seen.sqlite-journal",
                "seen.sqlite-wal",
                "seen.sqlite-shm",
                "seen.sqlite",
            ] {
                let child_path = entry.path().join(child);
                if child_path.exists() {
                    validate_existing_private_staging_file_for_removal(&child_path)
                        .map_err(|_| StoreError::ProviderFileStaging)?;
                }
                match fs::remove_file(child_path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err(StoreError::ProviderFileStaging),
                }
            }
            fs::remove_dir(entry.path()).map_err(|_| StoreError::ProviderFileStaging)?;
            reclaimed += 1;
        }
        Ok(())
    }

    fn cleanup_active_provider_file_publication(
        &self,
        scope_id: Uuid,
    ) -> std::result::Result<(), ProviderFileMaintenanceWarning> {
        let Some(active_scope_id) = self
            .provider_file_publication
            .borrow()
            .as_ref()
            .map(|active| active.scope_id)
        else {
            return Ok(());
        };
        if active_scope_id != scope_id {
            return Err(ProviderFileMaintenanceWarning::StagingCleanupDeferred {
                publication_id: scope_id.to_string(),
                operation: "scope-mismatch",
            });
        }
        if self.take_provider_file_fault(ProviderFileFaultPoint::Cleanup) {
            return Err(ProviderFileMaintenanceWarning::StagingCleanupDeferred {
                publication_id: scope_id.to_string(),
                operation: "fault-injection",
            });
        }

        let attached = self
            .provider_file_publication
            .borrow()
            .as_ref()
            .is_some_and(|active| active.attached);
        if attached {
            if self
                .conn
                .execute_batch(&format!("DETACH DATABASE {STAGING_SCHEMA}"))
                .is_err()
            {
                return Err(ProviderFileMaintenanceWarning::StagingCleanupDeferred {
                    publication_id: scope_id.to_string(),
                    operation: "detach",
                });
            }
            if let Some(active) = self.provider_file_publication.borrow_mut().as_mut() {
                active.attached = false;
            }
        }
        let staging_path = self
            .provider_file_publication
            .borrow()
            .as_ref()
            .and_then(|active| active.staging_path.clone());
        if let Some(path) = &staging_path {
            if let Err(error) = fs::remove_file(path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    return Err(ProviderFileMaintenanceWarning::StagingCleanupDeferred {
                        publication_id: scope_id.to_string(),
                        operation: "remove-file",
                    });
                }
            }
        }
        let staging_dir_path = self
            .provider_file_publication
            .borrow()
            .as_ref()
            .and_then(|active| active.staging_dir_path.clone());
        if let Some(path) = &staging_dir_path {
            if let Err(error) = fs::remove_dir(path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    return Err(ProviderFileMaintenanceWarning::StagingCleanupDeferred {
                        publication_id: scope_id.to_string(),
                        operation: "remove-directory",
                    });
                }
            }
        }
        self.provider_file_publication.replace(None);
        Ok(())
    }

    fn cleanup_abandoned_provider_file_publication(&self) -> Result<()> {
        let abandoned = self
            .provider_file_publication
            .borrow()
            .as_ref()
            .filter(|active| !active.lifecycle.load(Ordering::Acquire))
            .map(|active| active.scope_id);
        if let Some(scope_id) = abandoned {
            self.cleanup_active_provider_file_publication(scope_id)
                .map_err(maintenance_warning_as_error)?;
        }
        Ok(())
    }

    pub(crate) fn cleanup_provider_file_publication_on_drop(&self) {
        #[cfg(test)]
        self.provider_file_fault.set(None);
        let scope_id = self
            .provider_file_publication
            .borrow()
            .as_ref()
            .map(|active| active.scope_id);
        if let Some(scope_id) = scope_id {
            let _ = self.cleanup_active_provider_file_publication(scope_id);
        }
    }

    fn ensure_active_provider_file_publication(
        &self,
        scope: &ProviderFilePublicationScope,
    ) -> Result<()> {
        self.validate_provider_file_publication_scope(scope)?;
        let active = self.provider_file_publication.borrow();
        let Some(active) = active.as_ref() else {
            return Err(StoreError::InvalidProviderFilePublicationScope);
        };
        if active.scope_id != scope.scope_id || !active.lifecycle.load(Ordering::Acquire) {
            return Err(StoreError::InvalidProviderFilePublicationScope);
        }
        Ok(())
    }

    fn validate_provider_file_import_read_scope(
        &self,
        scope: &ProviderFilePublicationScope,
    ) -> Result<()> {
        self.ensure_active_provider_file_publication(scope)
    }

    fn validate_provider_file_publication_scope(
        &self,
        scope: &ProviderFilePublicationScope,
    ) -> Result<()> {
        if scope.store_identity != self.store_identity.digest()
            || !scope.lifecycle.load(Ordering::Acquire)
        {
            return Err(StoreError::InvalidProviderFilePublicationScope);
        }
        Ok(())
    }

    fn provider_file_owner_has_prior_material(
        &self,
        provider: CaptureProvider,
        material_source_format: &str,
        material_source_root: &str,
        source_path: &str,
    ) -> Result<bool> {
        provider_file_owner_has_prior_material(
            &self.conn,
            provider,
            material_source_format,
            material_source_root,
            source_path,
        )
    }
}
