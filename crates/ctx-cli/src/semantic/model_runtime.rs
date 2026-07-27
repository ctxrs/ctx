use std::{
    env,
    path::Path,
    sync::{Arc, Mutex},
    time::Instant,
};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use crate::output::compact_json;

use super::{
    cache_paths,
    health_search::{
        semantic_embed_policy_for, semantic_embedder_policy_status_json,
        semantic_embedder_runtime_status_json, SemanticEmbedPolicy,
    },
    indexing::{semantic_e5_passage_text, semantic_e5_query_text_value},
    model_contract::{
        semantic_model_key, SemanticCpuModelCacheMissing, SemanticCpuModelIntegrityError,
        SemanticModelFile, SEMANTIC_DIMENSIONS, SEMANTIC_HF_MODEL_CACHE_DIR,
        SEMANTIC_MANAGED_MODEL_CACHE_DIR, SEMANTIC_MODEL_ID, SEMANTIC_MODEL_REVISION,
        SEMANTIC_REQUIRED_MODEL_FILES,
    },
    resource_policy::{
        semantic_quiet_policy, throttle_semantic_batch, SemanticComputeClass, SemanticQuietPolicy,
        SemanticSystemResources,
    },
};

#[cfg(any(target_os = "macos", test))]
use super::health_search::semantic_model_acquisition_integrity_error;
#[cfg(test)]
use super::resource_policy::semantic_model_load_deferred;

#[derive(Clone, Default)]
pub(super) struct SharedSemanticRuntime {
    embedder: Arc<Mutex<Option<SemanticEmbedder>>>,
}

impl SharedSemanticRuntime {
    pub(super) fn is_loaded(&self) -> bool {
        self.embedder
            .lock()
            .map(|embedder| embedder.is_some())
            .unwrap_or(false)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Option<SemanticEmbedder>>> {
        self.embedder
            .lock()
            .map_err(|_| anyhow!("semantic embedder lock is poisoned"))
    }

    #[cfg(test)]
    pub(super) fn lock_for_test(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, Option<SemanticEmbedder>>> {
        self.lock()
    }

    pub(super) fn ensure_loaded_from_cache(&self, cache_dir: &Path) -> Result<Option<u64>> {
        self.ensure_loaded(cache_dir, SemanticModelAccess::ForegroundCacheOnly)
    }

    pub(super) fn ensure_loaded_for_daemon(&self, cache_dir: &Path) -> Result<Option<u64>> {
        self.ensure_loaded(cache_dir, SemanticModelAccess::DaemonNetwork)
    }

    fn ensure_loaded(&self, cache_dir: &Path, access: SemanticModelAccess) -> Result<Option<u64>> {
        let mut embedder = self.lock()?;
        if embedder.is_some() {
            return Ok(None);
        }
        let started = Instant::now();
        *embedder = Some(acquire_semantic_embedder_with_mode(cache_dir, access)?);
        Ok(Some(started.elapsed().as_millis() as u64))
    }

    pub(super) fn policy_status_json(&self) -> Result<Value> {
        let embedder = self.lock()?;
        Ok(semantic_embedder_policy_status_json(&embedder))
    }

    pub(super) fn runtime_status_json(&self) -> Result<Option<Value>> {
        let embedder = self.lock()?;
        Ok(semantic_embedder_runtime_status_json(&embedder))
    }

    pub(super) fn try_runtime_status_json(&self) -> Result<(Option<Value>, bool)> {
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
    pub(super) fn loaded_batch_size(&self) -> Result<Option<usize>> {
        Ok(self.lock()?.as_ref().map(|embedder| embedder.batch_size))
    }

    #[cfg(ctx_semantic_fastembed)]
    pub(super) fn active_batch_size(&self) -> Result<usize> {
        let embedder = self.lock()?;
        let embedder = embedder
            .as_ref()
            .ok_or_else(|| anyhow!("semantic embedder was not initialized"))?;
        Ok(embedder
            .batch_size
            .min(embedder.quiet_policy().batch_size)
            .max(1))
    }

    #[cfg(ctx_semantic_fastembed)]
    pub(super) fn embed_documents(
        &self,
        cache_dir: &Path,
        texts: Vec<String>,
        deadline: Option<Instant>,
    ) -> Result<(Vec<Vec<f32>>, SemanticQuietPolicy)> {
        let mut embedder = self.lock()?;
        let started = Instant::now();
        let first = embedder
            .as_mut()
            .ok_or_else(|| anyhow!("semantic embedder was not initialized"))?
            .embed_documents(texts.clone());
        let embeddings = match first {
            Ok(embeddings) => embeddings,
            Err(first_error) => {
                let runtime = embedder
                    .as_ref()
                    .ok_or_else(|| {
                        anyhow!("semantic embedder disappeared after inference failure")
                    })?
                    .runtime_info();
                *embedder = None;
                let mut replacement = reacquire_semantic_embedder(cache_dir, &runtime)
                    .context("reinitialize semantic embedder after document inference failure")?;
                let retry = replacement.embed_documents(texts).with_context(|| {
                    format!(
                        "semantic document inference failed twice; first failure: {first_error:#}"
                    )
                })?;
                *embedder = Some(replacement);
                retry
            }
        };
        let quiet_policy = embedder
            .as_ref()
            .ok_or_else(|| anyhow!("semantic embedder was not initialized"))?
            .quiet_policy();
        drop(embedder);
        let active = started.elapsed();
        let remaining = deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
        throttle_semantic_batch(active, quiet_policy, remaining);
        Ok((embeddings, quiet_policy))
    }

    #[cfg(ctx_semantic_fastembed)]
    pub(super) fn embed_query(
        &self,
        cache_dir: &Path,
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
                let mut replacement = reacquire_semantic_embedder(cache_dir, &runtime)
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

pub(super) const SEMANTIC_BACKEND_PREFERENCE_ENV: &str = "CTX_INTERNAL_SEMANTIC_BACKEND";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SemanticModelAccess {
    ForegroundCacheOnly,
    DaemonNetwork,
}

impl SemanticModelAccess {
    pub(super) fn network_allowed(self) -> bool {
        self == Self::DaemonNetwork
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BackendPreference {
    Auto,
    Cpu,
    CoreMl,
}

impl BackendPreference {
    pub(super) fn from_env() -> Result<Self> {
        Self::parse(env::var(SEMANTIC_BACKEND_PREFERENCE_ENV).ok().as_deref())
    }

    pub(super) fn parse(value: Option<&str>) -> Result<Self> {
        match value.map(str::trim).filter(|value| !value.is_empty()) {
            None | Some("auto") => Ok(Self::Auto),
            Some("cpu") => Ok(Self::Cpu),
            Some("coreml") => Ok(Self::CoreMl),
            Some(value) => Err(anyhow!(
                "unsupported {SEMANTIC_BACKEND_PREFERENCE_ENV} value {value:?}; expected auto, cpu, or coreml"
            )),
        }
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Cpu => "cpu",
            Self::CoreMl => "coreml",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SemanticEmbeddingRuntimeInfo {
    preference: BackendPreference,
    backend: &'static str,
    compute_class: SemanticComputeClass,
    compute_mode: Option<&'static str>,
    acquisition_source: &'static str,
    acquisition_fallback: Option<&'static str>,
}

impl SemanticEmbeddingRuntimeInfo {
    pub(super) fn to_json(&self) -> Value {
        compact_json(json!({
            "preference": self.preference.as_str(),
            "backend": self.backend,
            "compute_class": self.compute_class.as_str(),
            "compute_mode": self.compute_mode,
            "model_id": SEMANTIC_MODEL_ID,
            "model_key": semantic_model_key(),
            "dimensions": SEMANTIC_DIMENSIONS,
            "acquisition_source": self.acquisition_source,
            "acquisition_fallback": self.acquisition_fallback,
        }))
    }
}

#[cfg(ctx_semantic_fastembed)]
enum SemanticEmbeddingBackend {
    Cpu(fastembed::TextEmbedding),
    #[cfg(target_os = "macos")]
    CoreMl(CoreMlE5Embedder),
}

#[cfg(ctx_semantic_fastembed)]
impl SemanticEmbeddingBackend {
    pub(super) fn embed_query(&mut self, query: String) -> Result<Vec<f32>> {
        let query = semantic_e5_query_text_value(&query);
        let raw = match self {
            Self::Cpu(model) => model
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
            Self::Cpu(model) => model.embed(documents, Some(batch_size)).with_context(|| {
                format!("embed documents with semantic model {SEMANTIC_MODEL_ID}")
            })?,
            #[cfg(target_os = "macos")]
            Self::CoreMl(model) => model.embed_documents(documents)?,
        };
        normalize_and_validate_embeddings(raw, expected)
    }

    pub(super) fn name(&self) -> &'static str {
        match self {
            Self::Cpu(_) => "cpu",
            #[cfg(target_os = "macos")]
            Self::CoreMl(_) => "coreml",
        }
    }

    pub(super) fn compute_class(&self) -> SemanticComputeClass {
        match self {
            Self::Cpu(_) => SemanticComputeClass::Cpu,
            #[cfg(target_os = "macos")]
            Self::CoreMl(model) => model.compute_class(),
        }
    }

    pub(super) fn compute_mode(&self) -> Option<&'static str> {
        match self {
            Self::Cpu(_) => None,
            #[cfg(target_os = "macos")]
            Self::CoreMl(model) => Some(model.compute_mode()),
        }
    }
}

#[cfg(ctx_semantic_fastembed)]
pub(super) struct SemanticEmbedder {
    backend: SemanticEmbeddingBackend,
    pub(super) batch_size: usize,
    pub(super) policy: SemanticEmbedPolicy,
    preference: BackendPreference,
    acquisition_source: &'static str,
    acquisition_fallback: Option<&'static str>,
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
        SemanticEmbeddingRuntimeInfo {
            preference: self.preference,
            backend: self.backend.name(),
            compute_class: self.backend.compute_class(),
            compute_mode: self.backend.compute_mode(),
            acquisition_source: self.acquisition_source,
            acquisition_fallback: self.acquisition_fallback,
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
fn acquire_semantic_embedder_with_mode(
    cache_dir: &Path,
    access: SemanticModelAccess,
) -> Result<SemanticEmbedder> {
    let preference = BackendPreference::from_env()?;
    match preference {
        BackendPreference::Cpu => acquire_cpu_backend(
            cache_dir,
            semantic_embed_policy_for(SemanticComputeClass::Cpu),
            preference,
            access.network_allowed(),
        ),
        BackendPreference::CoreMl => {
            acquire_coreml_backend(cache_dir, preference, None, access.network_allowed())
        }
        BackendPreference::Auto => {
            #[cfg(target_os = "macos")]
            {
                match acquire_coreml_backend(cache_dir, preference, None, access.network_allowed())
                {
                    Ok(embedder) => Ok(embedder),
                    Err(error) if semantic_model_acquisition_integrity_error(&error) => Err(error),
                    Err(error) => {
                        let fallback = coreml_fallback_reason(&error);
                        acquire_cpu_backend(
                            cache_dir,
                            semantic_embed_policy_for(SemanticComputeClass::Cpu),
                            preference,
                            access.network_allowed(),
                        )
                        .map(|mut embedder| {
                            embedder.acquisition_fallback = Some(fallback);
                            embedder
                        })
                    }
                }
            }
            #[cfg(not(target_os = "macos"))]
            {
                acquire_cpu_backend(
                    cache_dir,
                    semantic_embed_policy_for(SemanticComputeClass::Cpu),
                    preference,
                    access.network_allowed(),
                )
            }
        }
    }
}

#[cfg(ctx_semantic_fastembed)]
fn reacquire_semantic_embedder(
    cache_dir: &Path,
    runtime: &SemanticEmbeddingRuntimeInfo,
) -> Result<SemanticEmbedder> {
    match runtime.backend {
        "cpu" => acquire_cpu_backend(
            cache_dir,
            semantic_embed_policy_for(SemanticComputeClass::Cpu),
            runtime.preference,
            false,
        )
        .map(|mut embedder| {
            embedder.acquisition_fallback = runtime.acquisition_fallback;
            embedder
        }),
        "coreml" => acquire_coreml_backend(
            cache_dir,
            runtime.preference,
            runtime.acquisition_fallback,
            false,
        ),
        backend => Err(anyhow!(
            "cannot reacquire unsupported semantic backend {backend:?}"
        )),
    }
}

mod cpu;
#[cfg(all(ctx_semantic_fastembed, not(test)))]
use cpu::acquire_cpu_backend;
#[cfg(all(test, ctx_semantic_fastembed))]
pub(super) use cpu::acquire_cpu_backend;
mod coreml;
#[cfg(ctx_semantic_fastembed)]
use coreml::acquire_coreml_backend;
#[cfg(test)]
use coreml::*;
#[cfg(all(ctx_semantic_fastembed, target_os = "macos"))]
use coreml::{coreml_fallback_reason, CoreMlE5Embedder};
#[cfg(all(test, ctx_semantic_fastembed))]
pub(super) use coreml::{pad_texts_to_exact_batch, semantic_fixed_shape_from_values};
mod cache;
mod onnx;
#[cfg(ctx_semantic_fastembed)]
pub(super) use cache::{
    maybe_cleanup_semantic_cpu_download_cache_after_cached_acquisition, read_semantic_model_file,
    replace_cpu_model_cache_from_pinned_revision, semantic_cpu_cache_repairable,
    semantic_cpu_cache_snapshot,
};

#[cfg(not(ctx_semantic_fastembed))]
pub(super) struct SemanticEmbedder;

#[cfg(not(ctx_semantic_fastembed))]
fn acquire_semantic_embedder_with_mode(
    _cache_dir: &Path,
    _access: SemanticModelAccess,
) -> Result<SemanticEmbedder> {
    Err(anyhow!(
        "semantic embedding model {SEMANTIC_MODEL_ID} is not supported on this platform"
    ))
}

#[cfg(all(test, ctx_semantic_fastembed))]
mod embedding_backend_tests {
    use super::*;

    #[test]
    pub(super) fn backend_preference_is_strict() {
        assert_eq!(
            BackendPreference::parse(None).unwrap(),
            BackendPreference::Auto
        );
        assert_eq!(
            BackendPreference::parse(Some("cpu")).unwrap(),
            BackendPreference::Cpu
        );
        assert_eq!(
            BackendPreference::parse(Some("coreml")).unwrap(),
            BackendPreference::CoreMl
        );
        assert!(BackendPreference::parse(Some("gpu")).is_err());
        assert!(BackendPreference::parse(Some("CPU")).is_err());
    }

    #[test]
    pub(super) fn foreground_model_access_is_cache_only() {
        assert!(!SemanticModelAccess::ForegroundCacheOnly.network_allowed());
        assert!(SemanticModelAccess::DaemonNetwork.network_allowed());
    }

    #[test]
    pub(super) fn coreml_cpu_only_uses_cpu_quiet_policy_class() {
        let cpu_only = CoreMlComputeMode::parse("cpu").unwrap();
        assert_eq!(cpu_only.compute_class(), SemanticComputeClass::Cpu);
        assert_eq!(cpu_only.as_str(), "cpu_only");
        let all = CoreMlComputeMode::parse("all").unwrap();
        assert_eq!(all.compute_class(), SemanticComputeClass::Accelerator);

        let available = 5 * 512 * 1024 * 1024;
        assert!(semantic_model_load_deferred(Some(available), cpu_only.compute_class()).is_none());
        assert!(semantic_model_load_deferred(Some(available), all.compute_class()).is_some());
    }

    #[test]
    pub(super) fn normalization_is_central_and_strict() {
        let mut vector = vec![0.0; SEMANTIC_DIMENSIONS];
        vector[0] = 3.0;
        vector[1] = 4.0;
        let normalized = normalize_and_validate_embeddings(vec![vector], 1).unwrap();
        assert!((normalized[0][0] - 0.6).abs() < 1e-6);
        assert!((normalized[0][1] - 0.8).abs() < 1e-6);

        assert!(normalize_and_validate_embeddings(Vec::new(), 1).is_err());
        assert!(normalize_and_validate_embeddings(vec![vec![1.0]], 1).is_err());
        assert!(
            normalize_and_validate_embeddings(vec![vec![0.0; SEMANTIC_DIMENSIONS]], 1).is_err()
        );
        let mut non_finite = vec![1.0; SEMANTIC_DIMENSIONS];
        non_finite[0] = f32::NAN;
        assert!(normalize_and_validate_embeddings(vec![non_finite], 1).is_err());
    }

    #[test]
    pub(super) fn runtime_info_keeps_space_identity_backend_independent() {
        let cpu = SemanticEmbeddingRuntimeInfo {
            preference: BackendPreference::Auto,
            backend: "cpu",
            compute_class: SemanticComputeClass::Cpu,
            compute_mode: None,
            acquisition_source: "cache",
            acquisition_fallback: None,
        };
        let coreml = SemanticEmbeddingRuntimeInfo {
            backend: "coreml",
            ..cpu.clone()
        };
        assert_eq!(cpu.to_json()["model_key"], coreml.to_json()["model_key"]);
        assert_ne!(cpu.to_json()["backend"], coreml.to_json()["backend"]);
    }

    #[test]
    pub(super) fn shared_runtime_clones_one_model_state_owner() {
        let runtime = SharedSemanticRuntime::default();
        let query_runtime = runtime.clone();

        assert!(Arc::ptr_eq(&runtime.embedder, &query_runtime.embedder));
        assert!(!runtime.is_loaded());
        assert!(!query_runtime.is_loaded());
    }
}
