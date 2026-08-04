use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration as StdDuration, Instant},
};

#[cfg(unix)]
use std::{fs, net::Shutdown, os::unix::net::UnixStream};

use crate::semantic::{
    daemon_wakeup::DaemonWakeup, source_backed_refresh_coordinator::CoreRefreshEngine,
};

use super::transport::{remove_daemon_service_endpoint, DaemonIpcService};

mod dispatch;
mod transport;
#[cfg(windows)]
#[path = "windows_security.rs"]
mod windows_security;

// Preserve the former parent-module paths for semantic-internal callers.
#[allow(unused_imports)]
pub(in crate::semantic) use dispatch::{
    handle_daemon_query_stream, handle_daemon_query_stream_inner,
};
pub(in crate::semantic) use transport::*;

pub(in crate::semantic) struct DaemonQueryService {
    pub(in crate::semantic) data_root: PathBuf,
    pub(in crate::semantic) service: DaemonIpcService,
    pub(in crate::semantic) activity: Arc<DaemonQueryActivity>,
    pub(in crate::semantic) source_refresh: Arc<CoreRefreshEngine>,
    pub(in crate::semantic) thread: Option<std::thread::JoinHandle<()>>,
    #[cfg(unix)]
    pub(in crate::semantic) socket_path: PathBuf,
    #[cfg(unix)]
    pub(in crate::semantic) socket_runtime_dir: Option<PathBuf>,
    #[cfg(unix)]
    pub(in crate::semantic) shutdown_stream: UnixStream,
    #[cfg(windows)]
    pub(in crate::semantic) pipe_name: String,
}

pub(in crate::semantic) const DAEMON_QUERY_REQUEST_MAX_BYTES: usize = 256 * 1024;
pub(in crate::semantic) const DAEMON_QUERY_REQUEST_READ_TIMEOUT: StdDuration =
    StdDuration::from_secs(2);

impl DaemonQueryService {
    pub(in crate::semantic) fn listener_finished(&self) -> bool {
        self.thread
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
    }

    #[cfg(all(test, unix))]
    pub(in crate::semantic) fn terminate_listener_for_test(&self) {
        let _ = self.shutdown_stream.shutdown(Shutdown::Both);
    }
}

impl Drop for DaemonQueryService {
    fn drop(&mut self) {
        self.activity.stop();
        #[cfg(unix)]
        {
            let _ = self.shutdown_stream.shutdown(Shutdown::Both);
        }
        #[cfg(windows)]
        transport::wake_windows_daemon_query_pipe(&self.pipe_name);
        remove_daemon_service_endpoint(&self.data_root, self.service);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        #[cfg(unix)]
        {
            let _ = fs::remove_file(&self.socket_path);
            if let Some(dir) = self.socket_runtime_dir.as_ref() {
                let _ = fs::remove_dir(dir);
            }
        }
    }
}

#[derive(Default)]
pub(in crate::semantic) struct DaemonQueryActivity {
    pub(in crate::semantic) state: Mutex<DaemonQueryActivityState>,
    idle_wakeup: Option<Arc<DaemonWakeup>>,
}

#[derive(Default)]
pub(in crate::semantic) struct DaemonQueryActivityState {
    pub(in crate::semantic) accepting: bool,
    pub(in crate::semantic) stopping: bool,
    pub(in crate::semantic) active_requests: usize,
    pub(in crate::semantic) generation: u64,
    wake_when_idle: bool,
}

pub(in crate::semantic) struct DaemonQueryRequestGuard {
    pub(in crate::semantic) activity: Arc<DaemonQueryActivity>,
}

impl DaemonQueryActivity {
    pub(in crate::semantic) fn new() -> Self {
        Self {
            state: Mutex::new(DaemonQueryActivityState {
                accepting: true,
                ..DaemonQueryActivityState::default()
            }),
            idle_wakeup: None,
        }
    }

    pub(in crate::semantic) fn with_idle_wakeup(idle_wakeup: Arc<DaemonWakeup>) -> Self {
        let mut activity = Self::new();
        activity.idle_wakeup = Some(idle_wakeup);
        activity
    }

    pub(in crate::semantic) fn state(&self) -> std::sync::MutexGuard<'_, DaemonQueryActivityState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub(in crate::semantic) fn begin_request(self: &Arc<Self>) -> Option<DaemonQueryRequestGuard> {
        let mut state = self.state();
        if !state.accepting || state.stopping {
            return None;
        }
        state.active_requests = state.active_requests.saturating_add(1);
        state.generation = state.generation.wrapping_add(1);
        drop(state);
        Some(DaemonQueryRequestGuard {
            activity: self.clone(),
        })
    }

    pub(in crate::semantic) fn snapshot(&self) -> (usize, u64) {
        let state = self.state();
        (state.active_requests, state.generation)
    }

    pub(in crate::semantic) fn wake_daemon_when_idle(&self) {
        let should_signal = {
            let mut state = self.state();
            if state.active_requests == 0 {
                true
            } else {
                state.wake_when_idle = true;
                false
            }
        };
        if should_signal {
            if let Some(wakeup) = self.idle_wakeup.as_ref() {
                wakeup.signal_ipc();
            }
        }
    }

    pub(in crate::semantic) fn cancel_idle_wakeup(&self) {
        self.state().wake_when_idle = false;
    }

    pub(in crate::semantic) fn try_stop_accepting_if_idle(&self, observed_generation: u64) -> bool {
        let mut state = self.state();
        if state.active_requests != 0 || state.generation != observed_generation {
            return false;
        }
        state.accepting = false;
        true
    }

    pub(in crate::semantic) fn resume_accepting(&self) {
        let mut state = self.state();
        if !state.stopping {
            state.accepting = true;
        }
    }

    pub(in crate::semantic) fn stop(&self) {
        let mut state = self.state();
        state.accepting = false;
        state.stopping = true;
    }

    pub(in crate::semantic) fn stopping(&self) -> bool {
        self.state().stopping
    }
}

impl Drop for DaemonQueryRequestGuard {
    fn drop(&mut self) {
        let mut state = self.activity.state();
        state.active_requests = state.active_requests.saturating_sub(1);
        state.generation = state.generation.wrapping_add(1);
        let should_signal = state.active_requests == 0 && state.wake_when_idle;
        if should_signal {
            state.wake_when_idle = false;
        }
        drop(state);
        if should_signal {
            if let Some(wakeup) = self.activity.idle_wakeup.as_ref() {
                wakeup.signal_ipc();
            }
        }
    }
}

pub(in crate::semantic) fn observe_daemon_query_activity(
    activity: Option<&DaemonQueryActivity>,
    idle_since: &mut Option<Instant>,
    observed_generation: &mut u64,
) {
    let Some(activity) = activity else {
        return;
    };
    let (active_requests, generation) = activity.snapshot();
    if active_requests != 0 || generation != *observed_generation {
        *idle_since = None;
        *observed_generation = generation;
    }
}

pub(in crate::semantic) fn daemon_can_begin_idle_shutdown(
    activity: Option<&DaemonQueryActivity>,
    observed_generation: u64,
) -> bool {
    activity.is_none_or(|activity| activity.try_stop_accepting_if_idle(observed_generation))
}
