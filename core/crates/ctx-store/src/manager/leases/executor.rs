impl StoreCloseExecutor {
    fn new() -> Result<Self> {
        let (tx, mut rx) = tokio_mpsc::unbounded_channel::<StoreCloseJob>();
        let (ready_tx, ready_rx) = sync_channel::<Result<()>>(1);
        std::thread::Builder::new()
            .name("ctx-store-close-executor".to_string())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|err| anyhow!("failed to build store close runtime: {err}"));
                match runtime {
                    Ok(runtime) => {
                        let _ = ready_tx.send(Ok(()));
                        runtime.block_on(async move {
                            let mut closes = tokio::task::JoinSet::new();
                            loop {
                                tokio::select! {
                                    maybe_job = rx.recv() => {
                                        match maybe_job {
                                            Some(job) => {
                                                closes.spawn(close_store_and_finish(
                                                    job.store,
                                                    Arc::clone(&job.registry),
                                                    job.workspace_id,
                                                    Arc::clone(&job.notify),
                                                ));
                                            }
                                            None => break,
                                        }
                                    }
                                    result = closes.join_next(), if !closes.is_empty() => {
                                        if let Some(Err(err)) = result {
                                            tracing::warn!("store close task failed: {err:#}");
                                        }
                                    }
                                }
                            }
                            while let Some(result) = closes.join_next().await {
                                if let Err(err) = result {
                                    tracing::warn!("store close task failed: {err:#}");
                                }
                            }
                        });
                    }
                    Err(err) => {
                        let _ = ready_tx.send(Err(err));
                    }
                }
            })
            .map_err(|err| anyhow!("failed to spawn store close executor thread: {err}"))?;
        ready_rx
            .recv()
            .map_err(|err| anyhow!("store close executor startup failed: {err}"))??;
        Ok(Self { tx })
    }

    fn submit(&self, job: StoreCloseJob) -> Result<()> {
        self.tx
            .send(job)
            .map_err(|_| anyhow!("store close executor is not available"))
    }
}
