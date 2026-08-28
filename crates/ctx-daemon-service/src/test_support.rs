use std::{io::Write, path::Path};

use anyhow::{bail, Result};
use ctx_client_observability::analytics::{
    Outcome, ProviderRefreshCompletedV1, PublicEventV1, Surface,
};
use ctx_history_capture::DiscoveryContext;
use ctx_semantic_index::{semantic_model_contract, SemanticVectorStore, SourceBackedGenerationPin};
use ctx_semantic_model::{
    ArtifactFetchRequest, ArtifactFetcher, SemanticModelConfig, SemanticModelPaths,
    SemanticOnnxRuntimePaths,
};
use serde_json::Value;

use crate::{
    CoreGenerationPublished, CoreGenerationPublishedPort, DaemonAvailability,
    DaemonAvailabilityDemand, DaemonAvailabilityPort, DaemonConfigPort, DaemonConfigSnapshot,
    DaemonObservationPort, DaemonTrigger,
};

pub(crate) static CONFIG: TestConfig = TestConfig;
pub(crate) static SOURCE_REFRESH_CONFIG: SourceRefreshConfig = SourceRefreshConfig;
pub(crate) static AVAILABILITY: TestAvailability = TestAvailability;
pub(crate) static GENERATION_PUBLISHED: TestCoreGenerationPublished = TestCoreGenerationPublished;
pub(crate) static ARTIFACT: TestArtifact = TestArtifact;
pub(crate) static OBSERVATION: TestObservation = TestObservation;

pub(crate) fn semantic_contract_fingerprint() -> Result<String> {
    ctx_semantic_index::source_backed_semantic_contract_fingerprint(semantic_model_contract())
}

pub(crate) fn semantic_vector_path(data_root: &Path) -> std::path::PathBuf {
    ctx_semantic_index::source_backed_semantic_vector_path(data_root)
}

pub(crate) fn seed_filter_unaware_semantic_state(path: &Path) -> Result<()> {
    ctx_semantic_index::test_support::seed_filter_unaware_derived_state(
        path,
        semantic_model_contract(),
    )
}

pub(crate) fn current_semantic_vector_schema_version() -> i64 {
    ctx_semantic_index::test_support::semantic_vector_schema_version()
}

pub(crate) fn semantic_generation_is_ready_empty(path: &Path, generation: &str) -> Result<bool> {
    let Some(store) = SemanticVectorStore::open_read_only(path, semantic_model_contract())? else {
        return Ok(false);
    };
    Ok(matches!(
        store.source_backed_generation_pin_exact(generation, 0)?,
        SourceBackedGenerationPin::ReadyEmpty
    ))
}

pub(crate) struct TestConfig;
pub(crate) struct SourceRefreshConfig;

pub(crate) struct TestAvailability;
pub(crate) struct TestObservation;

impl DaemonObservationPort for TestObservation {
    fn provider_refresh_event(
        &self,
        _job: &Value,
        _successor_pending: bool,
    ) -> Option<PublicEventV1> {
        Some(PublicEventV1::ProviderRefreshCompleted(
            ProviderRefreshCompletedV1::new(
                Surface::Daemon,
                Outcome::Success,
                std::time::Duration::ZERO,
            ),
        ))
    }

    fn deliver(&self, _data_root: &Path, _events: &[PublicEventV1]) {}
}

impl DaemonAvailabilityPort for TestAvailability {
    fn ensure_available(
        &self,
        _data_root: &Path,
        _trigger: DaemonTrigger,
        _demand: DaemonAvailabilityDemand,
    ) -> Result<DaemonAvailability> {
        Ok(DaemonAvailability::Available)
    }
}

impl DaemonConfigPort for TestConfig {
    fn load(&self, _data_root: &Path) -> Result<DaemonConfigSnapshot> {
        let mut config = DaemonConfigSnapshot::default();
        config.daemon.enabled = true;
        Ok(config)
    }

    fn semantic_model_config(&self, data_root: &Path) -> SemanticModelConfig {
        SemanticModelConfig::new(SemanticModelPaths::new(
            data_root.join("models"),
            SemanticOnnxRuntimePaths::new(data_root.join("runtime")),
        ))
    }

    fn discovery_context(&self, data_root: &Path) -> Result<DiscoveryContext> {
        Ok(DiscoveryContext::from_process(data_root))
    }
}

impl DaemonConfigPort for SourceRefreshConfig {
    fn load(&self, _data_root: &Path) -> Result<DaemonConfigSnapshot> {
        let mut config = DaemonConfigSnapshot::default();
        config.daemon.enabled = true;
        config.daemon.mode = crate::DaemonMode::SourceRefreshOnly;
        Ok(config)
    }

    fn semantic_model_config(&self, data_root: &Path) -> SemanticModelConfig {
        CONFIG.semantic_model_config(data_root)
    }

    fn discovery_context(&self, data_root: &Path) -> Result<DiscoveryContext> {
        CONFIG.discovery_context(data_root)
    }
}

pub(crate) struct TestArtifact;

impl ArtifactFetcher for TestArtifact {
    fn fetch_to_writer(
        &self,
        _request: ArtifactFetchRequest<'_>,
        _writer: &mut dyn Write,
    ) -> Result<u64> {
        bail!("test artifact fetch is disabled")
    }
}

pub(crate) struct TestCoreGenerationPublished;

impl CoreGenerationPublishedPort for TestCoreGenerationPublished {
    fn notify(&self, _data_root: &Path, _publication: &CoreGenerationPublished) -> Result<()> {
        Ok(())
    }
}
