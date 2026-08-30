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
    #[cfg(test)]
    static BEFORE_DEACTIVATE_FOR_TEST: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
}

const ACTIVE_COUNT_BITS: u32 = 32;
const ACTIVE_COUNT_MASK: u64 = u32::MAX as u64;
const INTERRUPT_EPOCH_INCREMENT: u64 = 1 << ACTIVE_COUNT_BITS;

// One atomic state closes the signal-versus-scope-teardown race: handlers
// increment the high interrupt epoch, while guard activation/deactivation
// changes only the low active-scope count with compare-exchange.
static BROKER_STATE: AtomicU64 = AtomicU64::new(0);

fn interrupt_epoch(state: u64) -> u64 {
    state >> ACTIVE_COUNT_BITS
}

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
    BROKER_STATE.fetch_add(INTERRUPT_EPOCH_INCREMENT, Ordering::SeqCst);
}

pub fn foreground_interrupt_epoch() -> u64 {
    interrupt_epoch(BROKER_STATE.load(Ordering::SeqCst))
}

pub fn foreground_operation_active() -> bool {
    BROKER_STATE.load(Ordering::SeqCst) & ACTIVE_COUNT_MASK != 0
}

pub fn foreground_interrupt_requested() -> bool {
    ACTIVE_OPERATION.with(|operation| {
        operation
            .borrow()
            .as_ref()
            .is_some_and(|operation| foreground_interrupt_epoch() != operation.epoch)
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
#[cfg(test)]
pub fn with_foreground_guard<T>(operation: impl FnOnce() -> Result<T>) -> Result<T> {
    with_foreground_guard_since(foreground_interrupt_epoch(), operation)
}

/// Enters an RAII foreground scope using an epoch captured by the final binary
/// before its handler is installed. This prevents an interrupt in the
/// install-to-guard window from being accepted as the operation baseline.
pub fn with_foreground_guard_since<T>(
    epoch: u64,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let guard = ForegroundOperationGuard::enter(epoch);
    let result = operation();
    let interrupted = guard.finish();

    if interrupted {
        return Err(FiniteWorkerInterrupted.into());
    }
    result
}

struct ForegroundOperationGuard {
    previous: Option<ForegroundOperation>,
    active: bool,
}

impl ForegroundOperationGuard {
    fn enter(epoch: u64) -> Self {
        let current = ForegroundOperation {
            epoch,
            leases: Vec::new(),
        };
        let previous = ACTIVE_OPERATION.with(|slot| slot.replace(Some(current)));
        BROKER_STATE
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |state| {
                ((state & ACTIVE_COUNT_MASK) < ACTIVE_COUNT_MASK).then_some(state + 1)
            })
            .expect("foreground operation nesting count must remain representable");
        Self {
            previous,
            active: true,
        }
    }

    fn finish(mut self) -> bool {
        let interrupted = self.finish_inner();
        self.active = false;
        interrupted
    }

    fn finish_inner(&mut self) -> bool {
        let mut current = ACTIVE_OPERATION.with(|slot| {
            slot.replace(self.previous.take())
                .expect("foreground operation state must remain installed")
        });

        let mut interrupted = foreground_interrupt_epoch() != current.epoch;
        if interrupted {
            // An interrupt has priority over any application or cleanup failure.
            // Each lease is a direct child capability; Joined observations were
            // discarded before retention and cannot be affected here.
            for lease in &mut current.leases {
                let _ = lease.interrupt_and_reap(FINITE_WORKER_RETIRE_TIMEOUT);
            }
        } else {
            // Normal success is passive: reap only children already known to
            // have exited. Active/coalesced finite work is left to the daemon
            // state machine.
            for lease in &mut current.leases {
                let _ = lease.reap_if_exited();
            }
        }
        #[cfg(test)]
        BEFORE_DEACTIVATE_FOR_TEST.with(|hook| {
            if let Some(hook) = hook.borrow_mut().take() {
                hook();
            }
        });
        loop {
            let state = BROKER_STATE.load(Ordering::SeqCst);
            if !interrupted && interrupt_epoch(state) != current.epoch {
                // A signal raced passive normal cleanup. Because the epoch and
                // active count share one CAS state, it cannot be baselined away
                // between this observation and scope deactivation.
                for lease in &mut current.leases {
                    let _ = lease.interrupt_and_reap(FINITE_WORKER_RETIRE_TIMEOUT);
                }
                interrupted = true;
                continue;
            }
            assert_ne!(
                state & ACTIVE_COUNT_MASK,
                0,
                "foreground operation count must remain active through cleanup"
            );
            if BROKER_STATE
                .compare_exchange(state, state - 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                break;
            }
        }
        interrupted
    }
}

impl Drop for ForegroundOperationGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = self.finish_inner();
        }
    }
}

pub(super) fn retain(lease: FiniteCoreWorkerLease) -> Result<()> {
    let Some(lease) = lease.into_owned() else {
        return Ok(());
    };
    let mut lease = Some(lease);
    let retained = ACTIVE_OPERATION.with(|operation| {
        let mut operation = operation.borrow_mut();
        let Some(operation) = operation.as_mut() else {
            return false;
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
        operation
            .leases
            .push(lease.take().expect("owned lease is retained once"));
        true
    });
    if retained {
        // Ownership is now in the guard before cancellation is observed, so
        // the interrupted return path cannot drop an unreaped direct child.
        return checkpoint();
    }
    // A library caller which bypassed final-binary scoping must not leak the
    // exact direct child it acquired. Cleanup errors are secondary to this
    // invariant violation but the bounded exact-child reap is always tried.
    let cleanup = lease
        .as_mut()
        .expect("unscoped owned lease remains available for cleanup")
        .interrupt_and_reap(FINITE_WORKER_RETIRE_TIMEOUT);
    let error =
        anyhow::anyhow!("finite worker ownership was acquired outside a foreground operation");
    match cleanup {
        Ok(()) => Err(error),
        Err(cleanup) => Err(error.context(format!("reap unscoped finite worker: {cleanup}"))),
    }
}

#[cfg(test)]
pub(crate) fn interrupt_for_test() {
    record_foreground_interrupt();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interruption_overrides_a_bounded_reap_timeout_for_exit_130() {
        let error = with_foreground_guard(|| {
            interrupt_for_test();
            Err::<(), _>(
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "finite worker did not exit after bounded kill escalation",
                )
                .into(),
            )
        })
        .unwrap_err();
        assert!(finite_worker_interrupted(&error));
    }

    #[test]
    fn interrupt_classification_survives_context() {
        let error = anyhow::Error::new(FiniteWorkerInterrupted).context("refresh publication");
        assert!(finite_worker_interrupted(&error));
    }

    #[test]
    fn interrupt_after_epoch_capture_is_not_adopted_as_the_scope_baseline() {
        let captured = foreground_interrupt_epoch();
        interrupt_for_test();

        let error = with_foreground_guard_since(captured, || Ok(())).unwrap_err();

        assert!(finite_worker_interrupted(&error));
    }

    #[test]
    fn interrupt_racing_scope_deactivation_is_not_lost() {
        BEFORE_DEACTIVATE_FOR_TEST.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(record_foreground_interrupt));
        });

        let error = with_foreground_guard(|| Ok(())).unwrap_err();

        assert!(finite_worker_interrupted(&error));
        assert!(!foreground_operation_active());
    }

    #[test]
    fn panicking_nested_scope_restores_its_parent() {
        with_foreground_guard(|| {
            let nested = std::panic::catch_unwind(|| {
                let _ = with_foreground_guard(|| -> Result<()> {
                    panic!("nested operation panic");
                });
            });
            assert!(nested.is_err());
            assert!(foreground_operation_active());
            checkpoint()
        })
        .unwrap();

        assert!(!foreground_operation_active());
    }
}
