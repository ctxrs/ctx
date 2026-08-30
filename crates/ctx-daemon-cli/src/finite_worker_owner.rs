//! Foreground finite-worker authority, deliberately without signal handling.
//!
//! The final `ctx` binary owns the single process signal broker and calls
//! [`record_foreground_interrupt`]. This library merely scopes epochs and the
//! exact child capabilities acquired during one foreground operation.

use std::{
    cell::RefCell,
    fmt,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use anyhow::Result;
use ctx_daemon_application::{FiniteCoreWorkerLease, FiniteWorkerLease};

const FINITE_WORKER_RETIRE_TIMEOUT: Duration = Duration::from_secs(5);

thread_local! {
    static ACTIVE_OPERATION: RefCell<Option<ForegroundOperation>> = const { RefCell::new(None) };
}

static INTERRUPT_EPOCH: AtomicU64 = AtomicU64::new(0);

struct ForegroundOperation {
    epoch: u64,
    leases: Vec<FiniteWorkerLease>,
}

/// The foreground operation was interrupted. The final binary maps this
/// typed error to conventional exit 130 before rendering any public format.
#[derive(Debug)]
pub struct FiniteWorkerInterrupted;

impl fmt::Display for FiniteWorkerInterrupted {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("foreground finite-worker operation interrupted")
    }
}

impl std::error::Error for FiniteWorkerInterrupted {}

pub fn finite_worker_interrupted(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.is::<FiniteWorkerInterrupted>())
}

/// Called only by the final-binary signal broker. It is intentionally safe in
/// a signal callback: no locks, allocation, child access, or I/O.
pub fn record_foreground_interrupt() {
    INTERRUPT_EPOCH.fetch_add(1, Ordering::SeqCst);
}

pub fn foreground_interrupt_requested() -> bool {
    ACTIVE_OPERATION.with(|operation| {
        operation
            .borrow()
            .as_ref()
            .is_some_and(|operation| INTERRUPT_EPOCH.load(Ordering::SeqCst) != operation.epoch)
    })
}

pub fn checkpoint() -> Result<()> {
    if foreground_interrupt_requested() {
        return Err(FiniteWorkerInterrupted.into());
    }
    Ok(())
}

/// Runs one foreground wait with a scoped epoch and exact-child authority.
/// Nested waits restore their parent's state rather than installing or
/// replacing handlers; every interrupted nested scope independently reaps
/// only the children it owns.
pub fn with_foreground_guard<T>(operation: impl FnOnce() -> Result<T>) -> Result<T> {
    let current = ForegroundOperation {
        epoch: INTERRUPT_EPOCH.load(Ordering::SeqCst),
        leases: Vec::new(),
    };
    let previous = ACTIVE_OPERATION.with(|slot| slot.replace(Some(current)));
    let result = operation();
    let mut current = ACTIVE_OPERATION.with(|slot| {
        slot.replace(previous)
            .expect("foreground operation state must remain installed")
    });

    if INTERRUPT_EPOCH.load(Ordering::SeqCst) != current.epoch {
        // An interrupt has priority over any application or cleanup failure.
        // Each lease is a direct child capability; Joined observations were
        // discarded before retention and cannot be affected here.
        for lease in &mut current.leases {
            let _ = lease.interrupt_and_reap(FINITE_WORKER_RETIRE_TIMEOUT);
        }
        return Err(FiniteWorkerInterrupted.into());
    }
    // Normal success is passive: reap only children already known to have
    // exited. Active/coalesced finite work is left to the daemon state machine.
    for lease in &mut current.leases {
        let _ = lease.reap_if_exited();
    }
    result
}

pub(super) fn retain(lease: FiniteCoreWorkerLease) -> Result<()> {
    let Some(lease) = lease.into_owned() else {
        return Ok(());
    };
    checkpoint()?;
    ACTIVE_OPERATION.with(|operation| {
        let mut operation = operation.borrow_mut();
        let Some(operation) = operation.as_mut() else {
            return Err(anyhow::anyhow!(
                "finite worker ownership was acquired outside a foreground operation"
            ));
        };
        // A recovery can replace a crashed worker. Reap predecessors that have
        // already exited, then retain the replacement without a retained-twice
        // invariant or any authority over a joined owner.
        operation
            .leases
            .retain_mut(|lease| match lease.reap_if_exited() {
                Ok(exited) => !exited,
                Err(_) => true,
            });
        operation.leases.push(lease);
        Ok(())
    })
}

#[cfg(test)]
pub(crate) fn interrupt_for_test() {
    record_foreground_interrupt();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interruption_overrides_a_cleanup_or_operation_error() {
        let error = with_foreground_guard(|| {
            interrupt_for_test();
            Err::<(), _>(anyhow::anyhow!("ordinary failure"))
        })
        .unwrap_err();
        assert!(finite_worker_interrupted(&error));
    }

    #[test]
    fn interrupt_classification_survives_context() {
        let error = anyhow::Error::new(FiniteWorkerInterrupted).context("refresh publication");
        assert!(finite_worker_interrupted(&error));
    }
}
