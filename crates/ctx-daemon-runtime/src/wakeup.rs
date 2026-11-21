use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::Duration;

use serde::Serialize;

const WAKE_FILESYSTEM: u8 = 1;
const WAKE_IPC: u8 = 1 << 1;
const WAKE_SHUTDOWN: u8 = 1 << 2;

pub trait CoalescingWakePayload: Clone + Default + Send + Sync + 'static {
    fn is_empty(&self) -> bool;
    fn merge(&mut self, other: Self);
}

pub type PayloadSink<P> = Arc<dyn Fn(&P) + Send + Sync>;

struct PayloadSinkSlot<P> {
    sink: RwLock<Option<PayloadSink<P>>>,
}

impl<P> Default for PayloadSinkSlot<P> {
    fn default() -> Self {
        Self {
            sink: RwLock::new(None),
        }
    }
}

impl<P> std::fmt::Debug for PayloadSinkSlot<P> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PayloadSinkSlot")
            .field(
                "installed",
                &self
                    .sink
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_some(),
            )
            .finish()
    }
}

impl<P> PayloadSinkSlot<P> {
    fn set(&self, sink: Option<PayloadSink<P>>) {
        *self
            .sink
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = sink;
    }

    fn is_installed(&self) -> bool {
        self.sink
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }

    fn dispatch(&self, payload: &P) -> bool {
        let sink = self
            .sink
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(sink) = sink {
            sink(payload);
            true
        } else {
            false
        }
    }
}

#[derive(Debug)]
struct WakeupState<P> {
    pending: u8,
    filesystem_signals: u64,
    ipc_signals: u64,
    shutdown_signals: u64,
    blocking_waits: u64,
    timeout_wakeups: u64,
    scheduled_retry_wakeups: u64,
    scheduled_refresh_wakeups: u64,
    work_cycles: u64,
    no_work_cycles: u64,
    payload: P,
    observed_payload: P,
}

impl<P: Default> Default for WakeupState<P> {
    fn default() -> Self {
        Self {
            pending: 0,
            filesystem_signals: 0,
            ipc_signals: 0,
            shutdown_signals: 0,
            blocking_waits: 0,
            timeout_wakeups: 0,
            scheduled_retry_wakeups: 0,
            scheduled_refresh_wakeups: 0,
            work_cycles: 0,
            no_work_cycles: 0,
            payload: P::default(),
            observed_payload: P::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Wake<P> {
    pub filesystem: bool,
    pub shutdown: bool,
    pub timed_out: bool,
    pub payload: P,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize)]
pub struct WakeupSnapshot {
    pub blocking_waits: u64,
    pub filesystem_signals: u64,
    pub ipc_signals: u64,
    pub shutdown_signals: u64,
    pub timeout_wakeups: u64,
    pub scheduled_retry_wakeups: u64,
    pub scheduled_refresh_wakeups: u64,
    pub work_cycles: u64,
    pub no_work_cycles: u64,
}

pub struct Wakeup<P: CoalescingWakePayload> {
    state: Mutex<WakeupState<P>>,
    changed: Condvar,
    payload_sink: PayloadSinkSlot<P>,
}

impl<P: CoalescingWakePayload> Default for Wakeup<P> {
    fn default() -> Self {
        Self {
            state: Mutex::new(WakeupState::default()),
            changed: Condvar::new(),
            payload_sink: PayloadSinkSlot::default(),
        }
    }
}

impl<P: CoalescingWakePayload> std::fmt::Debug for Wakeup<P> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Wakeup")
            .field("snapshot", &self.snapshot())
            .field("payload_sink", &self.payload_sink)
            .finish()
    }
}

impl<P: CoalescingWakePayload> Wakeup<P> {
    pub fn signal_filesystem(&self) {
        self.signal(WAKE_FILESYSTEM);
    }

    pub fn observe_payload(&self, payload: &P) {
        self.observe_payload_before_dispatch(payload, || {});
    }

    #[doc(hidden)]
    pub fn observe_payload_before_dispatch(&self, payload: &P, before_dispatch: impl FnOnce()) {
        if payload.is_empty() {
            return;
        }
        self.lock_state().observed_payload.merge(payload.clone());
        before_dispatch();
        if self.payload_sink.dispatch(payload) {
            self.lock_state().observed_payload = P::default();
        }
    }

    pub fn signal_payload(&self, payload: P) {
        if payload.is_empty() {
            return;
        }
        let mut state = self.lock_state();
        state.pending |= WAKE_FILESYSTEM;
        state.filesystem_signals = state.filesystem_signals.saturating_add(1);
        state.payload.merge(payload);
        self.changed.notify_one();
    }

    pub fn install_payload_sink(&self, sink: PayloadSink<P>) {
        self.payload_sink.set(Some(sink));
        let pending = self.lock_state().observed_payload.clone();
        if !pending.is_empty() && self.payload_sink.dispatch(&pending) {
            self.lock_state().observed_payload = P::default();
        }
    }

    pub fn has_payload_sink(&self) -> bool {
        self.payload_sink.is_installed()
    }

    pub fn signal_ipc(&self) {
        self.signal(WAKE_IPC);
    }

    pub fn signal_shutdown(&self) {
        self.signal(WAKE_SHUTDOWN);
    }

    pub fn shutdown_requested(&self) -> bool {
        self.lock_state().shutdown_signals != 0
    }

    fn signal(&self, reason: u8) {
        let mut state = self.lock_state();
        state.pending |= reason;
        if reason == WAKE_FILESYSTEM {
            state.filesystem_signals = state.filesystem_signals.saturating_add(1);
        } else if reason == WAKE_IPC {
            state.ipc_signals = state.ipc_signals.saturating_add(1);
        } else if reason == WAKE_SHUTDOWN {
            state.shutdown_signals = state.shutdown_signals.saturating_add(1);
        }
        self.changed.notify_one();
    }

    pub fn wait(&self, timeout: Duration) -> Wake<P> {
        let mut state = self.lock_state();
        state.blocking_waits = state.blocking_waits.saturating_add(1);
        let timed_out = if state.pending == 0 {
            let (next, result) = self
                .changed
                .wait_timeout_while(state, timeout, |state| state.pending == 0)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next;
            result.timed_out() && state.pending == 0
        } else {
            false
        };
        if timed_out {
            state.timeout_wakeups = state.timeout_wakeups.saturating_add(1);
        }
        let pending = std::mem::take(&mut state.pending);
        Wake {
            filesystem: pending & WAKE_FILESYSTEM != 0,
            shutdown: pending & WAKE_SHUTDOWN != 0,
            timed_out,
            payload: std::mem::take(&mut state.payload),
        }
    }

    pub fn pending_payload(&self) -> P {
        self.lock_state().payload.clone()
    }

    pub fn record_cycle(&self, did_work: bool) {
        let mut state = self.lock_state();
        if did_work {
            state.work_cycles = state.work_cycles.saturating_add(1);
        } else {
            state.no_work_cycles = state.no_work_cycles.saturating_add(1);
        }
    }

    pub fn record_scheduled_retry_wakeup(&self) {
        let mut state = self.lock_state();
        state.scheduled_retry_wakeups = state.scheduled_retry_wakeups.saturating_add(1);
    }

    pub fn record_scheduled_refresh_wakeup(&self) {
        let mut state = self.lock_state();
        state.scheduled_refresh_wakeups = state.scheduled_refresh_wakeups.saturating_add(1);
    }

    pub fn snapshot(&self) -> WakeupSnapshot {
        let state = self.lock_state();
        WakeupSnapshot {
            blocking_waits: state.blocking_waits,
            filesystem_signals: state.filesystem_signals,
            ipc_signals: state.ipc_signals,
            shutdown_signals: state.shutdown_signals,
            timeout_wakeups: state.timeout_wakeups,
            scheduled_retry_wakeups: state.scheduled_retry_wakeups,
            scheduled_refresh_wakeups: state.scheduled_refresh_wakeups,
            work_cycles: state.work_cycles,
            no_work_cycles: state.no_work_cycles,
        }
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, WakeupState<P>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
