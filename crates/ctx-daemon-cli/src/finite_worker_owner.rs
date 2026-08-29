//! Foreground-only ownership for a finite worker started by this CLI.
//!
//! This is intentionally a process-local, narrow bridge around source-refresh
//! publication.  It does not alter durable request terminality: the worker
//! owns that protocol and other clients retain their existing recovery path.

use std::{
    cell::{Cell, RefCell},
    fmt,
    sync::{
        atomic::{AtomicU64, Ordering},
        OnceLock,
    },
    time::Duration,
};

use anyhow::Result;

const FINITE_WORKER_RETIRE_TIMEOUT: Duration = Duration::from_secs(5);

thread_local! {
    static RETAINED_LEASE: RefCell<Option<ctx_daemon_application::FiniteCoreWorkerLease>> = const { RefCell::new(None) };
    static ACTIVE_INTERRUPT_EPOCH: Cell<Option<u64>> = const { Cell::new(None) };
}

static INTERRUPT_EPOCH: AtomicU64 = AtomicU64::new(0);
static INTERRUPT_HANDLER: OnceLock<std::result::Result<(), ()>> = OnceLock::new();

/// The foreground operation was interrupted after its finite worker was made
/// eligible for ownership.  The CLI maps this to the conventional exit 130.
#[derive(Debug)]
pub struct FiniteWorkerInterrupted;

impl fmt::Display for FiniteWorkerInterrupted {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("foreground import interrupted")
    }
}

impl std::error::Error for FiniteWorkerInterrupted {}

pub fn finite_worker_interrupted(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.is::<FiniteWorkerInterrupted>())
}

pub(super) fn retain(lease: ctx_daemon_application::FiniteCoreWorkerLease) {
    if !lease.controls_authenticated_child() {
        return;
    }
    RETAINED_LEASE.with(|retained| {
        let mut retained = retained.borrow_mut();
        // A single foreground refresh operation may retry availability after
        // an endpoint transition.  The original lease is the only one that
        // can carry authority; a second lease is an invariant violation, not
        // an opportunity to replace a worker.
        assert!(retained.is_none(), "finite worker lease retained twice");
        *retained = Some(lease);
    });
}

pub(super) fn foreground_interrupt_requested() -> bool {
    ACTIVE_INTERRUPT_EPOCH.with(|epoch| {
        epoch
            .get()
            .is_some_and(|before| INTERRUPT_EPOCH.load(Ordering::SeqCst) != before)
    })
}

pub(super) fn with_foreground_guard<T>(operation: impl FnOnce() -> Result<T>) -> Result<T> {
    install_interrupt_handler()?;
    let before = INTERRUPT_EPOCH.load(Ordering::SeqCst);
    ACTIVE_INTERRUPT_EPOCH.with(|epoch| epoch.set(Some(before)));
    let result = operation();
    let interrupted = foreground_interrupt_requested();
    ACTIVE_INTERRUPT_EPOCH.with(|epoch| epoch.set(None));

    let mut lease = RETAINED_LEASE.with(|retained| retained.borrow_mut().take());
    if interrupted {
        if let Some(lease) = lease.as_mut() {
            // Ctrl-C was already delivered to the attached child by the
            // terminal/console.  Wait for its native graceful path, then
            // force only this authenticated direct child if it remains live.
            lease.interrupt_and_reap(FINITE_WORKER_RETIRE_TIMEOUT)?;
        }
        return Err(FiniteWorkerInterrupted.into());
    }
    if let Some(lease) = lease.as_mut() {
        lease.reap_after_completion(FINITE_WORKER_RETIRE_TIMEOUT)?;
    }
    result
}

fn install_interrupt_handler() -> Result<()> {
    match INTERRUPT_HANDLER.get_or_init(|| {
        ctrlc::set_handler(|| {
            INTERRUPT_EPOCH.fetch_add(1, Ordering::SeqCst);
        })
        .map_err(|_| ())
    }) {
        Ok(()) => Ok(()),
        Err(()) => anyhow::bail!("install foreground import interrupt handler"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupt_classification_survives_context() {
        let error = anyhow::Error::new(FiniteWorkerInterrupted).context("refresh publication");
        assert!(finite_worker_interrupted(&error));
    }
}
