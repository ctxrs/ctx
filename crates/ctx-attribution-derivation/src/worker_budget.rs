use std::collections::BTreeSet;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex};

use crate::protocol::{ErrorClass, ProtocolError};

mod planning;
mod telemetry;
#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use planning::configured_provider_worker_limits;
pub use planning::default_provider_worker_budget;

pub const CORE_PREPARATION_WORKERS_ENV: &str = "CTX_CORE_PREPARATION_WORKERS";
pub const MAX_CONFIGURED_CORE_PREPARATION_WORKERS: usize = 16;
pub const MAX_PUBLICATION_WORKERS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProviderWorkerLimits {
    pub(crate) preparation_workers: usize,
    pub(crate) finish_workers: usize,
}

#[derive(Debug, Default)]
struct WorkerActivity {
    pending_preparation_leases: usize,
    preparation_leases: BTreeSet<String>,
    preparation_active: usize,
    preparation_peak: usize,
    pending_finish_leases: usize,
    finish_leases: usize,
}

#[derive(Debug)]
pub struct ProviderWorkerBudget {
    limits: ProviderWorkerLimits,
    activity: Mutex<WorkerActivity>,
    activity_changed: Condvar,
}

pub struct PreparationWorkerGuard<'a> {
    budget: &'a ProviderWorkerBudget,
}

#[derive(Debug)]
pub(crate) struct PreparationPhaseLease {
    budget: Arc<ProviderWorkerBudget>,
    active: bool,
    _thread_bound: PhantomData<Rc<()>>,
}

pub(crate) struct ProviderFinishPhase {
    budget: Arc<ProviderWorkerBudget>,
    active: bool,
    _thread_bound: PhantomData<Rc<()>>,
}

struct PendingFinishPhase {
    budget: Arc<ProviderWorkerBudget>,
    active: bool,
}

impl ProviderWorkerBudget {
    pub(crate) fn begin_preparation(
        self: &Arc<Self>,
    ) -> Result<PreparationPhaseLease, ProtocolError> {
        let mut activity = self.lock_activity()?;
        while activity.pending_finish_leases != 0 || activity.finish_leases != 0 {
            activity = self.wait_activity(activity)?;
        }
        if activity.pending_preparation_leases == 0 && activity.preparation_leases.is_empty() {
            activity.preparation_peak = 0;
        }
        activity.pending_preparation_leases = activity
            .pending_preparation_leases
            .checked_add(1)
            .ok_or_else(worker_activity_overflow)?;
        Ok(PreparationPhaseLease {
            budget: Arc::clone(self),
            active: true,
            _thread_bound: PhantomData,
        })
    }

    pub(crate) fn enter_preparation(&self) -> Result<PreparationWorkerGuard<'_>, ProtocolError> {
        let mut activity = self.lock_activity()?;
        while activity.pending_finish_leases != 0
            || activity.finish_leases != 0
            || activity.preparation_active >= self.limits.preparation_workers
        {
            activity = self.wait_activity(activity)?;
        }
        activity.preparation_active = activity
            .preparation_active
            .checked_add(1)
            .ok_or_else(worker_activity_overflow)?;
        activity.preparation_peak = activity.preparation_peak.max(activity.preparation_active);
        Ok(PreparationWorkerGuard { budget: self })
    }

    pub(crate) fn close_preparation(
        self: &Arc<Self>,
        materialization_id: &str,
    ) -> Result<ProviderFinishPhase, ProtocolError> {
        let mut activity = self.lock_activity()?;
        activity.preparation_leases.remove(materialization_id);
        activity.pending_finish_leases = activity
            .pending_finish_leases
            .checked_add(1)
            .ok_or_else(worker_activity_overflow)?;
        let mut pending = PendingFinishPhase {
            budget: Arc::clone(self),
            active: true,
        };
        self.activity_changed.notify_all();
        while activity.pending_preparation_leases != 0
            || activity.preparation_active != 0
            || activity.finish_leases != 0
        {
            activity = self.wait_activity(activity)?;
            activity.preparation_leases.remove(materialization_id);
        }
        activity.pending_finish_leases -= 1;
        activity.finish_leases = 1;
        pending.active = false;
        self.activity_changed.notify_all();
        Ok(ProviderFinishPhase {
            budget: Arc::clone(self),
            active: true,
            _thread_bound: PhantomData,
        })
    }

    pub(crate) fn abort_preparation(&self, materialization_id: &str) {
        let mut activity = self.lock_activity_for_drop();
        activity.preparation_leases.remove(materialization_id);
        self.activity_changed.notify_all();
    }

    fn commit_preparation(&self, materialization_id: String) -> Result<(), ProtocolError> {
        let mut activity = self.lock_activity()?;
        if activity.pending_preparation_leases == 0 {
            return Err(ProtocolError::new(
                ErrorClass::Internal,
                "Provider preparation phase lease is unavailable",
            ));
        }
        activity.pending_preparation_leases -= 1;
        activity.preparation_leases.insert(materialization_id);
        self.activity_changed.notify_all();
        Ok(())
    }

    fn cancel_pending_preparation(&self) {
        let mut activity = self.lock_activity_for_drop();
        activity.pending_preparation_leases = activity.pending_preparation_leases.saturating_sub(1);
        self.activity_changed.notify_all();
    }

    fn cancel_pending_finish(&self) {
        let mut activity = self.lock_activity_for_drop();
        activity.pending_finish_leases = activity.pending_finish_leases.saturating_sub(1);
        self.activity_changed.notify_all();
    }

    fn end_finish_phase(&self) {
        let mut activity = self.lock_activity_for_drop();
        activity.finish_leases = activity.finish_leases.saturating_sub(1);
        self.activity_changed.notify_all();
    }

    fn release_preparation_worker(&self) {
        let mut activity = self.lock_activity_for_drop();
        activity.preparation_active = activity.preparation_active.saturating_sub(1);
        self.activity_changed.notify_all();
    }

    fn lock_activity_for_drop(&self) -> std::sync::MutexGuard<'_, WorkerActivity> {
        self.activity
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn lock_activity(&self) -> Result<std::sync::MutexGuard<'_, WorkerActivity>, ProtocolError> {
        self.activity.lock().map_err(|_| {
            ProtocolError::new(
                ErrorClass::Internal,
                "Provider worker activity state was poisoned",
            )
        })
    }

    fn wait_activity<'a>(
        &self,
        activity: std::sync::MutexGuard<'a, WorkerActivity>,
    ) -> Result<std::sync::MutexGuard<'a, WorkerActivity>, ProtocolError> {
        self.activity_changed.wait(activity).map_err(|_| {
            ProtocolError::new(
                ErrorClass::Internal,
                "Provider worker activity wait was poisoned",
            )
        })
    }
}

impl Drop for ProviderFinishPhase {
    fn drop(&mut self) {
        if self.active {
            self.budget.end_finish_phase();
            self.active = false;
        }
    }
}

impl Drop for PendingFinishPhase {
    fn drop(&mut self) {
        if self.active {
            self.budget.cancel_pending_finish();
            self.active = false;
        }
    }
}

impl PreparationPhaseLease {
    pub(crate) fn commit(mut self, materialization_id: String) -> Result<(), ProtocolError> {
        self.budget.commit_preparation(materialization_id)?;
        self.active = false;
        Ok(())
    }
}

impl Drop for PreparationPhaseLease {
    fn drop(&mut self) {
        if self.active {
            self.budget.cancel_pending_preparation();
            self.active = false;
        }
    }
}

impl Drop for PreparationWorkerGuard<'_> {
    fn drop(&mut self) {
        self.budget.release_preparation_worker();
    }
}

fn worker_activity_overflow() -> ProtocolError {
    ProtocolError::new(ErrorClass::Internal, "Provider worker activity overflowed")
}
