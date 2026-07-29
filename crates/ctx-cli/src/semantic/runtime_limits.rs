pub(super) const SEMANTIC_SEARCH_CANDIDATES: usize = 200;
pub(super) const SEMANTIC_SOFT_FILTER_SEARCH_CANDIDATES: usize = 1_000;
pub(super) const SEMANTIC_EXACT_TOP_K_MAX: usize = 4_096;
pub(super) const SEMANTIC_EXACT_QUERY_CONCURRENCY: usize = 2;
pub(super) const SEMANTIC_CHUNK_TARGET_CHARS: usize =
    ctx_history_index::SEMANTIC_CHUNK_TARGET_CHARS;
pub(crate) const SEMANTIC_CHUNK_OVERLAP_CHARS: usize =
    ctx_history_index::SEMANTIC_CHUNK_OVERLAP_CHARS;
pub(super) const SEMANTIC_SOURCE_MAX_CHARS: usize = ctx_history_index::SEMANTIC_SOURCE_MAX_CHARS;
#[cfg(ctx_semantic_fastembed)]
pub(super) const SEMANTIC_EMBED_THREADS_MAX: usize = 8;
#[cfg(ctx_semantic_fastembed)]
pub(super) const SEMANTIC_EMBED_BATCH_MAX: usize = 512;
pub(super) const SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT: usize = 512;
pub(super) const SEMANTIC_WORKER_LOCK_FILE: &str = "semantic-worker.lock";
pub(super) const SEMANTIC_WORKER_STATUS_FILE: &str = "semantic-worker.json";
pub(super) const SEMANTIC_WORKER_BATCH_DEFAULT: usize = 5_000;
pub(crate) const SEMANTIC_WORKER_BATCH_MAX: usize = 1_000_000;
pub(super) const SEMANTIC_WORKER_MAX_SECONDS_DEFAULT: u64 = 60;
pub(crate) const SEMANTIC_WORKER_MAX_SECONDS_CAP: u64 = 86_400;
pub(super) const SEMANTIC_MODEL_INIT_MIN_REMAINING_SECS: u64 = 15;
pub(super) const SEMANTIC_VECTOR_BUSY_TIMEOUT_MS: u64 = 30_000;
pub(super) const SEMANTIC_PRUNE_EVENTS_PER_PASS: usize = 256;
pub(super) const SEMANTIC_DEADLINE_CHUNKS_PER_SECOND: usize = 3;
pub(super) const SEMANTIC_DEADLINE_MIN_CHUNK_BATCH: usize = 16;
pub(super) const DAEMON_DIR: &str = "daemon";
pub(super) const DAEMON_JOBS_DIR: &str = "jobs";
pub(super) const DAEMON_LOCK_FILE: &str = "daemon.lock";
pub(super) const DAEMON_STATUS_FILE: &str = "status.json";
#[cfg(unix)]
pub(super) const DAEMON_QUERY_SOCKET_FILE: &str = "query.sock";
pub(super) const DAEMON_QUERY_ENDPOINT_FILE: &str = "query-endpoint.json";
pub(super) const DAEMON_HISTORY_REFRESH_JOB_FILE: &str = "history-refresh.json";
pub(super) const DAEMON_SEMANTIC_JOB_FILE: &str = "semantic-index.json";
pub(super) const DAEMON_IDLE_EXIT_SECONDS_DEFAULT: u64 = 30;
pub(crate) const DAEMON_IDLE_EXIT_SECONDS_CAP: u64 = 24 * 60 * 60;
pub(super) const DAEMON_LOOP_INTERVAL_SECONDS_DEFAULT: u64 = 5;
pub(super) const DAEMON_AUTOSTART_IDLE_EXIT_SECONDS_DEFAULT: u64 = 5;
pub(super) const DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS_DEFAULT: u64 = 5;
pub(super) const DAEMON_BACKGROUND_CHILD_ENV: &str = "CTX_DAEMON_BACKGROUND_CHILD";
pub(super) const DAEMON_AUTOSTART_OFF_ENV: &str = "CTX_DAEMON_AUTOSTART_OFF";
pub(super) const DAEMON_SEMANTIC_BOOTSTRAP_PASSES_BEFORE_REFRESH: usize = 1;
pub(super) const DAEMON_LOCK_STALE_AFTER_MS: i64 = 25 * 60 * 60 * 1_000;
pub(super) const PID_LOCK_INCOMPLETE_GRACE: StdDuration = StdDuration::from_secs(30);
pub(super) const PID_LOCK_PROTOCOL: &str = "advisory-v1";
pub(super) const PID_LOCK_ACQUIRE_ATTEMPTS: usize = 20;
pub(super) const PID_LOCK_ACQUIRE_RETRY: StdDuration = StdDuration::from_millis(2);
pub(super) const DAEMON_SEMANTIC_RESERVE_GRACE_SECS: u64 = 10;
pub(super) const DAEMON_MIN_REMAINING_FOR_JOB_SECS: u64 = 2;
use std::time::Duration as StdDuration;
