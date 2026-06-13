use std::time::{Duration, Instant};

use super::DaemonState;

mod sweep;

const DEFAULT_SESSION_CACHE_TTL_HOURS: u64 = 24;
const DEFAULT_WORKSPACE_CACHE_TTL_DAYS: u64 = 1;
const DEFAULT_CACHE_SWEEP_INTERVAL_SECS: u64 = 5 * 60;

#[derive(Clone, Copy, Debug)]
pub struct CacheSweepConfig {
    pub session_ttl: Duration,
    pub workspace_ttl: Duration,
    pub interval: Duration,
}

impl CacheSweepConfig {
    pub fn from_env() -> Self {
        let session_ttl_hours = std::env::var("CTX_SESSION_CACHE_TTL_HOURS")
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(DEFAULT_SESSION_CACHE_TTL_HOURS);
        let workspace_ttl_days = std::env::var("CTX_WORKSPACE_CACHE_TTL_DAYS")
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(DEFAULT_WORKSPACE_CACHE_TTL_DAYS);
        let interval_secs = std::env::var("CTX_CACHE_SWEEP_INTERVAL_SECS")
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(DEFAULT_CACHE_SWEEP_INTERVAL_SECS);
        Self {
            session_ttl: Duration::from_secs(session_ttl_hours * 60 * 60),
            workspace_ttl: Duration::from_secs(workspace_ttl_days * 24 * 60 * 60),
            interval: Duration::from_secs(interval_secs.max(30)),
        }
    }
}

#[derive(Default, Debug)]
pub struct CacheSweepStats {
    pub session_head_evicted: usize,
    pub session_meta_evicted: usize,
    pub schedulers_evicted: usize,
    pub broadcasters_evicted: usize,
    pub session_event_heads_evicted: usize,
    pub file_completions_evicted: usize,
    pub workspace_file_completions_evicted: usize,
    pub git_status_evicted: usize,
    pub worktree_vcs_evicted: usize,
    pub workspace_snapshot_evicted: usize,
    pub workspace_heads_evicted: usize,
    pub worktree_bootstrap_evicted: usize,
    pub workspace_stores_evicted: usize,
}

impl CacheSweepStats {
    pub fn total_evicted(&self) -> usize {
        self.session_head_evicted
            + self.session_meta_evicted
            + self.schedulers_evicted
            + self.broadcasters_evicted
            + self.session_event_heads_evicted
            + self.file_completions_evicted
            + self.workspace_file_completions_evicted
            + self.git_status_evicted
            + self.worktree_vcs_evicted
            + self.workspace_snapshot_evicted
            + self.workspace_heads_evicted
            + self.worktree_bootstrap_evicted
            + self.workspace_stores_evicted
    }
}

#[derive(Debug)]
pub struct TimedEntry<T> {
    pub value: T,
    pub last_access: Instant,
}

impl<T> TimedEntry<T> {
    pub fn new(value: T) -> Self {
        Self {
            value,
            last_access: Instant::now(),
        }
    }

    pub fn touch(&mut self) {
        self.last_access = Instant::now();
    }

    pub fn touch_at(&mut self, now: Instant) {
        self.last_access = now;
    }
}
