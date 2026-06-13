impl EventLogRuntime {
    pub(super) async fn load(pool: &Pool<Sqlite>) -> Result<Self> {
        let last_seq: Option<i64> = sqlx::query_scalar("SELECT MAX(seq) FROM session_events")
            .fetch_one(pool)
            .await?;
        let checkpoint_seq: Option<i64> =
            sqlx::query_scalar("SELECT checkpoint_seq FROM event_log_checkpoints WHERE id = 1")
                .fetch_optional(pool)
                .await?
                .flatten();
        let max_seq = last_seq.unwrap_or(0).max(checkpoint_seq.unwrap_or(0));
        Ok(Self {
            next_seq: AtomicI64::new(max_seq.saturating_add(1)),
            config: EventLogConfig::from_env(),
            persister: OnceLock::new(),
        })
    }

    pub(super) fn start_persister(&self, store: Store) -> Result<()> {
        if self.persister.get().is_some() {
            return Ok(());
        }
        let initial_seq = self.next_seq.load(Ordering::Relaxed).saturating_sub(1);
        let persister = EventLogPersister::spawn(store, self.config, initial_seq)?;
        let _ = self.persister.set(persister);
        Ok(())
    }

    pub(super) fn next_seq(&self) -> i64 {
        self.next_seq.fetch_add(1, Ordering::Relaxed)
    }

    pub(super) async fn enqueue(&self, event: SessionEvent) -> Result<()> {
        match self.persister.get() {
            Some(persister) => persister.enqueue(event).await,
            None => Err(anyhow::anyhow!("event log persister unavailable")),
        }
    }

    pub(super) async fn flush(&self) -> Result<()> {
        match self.persister.get() {
            Some(persister) => persister.flush().await,
            None => Ok(()),
        }
    }

    pub(super) async fn shutdown(&self) -> Result<()> {
        match self.persister.get() {
            Some(persister) => persister.shutdown().await,
            None => Ok(()),
        }
    }

    pub(super) fn shutdown_blocking(&self) -> Result<()> {
        match self.persister.get() {
            Some(persister) => persister.shutdown_blocking(),
            None => Ok(()),
        }
    }
}

#[derive(Clone)]
pub(super) struct EventLogPersister {
    tx: mpsc::Sender<EventLogCommand>,
}

pub(super) enum EventLogCommand {
    Event(SessionEvent),
    Flush(oneshot::Sender<Result<()>>),
    Shutdown(oneshot::Sender<Result<()>>),
}

impl EventLogPersister {
    fn spawn(store: Store, config: EventLogConfig, initial_seq: i64) -> Result<Self> {
        let (tx, mut rx) = mpsc::channel(EVENT_LOG_QUEUE_CAPACITY);
        store_background_runtime()?.spawn(async move {
            let mut buffer: Vec<SessionEvent> = Vec::new();
            let mut flush_waiters: Vec<oneshot::Sender<Result<()>>> = Vec::new();
            let mut last_applied_seq = initial_seq;
            let mut last_checkpoint_seq = initial_seq;
            let mut flush_interval = tokio::time::interval(config.flush_interval);
            let mut checkpoint_interval = tokio::time::interval(config.checkpoint_interval);

            loop {
                tokio::select! {
                    cmd = rx.recv() => {
                        match cmd {
                            Some(EventLogCommand::Event(event)) => {
                                last_applied_seq = last_applied_seq.max(event.seq);
                                buffer.push(event);
                                if buffer.len() >= config.batch_size {
                                    if let Err(err) = flush_event_batch(&store, &mut buffer).await {
                                        tracing::warn!("event log flush failed: {err:#}");
                                    }
                                }
                            }
                            Some(EventLogCommand::Flush(tx)) => {
                                flush_waiters.push(tx);
                                let result = if buffer.is_empty() {
                                    Ok(())
                                } else {
                                    flush_event_batch(&store, &mut buffer).await
                                };
                                for waiter in flush_waiters.drain(..) {
                                    let send_result = match &result {
                                        Ok(()) => Ok(()),
                                        Err(err) => Err(anyhow::anyhow!("{err:#}")),
                                    };
                                    let _ = waiter.send(send_result);
                                }
                            }
                            Some(EventLogCommand::Shutdown(tx)) => {
                                let result = if buffer.is_empty() {
                                    Ok(())
                                } else {
                                    flush_event_batch(&store, &mut buffer).await
                                };
                                let checkpoint_result = if result.is_ok() && last_applied_seq > last_checkpoint_seq {
                                    store
                                        .upsert_event_log_checkpoint(last_applied_seq, None)
                                        .await
                                        .map(|_| ())
                                } else {
                                    Ok(())
                                };
                                let final_result = result.and(checkpoint_result);
                                let send_result = match &final_result {
                                    Ok(()) => Ok(()),
                                    Err(err) => Err(anyhow::anyhow!("{err:#}")),
                                };
                                let _ = tx.send(send_result);
                                return;
                            }
                            None => {
                                let _ = flush_event_batch(&store, &mut buffer).await;
                                return;
                            }
                        }
                    }
                    _ = flush_interval.tick() => {
                        if !buffer.is_empty() {
                            if let Err(err) = flush_event_batch(&store, &mut buffer).await {
                                tracing::warn!("event log flush failed: {err:#}");
                            }
                        }
                    }
                    _ = checkpoint_interval.tick() => {
                        if last_applied_seq > last_checkpoint_seq {
                            let result = store
                                .upsert_event_log_checkpoint(last_applied_seq, None)
                                .await;
                            if let Err(err) = result {
                                tracing::warn!("event log checkpoint failed: {err:#}");
                            } else {
                                last_checkpoint_seq = last_applied_seq;
                            }
                        }
                    }
                }
            }
        });
        Ok(Self { tx })
    }

    async fn enqueue(&self, event: SessionEvent) -> Result<()> {
        self.tx
            .send(EventLogCommand::Event(event))
            .await
            .context("enqueueing session event for persistence")
    }

    async fn flush(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(EventLogCommand::Flush(tx))
            .await
            .context("requesting event log flush")?;
        rx.await.context("waiting for event log flush")?
    }

    async fn shutdown(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(EventLogCommand::Shutdown(tx)).await.is_err() {
            return Ok(());
        }
        rx.await.context("waiting for event log shutdown")?
    }

    fn shutdown_blocking(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        if self
            .tx
            .blocking_send(EventLogCommand::Shutdown(tx))
            .is_err()
        {
            return Ok(());
        }
        rx.blocking_recv()
            .context("waiting for event log shutdown")?
    }
}

pub(super) async fn flush_event_batch(store: &Store, buffer: &mut Vec<SessionEvent>) -> Result<()> {
    if buffer.is_empty() {
        return Ok(());
    }
    let batch = std::mem::take(buffer);
    if let Err(err) = store.persist_session_events_batch(&batch).await {
        buffer.extend(batch);
        return Err(err);
    }
    Ok(())
}
