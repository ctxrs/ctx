use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::Duration,
};

use anyhow::{Context, Result};

use crate::{daemon_wakeup::DaemonWakeup, query_service::DaemonLifecycleState};

const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(1);
const FORCED_EXIT_CODE: i32 = 1;

pub(super) fn install_daemon_process_signal_handler(
    wakeup: Arc<DaemonWakeup>,
    lifecycle_state: Arc<DaemonLifecycleState>,
) -> Result<()> {
    let signal_received = AtomicBool::new(false);
    let (deadline_sender, deadline_receiver) = mpsc::sync_channel(1);
    // Background daemons can inherit ignored SIGINT. This process owns its
    // signal lifecycle, so install handlers for both SIGINT and SIGTERM.
    ctrlc::set_handler(move || {
        if signal_received.swap(true, Ordering::SeqCst) {
            std::process::exit(FORCED_EXIT_CODE);
        }
        let _ = deadline_sender.try_send(());
        lifecycle_state.mark_stopping();
        wakeup.signal_shutdown();
    })
    .context("install ctx daemon process signal handler")?;
    std::thread::Builder::new()
        .name("ctx-daemon-signal-deadline".to_owned())
        .spawn(move || {
            if deadline_receiver.recv().is_ok() {
                std::thread::sleep(SHUTDOWN_DEADLINE);
                std::process::exit(FORCED_EXIT_CODE);
            }
        })
        .context("start ctx daemon process signal deadline")?;
    Ok(())
}
