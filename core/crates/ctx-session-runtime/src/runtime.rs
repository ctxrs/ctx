use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, mpsc, watch, Mutex};

use ctx_core::ids::{SessionId, TaskId, TurnId, WorkspaceId};
use ctx_core::models::{
    Session, SessionEvent, SessionEventType, SessionHeadDelta, SessionHeadSnapshot,
    SessionSummaryDelta, SessionTurn, SessionTurnToolSummary,
};
use ctx_session_tools::order_seq::OrderSeqState;
use ctx_store::Store;

mod lifecycle;
mod publication;
mod scheduler;
mod state;
mod task_delta;

pub use lifecycle::{SessionLifecycleHost, SessionPinState};
pub use publication::{
    SessionEventPublicationHost, SessionHeadRefreshHost, SessionHeadRefreshLoad,
    SessionReplayCursor,
};
pub use state::{
    SessionCacheSweepStats, SessionHeadCacheKey, SessionRuntimeCacheDebugStats,
    SessionRuntimeStats, TimedEntry,
};
pub use task_delta::{ActiveTaskRefreshEntry, SessionTaskDeltaRefreshHost};

#[cfg(test)]
mod tests;

const DEFAULT_PROVIDER_INACTIVITY_TIMEOUT_SECS: u64 = 30 * 60;
const TASK_DELTA_REFRESH_DEBOUNCE_MS: u64 = 250;

pub struct SessionRuntime<SchedulerCommand> {
    pub session_head_cache:
        Mutex<HashMap<SessionId, TimedEntry<HashMap<SessionHeadCacheKey, SessionHeadSnapshot>>>>,
    pub schedulers: Mutex<HashMap<SessionId, TimedEntry<mpsc::Sender<SchedulerCommand>>>>,
    pub provider_inactivity_timeout: Mutex<Duration>,
    pub broadcasters: Mutex<HashMap<SessionId, TimedEntry<broadcast::Sender<SessionEvent>>>>,
    pub session_event_heads: Mutex<HashMap<SessionId, TimedEntry<watch::Sender<i64>>>>,
    pub order_seq_states: Mutex<HashMap<SessionId, TimedEntry<Arc<Mutex<OrderSeqState>>>>>,
    pub active_task_refreshes: Arc<Mutex<HashMap<TaskId, ActiveTaskRefreshEntry>>>,
    pub task_session_creation_locks:
        Mutex<HashMap<TaskId, std::sync::Weak<tokio::sync::Mutex<()>>>>,
    pub running_sessions: Arc<Mutex<HashSet<SessionId>>>,
    pub session_pins: Mutex<HashMap<SessionId, SessionPinState>>,
    pub session_meta_cache: Mutex<HashMap<SessionId, TimedEntry<Session>>>,
}

impl<SchedulerCommand> SessionRuntime<SchedulerCommand> {
    pub fn new(provider_inactivity_timeout: Duration) -> Self {
        Self {
            session_head_cache: Mutex::new(HashMap::new()),
            schedulers: Mutex::new(HashMap::new()),
            provider_inactivity_timeout: Mutex::new(provider_inactivity_timeout),
            broadcasters: Mutex::new(HashMap::new()),
            session_event_heads: Mutex::new(HashMap::new()),
            order_seq_states: Mutex::new(HashMap::new()),
            active_task_refreshes: Arc::new(Mutex::new(HashMap::new())),
            task_session_creation_locks: Mutex::new(HashMap::new()),
            running_sessions: Arc::new(Mutex::new(HashSet::new())),
            session_pins: Mutex::new(HashMap::new()),
            session_meta_cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn new_from_env() -> Self {
        Self::new(provider_inactivity_timeout_from_env())
    }

    pub async fn task_session_creation_lock(&self, task_id: TaskId) -> Arc<Mutex<()>> {
        let mut locks = self.task_session_creation_locks.lock().await;
        locks.retain(|_, weak| weak.upgrade().is_some());
        match locks.get(&task_id).and_then(std::sync::Weak::upgrade) {
            Some(lock) => lock,
            None => {
                let lock = Arc::new(Mutex::new(()));
                locks.insert(task_id, Arc::downgrade(&lock));
                lock
            }
        }
    }

    pub async fn is_running(&self, session_id: SessionId) -> bool {
        self.running_sessions.lock().await.contains(&session_id)
    }

    pub async fn list_running_sessions(&self) -> Vec<SessionId> {
        self.running_sessions.lock().await.iter().copied().collect()
    }

    pub async fn provider_inactivity_timeout(&self) -> Duration {
        *self.provider_inactivity_timeout.lock().await
    }

    pub async fn set_provider_inactivity_timeout(&self, timeout: Duration) {
        *self.provider_inactivity_timeout.lock().await = timeout;
    }
}

pub fn provider_inactivity_timeout_from_env() -> Duration {
    std::env::var("CTX_PROVIDER_TURN_INACTIVITY_TIMEOUT_MS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|millis| *millis > 0)
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_secs(DEFAULT_PROVIDER_INACTIVITY_TIMEOUT_SECS))
}
