//! Final-binary Ctrl-C broker for scoped foreground finite-worker operations.

use std::sync::OnceLock;

use anyhow::Result;

static INSTALL: OnceLock<std::result::Result<(), String>> = OnceLock::new();

/// Installs the one process handler from the final executable. The handler
/// does only an atomic epoch increment; scoped guards own all child signaling,
/// waiting, and reaping after they observe that epoch.
pub(crate) fn install() -> Result<()> {
    match INSTALL.get_or_init(|| {
        ctrlc::set_handler(ctx_daemon_cli::record_foreground_interrupt)
            .map_err(|error| error.to_string())
    }) {
        Ok(()) => Ok(()),
        Err(error) => Err(anyhow::anyhow!(
            "install final-binary Ctrl-C broker: {error}"
        )),
    }
}
