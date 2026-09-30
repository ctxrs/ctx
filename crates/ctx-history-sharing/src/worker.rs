use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use crate::{Collector, Error, Result, SharingStore, TickOutcome};

pub const RETRY_CADENCE: Duration = Duration::from_secs(30);

/// Lifecycle guard for a single explicitly configured destination. The daemon
/// owns this guard; foreground reads, status and connecting never start it.
pub struct SharingWorker {
    stop: Arc<AtomicBool>,
    wake: SyncSender<()>,
    thread: Option<JoinHandle<()>>,
}

impl SharingWorker {
    pub fn start(data_root: PathBuf, sharing_root: PathBuf) -> Result<Option<Self>> {
        let store = SharingStore::new(sharing_root.clone());
        if store
            .settings()?
            .is_none_or(|settings| settings.policy.is_none())
        {
            return Ok(None);
        }
        let collector = Collector::new(data_root, sharing_root);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let (wake, receiver) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("ctx-history-sharing".into())
            .spawn(move || {
                while !worker_stop.load(Ordering::Acquire) {
                    match collector.tick_with_stop(&worker_stop) {
                        TickOutcome::Progress => continue,
                        _ => {
                            if matches!(
                                receiver.recv_timeout(RETRY_CADENCE),
                                Err(mpsc::RecvTimeoutError::Disconnected)
                            ) {
                                break;
                            }
                        }
                    }
                }
            })
            .map_err(|_| Error::State)?;
        Ok(Some(Self {
            stop,
            wake,
            thread: Some(thread),
        }))
    }

    /// Startup discovery for `<data_root>/sharing/<name>`. Each broken optional
    /// connection is isolated; no sharing error can fail daemon startup.
    pub fn start_all(data_root: PathBuf) -> Vec<Self> {
        let Ok(entries) = std::fs::read_dir(data_root.join("sharing")) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| Self::start(data_root.clone(), e.path()).ok().flatten())
            .collect()
    }

    pub fn wake(&self) {
        let _ = self.wake.try_send(());
    }

    pub fn shutdown(mut self) {
        self.stop_and_join();
    }

    fn stop_and_join(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.wake();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for SharingWorker {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}
