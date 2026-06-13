use std::collections::HashSet;
use std::sync::Arc;

use ctx_providers::adapters::{
    ProviderAdapter, ProviderRestartMode, ProviderSessionSweepConfig, ProviderSessionSweepStats,
};

use crate::ProviderRuntime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderAdapterRestartStatus {
    Ok,
    Unsupported,
    Error,
}

impl ProviderAdapterRestartStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Unsupported => "unsupported",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAdapterRestartResult {
    pub provider_id: String,
    pub status: ProviderAdapterRestartStatus,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderAdapterRestartAttempt {
    Restarted,
    Missing,
    Failed(String),
}

impl ProviderRuntime {
    pub async fn provider_worker_adapters_for_shutdown(
        &self,
    ) -> Vec<(String, Arc<dyn ProviderAdapter>)> {
        self.all_provider_adapter_entries().await
    }

    pub async fn sweep_provider_workers_once(
        &self,
        config: ProviderSessionSweepConfig,
    ) -> ProviderSessionSweepStats {
        let mut stats = ProviderSessionSweepStats::default();
        let mut seen = HashSet::<usize>::new();
        for (_, adapter) in self.provider_worker_adapters_for_shutdown().await {
            let identity = (Arc::as_ptr(&adapter) as *const ()) as usize;
            if !seen.insert(identity) {
                continue;
            }
            match adapter.reap_idle_sessions(config).await {
                Ok(adapter_stats) => {
                    stats.reaped += adapter_stats.reaped;
                    stats.skipped_busy += adapter_stats.skipped_busy;
                    stats.dead_removed += adapter_stats.dead_removed;
                    stats.status_errors += adapter_stats.status_errors;
                }
                Err(err) => {
                    stats.status_errors += 1;
                    tracing::debug!(err = %err, "provider worker sweep failed");
                }
            }
        }
        stats
    }

    pub async fn shutdown_provider_adapters(&self, reason: &str) {
        for (id, adapter) in self.provider_worker_adapters_for_shutdown().await {
            if let Err(err) = adapter
                .restart(reason, ProviderRestartMode::Immediate)
                .await
            {
                tracing::debug!(
                    "failed to stop provider adapter {id} during daemon shutdown: {err:#}"
                );
            }
        }
    }

    pub async fn restart_all_provider_adapters(
        &self,
        reason: &str,
        mode: ProviderRestartMode,
    ) -> Vec<ProviderAdapterRestartResult> {
        let adapters = self.provider_worker_adapters_for_shutdown().await;
        let mut results = Vec::with_capacity(adapters.len());
        for (provider_id, adapter) in adapters {
            results.push(restart_provider_adapter(provider_id, adapter, reason, mode).await);
        }
        results
    }

    pub async fn restart_provider_adapter_by_id(
        &self,
        provider_id: &str,
        reason: &str,
        mode: ProviderRestartMode,
    ) -> ProviderAdapterRestartAttempt {
        let Some(adapter) = self.provider_adapter(provider_id).await else {
            return ProviderAdapterRestartAttempt::Missing;
        };
        match adapter.restart(reason, mode).await {
            Ok(()) => ProviderAdapterRestartAttempt::Restarted,
            Err(err) => ProviderAdapterRestartAttempt::Failed(format!("{err:#}")),
        }
    }

    pub async fn drain_restart_provider_adapters_for_auth_change(
        &self,
        provider_id: &str,
        reason: &str,
    ) -> anyhow::Result<()> {
        let adapters = self
            .provider_adapter_entries_for_provider(provider_id)
            .await;
        let mut failures = Vec::new();
        for (id, adapter) in adapters {
            if !adapter.supports_restart_mode(ProviderRestartMode::Drain) {
                tracing::info!("skipping drain-restart for {id} after auth change: adapter does not support drain restart");
                continue;
            }
            if let Err(err) = adapter.restart(reason, ProviderRestartMode::Drain).await {
                tracing::warn!("failed to drain-restart {id} after auth change: {err}");
                failures.push(format!("{id}: {err:#}"));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            anyhow::bail!(
                "provider auth updated but drain-restart failed for {provider_id}: {}",
                failures.join("; ")
            );
        }
    }

    pub async fn set_provider_session_pinned(&self, session_key: String, pinned: bool) {
        let mut seen = HashSet::<usize>::new();
        for (_, adapter) in self.provider_worker_adapters_for_shutdown().await {
            let identity = (Arc::as_ptr(&adapter) as *const ()) as usize;
            if !seen.insert(identity) {
                continue;
            }
            if let Err(err) = adapter
                .set_session_pinned(session_key.clone(), pinned)
                .await
            {
                tracing::debug!(
                    session_id = %session_key,
                    pinned,
                    err = %err,
                    "failed to update provider worker pin state"
                );
            }
        }
    }
}

async fn restart_provider_adapter(
    provider_id: String,
    adapter: Arc<dyn ProviderAdapter>,
    reason: &str,
    mode: ProviderRestartMode,
) -> ProviderAdapterRestartResult {
    match adapter.restart(reason, mode).await {
        Ok(()) => ProviderAdapterRestartResult {
            provider_id,
            status: ProviderAdapterRestartStatus::Ok,
            message: None,
        },
        Err(err) => {
            let message = err.to_string();
            let status = if message.to_lowercase().contains("does not support") {
                ProviderAdapterRestartStatus::Unsupported
            } else {
                ProviderAdapterRestartStatus::Error
            };
            ProviderAdapterRestartResult {
                provider_id,
                status,
                message: Some(message),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Mutex as StdMutex;

    use anyhow::Result;
    use async_trait::async_trait;
    use ctx_providers::adapters::{
        ProviderAdapter, ProviderHealth, ProviderProcessInfo, ProviderRestartMode,
        ProviderSessionSweepConfig, ProviderSessionSweepStats, ProviderStatus, ProviderUsability,
        RunHandle, TurnInput,
    };

    use super::*;

    #[derive(Default)]
    struct RecordingProviderAdapter {
        restart_calls: StdMutex<Vec<(String, ProviderRestartMode)>>,
        reap_calls: StdMutex<Vec<ProviderSessionSweepConfig>>,
        reap_result: StdMutex<ProviderSessionSweepStats>,
        pin_calls: StdMutex<Vec<(String, bool)>>,
        restart_error: StdMutex<Option<String>>,
        supports_drain_restart: bool,
    }

    impl RecordingProviderAdapter {
        fn restart_calls(&self) -> Vec<(String, ProviderRestartMode)> {
            self.restart_calls
                .lock()
                .expect("recording adapter restart lock")
                .clone()
        }

        fn reap_calls(&self) -> Vec<ProviderSessionSweepConfig> {
            self.reap_calls
                .lock()
                .expect("recording adapter reap lock")
                .clone()
        }

        fn set_reap_result(&self, stats: ProviderSessionSweepStats) {
            *self
                .reap_result
                .lock()
                .expect("recording adapter reap result lock") = stats;
        }

        fn pin_calls(&self) -> Vec<(String, bool)> {
            self.pin_calls
                .lock()
                .expect("recording adapter pin lock")
                .clone()
        }

        fn set_restart_error(&self, error: &str) {
            *self
                .restart_error
                .lock()
                .expect("recording adapter restart error lock") = Some(error.to_string());
        }
    }

    fn recording_adapter_with_drain_restart() -> Arc<RecordingProviderAdapter> {
        Arc::new(RecordingProviderAdapter {
            supports_drain_restart: true,
            ..RecordingProviderAdapter::default()
        })
    }

    #[async_trait]
    impl ProviderAdapter for RecordingProviderAdapter {
        async fn inspect(&self) -> Result<ProviderStatus> {
            Ok(ProviderStatus {
                provider_id: "recording".into(),
                installed: true,
                detected_path: None,
                version: Some("test".into()),
                capabilities: None,
                health: ProviderHealth::Ok,
                diagnostics: Vec::new(),
                details: HashMap::new(),
                usability: ProviderUsability::default(),
            })
        }

        async fn run(
            &self,
            _input: TurnInput,
            _workdir: PathBuf,
            _env: HashMap<String, String>,
            _event_sink: tokio::sync::mpsc::Sender<ctx_providers::events::NormalizedEvent>,
            _hooks: ctx_providers::adapters::ProviderRunHooks,
        ) -> Result<RunHandle> {
            anyhow::bail!("not used in test");
        }

        async fn cancel(&self, _handle: &mut RunHandle) -> Result<()> {
            Ok(())
        }

        async fn list_processes(&self) -> Vec<ProviderProcessInfo> {
            Vec::new()
        }

        async fn restart(&self, reason: &str, mode: ProviderRestartMode) -> Result<()> {
            self.restart_calls
                .lock()
                .expect("recording adapter restart lock")
                .push((reason.to_string(), mode));
            if let Some(error) = self
                .restart_error
                .lock()
                .expect("recording adapter restart error lock")
                .clone()
            {
                anyhow::bail!("{error}");
            }
            Ok(())
        }

        fn supports_restart_mode(&self, mode: ProviderRestartMode) -> bool {
            match mode {
                ProviderRestartMode::Immediate => true,
                ProviderRestartMode::Drain => self.supports_drain_restart,
            }
        }

        async fn reap_idle_sessions(
            &self,
            config: ProviderSessionSweepConfig,
        ) -> Result<ProviderSessionSweepStats> {
            self.reap_calls
                .lock()
                .expect("recording adapter reap lock")
                .push(config);
            Ok(*self
                .reap_result
                .lock()
                .expect("recording adapter reap result lock"))
        }

        async fn set_session_pinned(&self, session_key: String, pinned: bool) -> Result<()> {
            self.pin_calls
                .lock()
                .expect("recording adapter pin lock")
                .push((session_key, pinned));
            Ok(())
        }
    }

    #[tokio::test]
    async fn shutdown_adapter_collection_includes_root_and_target_adapters() {
        let root_adapter = Arc::new(RecordingProviderAdapter::default());
        let target_adapter = Arc::new(RecordingProviderAdapter::default());
        let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
        providers.insert("root".into(), root_adapter);
        let runtime = ProviderRuntime::new(providers);
        runtime
            .upsert_target_provider_adapter("root@host".into(), target_adapter)
            .await;

        let mut ids = runtime
            .provider_worker_adapters_for_shutdown()
            .await
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        ids.sort();

        assert_eq!(ids, vec!["root".to_string(), "root@host".to_string()]);
    }

    #[tokio::test]
    async fn shutdown_requests_immediate_restart_for_all_worker_adapters() {
        let root_adapter = Arc::new(RecordingProviderAdapter::default());
        let target_adapter = Arc::new(RecordingProviderAdapter::default());
        let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
        providers.insert("root".into(), root_adapter.clone());
        let runtime = ProviderRuntime::new(providers);
        runtime
            .upsert_target_provider_adapter("root@host".into(), target_adapter.clone())
            .await;

        runtime.shutdown_provider_adapters("test shutdown").await;

        assert_eq!(
            root_adapter.restart_calls(),
            vec![("test shutdown".to_string(), ProviderRestartMode::Immediate)]
        );
        assert_eq!(
            target_adapter.restart_calls(),
            vec![("test shutdown".to_string(), ProviderRestartMode::Immediate)]
        );
    }

    #[tokio::test]
    async fn sweep_dedupes_shared_adapters_and_aggregates_stats() {
        let shared_adapter = Arc::new(RecordingProviderAdapter::default());
        shared_adapter.set_reap_result(ProviderSessionSweepStats {
            reaped: 1,
            skipped_busy: 2,
            dead_removed: 0,
            status_errors: 0,
        });
        let other_adapter = Arc::new(RecordingProviderAdapter::default());
        other_adapter.set_reap_result(ProviderSessionSweepStats {
            reaped: 0,
            skipped_busy: 0,
            dead_removed: 1,
            status_errors: 1,
        });

        let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
        providers.insert("root".into(), shared_adapter.clone());
        providers.insert("other".into(), other_adapter.clone());
        let runtime = ProviderRuntime::new(providers);
        runtime
            .upsert_target_provider_adapter("root@host".into(), shared_adapter.clone())
            .await;

        let config = ProviderSessionSweepConfig {
            idle_ttl: std::time::Duration::from_secs(7),
            max_idle_sessions: 3,
            interval: std::time::Duration::from_secs(11),
        };
        let stats = runtime.sweep_provider_workers_once(config).await;

        assert_eq!(
            stats,
            ProviderSessionSweepStats {
                reaped: 1,
                skipped_busy: 2,
                dead_removed: 1,
                status_errors: 1,
            }
        );
        assert_eq!(shared_adapter.reap_calls().len(), 1);
        assert_eq!(other_adapter.reap_calls().len(), 1);
        assert_eq!(shared_adapter.reap_calls()[0].idle_ttl, config.idle_ttl);
        assert_eq!(
            shared_adapter.reap_calls()[0].max_idle_sessions,
            config.max_idle_sessions
        );
        assert_eq!(shared_adapter.reap_calls()[0].interval, config.interval);
    }

    #[tokio::test]
    async fn pin_propagation_dedupes_shared_worker_adapters() {
        let shared_adapter = Arc::new(RecordingProviderAdapter::default());
        let other_adapter = Arc::new(RecordingProviderAdapter::default());
        let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
        providers.insert("root".into(), shared_adapter.clone());
        providers.insert("other".into(), other_adapter.clone());
        let runtime = ProviderRuntime::new(providers);
        runtime
            .upsert_target_provider_adapter("root@host".into(), shared_adapter.clone())
            .await;

        runtime
            .set_provider_session_pinned("session-1".to_string(), true)
            .await;

        assert_eq!(
            shared_adapter.pin_calls(),
            vec![("session-1".to_string(), true)]
        );
        assert_eq!(
            other_adapter.pin_calls(),
            vec![("session-1".to_string(), true)]
        );
    }

    #[tokio::test]
    async fn restart_all_provider_adapters_reports_ok_unsupported_and_error() {
        let ok_adapter = Arc::new(RecordingProviderAdapter::default());
        let unsupported_adapter = Arc::new(RecordingProviderAdapter::default());
        unsupported_adapter.set_restart_error("provider does not support drain restart");
        let error_adapter = Arc::new(RecordingProviderAdapter::default());
        error_adapter.set_restart_error("boom");

        let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
        providers.insert("ok".into(), ok_adapter);
        providers.insert("unsupported".into(), unsupported_adapter);
        providers.insert("error".into(), error_adapter);
        let runtime = ProviderRuntime::new(providers);

        let mut results = runtime
            .restart_all_provider_adapters("dev restart", ProviderRestartMode::Drain)
            .await;
        results.sort_by(|left, right| left.provider_id.cmp(&right.provider_id));

        assert_eq!(
            results
                .into_iter()
                .map(|result| (
                    result.provider_id,
                    result.status,
                    result.message.as_deref().map(str::to_string),
                ))
                .collect::<Vec<_>>(),
            vec![
                (
                    "error".to_string(),
                    ProviderAdapterRestartStatus::Error,
                    Some("boom".to_string()),
                ),
                ("ok".to_string(), ProviderAdapterRestartStatus::Ok, None),
                (
                    "unsupported".to_string(),
                    ProviderAdapterRestartStatus::Unsupported,
                    Some("provider does not support drain restart".to_string()),
                ),
            ]
        );
    }

    #[tokio::test]
    async fn restart_provider_adapter_by_id_reports_success_missing_and_failure() {
        let ok_adapter = Arc::new(RecordingProviderAdapter::default());
        let failing_adapter = Arc::new(RecordingProviderAdapter::default());
        failing_adapter.set_restart_error("restart failed");

        let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
        providers.insert("ok".into(), ok_adapter.clone());
        providers.insert("failing".into(), failing_adapter.clone());
        let runtime = ProviderRuntime::new(providers);

        assert_eq!(
            runtime
                .restart_provider_adapter_by_id(
                    "ok",
                    "memory pressure",
                    ProviderRestartMode::Immediate
                )
                .await,
            ProviderAdapterRestartAttempt::Restarted
        );
        assert_eq!(
            runtime
                .restart_provider_adapter_by_id(
                    "missing",
                    "memory pressure",
                    ProviderRestartMode::Immediate
                )
                .await,
            ProviderAdapterRestartAttempt::Missing
        );
        assert_eq!(
            runtime
                .restart_provider_adapter_by_id(
                    "failing",
                    "memory pressure",
                    ProviderRestartMode::Immediate
                )
                .await,
            ProviderAdapterRestartAttempt::Failed("restart failed".to_string())
        );
        assert_eq!(
            ok_adapter.restart_calls(),
            vec![(
                "memory pressure".to_string(),
                ProviderRestartMode::Immediate
            )]
        );
        assert_eq!(
            failing_adapter.restart_calls(),
            vec![(
                "memory pressure".to_string(),
                ProviderRestartMode::Immediate
            )]
        );
    }

    #[tokio::test]
    async fn drain_restart_for_auth_change_targets_provider_adapters_only() {
        let root_adapter = recording_adapter_with_drain_restart();
        let target_adapter = recording_adapter_with_drain_restart();
        let skipped_adapter = Arc::new(RecordingProviderAdapter::default());
        let unrelated_adapter = recording_adapter_with_drain_restart();
        let failing_adapter = recording_adapter_with_drain_restart();
        failing_adapter.set_restart_error("target failed");

        let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
        providers.insert("root".into(), root_adapter.clone());
        providers.insert("other".into(), unrelated_adapter.clone());
        let runtime = ProviderRuntime::new(providers);
        runtime
            .upsert_target_provider_adapter("root@host".into(), target_adapter.clone())
            .await;
        runtime
            .upsert_target_provider_adapter("root@skipped".into(), skipped_adapter.clone())
            .await;
        runtime
            .upsert_target_provider_adapter("root@failing".into(), failing_adapter.clone())
            .await;

        let err = runtime
            .drain_restart_provider_adapters_for_auth_change("root", "auth updated")
            .await
            .expect_err("failing target adapter should fail aggregate restart");

        assert_eq!(
            root_adapter.restart_calls(),
            vec![("auth updated".to_string(), ProviderRestartMode::Drain)]
        );
        assert_eq!(
            target_adapter.restart_calls(),
            vec![("auth updated".to_string(), ProviderRestartMode::Drain)]
        );
        assert!(skipped_adapter.restart_calls().is_empty());
        assert!(unrelated_adapter.restart_calls().is_empty());
        assert_eq!(
            failing_adapter.restart_calls(),
            vec![("auth updated".to_string(), ProviderRestartMode::Drain)]
        );
        assert!(
            err.to_string().contains(
                "provider auth updated but drain-restart failed for root: root@failing: target failed"
            ),
            "{err:#}"
        );
    }
}
