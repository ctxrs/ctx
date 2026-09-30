use crate::*;
use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

pub fn serve_blocking(server: Arc<HistoryServer>) -> Result<()> {
    serve_blocking_with_ready(server, |_| Ok(()))
}

/// A failed bind skips ready; callback failure closes the listener.
pub fn serve_blocking_with_ready(
    server: Arc<HistoryServer>,
    ready: impl FnOnce(SocketAddr) -> Result<()>,
) -> Result<()> {
    serve_blocking_with_hooks(server, ready, None)
}

/// The optional hook owns consent/delivery and must bound its own IO. No hook
/// receives addresses, roots, credentials or other server identity.
pub fn serve_blocking_with_hooks(
    server: Arc<HistoryServer>,
    ready: impl FnOnce(SocketAddr) -> Result<()>,
    hook: Option<ServerRuntimeHook>,
) -> Result<()> {
    let started = Instant::now();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            server.lifecycle(
                ServerLifecycle::Failed,
                ServerStage::Runtime,
                started.elapsed(),
                Some(ServerFailure::Io),
            );
            if let Some(hook) = hook {
                hook(ServerRuntimeTick::Failed);
            }
            return Err(error.into());
        }
    };
    runtime.block_on(serve_with_hooks(server, ready, hook))
}

pub async fn serve(server: Arc<HistoryServer>) -> Result<()> {
    serve_with_hooks(server, |_| Ok(()), None).await
}

pub async fn serve_with_hooks(
    server: Arc<HistoryServer>,
    ready: impl FnOnce(SocketAddr) -> Result<()>,
    hook: Option<ServerRuntimeHook>,
) -> Result<()> {
    let started = Instant::now();
    let mut stage = ServerStage::Configuration;
    let prepared: Result<_> = async {
        server.config.validate()?;
        stage = ServerStage::Bind;
        let listener = tokio::net::TcpListener::bind(server.config.bind).await?;
        let address = listener.local_addr()?;
        stage = ServerStage::ReadyCallback;
        ready(address)?;
        eprintln!("ctx history server listening on {address}");
        Ok(listener)
    }
    .await;
    let listener = match prepared {
        Ok(listener) => listener,
        Err(error) => {
            server.lifecycle(
                ServerLifecycle::Failed,
                stage,
                started.elapsed(),
                Some(ServerFailure::from(&error)),
            );
            run_hook(hook, ServerRuntimeTick::Failed).await;
            return Err(error);
        }
    };
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let index_server = server.clone();
    let indexer = tokio::spawn(async move {
        let mut reported_failure = false;
        let mut after = String::new();
        loop {
            tokio::select! {
                _=stopped.changed()=>break,
                _=tokio::time::sleep(Duration::from_millis(250))=>{}
            }
            let server = index_server.clone();
            let mut cursor = std::mem::take(&mut after);
            let result = tokio::task::spawn_blocking(move || {
                let result = server.index_sweep(&mut cursor);
                (result, cursor)
            })
            .await;
            let failure = match result {
                Ok((result, cursor)) => {
                    after = cursor;
                    result.as_ref().err().map(ServerFailure::from)
                }
                Err(_) => Some(ServerFailure::Unavailable),
            };
            let failed = failure.is_some();
            if failed != reported_failure {
                index_server.observe(ServerObservation::IndexerHealth { failure });
            }
            if failed && !reported_failure {
                if let Ok(connection) = index_server.lock() {
                    let _ = crate::catalog::audit(&connection, "index_unavailable", None, None);
                }
            }
            reported_failure = failed;
        }
    });
    server.lifecycle(
        ServerLifecycle::Ready,
        ServerStage::Serve,
        started.elapsed(),
        None,
    );
    let runtime_server = server.clone();
    let mut runtime_stop = stop.subscribe();
    // One optional task uses this runtime's existing blocking pool; it never
    // overlaps hooks or blocks the HTTP/indexer tasks on telemetry delivery.
    let runtime_hook = hook.clone();
    let observation_task = (server.observer.is_some() || hook.is_some()).then(|| {
        tokio::spawn(async move {
            run_hook(runtime_hook.clone(), ServerRuntimeTick::Ready).await;
            let mut liveness = Instant::now();
            loop {
                tokio::select! {
                    _=runtime_stop.changed()=>break,
                    _=tokio::time::sleep(Duration::from_secs(30))=>{}
                }
                let server = runtime_server.clone();
                let hook = runtime_hook.clone();
                let due = liveness.elapsed() >= Duration::from_secs(300);
                if due {
                    liveness = Instant::now();
                }
                let _ = tokio::task::spawn_blocking(move || {
                    if due {
                        server.lifecycle(
                            ServerLifecycle::Liveness,
                            ServerStage::Serve,
                            started.elapsed(),
                            None,
                        );
                    }
                    if let Some(hook) = hook {
                        hook(ServerRuntimeTick::Interval);
                    }
                })
                .await;
            }
        })
    });
    let shutdown_server = server.clone();
    let served = axum::serve(listener, super::router(server.clone()))
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            shutdown_server.lifecycle(
                ServerLifecycle::ShuttingDown,
                ServerStage::Shutdown,
                started.elapsed(),
                None,
            );
        })
        .await;
    let _ = stop.send(true);
    let _ = indexer.await;
    if let Some(task) = observation_task {
        let _ = task.await;
    }
    server.lifecycle(
        ServerLifecycle::Stopped,
        ServerStage::Shutdown,
        started.elapsed(),
        served.as_ref().err().map(|_| ServerFailure::Io),
    );
    run_hook(hook, ServerRuntimeTick::Stopped).await;
    served.map_err(Error::Io)
}

async fn run_hook(hook: Option<ServerRuntimeHook>, reason: ServerRuntimeTick) {
    if let Some(hook) = hook {
        let _ = tokio::task::spawn_blocking(move || hook(reason)).await;
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! {_=tokio::signal::ctrl_c()=>{},_=terminate.recv()=>{}}
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
