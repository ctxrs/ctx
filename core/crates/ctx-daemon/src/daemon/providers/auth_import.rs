use std::collections::BTreeSet;
use std::path::Path;

use ctx_observability::logs;
use ctx_provider_auth_import as provider_auth_import;
use ctx_provider_auth_import::{
    ProviderAuthImportCandidatesRouteResponse, ProviderAuthImportProfilesRouteResponse,
    ProviderAuthImportRouteError, ProviderAuthImportRouteRequest, ProviderAuthImportRouteResponse,
};
use ctx_provider_runtime::ProviderRuntime;

use crate::daemon::ProviderAuthImportHandle;

fn redacted_route_error(error: anyhow::Error) -> ProviderAuthImportRouteError {
    ProviderAuthImportRouteError::new(logs::redact_sensitive(&error.to_string()))
}

fn raw_route_error(error: anyhow::Error) -> ProviderAuthImportRouteError {
    ProviderAuthImportRouteError::new(error.to_string())
}

pub async fn list_provider_auth_import_candidates(
) -> anyhow::Result<Vec<provider_auth_import::ProviderAuthImportCandidate>> {
    provider_auth_import::list_provider_auth_import_candidates().await
}

fn normalize_candidate_ids(candidate_ids: Vec<String>) -> Vec<String> {
    candidate_ids
        .into_iter()
        .filter_map(|id| {
            let trimmed = id.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        })
        .collect()
}

pub async fn list_provider_auth_import_profiles(
    data_root: &Path,
) -> anyhow::Result<Vec<provider_auth_import::ProviderImportedAuthProfile>> {
    provider_auth_import::list_provider_auth_profiles(data_root).await
}

fn mutated_provider_ids(
    results: &[provider_auth_import::ProviderAuthImportResult],
) -> BTreeSet<String> {
    results
        .iter()
        .filter(|result| {
            provider_auth_import::provider_auth_import_result_mutates_effective_auth(result)
        })
        .map(|result| result.provider_id.clone())
        .collect()
}

async fn restart_mutated_providers(
    providers: &ProviderRuntime,
    provider_ids: BTreeSet<String>,
) -> anyhow::Result<()> {
    let mut restart_errors = Vec::new();
    for provider_id in provider_ids {
        if let Err(error) = super::restarts::restart_provider_for_auth_change_with_runtime(
            providers,
            &provider_id,
            &format!("{provider_id} auth updated"),
        )
        .await
        {
            restart_errors.push(error.to_string());
        }
    }
    if !restart_errors.is_empty() {
        anyhow::bail!(
            "provider auth updated but one or more runtime restarts failed: {}",
            restart_errors.join("; ")
        );
    }

    Ok(())
}

pub async fn import_provider_auth_candidates(
    data_root: &Path,
    providers: &ProviderRuntime,
    candidate_ids: Vec<String>,
) -> anyhow::Result<Vec<provider_auth_import::ProviderAuthImportResult>> {
    let ids = normalize_candidate_ids(candidate_ids);
    let results = provider_auth_import::import_provider_auth_candidates(data_root, &ids).await?;
    restart_mutated_providers(providers, mutated_provider_ids(&results)).await?;

    Ok(results)
}

impl ProviderAuthImportHandle {
    pub async fn list_provider_auth_import_candidates_for_route(
        &self,
    ) -> Result<ProviderAuthImportCandidatesRouteResponse, ProviderAuthImportRouteError> {
        let candidates = list_provider_auth_import_candidates()
            .await
            .map_err(redacted_route_error)?;
        Ok(ProviderAuthImportCandidatesRouteResponse::new(candidates))
    }

    pub async fn list_provider_auth_import_profiles_for_route(
        &self,
    ) -> Result<ProviderAuthImportProfilesRouteResponse, ProviderAuthImportRouteError> {
        let profiles = list_provider_auth_import_profiles(self.data_root())
            .await
            .map_err(raw_route_error)?;
        Ok(ProviderAuthImportProfilesRouteResponse::new(profiles))
    }

    pub async fn import_provider_auth_candidates_for_route(
        &self,
        request: ProviderAuthImportRouteRequest,
    ) -> Result<ProviderAuthImportRouteResponse, ProviderAuthImportRouteError> {
        let results = import_provider_auth_candidates(
            self.data_root(),
            self.providers(),
            request.into_candidate_ids(),
        )
        .await
        .map_err(raw_route_error)?;
        Ok(ProviderAuthImportRouteResponse::new(results))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use anyhow::Result;
    use async_trait::async_trait;
    use ctx_providers::adapters::{
        ProviderAdapter, ProviderHealth, ProviderProcessInfo, ProviderRestartMode, ProviderStatus,
        ProviderUsability, RunHandle, TurnInput,
    };

    #[derive(Default)]
    struct RecordingProviderAdapter {
        restart_calls: Mutex<Vec<(String, ProviderRestartMode)>>,
        restart_error: Mutex<Option<String>>,
    }

    impl RecordingProviderAdapter {
        fn restart_calls(&self) -> Vec<(String, ProviderRestartMode)> {
            self.restart_calls
                .lock()
                .expect("recording adapter restart lock")
                .clone()
        }

        fn set_restart_error(&self, error: &str) {
            *self
                .restart_error
                .lock()
                .expect("recording adapter restart error lock") = Some(error.to_string());
        }
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
            matches!(mode, ProviderRestartMode::Drain)
        }
    }

    fn auth_import_result(
        candidate_id: &str,
        provider_id: &str,
        status: &str,
    ) -> provider_auth_import::ProviderAuthImportResult {
        provider_auth_import::ProviderAuthImportResult {
            candidate_id: candidate_id.to_string(),
            provider_id: provider_id.to_string(),
            status: status.to_string(),
            profile_id: None,
            message: None,
        }
    }

    #[test]
    fn candidate_route_errors_are_redacted_before_crossing_route_boundary() {
        let error = redacted_route_error(anyhow::anyhow!(
            r#"candidate scan failed with {{"ctx_mcp_token":"secret-token"}}"#
        ));

        assert!(error.message().contains("[REDACTED]"));
        assert!(!error.message().contains("secret-token"));
    }

    #[test]
    fn profile_and_import_route_errors_keep_actionable_context() {
        let error = raw_route_error(anyhow::anyhow!(
            "parsing imported auth registry at /tmp/profiles.json"
        ));

        assert!(error.message().contains("parsing imported auth registry"));
        assert!(error.message().contains("profiles.json"));
    }

    #[test]
    fn import_route_normalizes_candidate_ids_before_matching() {
        assert_eq!(
            normalize_candidate_ids(vec![
                "  codex-auth  ".to_string(),
                "\t".to_string(),
                "gemini-env\n".to_string(),
                "".to_string(),
            ]),
            vec!["codex-auth".to_string(), "gemini-env".to_string()]
        );
    }

    #[test]
    fn auth_import_mutated_provider_ids_are_deduped_and_include_already_imported() {
        let provider_ids = mutated_provider_ids(&[
            auth_import_result("one", "codex", "imported"),
            auth_import_result("two", "codex", "already_imported"),
            auth_import_result("three", "gemini", "updated"),
            auth_import_result("four", "qwen", "unsupported"),
            auth_import_result("five", "amp", "error"),
        ]);

        assert_eq!(
            provider_ids.into_iter().collect::<Vec<_>>(),
            vec!["codex".to_string(), "gemini".to_string()]
        );
    }

    #[tokio::test]
    async fn auth_import_restarts_each_mutated_provider_once() {
        let codex = Arc::new(RecordingProviderAdapter::default());
        let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
        providers.insert("codex".to_string(), codex.clone());
        let runtime = ProviderRuntime::new(providers);

        restart_mutated_providers(
            &runtime,
            mutated_provider_ids(&[
                auth_import_result("one", "codex", "imported"),
                auth_import_result("two", "codex", "already_imported"),
            ]),
        )
        .await
        .expect("restart mutated providers");

        assert_eq!(
            codex.restart_calls(),
            vec![("codex auth updated".to_string(), ProviderRestartMode::Drain)]
        );
    }

    #[tokio::test]
    async fn auth_import_restart_failures_are_aggregated() {
        let codex = Arc::new(RecordingProviderAdapter::default());
        codex.set_restart_error("codex restart failed");
        let gemini = Arc::new(RecordingProviderAdapter::default());
        gemini.set_restart_error("gemini restart failed");
        let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
        providers.insert("codex".to_string(), codex);
        providers.insert("gemini".to_string(), gemini);
        let runtime = ProviderRuntime::new(providers);

        let error = restart_mutated_providers(
            &runtime,
            mutated_provider_ids(&[
                auth_import_result("one", "codex", "imported"),
                auth_import_result("two", "gemini", "updated"),
            ]),
        )
        .await
        .expect_err("restart failures should aggregate");
        let message = error.to_string();

        assert!(message.contains("one or more runtime restarts failed"));
        assert!(message.contains("codex restart failed"), "{message}");
        assert!(message.contains("gemini restart failed"), "{message}");
    }
}
