fn store_background_runtime() -> Result<&'static tokio::runtime::Runtime> {
    static RUNTIME: OnceLock<std::result::Result<tokio::runtime::Runtime, String>> =
        OnceLock::new();
    match RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("ctx-store-bg")
            .enable_all()
            .build()
            .map_err(|err| format!("failed to initialize ctx-store background runtime: {err}"))
    }) {
        Ok(runtime) => Ok(runtime),
        Err(err) => Err(anyhow::anyhow!(err.clone())),
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct EventLogConfig {
    flush_interval: Duration,
    batch_size: usize,
    checkpoint_interval: Duration,
}

impl EventLogConfig {
    fn from_env() -> Self {
        let flush_ms = env_u64("CTX_EVENT_LOG_FLUSH_MS").unwrap_or(DEFAULT_EVENT_LOG_FLUSH_MS);
        let batch_size =
            env_usize("CTX_EVENT_LOG_BATCH_SIZE").unwrap_or(DEFAULT_EVENT_LOG_BATCH_SIZE);
        let checkpoint_ms =
            env_u64("CTX_EVENT_LOG_CHECKPOINT_MS").unwrap_or(DEFAULT_EVENT_LOG_CHECKPOINT_MS);
        Self {
            flush_interval: Duration::from_millis(flush_ms.max(1)),
            batch_size: batch_size.max(1),
            checkpoint_interval: Duration::from_millis(checkpoint_ms.max(1)),
        }
    }
}

pub(super) struct EventLogRuntime {
    next_seq: AtomicI64,
    config: EventLogConfig,
    persister: OnceLock<EventLogPersister>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct ActiveHeadProjectionConfig {
    flush_interval: Duration,
}

impl ActiveHeadProjectionConfig {
    fn from_env() -> Self {
        let flush_ms = env_u64("CTX_ACTIVE_HEAD_PROJECTION_FLUSH_MS")
            .unwrap_or(DEFAULT_ACTIVE_HEAD_PROJECTION_FLUSH_MS);
        Self {
            flush_interval: Duration::from_millis(flush_ms.max(1)),
        }
    }
}

pub(super) struct ActiveHeadProjectionRuntime {
    config: ActiveHeadProjectionConfig,
    projector: OnceLock<ActiveHeadProjectionProjector>,
}

impl ActiveHeadProjectionRuntime {
    pub(super) fn new() -> Self {
        Self {
            config: ActiveHeadProjectionConfig::from_env(),
            projector: OnceLock::new(),
        }
    }

    pub(super) fn start_projector(&self, store: Store) -> Result<()> {
        if self.projector.get().is_some() {
            return Ok(());
        }
        let projector = ActiveHeadProjectionProjector::spawn(store, self.config)?;
        let _ = self.projector.set(projector);
        Ok(())
    }

    pub(super) async fn enqueue(
        &self,
        session_id: SessionId,
        last_event_seq: Option<i64>,
    ) -> Result<()> {
        match self.projector.get() {
            Some(projector) => projector.enqueue(session_id, last_event_seq).await,
            None => Err(anyhow::anyhow!(
                "active head projection projector unavailable"
            )),
        }
    }

    pub(super) async fn flush(&self) -> Result<()> {
        match self.projector.get() {
            Some(projector) => projector.flush().await,
            None => Ok(()),
        }
    }

    pub(super) async fn shutdown(&self) -> Result<()> {
        match self.projector.get() {
            Some(projector) => projector.shutdown().await,
            None => Ok(()),
        }
    }

    pub(super) fn shutdown_blocking(&self) -> Result<()> {
        match self.projector.get() {
            Some(projector) => projector.shutdown_blocking(),
            None => Ok(()),
        }
    }
}

#[derive(Clone)]
pub(super) struct ActiveHeadProjectionProjector {
    tx: mpsc::Sender<ActiveHeadProjectionCommand>,
}

pub(super) enum ActiveHeadProjectionCommand {
    Dirty {
        session_id: SessionId,
        last_event_seq: Option<i64>,
    },
    Flush(oneshot::Sender<Result<()>>),
    Shutdown(oneshot::Sender<Result<()>>),
}

impl ActiveHeadProjectionProjector {
    fn spawn(store: Store, config: ActiveHeadProjectionConfig) -> Result<Self> {
        let (tx, mut rx) = mpsc::channel(ACTIVE_HEAD_PROJECTION_QUEUE_CAPACITY);
        store_background_runtime()?.spawn(async move {
            let mut dirty: HashMap<SessionId, Option<i64>> = HashMap::new();
            let mut flush_waiters: Vec<oneshot::Sender<Result<()>>> = Vec::new();
            // The configured interval is a debounce window; an immediate first tick defeats
            // bulk writers that temporarily lengthen it to keep projection refresh explicit.
            let mut flush_interval = tokio::time::interval_at(
                tokio::time::Instant::now() + config.flush_interval,
                config.flush_interval,
            );

            loop {
                tokio::select! {
                    cmd = rx.recv() => {
                        match cmd {
                            Some(ActiveHeadProjectionCommand::Dirty { session_id, last_event_seq }) => {
                                let coalesced = if let Some(existing) = dirty.get_mut(&session_id) {
                                    *existing = merge_projection_last_event_seq(*existing, last_event_seq);
                                    true
                                } else {
                                    dirty.insert(session_id, last_event_seq);
                                    false
                                };
                                record_active_head_projection_enqueue(coalesced);
                                set_active_head_projection_pending_sessions(dirty.len());
                            }
                            Some(ActiveHeadProjectionCommand::Flush(tx)) => {
                                flush_waiters.push(tx);
                                let result = flush_active_head_projection_batch(&store, &mut dirty).await;
                                set_active_head_projection_pending_sessions(dirty.len());
                                for waiter in flush_waiters.drain(..) {
                                    let send_result = match &result {
                                        Ok(()) => Ok(()),
                                        Err(err) => Err(anyhow::anyhow!("{err:#}")),
                                    };
                                    let _ = waiter.send(send_result);
                                }
                            }
                            Some(ActiveHeadProjectionCommand::Shutdown(tx)) => {
                                let result = flush_active_head_projection_batch(&store, &mut dirty).await;
                                set_active_head_projection_pending_sessions(0);
                                let send_result = match &result {
                                    Ok(()) => Ok(()),
                                    Err(err) => Err(anyhow::anyhow!("{err:#}")),
                                };
                                let _ = tx.send(send_result);
                                return;
                            }
                            None => {
                                let _ = flush_active_head_projection_batch(&store, &mut dirty).await;
                                set_active_head_projection_pending_sessions(0);
                                return;
                            }
                        }
                    }
                    _ = flush_interval.tick() => {
                        if !dirty.is_empty() {
                            if let Err(err) = flush_active_head_projection_batch(&store, &mut dirty).await {
                                tracing::warn!("active head projection flush failed: {err:#}");
                            }
                            set_active_head_projection_pending_sessions(dirty.len());
                        }
                    }
                }
            }
        });
        Ok(Self { tx })
    }

    async fn enqueue(&self, session_id: SessionId, last_event_seq: Option<i64>) -> Result<()> {
        self.tx
            .send(ActiveHeadProjectionCommand::Dirty {
                session_id,
                last_event_seq,
            })
            .await
            .context("enqueueing active head projection refresh")
    }

    async fn flush(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(ActiveHeadProjectionCommand::Flush(tx))
            .await
            .context("requesting active head projection flush")?;
        rx.await
            .context("waiting for active head projection flush")?
    }

    async fn shutdown(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        if self
            .tx
            .send(ActiveHeadProjectionCommand::Shutdown(tx))
            .await
            .is_err()
        {
            return Ok(());
        }
        rx.await
            .context("waiting for active head projection shutdown")?
    }

    fn shutdown_blocking(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        if self
            .tx
            .blocking_send(ActiveHeadProjectionCommand::Shutdown(tx))
            .is_err()
        {
            return Ok(());
        }
        rx.blocking_recv()
            .context("waiting for active head projection shutdown")?
    }
}

fn merge_projection_last_event_seq(current: Option<i64>, next: Option<i64>) -> Option<i64> {
    match (current, next) {
        (Some(current), Some(next)) => Some(current.max(next)),
        (Some(current), None) => Some(current),
        (None, Some(next)) => Some(next),
        (None, None) => None,
    }
}

async fn flush_active_head_projection_batch(
    store: &Store,
    dirty: &mut HashMap<SessionId, Option<i64>>,
) -> Result<()> {
    if dirty.is_empty() {
        return Ok(());
    }
    let mut batch: std::collections::VecDeque<_> = dirty.drain().collect();
    let session_count = batch.len() as u64;
    while let Some((session_id, last_event_seq)) = batch.pop_front() {
        if let Err(err) = store
            .refresh_active_snapshot_head(session_id, last_event_seq)
            .await
        {
            dirty.insert(session_id, last_event_seq);
            for (remaining_session_id, remaining_last_event_seq) in batch {
                dirty.insert(remaining_session_id, remaining_last_event_seq);
            }
            record_active_head_projection_flush(session_count, true);
            return Err(err);
        }
    }
    record_active_head_projection_flush(session_count, false);
    Ok(())
}

struct ActiveHeadProjectionMetrics {
    pending_sessions: AtomicU64,
    enqueued_updates: AtomicU64,
    coalesced_updates: AtomicU64,
    flushes: AtomicU64,
    flushed_sessions: AtomicU64,
    errors: AtomicU64,
}

impl ActiveHeadProjectionMetrics {
    fn new() -> Self {
        Self {
            pending_sessions: AtomicU64::new(0),
            enqueued_updates: AtomicU64::new(0),
            coalesced_updates: AtomicU64::new(0),
            flushes: AtomicU64::new(0),
            flushed_sessions: AtomicU64::new(0),
            errors: AtomicU64::new(0),
        }
    }

    fn snapshot(&self) -> ActiveHeadProjectionMetricsSnapshot {
        ActiveHeadProjectionMetricsSnapshot {
            pending_sessions: self.pending_sessions.load(Ordering::Relaxed),
            enqueued_updates: self.enqueued_updates.load(Ordering::Relaxed),
            coalesced_updates: self.coalesced_updates.load(Ordering::Relaxed),
            flushes: self.flushes.load(Ordering::Relaxed),
            flushed_sessions: self.flushed_sessions.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Copy)]
struct ActiveHeadProjectionMetricsSnapshot {
    pending_sessions: u64,
    enqueued_updates: u64,
    coalesced_updates: u64,
    flushes: u64,
    flushed_sessions: u64,
    errors: u64,
}

impl ActiveHeadProjectionMetricsSnapshot {
    fn delta(self, previous: Self) -> Self {
        Self {
            pending_sessions: self.pending_sessions,
            enqueued_updates: self
                .enqueued_updates
                .saturating_sub(previous.enqueued_updates),
            coalesced_updates: self
                .coalesced_updates
                .saturating_sub(previous.coalesced_updates),
            flushes: self.flushes.saturating_sub(previous.flushes),
            flushed_sessions: self
                .flushed_sessions
                .saturating_sub(previous.flushed_sessions),
            errors: self.errors.saturating_sub(previous.errors),
        }
    }
}

fn active_head_projection_metrics() -> &'static ActiveHeadProjectionMetrics {
    static METRICS: OnceLock<ActiveHeadProjectionMetrics> = OnceLock::new();
    static LOGGER: OnceLock<()> = OnceLock::new();
    let metrics = METRICS.get_or_init(ActiveHeadProjectionMetrics::new);
    LOGGER.get_or_init(|| {
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let mut interval =
                    tokio::time::interval(Duration::from_secs(WRITE_METRICS_INTERVAL_SECS));
                let mut previous = metrics.snapshot();
                loop {
                    interval.tick().await;
                    let current = metrics.snapshot();
                    let delta = current.delta(previous);
                    previous = current;
                    if delta.enqueued_updates == 0
                        && delta.flushes == 0
                        && delta.coalesced_updates == 0
                        && delta.errors == 0
                        && current.pending_sessions == 0
                    {
                        continue;
                    }
                    info!(
                        target: "ctx_store.active_head_projection",
                        interval_s = WRITE_METRICS_INTERVAL_SECS,
                        pending_sessions = current.pending_sessions,
                        enqueued_updates = delta.enqueued_updates,
                        coalesced_updates = delta.coalesced_updates,
                        flushes = delta.flushes,
                        flushed_sessions = delta.flushed_sessions,
                        errors = delta.errors,
                    );
                }
            });
        }
    });
    metrics
}

fn set_active_head_projection_pending_sessions(count: usize) {
    if !write_metrics_enabled() && !env_flag_enabled("CTX_ACTIVE_HEAD_PROJECTION_METRICS") {
        return;
    }
    active_head_projection_metrics()
        .pending_sessions
        .store(count as u64, Ordering::Relaxed);
}

fn record_active_head_projection_enqueue(coalesced: bool) {
    if !write_metrics_enabled() && !env_flag_enabled("CTX_ACTIVE_HEAD_PROJECTION_METRICS") {
        return;
    }
    let metrics = active_head_projection_metrics();
    metrics.enqueued_updates.fetch_add(1, Ordering::Relaxed);
    if coalesced {
        metrics.coalesced_updates.fetch_add(1, Ordering::Relaxed);
    }
}

fn record_active_head_projection_flush(flushed_sessions: u64, errored: bool) {
    if !write_metrics_enabled() && !env_flag_enabled("CTX_ACTIVE_HEAD_PROJECTION_METRICS") {
        return;
    }
    let metrics = active_head_projection_metrics();
    metrics.flushes.fetch_add(1, Ordering::Relaxed);
    metrics
        .flushed_sessions
        .fetch_add(flushed_sessions, Ordering::Relaxed);
    if errored {
        metrics.errors.fetch_add(1, Ordering::Relaxed);
    }
}
