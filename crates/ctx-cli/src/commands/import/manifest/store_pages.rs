use anyhow::Result;

use ctx_history_core::{utc_now, CaptureProvider};
use ctx_history_store::{SourceImportFile, Store};

use crate::commands::import::provider_path_text;
use crate::provider_sources::SourceInfo;

use super::{InventoryControl, InventoryPhase, ReconciliationStage, SourceImportInventory};

// Inventory rows are small, independently bounded observations. Persist enough
// of them per SQLite transaction to avoid repeatedly rewriting the same B-tree
// pages for large provider trees while retaining a sub-megabyte working set.
pub(super) const SOURCE_IMPORT_STORE_PAGE_SIZE: usize = 512;

pub(super) struct InventoryPageStore<'a> {
    store: &'a Store,
}

impl<'a> InventoryPageStore<'a> {
    pub(super) fn new(store: &'a Store) -> Self {
        Self { store }
    }

    pub(super) fn persist_data_page(
        &self,
        files: &[SourceImportFile],
        force_reindex: bool,
    ) -> Result<()> {
        if files.is_empty() {
            return Ok(());
        }
        debug_assert!(files.len() <= SOURCE_IMPORT_STORE_PAGE_SIZE);
        self.store.begin_immediate_batch()?;
        let persist = (|| -> Result<()> {
            self.store.upsert_source_import_files(files)?;
            if force_reindex {
                self.store.reset_source_import_files_pending(files)?;
            }
            Ok(())
        })();
        self.finish(persist)
    }

    pub(super) fn persist_control(
        &self,
        control: &InventoryControl,
        phase: InventoryPhase,
        keyset: Option<&str>,
        inventory: SourceImportInventory,
    ) -> Result<()> {
        let file = match phase {
            InventoryPhase::Discovering => {
                control.discovering_file(inventory.files, inventory.bytes)
            }
            InventoryPhase::Reconciling(ReconciliationStage::Preference) => {
                control.reconciling_preference_file(keyset, inventory.files, inventory.bytes)
            }
            InventoryPhase::Reconciling(ReconciliationStage::Missing) => {
                control.reconciling_missing_file(keyset, inventory.files, inventory.bytes)
            }
            InventoryPhase::Complete => {
                control.complete_file(keyset, inventory.files, inventory.bytes)
            }
        }?;
        self.persist_data_page(std::slice::from_ref(&file), false)
    }

    pub(super) fn source_stats(
        &self,
        provider: CaptureProvider,
        source_root: &str,
    ) -> Result<(usize, u64)> {
        Ok(self
            .store
            .source_import_file_stats_for_source(provider, source_root)?)
    }

    pub(super) fn reconcile_shadowed_page(
        &self,
        control: &InventoryControl,
        inventory: SourceImportInventory,
        after_source_path: Option<&str>,
    ) -> Result<Option<String>> {
        self.store.begin_immediate_batch()?;
        let reconciled = (|| -> Result<Option<String>> {
            let next = self.store.mark_source_import_shadowed_paths_stale_page(
                control.provider(),
                control.source_root(),
                control.observed_at_ms(),
                after_source_path,
            )?;
            if let Some(keyset) = next.as_deref() {
                let file = control.reconciling_preference_file(
                    Some(keyset),
                    inventory.files,
                    inventory.bytes,
                )?;
                self.store
                    .upsert_source_import_files(std::slice::from_ref(&file))?;
            }
            Ok(next)
        })();
        self.finish(reconciled)
    }

    pub(super) fn reconcile_missing_page(
        &self,
        control: &InventoryControl,
        inventory: SourceImportInventory,
        after_source_path: Option<&str>,
    ) -> Result<Option<String>> {
        self.store.begin_immediate_batch()?;
        let reconciled = (|| -> Result<Option<String>> {
            let next = self.store.reconcile_source_import_missing_paths_page(
                control.provider(),
                control.source_root(),
                control.observed_at_ms(),
                after_source_path,
            )?;
            if let Some(keyset) = next.as_deref() {
                let file = control.reconciling_missing_file(
                    Some(keyset),
                    inventory.files,
                    inventory.bytes,
                )?;
                self.store
                    .upsert_source_import_files(std::slice::from_ref(&file))?;
            }
            Ok(next)
        })();
        self.finish(reconciled)
    }

    pub(super) fn reconcile_single_file_missing_page(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        observed_at_ms: i64,
        after_source_path: Option<&str>,
    ) -> Result<Option<String>> {
        self.store.begin_immediate_batch()?;
        let reconciled = self
            .store
            .reconcile_source_import_single_file_missing_paths_page(
                provider,
                source_root,
                observed_at_ms,
                after_source_path,
            )
            .map_err(anyhow::Error::from);
        self.finish(reconciled)
    }

    pub(super) fn persist_files_and_mark_missing(
        &self,
        source: &SourceInfo,
        files: &[SourceImportFile],
    ) -> Result<()> {
        let source_root = provider_path_text(&source.path)?.to_owned();
        let current_paths = files
            .iter()
            .map(|file| file.source_path.clone())
            .collect::<Vec<_>>();
        let observed_at_ms = utc_now().timestamp_millis();
        self.store.begin_immediate_batch()?;
        let persist = (|| -> Result<()> {
            self.store.upsert_source_import_files(files)?;
            self.store.mark_source_import_missing_paths_stale(
                source.provider,
                &source_root,
                &current_paths,
                observed_at_ms,
            )?;
            Ok(())
        })();
        self.finish(persist)
    }

    fn finish<T>(&self, result: Result<T>) -> Result<T> {
        match result {
            Ok(value) => match self.store.commit_batch() {
                Ok(()) => Ok(value),
                Err(error) => {
                    let _ = self.store.rollback_batch();
                    Err(error.into())
                }
            },
            Err(error) => {
                let _ = self.store.rollback_batch();
                Err(error)
            }
        }
    }
}
