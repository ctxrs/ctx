//! Final-binary Ctrl-C broker for scoped foreground finite-worker operations.

#[cfg(not(test))]
use std::sync::OnceLock;

use anyhow::Result;

#[cfg(not(test))]
static INSTALL: OnceLock<std::result::Result<(), String>> = OnceLock::new();

/// Installs the one process handler from the final executable. The handler
/// does only an atomic epoch increment; scoped guards own all child signaling,
/// waiting, and reaping after they observe that epoch.
#[cfg(not(test))]
pub(crate) fn install() -> Result<()> {
    match INSTALL.get_or_init(|| {
        ctrlc::set_handler(|| {
            // Record first, then classify against the same packed broker
            // state. A guard can deactivate only by a CAS which observes this
            // epoch; an interrupt after deactivation exits finalization 130.
            ctx_daemon_cli::record_foreground_interrupt();
            if !ctx_daemon_cli::foreground_operation_active() {
                std::process::exit(130);
            }
        })
        .map_err(|error| error.to_string())
    }) {
        Ok(()) => Ok(()),
        Err(error) => Err(anyhow::anyhow!(
            "install final-binary Ctrl-C broker: {error}"
        )),
    }
}

#[cfg(test)]
pub(crate) fn install() -> Result<()> {
    // Unit binaries contain daemon-side tests which own a different process
    // handler. Final-binary contract tests compile the production path and
    // provide the native install/signal evidence.
    Ok(())
}

pub(crate) fn with_scope<T>(operation: impl FnOnce() -> Result<T>) -> Result<T> {
    let epoch = ctx_daemon_cli::foreground_interrupt_epoch();
    ctx_daemon_cli::with_foreground_guard_since(epoch, || {
        install()?;
        operation()
    })
}
