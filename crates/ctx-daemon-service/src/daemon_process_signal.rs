use std::{
    ffi::OsStr,
    io::Read,
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

pub(super) fn install_daemon_shutdown_handlers(
    handle_process_signals: bool,
    finite_worker: bool,
    wakeup: Arc<DaemonWakeup>,
    lifecycle_state: Arc<DaemonLifecycleState>,
) -> Result<()> {
    let lifeline_enabled = finite_worker
        && std::env::var_os(ctx_daemon_runtime::FINITE_WORKER_LIFELINE_ENV).as_deref()
            == Some(OsStr::new("1"));
    if !handle_process_signals && !lifeline_enabled {
        return Ok(());
    }
    let (deadline_sender, deadline_receiver) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("ctx-daemon-signal-deadline".to_owned())
        .spawn(move || {
            if deadline_receiver.recv().is_ok() {
                std::thread::sleep(SHUTDOWN_DEADLINE);
                std::process::exit(FORCED_EXIT_CODE);
            }
        })
        .context("start ctx daemon process signal deadline")?;
    if handle_process_signals {
        let signal_received = AtomicBool::new(false);
        let signal_deadline = deadline_sender.clone();
        let signal_wakeup = Arc::clone(&wakeup);
        let signal_lifecycle = Arc::clone(&lifecycle_state);
        // Background daemons can inherit ignored SIGINT. This process owns its
        // signal lifecycle, so install handlers for both SIGINT and SIGTERM.
        ctrlc::set_handler(move || {
            if signal_received.swap(true, Ordering::SeqCst) {
                std::process::exit(FORCED_EXIT_CODE);
            }
            let _ = signal_deadline.try_send(());
            signal_lifecycle.mark_stopping();
            signal_wakeup.signal_shutdown();
        })
        .context("install ctx daemon process signal handler")?;
    }
    if lifeline_enabled {
        std::thread::Builder::new()
            .name("ctx-finite-worker-lifeline".to_owned())
            .spawn(move || {
                monitor_parent_lifeline(std::io::stdin().lock(), move || {
                    let _ = deadline_sender.try_send(());
                    lifecycle_state.mark_stopping();
                    wakeup.signal_shutdown();
                });
            })
            .context("start ctx finite worker parent-lifeline monitor")?;
    }
    Ok(())
}

fn monitor_parent_lifeline(mut reader: impl Read, on_parent_lost: impl FnOnce()) {
    let mut byte = [0_u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(1) if byte[0] == ctx_daemon_runtime::FINITE_WORKER_LIFELINE_DETACH_BYTE => {
                // The owning CLI exited normally; let FiniteCoreWorkerExit
                // govern quiescence and any coalesced successor requests.
                return;
            }
            Ok(0) | Ok(_) => {
                on_parent_lost();
                return;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => {
                on_parent_lost();
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_lifeline_detach_byte_does_not_trigger_shutdown() {
        let triggered = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&triggered);
        let data = [ctx_daemon_runtime::FINITE_WORKER_LIFELINE_DETACH_BYTE];
        monitor_parent_lifeline(std::io::Cursor::new(data), move || {
            flag.store(true, Ordering::SeqCst);
        });
        assert!(!triggered.load(Ordering::SeqCst));
    }

    #[test]
    fn parent_lifeline_eof_without_detach_byte_triggers_shutdown() {
        let triggered = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&triggered);
        monitor_parent_lifeline(std::io::Cursor::new(Vec::<u8>::new()), move || {
            flag.store(true, Ordering::SeqCst);
        });
        assert!(triggered.load(Ordering::SeqCst));
    }

    #[test]
    fn parent_lifeline_unexpected_byte_triggers_shutdown() {
        let triggered = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&triggered);
        monitor_parent_lifeline(std::io::Cursor::new(*b"X"), move || {
            flag.store(true, Ordering::SeqCst);
        });
        assert!(triggered.load(Ordering::SeqCst));
    }
}
