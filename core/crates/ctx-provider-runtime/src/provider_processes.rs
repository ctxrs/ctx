use ctx_providers::adapters::ProviderProcessInfo;

use crate::ProviderRuntime;

impl ProviderRuntime {
    pub async fn list_provider_processes(&self) -> Vec<ProviderProcessInfo> {
        let providers = self.provider_adapter_entries().await;
        let mut processes = Vec::new();
        for (_, adapter) in providers {
            processes.extend(adapter.list_processes().await);
        }
        processes
    }

    pub async fn provider_process_pids(&self) -> Vec<u32> {
        self.list_provider_processes()
            .await
            .into_iter()
            .map(|process| process.pid)
            .collect()
    }

    pub async fn has_running_provider_processes(&self) -> bool {
        let providers = self.provider_adapter_entries().await;
        for (_, adapter) in providers {
            if !adapter.list_processes().await.is_empty() {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    use anyhow::Result;
    use async_trait::async_trait;
    use ctx_providers::adapters::{
        ProviderAdapter, ProviderHealth, ProviderProcessInfo, ProviderStatus, ProviderUsability,
        RunHandle, TurnInput,
    };

    use super::*;

    struct ProcessListingAdapter {
        processes: Vec<ProviderProcessInfo>,
    }

    #[async_trait]
    impl ProviderAdapter for ProcessListingAdapter {
        async fn inspect(&self) -> Result<ProviderStatus> {
            Ok(ProviderStatus {
                provider_id: "process-listing".into(),
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
            self.processes.clone()
        }
    }

    #[tokio::test]
    async fn list_provider_processes_collects_processes_from_root_adapters() {
        let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
        providers.insert(
            "alpha".into(),
            Arc::new(ProcessListingAdapter {
                processes: vec![ProviderProcessInfo {
                    provider_id: "alpha".into(),
                    pid: 11,
                    label: Some("alpha-main".into()),
                }],
            }),
        );
        providers.insert(
            "beta".into(),
            Arc::new(ProcessListingAdapter {
                processes: vec![
                    ProviderProcessInfo {
                        provider_id: "beta".into(),
                        pid: 22,
                        label: Some("beta-main".into()),
                    },
                    ProviderProcessInfo {
                        provider_id: "beta".into(),
                        pid: 23,
                        label: Some("beta-helper".into()),
                    },
                ],
            }),
        );

        let runtime = ProviderRuntime::new(providers);
        let mut pids = runtime.provider_process_pids().await;
        pids.sort_unstable();

        assert_eq!(pids, vec![11, 22, 23]);
        assert!(runtime.has_running_provider_processes().await);
    }

    #[tokio::test]
    async fn has_running_provider_processes_is_false_without_adapter_processes() {
        let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
        providers.insert(
            "idle".into(),
            Arc::new(ProcessListingAdapter {
                processes: Vec::new(),
            }),
        );

        let runtime = ProviderRuntime::new(providers);

        assert!(runtime.list_provider_processes().await.is_empty());
        assert!(runtime.provider_process_pids().await.is_empty());
        assert!(!runtime.has_running_provider_processes().await);
    }
}
