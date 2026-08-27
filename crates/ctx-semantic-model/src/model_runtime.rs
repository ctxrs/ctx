use std::{
    fmt,
    sync::{Arc, Mutex},
    time::Instant,
};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use crate::json::compact_json;
use crate::ArtifactFetcher;

use super::{
    cache_paths,
    configuration::{SemanticBackendPreference, SemanticModelConfig},
    health_search::semantic_embed_policy_for,
    model_contract::{
        semantic_e5_passage_text, semantic_e5_query_text, semantic_model_key, SemanticBackendKind,
        SemanticCpuModelCacheMissing, SemanticCpuModelIntegrityError, SemanticModelFile,
        SemanticOrtModelVariant, SEMANTIC_BACKEND, SEMANTIC_DIMENSIONS,
        SEMANTIC_MODEL_CONTRACT_VERSION, SEMANTIC_MODEL_ID, SEMANTIC_MODEL_REVISION,
        SEMANTIC_REQUIRED_MODEL_FILES,
    },
    resource_policy::{
        semantic_quiet_policy, throttle_semantic_batch, SemanticComputeClass,
        SemanticIndexingExecutionPolicy, SemanticIndexingIntensity, SemanticQuietPolicy,
        SemanticSystemResources,
    },
};

#[cfg(test)]
use super::cache_paths::SEMANTIC_MANAGED_MODEL_CACHE_DIR;

#[cfg(any(target_os = "macos", test, feature = "test-support"))]
use super::health_search::semantic_model_acquisition_integrity_error;
#[cfg(any(target_os = "macos", test, feature = "test-support"))]
use super::model_contract::SemanticModelLoadDeferred;
#[cfg(test)]
use super::resource_policy::{
    semantic_model_load_deferred, SEMANTIC_ACCELERATOR_MODEL_LOAD_MIN_AVAILABLE_BYTES,
};

#[derive(Clone, Default)]
pub struct SharedSemanticRuntime {
    embedder: Arc<Mutex<Option<SemanticEmbedder>>>,
}

#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub struct SemanticRuntimeBusyGuard<'a> {
    _guard: std::sync::MutexGuard<'a, Option<SemanticEmbedder>>,
}

pub fn semantic_query_service_supported() -> bool {
    cfg!(ctx_semantic_fastembed)
}

impl SharedSemanticRuntime {
    pub fn is_loaded(&self) -> bool {
        self.embedder
            .lock()
            .map(|embedder| embedder.is_some())
            .unwrap_or(false)
    }

    pub fn release_if_idle(&self) -> Result<bool> {
        match self.embedder.try_lock() {
            Ok(mut embedder) => Ok(embedder.take().is_some()),
            Err(std::sync::TryLockError::WouldBlock) => Ok(false),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                Err(anyhow!("semantic embedder lock is poisoned"))
            }
        }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Option<SemanticEmbedder>>> {
        self.embedder
            .lock()
            .map_err(|_| anyhow!("semantic embedder lock is poisoned"))
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn lock_for_test(&self) -> Result<SemanticRuntimeBusyGuard<'_>> {
        Ok(SemanticRuntimeBusyGuard {
            _guard: self.lock()?,
        })
    }

    pub fn ensure_loaded_from_cache(&self, config: &SemanticModelConfig) -> Result<Option<u64>> {
        self.ensure_loaded(config, None)
    }

    pub fn acquire_for_daemon(
        &self,
        config: &SemanticModelConfig,
        artifact_fetcher: &dyn ArtifactFetcher,
    ) -> Result<SemanticDaemonModelAcquisition> {
        acquire_semantic_model_for_daemon(config, artifact_fetcher)
    }

    pub fn acquire_cpu_fallback_for_daemon(
        &self,
        config: &SemanticModelConfig,
        fallback: &'static str,
    ) -> Result<SemanticDaemonModelAcquisition> {
        #[cfg(ctx_semantic_fastembed)]
        {
            let accelerator = match fallback {
                "cuda_load_error" => Some(SemanticModelAcquisitionBackend::Cuda),
                "windows_ml_load_error" => Some(SemanticModelAcquisitionBackend::WindowsMl),
                _ => None,
            };
            match accelerator {
                Some(backend) => match acquire_accelerator_model_for_daemon(config, backend) {
                    Ok(acquisition) => Ok(acquisition
                        .as_cpu_fallback_for(backend)
                        .with_fallback(fallback)),
                    Err(_) => acquire_cpu_model_for_daemon(config.paths().model_cache_dir())
                        .map(|acquisition| acquisition.with_fallback(fallback)),
                },
                None => acquire_cpu_model_for_daemon(config.paths().model_cache_dir())
                    .map(|acquisition| acquisition.with_fallback(fallback)),
            }
        }
        #[cfg(not(ctx_semantic_fastembed))]
        {
            let _ = (config, fallback);
            Err(anyhow!(
                "semantic embedding model {SEMANTIC_MODEL_ID} is not supported on this platform"
            ))
        }
    }

    /// Loads a compatible cached model or acquires the opted-in local model
    /// before loading it. This is the foreground counterpart to daemon model
    /// startup and preserves the same accelerator-to-CPU fallback contract.
    pub fn ensure_loaded_with_acquisition(
        &self,
        config: &SemanticModelConfig,
        artifact_fetcher: &dyn ArtifactFetcher,
    ) -> Result<Option<u64>> {
        if self.is_loaded() {
            return Ok(None);
        }
        let mut acquisition = self.acquire_for_daemon(config, artifact_fetcher)?;
        let mut cpu_fallback_available = true;
        loop {
            match self.ensure_loaded_after_daemon_acquisition(config, acquisition) {
                Ok(load_ms) => return Ok(load_ms),
                Err(error)
                    if cpu_fallback_available
                        && error
                            .downcast_ref::<SemanticDaemonCpuFallbackRequired>()
                            .is_some() =>
                {
                    let fallback = error
                        .downcast_ref::<SemanticDaemonCpuFallbackRequired>()
                        .expect("matched semantic CPU fallback");
                    acquisition =
                        self.acquire_cpu_fallback_for_daemon(config, fallback.reason())?;
                    cpu_fallback_available = false;
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub fn ensure_loaded_after_daemon_acquisition(
        &self,
        config: &SemanticModelConfig,
        acquisition: SemanticDaemonModelAcquisition,
    ) -> Result<Option<u64>> {
        self.ensure_loaded(config, Some(acquisition))
    }

    fn ensure_loaded(
        &self,
        config: &SemanticModelConfig,
        acquisition: Option<SemanticDaemonModelAcquisition>,
    ) -> Result<Option<u64>> {
        let mut embedder = self.lock()?;
        if embedder.is_some() {
            return Ok(None);
        }
        let started = Instant::now();
        let mut acquired = match acquisition {
            Some(acquisition) => {
                acquire_semantic_embedder_after_daemon_acquisition(config, acquisition)?
            }
            None => acquire_semantic_embedder_from_cache(config)?,
        };
        if let Some(acquisition) = acquisition {
            acquisition.apply_to(&mut acquired);
        }
        *embedder = Some(acquired);
        Ok(Some(started.elapsed().as_millis() as u64))
    }

    pub fn try_runtime_status_json(&self) -> Result<(Option<Value>, bool)> {
        match self.embedder.try_lock() {
            Ok(embedder) => {
                #[cfg(ctx_semantic_fastembed)]
                let status = embedder
                    .as_ref()
                    .map(|embedder| embedder.runtime_info().to_json());
                #[cfg(not(ctx_semantic_fastembed))]
                let status = None;
                Ok((status, false))
            }
            Err(std::sync::TryLockError::WouldBlock) => Ok((None, true)),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                Err(anyhow!("semantic embedder lock is poisoned"))
            }
        }
    }

    #[cfg(ctx_semantic_fastembed)]
    pub fn embed_query(
        &self,
        config: &SemanticModelConfig,
        query: String,
    ) -> Result<(Vec<f32>, SemanticEmbeddingRuntimeInfo)> {
        let mut embedder = self.lock()?;
        let first = embedder
            .as_mut()
            .ok_or_else(|| anyhow!("semantic embedder was not initialized"))?
            .embed_query(query.clone());
        let embedding = match first {
            Ok(embedding) => embedding,
            Err(first_error) => {
                let runtime = embedder
                    .as_ref()
                    .ok_or_else(|| {
                        anyhow!("semantic embedder disappeared after inference failure")
                    })?
                    .runtime_info();
                *embedder = None;
                let mut replacement = reacquire_semantic_embedder(config, &runtime)
                    .context("reinitialize semantic embedder after query inference failure")?;
                let retry = replacement.embed_query(query).with_context(|| {
                    format!("semantic query inference failed twice; first failure: {first_error:#}")
                })?;
                *embedder = Some(replacement);
                retry
            }
        };
        let runtime = embedder
            .as_ref()
            .ok_or_else(|| anyhow!("semantic embedder was not initialized"))?
            .runtime_info();
        Ok((embedding, runtime))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub(super) enum SemanticModelAcquisitionBackend {
    Cpu,
    CoreMl,
    Cuda,
    WindowsMl,
}

impl SemanticModelAcquisitionBackend {
    #[cfg_attr(not(test), allow(dead_code))]
    fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::CoreMl => "coreml",
            Self::Cuda => "ort_cuda",
            Self::WindowsMl => "windows_ml",
        }
    }

    fn kind(self) -> SemanticBackendKind {
        match self {
            Self::Cpu => SemanticBackendKind::Cpu,
            Self::CoreMl => SemanticBackendKind::CoreMl,
            Self::Cuda => SemanticBackendKind::OrtCuda,
            Self::WindowsMl => SemanticBackendKind::WindowsMl,
        }
    }

    fn model_variant(self) -> Option<SemanticOrtModelVariant> {
        match self {
            Self::Cpu => Some(SemanticOrtModelVariant::CpuFp32),
            Self::Cuda | Self::WindowsMl => Some(SemanticOrtModelVariant::AcceleratorO4Fp16),
            Self::CoreMl => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SemanticModelAcquisitionSource {
    Cache,
    Download,
}

impl SemanticModelAcquisitionSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Cache => "cache",
            Self::Download => "download",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemanticDaemonModelAcquisition {
    backend: SemanticModelAcquisitionBackend,
    assets: SemanticModelAcquisitionBackend,
    model_variant: Option<SemanticOrtModelVariant>,
    source: SemanticModelAcquisitionSource,
    fallback: Option<&'static str>,
    allow_cpu_fallback: bool,
}

impl SemanticDaemonModelAcquisition {
    fn new(
        backend: SemanticModelAcquisitionBackend,
        source: SemanticModelAcquisitionSource,
    ) -> Self {
        Self {
            backend,
            assets: backend,
            model_variant: backend.model_variant(),
            source,
            fallback: None,
            allow_cpu_fallback: false,
        }
    }

    fn with_fallback(mut self, fallback: &'static str) -> Self {
        self.fallback = Some(fallback);
        self
    }

    fn allowing_cpu_fallback_for_auto(mut self) -> Self {
        self.allow_cpu_fallback = true;
        self
    }

    fn as_cpu_fallback_for(mut self, assets: SemanticModelAcquisitionBackend) -> Self {
        self.backend = SemanticModelAcquisitionBackend::Cpu;
        self.assets = assets;
        self.model_variant = assets.model_variant();
        self
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn verified_cpu_cache_for_test() -> Self {
        Self::new(
            SemanticModelAcquisitionBackend::Cpu,
            SemanticModelAcquisitionSource::Cache,
        )
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn verified_coreml_cache_for_test() -> Self {
        Self::new(
            SemanticModelAcquisitionBackend::CoreMl,
            SemanticModelAcquisitionSource::Cache,
        )
        .allowing_cpu_fallback_for_auto()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn downloaded_cpu_fallback_for_test(fallback: &'static str) -> Self {
        Self::new(
            SemanticModelAcquisitionBackend::Cpu,
            SemanticModelAcquisitionSource::Download,
        )
        .with_fallback(fallback)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn fallback(self) -> Option<&'static str> {
        self.fallback
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn source(self) -> &'static str {
        self.source.as_str()
    }

    #[cfg(ctx_semantic_fastembed)]
    fn apply_to(self, embedder: &mut SemanticEmbedder) {
        if embedder.backend.kind() == self.backend.kind() {
            embedder.acquisition_source = self.source.as_str();
            embedder.acquisition_fallback = self.fallback;
        }
    }

    #[cfg(not(ctx_semantic_fastembed))]
    fn apply_to(self, _embedder: &mut SemanticEmbedder) {}
}

#[derive(Debug)]
pub struct SemanticDaemonCpuFallbackRequired {
    reason: &'static str,
    accelerator_error: String,
}

impl SemanticDaemonCpuFallbackRequired {
    fn new(reason: &'static str, error: &anyhow::Error) -> Self {
        Self {
            reason,
            accelerator_error: format!("{error:#}"),
        }
    }

    pub fn reason(&self) -> &'static str {
        self.reason
    }
}

impl fmt::Display for SemanticDaemonCpuFallbackRequired {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "daemon CPU fallback required after accelerator load failure: {}",
            self.accelerator_error
        )
    }
}

impl std::error::Error for SemanticDaemonCpuFallbackRequired {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticEmbeddingRuntimeInfo {
    preference: SemanticBackendPreference,
    backend: SemanticBackendKind,
    assets_backend: SemanticBackendKind,
    model_variant: Option<SemanticOrtModelVariant>,
    compute_class: SemanticComputeClass,
    compute_mode: Option<&'static str>,
    acquisition_source: &'static str,
    acquisition_fallback: Option<&'static str>,
    runtime_artifact_identity: String,
    model_fingerprint: String,
    backend_fingerprint: String,
    canary_passed: bool,
}

impl SemanticEmbeddingRuntimeInfo {
    pub fn to_json(&self) -> Value {
        compact_json(json!({
            "preference": self.preference.as_str(),
            "backend": self.backend.as_str(),
            "execution_provider": self.backend.execution_provider(),
            "compute_class": self.compute_class.as_str(),
            "compute_mode": self.compute_mode,
            "model_id": SEMANTIC_MODEL_ID,
            "model_key": semantic_model_key(),
            "model_contract": SEMANTIC_BACKEND,
            "model_contract_version": SEMANTIC_MODEL_CONTRACT_VERSION,
            "model_variant": self.model_variant.map(SemanticOrtModelVariant::as_str),
            "model_fingerprint": self.model_fingerprint,
            "backend_fingerprint": self.backend_fingerprint,
            "runtime_artifact_identity": self.runtime_artifact_identity,
            "dimensions": SEMANTIC_DIMENSIONS,
            "canary": if self.canary_passed { "passed" } else { "not_run" },
            "acquisition_source": self.acquisition_source,
            "acquisition_fallback": self.acquisition_fallback,
        }))
    }
}

#[cfg(ctx_semantic_fastembed)]
enum SemanticEmbeddingBackend {
    Ort {
        model: fastembed::TextEmbedding,
        kind: SemanticBackendKind,
        assets: SemanticBackendKind,
        variant: SemanticOrtModelVariant,
        runtime_artifact_identity: String,
        _windows_ml_registration: Option<windows_ml::WindowsMlProviderRegistration>,
    },
    #[cfg(target_os = "macos")]
    CoreMl(CoreMlE5Embedder),
}

#[cfg(ctx_semantic_fastembed)]
impl SemanticEmbeddingBackend {
    pub(super) fn embed_query(&mut self, query: String) -> Result<Vec<f32>> {
        let query = semantic_e5_query_text(&query);
        let raw = match self {
            Self::Ort { model, .. } => model
                .embed(vec![query], Some(1))
                .with_context(|| format!("embed query with semantic model {SEMANTIC_MODEL_ID}"))?,
            #[cfg(target_os = "macos")]
            Self::CoreMl(model) => vec![model.embed_query(query)?],
        };
        let mut embeddings = normalize_and_validate_embeddings(raw, 1)?;
        embeddings
            .pop()
            .ok_or_else(|| anyhow!("semantic query embedding was empty"))
    }

    pub(super) fn embed_documents(
        &mut self,
        documents: Vec<String>,
        batch_size: usize,
    ) -> Result<Vec<Vec<f32>>> {
        let expected = documents.len();
        if expected == 0 {
            return Ok(Vec::new());
        }
        let documents = documents
            .into_iter()
            .map(|text| semantic_e5_passage_text(&text))
            .collect::<Vec<_>>();
        let raw = match self {
            Self::Ort { model, .. } => {
                model.embed(documents, Some(batch_size)).with_context(|| {
                    format!("embed documents with semantic model {SEMANTIC_MODEL_ID}")
                })?
            }
            #[cfg(target_os = "macos")]
            Self::CoreMl(model) => model.embed_documents(documents)?,
        };
        normalize_and_validate_embeddings(raw, expected)
    }

    pub(super) fn kind(&self) -> SemanticBackendKind {
        match self {
            Self::Ort { kind, .. } => *kind,
            #[cfg(target_os = "macos")]
            Self::CoreMl(_) => SemanticBackendKind::CoreMl,
        }
    }

    pub(super) fn model_variant(&self) -> Option<SemanticOrtModelVariant> {
        match self {
            Self::Ort { variant, .. } => Some(*variant),
            #[cfg(target_os = "macos")]
            Self::CoreMl(_) => None,
        }
    }

    pub(super) fn assets_backend(&self) -> SemanticBackendKind {
        match self {
            Self::Ort { assets, .. } => *assets,
            #[cfg(target_os = "macos")]
            Self::CoreMl(_) => SemanticBackendKind::CoreMl,
        }
    }

    pub(super) fn runtime_artifact_identity(&self) -> String {
        match self {
            Self::Ort {
                runtime_artifact_identity,
                ..
            } => runtime_artifact_identity.clone(),
            #[cfg(target_os = "macos")]
            Self::CoreMl(_) => "coreml-native:0.2.0".to_owned(),
        }
    }

    pub(super) fn compute_class(&self) -> SemanticComputeClass {
        match self {
            Self::Ort {
                kind: SemanticBackendKind::Cpu,
                ..
            } => SemanticComputeClass::Cpu,
            Self::Ort { .. } => SemanticComputeClass::Accelerator,
            #[cfg(target_os = "macos")]
            Self::CoreMl(model) => model.compute_class(),
        }
    }

    pub(super) fn compute_mode(&self) -> Option<&'static str> {
        match self {
            Self::Ort {
                kind: SemanticBackendKind::WindowsMl,
                ..
            } => Some("gpu_high_performance"),
            Self::Ort { .. } => None,
            #[cfg(target_os = "macos")]
            Self::CoreMl(model) => Some(model.compute_mode()),
        }
    }
}

#[cfg(ctx_semantic_fastembed)]
pub(super) struct SemanticEmbedder {
    backend: SemanticEmbeddingBackend,
    pub(super) batch_size: usize,
    preference: SemanticBackendPreference,
    acquisition_source: &'static str,
    acquisition_fallback: Option<&'static str>,
    model_fingerprint: String,
    backend_fingerprint: String,
    canary_passed: bool,
}

#[cfg(ctx_semantic_fastembed)]
impl SemanticEmbedder {
    pub(super) fn embed_query(&mut self, query: String) -> Result<Vec<f32>> {
        self.backend.embed_query(query)
    }

    pub(super) fn embed_documents(&mut self, documents: Vec<String>) -> Result<Vec<Vec<f32>>> {
        self.backend.embed_documents(documents, self.batch_size)
    }

    pub(super) fn runtime_info(&self) -> SemanticEmbeddingRuntimeInfo {
        let backend = self.backend.kind();
        SemanticEmbeddingRuntimeInfo {
            preference: self.preference,
            backend,
            assets_backend: self.backend.assets_backend(),
            model_variant: self.backend.model_variant(),
            compute_class: self.backend.compute_class(),
            compute_mode: self.backend.compute_mode(),
            acquisition_source: self.acquisition_source,
            acquisition_fallback: self.acquisition_fallback,
            runtime_artifact_identity: self.backend.runtime_artifact_identity(),
            model_fingerprint: self.model_fingerprint.clone(),
            backend_fingerprint: self.backend_fingerprint.clone(),
            canary_passed: self.canary_passed,
        }
    }

    pub(super) fn quiet_policy(&self) -> SemanticQuietPolicy {
        semantic_quiet_policy(
            SemanticSystemResources::current(),
            self.backend.compute_class(),
        )
    }
}

#[cfg(ctx_semantic_fastembed)]
pub(super) fn normalize_and_validate_embeddings(
    mut embeddings: Vec<Vec<f32>>,
    expected_count: usize,
) -> Result<Vec<Vec<f32>>> {
    if embeddings.len() != expected_count {
        return Err(anyhow!(
            "semantic model returned {} embeddings, expected {expected_count}",
            embeddings.len()
        ));
    }
    for embedding in &mut embeddings {
        if embedding.len() != SEMANTIC_DIMENSIONS {
            return Err(anyhow!(
                "semantic model returned {} dimensions, expected {}",
                embedding.len(),
                SEMANTIC_DIMENSIONS
            ));
        }
        if embedding.iter().any(|value| !value.is_finite()) {
            return Err(anyhow!(
                "semantic model returned a non-finite embedding value"
            ));
        }
        let norm = embedding
            .iter()
            .map(|value| f64::from(*value) * f64::from(*value))
            .sum::<f64>()
            .sqrt();
        if !norm.is_finite() || norm <= f64::EPSILON {
            return Err(anyhow!("semantic model returned a zero-norm embedding"));
        }
        for value in embedding {
            *value = (f64::from(*value) / norm) as f32;
        }
    }
    Ok(embeddings)
}

#[cfg(ctx_semantic_fastembed)]
fn acquire_semantic_embedder_from_cache(config: &SemanticModelConfig) -> Result<SemanticEmbedder> {
    let preference = config.backend_preference()?;
    let acquired = match preference {
        SemanticBackendPreference::Cpu => acquire_cpu_backend(
            config,
            semantic_embed_policy_for(SemanticComputeClass::Cpu, config),
            preference,
        ),
        SemanticBackendPreference::CoreMl => acquire_coreml_backend(config, preference, None),
        SemanticBackendPreference::Cuda => {
            acquire_accelerator_backend(config, preference, SemanticBackendKind::OrtCuda)
        }
        SemanticBackendPreference::WindowsMl => {
            acquire_accelerator_backend(config, preference, SemanticBackendKind::WindowsMl)
        }
        SemanticBackendPreference::Auto => {
            #[cfg(target_os = "macos")]
            {
                acquire_auto_coreml_backend_with(
                    || {
                        acquire_coreml_backend(config, preference, None)
                            .and_then(authorize_loaded_backend)
                    },
                    |fallback| {
                        acquire_cpu_backend(
                            config,
                            semantic_embed_policy_for(SemanticComputeClass::Cpu, config),
                            preference,
                        )
                        .map(|mut embedder| {
                            embedder.acquisition_fallback = Some(fallback);
                            embedder
                        })
                    },
                )
            }
            #[cfg(not(target_os = "macos"))]
            {
                match automatic_ort_accelerator_backend() {
                    Some(kind) => match acquire_accelerator_backend(config, preference, kind) {
                        Ok(embedder) => Ok(embedder),
                        Err(error) => acquire_cpu_fallback_backend(
                            config,
                            semantic_embed_policy_for(SemanticComputeClass::Cpu, config),
                            preference,
                            kind,
                        )
                        .map(|mut embedder| {
                            embedder.acquisition_fallback = Some(accelerator_fallback_reason(kind));
                            embedder
                        })
                        .with_context(|| {
                            format!(
                                "accelerator {} was unusable and CPU fallback failed: {error:#}",
                                kind.as_str()
                            )
                        }),
                    },
                    None => acquire_cpu_backend(
                        config,
                        semantic_embed_policy_for(SemanticComputeClass::Cpu, config),
                        preference,
                    ),
                }
            }
        }
    }?;
    authorize_loaded_backend(acquired)
}

#[cfg(ctx_semantic_fastembed)]
fn acquire_semantic_embedder_after_daemon_acquisition(
    config: &SemanticModelConfig,
    acquisition: SemanticDaemonModelAcquisition,
) -> Result<SemanticEmbedder> {
    let preference = config.backend_preference()?;
    let acquired = match acquisition.backend {
        SemanticModelAcquisitionBackend::Cpu => acquire_ort_backend(
            config,
            semantic_embed_policy_for(SemanticComputeClass::Cpu, config),
            preference,
            SemanticBackendKind::Cpu,
            acquisition.assets.kind(),
        ),
        SemanticModelAcquisitionBackend::CoreMl => {
            match acquire_coreml_backend(config, preference, None)
                .and_then(authorize_loaded_backend)
            {
                Ok(embedder) => Ok(embedder),
                #[cfg(any(target_os = "macos", test))]
                Err(error) => Err(map_daemon_coreml_load_error(acquisition, error)),
                #[cfg(not(any(target_os = "macos", test)))]
                Err(error) => Err(error),
            }
        }
        SemanticModelAcquisitionBackend::Cuda | SemanticModelAcquisitionBackend::WindowsMl => {
            let kind = acquisition.backend.kind();
            acquire_accelerator_backend(config, preference, kind)
                .map_err(|error| map_daemon_accelerator_load_error(acquisition, error))
        }
    }?;
    authorize_loaded_backend(acquired)
}

#[cfg(all(
    ctx_semantic_fastembed,
    any(target_os = "macos", test, feature = "test-support")
))]
pub(crate) fn map_daemon_coreml_load_error(
    acquisition: SemanticDaemonModelAcquisition,
    error: anyhow::Error,
) -> anyhow::Error {
    if acquisition.allow_cpu_fallback
        && error.downcast_ref::<SemanticModelLoadDeferred>().is_none()
        && !semantic_model_acquisition_integrity_error(&error)
    {
        let fallback = coreml_fallback_reason(&error);
        SemanticDaemonCpuFallbackRequired::new(fallback, &error).into()
    } else {
        error
    }
}

#[cfg(ctx_semantic_fastembed)]
fn acquire_semantic_model_for_daemon(
    config: &SemanticModelConfig,
    artifact_fetcher: &dyn ArtifactFetcher,
) -> Result<SemanticDaemonModelAcquisition> {
    let preference = config.backend_preference()?;
    let cache_dir = config.paths().model_cache_dir();
    match preference {
        SemanticBackendPreference::Cpu => acquire_cpu_model_for_daemon(cache_dir),
        SemanticBackendPreference::CoreMl => {
            acquire_coreml_model_for_daemon(config, artifact_fetcher)
        }
        SemanticBackendPreference::Cuda => {
            acquire_accelerator_model_for_daemon(config, SemanticModelAcquisitionBackend::Cuda)
        }
        SemanticBackendPreference::WindowsMl => {
            acquire_accelerator_model_for_daemon(config, SemanticModelAcquisitionBackend::WindowsMl)
        }
        SemanticBackendPreference::Auto => {
            #[cfg(target_os = "macos")]
            {
                acquire_auto_semantic_model_for_daemon_with(
                    || acquire_coreml_model_for_daemon(config, artifact_fetcher),
                    || acquire_cpu_model_for_daemon(cache_dir),
                )
            }
            #[cfg(not(target_os = "macos"))]
            {
                match automatic_ort_accelerator_backend() {
                    Some(kind) => {
                        let backend = match kind {
                            SemanticBackendKind::OrtCuda => SemanticModelAcquisitionBackend::Cuda,
                            SemanticBackendKind::WindowsMl => {
                                SemanticModelAcquisitionBackend::WindowsMl
                            }
                            _ => unreachable!(),
                        };
                        match acquire_accelerator_model_for_daemon(config, backend) {
                            Ok(acquisition) => Ok(acquisition.allowing_cpu_fallback_for_auto()),
                            Err(_) => acquire_cpu_model_for_daemon(cache_dir).map(|acquisition| {
                                acquisition.with_fallback(accelerator_fallback_reason(kind))
                            }),
                        }
                    }
                    None => acquire_cpu_model_for_daemon(cache_dir),
                }
            }
        }
    }
}

#[cfg(all(ctx_semantic_fastembed, any(target_os = "macos", test)))]
fn acquire_auto_semantic_model_for_daemon_with<CoreMl, Cpu>(
    acquire_coreml: CoreMl,
    acquire_cpu: Cpu,
) -> Result<SemanticDaemonModelAcquisition>
where
    CoreMl: FnOnce() -> Result<SemanticDaemonModelAcquisition>,
    Cpu: FnOnce() -> Result<SemanticDaemonModelAcquisition>,
{
    match acquire_coreml() {
        Ok(acquisition) => Ok(acquisition.allowing_cpu_fallback_for_auto()),
        Err(error) if error.downcast_ref::<SemanticModelLoadDeferred>().is_some() => Err(error),
        Err(error) if semantic_model_acquisition_integrity_error(&error) => Err(error),
        Err(error) => {
            let fallback = coreml_fallback_reason(&error);
            acquire_cpu().map(|acquisition| acquisition.with_fallback(fallback))
        }
    }
}

#[cfg(ctx_semantic_fastembed)]
fn reacquire_semantic_embedder(
    config: &SemanticModelConfig,
    runtime: &SemanticEmbeddingRuntimeInfo,
) -> Result<SemanticEmbedder> {
    let acquired = match runtime.backend {
        SemanticBackendKind::Cpu => acquire_ort_backend(
            config,
            semantic_embed_policy_for(SemanticComputeClass::Cpu, config),
            runtime.preference,
            SemanticBackendKind::Cpu,
            runtime.assets_backend,
        )
        .map(|mut embedder| {
            embedder.acquisition_fallback = runtime.acquisition_fallback;
            embedder
        }),
        SemanticBackendKind::CoreMl => recover_coreml_after_inference_with(
            runtime.preference,
            || acquire_coreml_backend(config, runtime.preference, runtime.acquisition_fallback),
            || {
                acquire_cpu_backend(
                    config,
                    semantic_embed_policy_for(SemanticComputeClass::Cpu, config),
                    runtime.preference,
                )
                .map(|mut embedder| {
                    embedder.acquisition_fallback =
                        Some(accelerator_fallback_reason(SemanticBackendKind::CoreMl));
                    embedder
                })
            },
        ),
        SemanticBackendKind::OrtCuda | SemanticBackendKind::WindowsMl => {
            acquire_cpu_fallback_backend(
                config,
                semantic_embed_policy_for(SemanticComputeClass::Cpu, config),
                runtime.preference,
                runtime.assets_backend,
            )
            .map(|mut embedder| {
                embedder.acquisition_fallback = Some(accelerator_fallback_reason(runtime.backend));
                embedder
            })
        }
    }?;
    authorize_loaded_backend(acquired)
}

mod cpu;
#[cfg(ctx_semantic_fastembed)]
mod document_batches;
#[cfg(all(test, ctx_semantic_fastembed))]
pub(super) use cpu::acquire_cpu_backend;
#[cfg(all(ctx_semantic_fastembed, not(target_os = "macos")))]
use cpu::automatic_ort_accelerator_backend;
pub use cpu::prepare_platform_semantic_acceleration;
#[cfg(all(ctx_semantic_fastembed, not(test)))]
use cpu::{
    accelerator_fallback_reason, acquire_accelerator_backend, acquire_accelerator_model_for_daemon,
    acquire_cpu_backend, acquire_cpu_fallback_backend, acquire_cpu_model_for_daemon,
    acquire_ort_backend, authorize_loaded_backend, map_daemon_accelerator_load_error,
};
#[cfg(all(test, ctx_semantic_fastembed))]
use cpu::{
    accelerator_fallback_reason, acquire_accelerator_backend, acquire_accelerator_model_for_daemon,
    acquire_cpu_fallback_backend, acquire_cpu_model_for_daemon, acquire_ort_backend,
    authorize_loaded_backend, map_daemon_accelerator_load_error, run_semantic_contract_canary,
    semantic_backend_requires_contract_canary, SemanticContractCanaryExecutor,
};
pub use cpu::{semantic_native_accelerator_target, SemanticNativeAcceleratorTarget};
mod coreml;
#[cfg(all(
    ctx_semantic_fastembed,
    any(target_os = "macos", test, feature = "test-support")
))]
use coreml::coreml_fallback_reason;
#[cfg(test)]
use coreml::*;
#[cfg(all(ctx_semantic_fastembed, target_os = "macos"))]
use coreml::{acquire_auto_coreml_backend_with, CoreMlE5Embedder};
#[cfg(ctx_semantic_fastembed)]
use coreml::{
    acquire_coreml_backend, acquire_coreml_model_for_daemon, recover_coreml_after_inference_with,
};
#[cfg(all(test, ctx_semantic_fastembed))]
pub(super) use coreml::{pad_texts_to_exact_batch, semantic_fixed_shape_from_values};
mod cache;
mod onnx;
mod windows_ml;
#[cfg(ctx_semantic_fastembed)]
pub(super) use cache::{
    maybe_cleanup_semantic_cpu_download_cache_after_cached_acquisition,
    read_semantic_ort_model_file, replace_cpu_model_cache_from_pinned_revision,
    semantic_cpu_cache_repairable, semantic_cpu_cache_snapshot, semantic_ort_cache_snapshot,
};
#[allow(unused_imports)]
#[cfg(all(any(test, feature = "test-support"), ctx_semantic_fastembed))]
pub(crate) use onnx::load_missing_semantic_onnxruntime_for_test;

#[cfg(not(ctx_semantic_fastembed))]
pub(super) struct SemanticEmbedder;

#[cfg(not(ctx_semantic_fastembed))]
fn acquire_semantic_embedder_from_cache(_config: &SemanticModelConfig) -> Result<SemanticEmbedder> {
    Err(anyhow!(
        "semantic embedding model {SEMANTIC_MODEL_ID} is not supported on this platform"
    ))
}

#[cfg(not(ctx_semantic_fastembed))]
fn acquire_semantic_model_for_daemon(
    _config: &SemanticModelConfig,
    _artifact_fetcher: &dyn ArtifactFetcher,
) -> Result<SemanticDaemonModelAcquisition> {
    Err(anyhow!(
        "semantic embedding model {SEMANTIC_MODEL_ID} is not supported on this platform"
    ))
}

#[cfg(all(test, ctx_semantic_fastembed))]
mod tests;
