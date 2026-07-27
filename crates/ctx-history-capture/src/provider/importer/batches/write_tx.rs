use std::{io::Write, num::NonZeroUsize};

use ctx_history_store::Store;
use serde::Serialize;

use crate::{CaptureError, Result};

pub(super) const IMPORT_TRANSACTION_BATCH_BYTES: usize = 8 * 1024 * 1024;
pub(super) const IMPORT_TRANSACTION_BATCH_UNITS: usize = 64;
pub(super) fn provider_transaction_batch_size() -> Option<NonZeroUsize> {
    NonZeroUsize::new(IMPORT_TRANSACTION_BATCH_UNITS)
}
fn serialized_len(value: &impl Serialize) -> Result<usize> {
    let mut counter = ByteCounter::default();
    serde_json::to_writer(&mut counter, value)?;
    Ok(counter.bytes)
}

pub(super) fn serialized_len_or_rollback(
    transaction: &mut ProviderImportTransaction,
    store: &Store,
    value: &impl Serialize,
) -> Result<usize> {
    match serialized_len(value) {
        Ok(bytes) => Ok(bytes),
        Err(err) => {
            transaction.rollback(store);
            Err(err)
        }
    }
}

#[derive(Default)]
struct ByteCounter {
    bytes: usize,
}

impl Write for ByteCounter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.bytes = self.bytes.saturating_add(buffer.len());
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(crate) struct ProviderImportTransaction {
    active: bool,
    batch_size: Option<NonZeroUsize>,
    units: usize,
    bytes: usize,
    #[cfg(test)]
    committed_transactions: usize,
}

impl ProviderImportTransaction {
    pub(super) fn begin(
        store: &Store,
        has_work: bool,
        batch_size: Option<NonZeroUsize>,
    ) -> Result<Self> {
        if has_work {
            store.begin_import_batch()?;
        }
        Ok(Self {
            active: has_work,
            batch_size,
            units: 0,
            bytes: 0,
            #[cfg(test)]
            committed_transactions: 0,
        })
    }

    pub(crate) fn begin_bounded(store: &Store, has_work: bool) -> Result<Self> {
        Self::begin(store, has_work, provider_transaction_batch_size())
    }

    pub(crate) fn begin_projection(store: &Store) -> Result<Self> {
        Self::begin_bounded(store, true)
    }

    pub(crate) fn prepare_unit(&mut self, store: &Store, unit_bytes: usize) -> Result<()> {
        if unit_bytes > IMPORT_TRANSACTION_BATCH_BYTES {
            self.rollback(store);
            return Err(CaptureError::InvalidPayload(format!(
                "normalized provider Store unit requires {unit_bytes} serialized bytes; transaction limit is {IMPORT_TRANSACTION_BATCH_BYTES} bytes"
            )));
        }
        let unit_limit_reached = self
            .batch_size
            .is_some_and(|batch_size| self.units >= batch_size.get());
        let byte_limit_exceeded = self.batch_size.is_some()
            && self.units > 0
            && self.bytes.saturating_add(unit_bytes) > IMPORT_TRANSACTION_BATCH_BYTES;
        let result = if self.active && (unit_limit_reached || byte_limit_exceeded) {
            self.rotate(store)
        } else {
            Ok(())
        };
        if result.is_err() {
            self.rollback(store);
        }
        result
    }

    pub(crate) fn record_unit(&mut self, _store: &Store, unit_bytes: usize) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        self.units = self.units.saturating_add(1);
        self.bytes = self.bytes.saturating_add(unit_bytes);
        Ok(())
    }

    fn rotate(&mut self, store: &Store) -> Result<()> {
        store.commit_import_batch()?;
        #[cfg(test)]
        {
            self.committed_transactions = self.committed_transactions.saturating_add(1);
            record_provider_transaction_commit();
        }
        self.active = false;
        store.begin_import_batch()?;
        self.active = true;
        self.units = 0;
        self.bytes = 0;
        Ok(())
    }

    pub(crate) fn commit(&mut self, store: &Store) -> Result<()> {
        let result = if self.active {
            store.commit_import_batch().map_err(CaptureError::from)
        } else {
            Ok(())
        };
        if result.is_ok() {
            #[cfg(test)]
            if self.active {
                self.committed_transactions = self.committed_transactions.saturating_add(1);
                record_provider_transaction_commit();
            }
            self.active = false;
        } else {
            self.rollback(store);
        }
        result
    }

    #[cfg(test)]
    pub(super) fn committed_transactions(&self) -> usize {
        self.committed_transactions
    }

    pub(crate) fn rollback(&mut self, store: &Store) {
        if self.active {
            let _ = store.rollback_import_batch();
            self.active = false;
        }
    }
}

#[cfg(test)]
std::thread_local! {
    static PROVIDER_TRANSACTION_COMMITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn record_provider_transaction_commit() {
    PROVIDER_TRANSACTION_COMMITS.with(|commits| commits.set(commits.get().saturating_add(1)));
}

#[cfg(test)]
pub(super) fn reset_provider_transaction_commits() {
    PROVIDER_TRANSACTION_COMMITS.with(|commits| commits.set(0));
}

#[cfg(test)]
pub(super) fn provider_transaction_commits() -> usize {
    PROVIDER_TRANSACTION_COMMITS.with(std::cell::Cell::get)
}
