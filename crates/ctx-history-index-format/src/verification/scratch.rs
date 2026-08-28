use std::{
    cell::RefCell,
    sync::{Arc, Mutex},
};

use crate::{IndexError, Result};

// The candidate verifier needs only bounded heap scratch. Test and
// qualification builds additionally use the disk allowance for the exhaustive
// logical verifier's spill files.
const MAX_VERIFICATION_SCRATCH_DISK_BYTES: u64 = 16 * 1024 * 1024 * 1024;
pub(super) const MAX_VERIFICATION_SCRATCH_HEAP_BYTES: u64 = 16 * 1024 * 1024;

thread_local! {
    static ACTIVE_SCRATCH_BUDGET: RefCell<Option<VerificationScratchBudget>> = const { RefCell::new(None) };
}

#[derive(Clone, Debug)]
pub(super) struct VerificationScratchBudget {
    state: Arc<Mutex<ScratchUsage>>,
    maximum_disk_bytes: u64,
    maximum_heap_bytes: u64,
}

#[derive(Debug, Default)]
struct ScratchUsage {
    disk_bytes: u64,
    heap_bytes: u64,
}

#[derive(Debug)]
pub(super) struct ScratchReservation {
    pub(super) budget: VerificationScratchBudget,
    disk_bytes: u64,
    heap_bytes: u64,
}

struct ActiveScratchGuard;

impl VerificationScratchBudget {
    pub(super) fn production() -> Self {
        Self::with_limits(
            MAX_VERIFICATION_SCRATCH_DISK_BYTES,
            MAX_VERIFICATION_SCRATCH_HEAP_BYTES,
        )
    }

    pub(super) fn with_limits(maximum_disk_bytes: u64, maximum_heap_bytes: u64) -> Self {
        Self {
            state: Arc::new(Mutex::new(ScratchUsage::default())),
            maximum_disk_bytes,
            maximum_heap_bytes,
        }
    }

    pub(super) fn reserve(&self, disk_bytes: u64, heap_bytes: u64) -> Result<ScratchReservation> {
        let mut usage = self.state.lock().map_err(|_| {
            IndexError::WriterInvariant("verification scratch budget lock poisoned")
        })?;
        let required_disk_bytes = usage
            .disk_bytes
            .checked_add(disk_bytes)
            .ok_or(IndexError::CountOverflow)?;
        if required_disk_bytes > self.maximum_disk_bytes {
            return Err(IndexError::VerificationScratchLimitExceeded {
                required_bytes: required_disk_bytes,
                maximum_bytes: self.maximum_disk_bytes,
            });
        }
        let required_heap_bytes = usage
            .heap_bytes
            .checked_add(heap_bytes)
            .ok_or(IndexError::CountOverflow)?;
        if required_heap_bytes > self.maximum_heap_bytes {
            return Err(IndexError::VerificationScratchLimitExceeded {
                required_bytes: required_heap_bytes,
                maximum_bytes: self.maximum_heap_bytes,
            });
        }
        usage.disk_bytes = required_disk_bytes;
        usage.heap_bytes = required_heap_bytes;
        drop(usage);
        Ok(ScratchReservation {
            budget: self.clone(),
            disk_bytes,
            heap_bytes,
        })
    }
}

impl ScratchReservation {
    pub(super) fn absorb(&mut self, mut other: Self) -> Result<()> {
        if !Arc::ptr_eq(&self.budget.state, &other.budget.state) {
            return Err(IndexError::WriterInvariant(
                "verification scratch reservation budget changed",
            ));
        }
        self.disk_bytes = self
            .disk_bytes
            .checked_add(other.disk_bytes)
            .ok_or(IndexError::CountOverflow)?;
        self.heap_bytes = self
            .heap_bytes
            .checked_add(other.heap_bytes)
            .ok_or(IndexError::CountOverflow)?;
        other.disk_bytes = 0;
        other.heap_bytes = 0;
        Ok(())
    }
}

impl Drop for ScratchReservation {
    fn drop(&mut self) {
        let mut usage = self
            .budget
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        usage.disk_bytes = usage.disk_bytes.saturating_sub(self.disk_bytes);
        usage.heap_bytes = usage.heap_bytes.saturating_sub(self.heap_bytes);
    }
}

impl Drop for ActiveScratchGuard {
    fn drop(&mut self) {
        ACTIVE_SCRATCH_BUDGET.with(|active| {
            active.borrow_mut().take();
        });
    }
}

pub(super) fn with_verification_scratch_budget<T>(verify: impl FnOnce() -> Result<T>) -> Result<T> {
    let installed = ACTIVE_SCRATCH_BUDGET.with(|active| {
        let mut active = active.borrow_mut();
        if active.is_some() {
            false
        } else {
            *active = Some(VerificationScratchBudget::production());
            true
        }
    });
    let _guard = installed.then_some(ActiveScratchGuard);
    verify()
}

pub(super) fn active_scratch_budget() -> VerificationScratchBudget {
    ACTIVE_SCRATCH_BUDGET
        .with(|active| active.borrow().clone())
        .unwrap_or_else(VerificationScratchBudget::production)
}

pub(super) fn reserve_verification_scratch(
    disk_bytes: u64,
    heap_bytes: u64,
) -> Result<ScratchReservation> {
    active_scratch_budget().reserve(disk_bytes, heap_bytes)
}
