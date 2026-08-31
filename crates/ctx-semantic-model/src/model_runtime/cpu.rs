#[cfg(ctx_semantic_fastembed)]
pub(crate) fn acquire_cpu_backend(
    config: &SemanticModelConfig,
    policy: SemanticEmbedPolicy,
    preference: SemanticBackendPreference,
) -> Result<SemanticEmbedder> {
    acquire_ort_backend(
        config,
        policy,
        preference,
        SemanticBackendKind::Cpu,
        SemanticBackendKind::Cpu,
    )
}

#[cfg(ctx_semantic_fastembed)]
pub(crate) fn acquire_cpu_fallback_backend(
    config: &SemanticModelConfig,
    policy: SemanticEmbedPolicy,
    preference: SemanticBackendPreference,
    accelerator_assets: SemanticBackendKind,
) -> Result<SemanticEmbedder> {
    acquire_ort_backend(
        config,
        policy.clone(),
        preference,
        SemanticBackendKind::Cpu,
        accelerator_assets,
    )
    .or_else(|accelerator_model_error| {
        acquire_cpu_backend(config, policy, preference).with_context(|| {
            format!(
                "accelerator-model CPU fallback failed ({accelerator_model_error:#}); pinned fp32 CPU fallback also failed"
            )
        })
    })
}

#[cfg(ctx_semantic_fastembed)]
pub(crate) fn acquire_ort_backend(
    config: &SemanticModelConfig,
    policy: SemanticEmbedPolicy,
    preference: SemanticBackendPreference,
    kind: SemanticBackendKind,
    assets: SemanticBackendKind,
) -> Result<SemanticEmbedder> {
    acquire_ort_backend_with_provider_policy(config, policy, preference, kind, assets, true)
}

#[cfg(ctx_semantic_fastembed)]
pub(crate) fn acquire_ort_backend_passively(
    config: &SemanticModelConfig,
    policy: SemanticEmbedPolicy,
    preference: SemanticBackendPreference,
    kind: SemanticBackendKind,
    assets: SemanticBackendKind,
) -> Result<SemanticEmbedder> {
    acquire_ort_backend_with_provider_policy(config, policy, preference, kind, assets, false)
}

#[cfg(ctx_semantic_fastembed)]
fn acquire_ort_backend_with_provider_policy(
    config: &SemanticModelConfig,
    policy: SemanticEmbedPolicy,
    preference: SemanticBackendPreference,
    kind: SemanticBackendKind,
    assets: SemanticBackendKind,
    allow_provider_prepare: bool,
) -> Result<SemanticEmbedder> {
    if let Some(deferred) = semantic_cpu_model_load_deferred(policy.available_memory_bytes) {
        if kind == SemanticBackendKind::Cpu {
            return Err(deferred.into());
        }
    }
    if config.deprecated_model_onnx_present() {
        return Err(anyhow!(
            "CTX_SEMANTIC_MODEL_ONNX is no longer accepted; embeddings use the verified {SEMANTIC_MODEL_ID} cache"
        ));
    }
    let cache_dir = config.paths().model_cache_dir();
    let variant = SemanticOrtModelVariant::for_backend(assets);
    let snapshot = semantic_ort_cache_snapshot(cache_dir, variant)?;
    let (model, runtime_artifact_identity, windows_ml_registration) = load_cached_ort_model(
        &snapshot,
        config,
        &policy,
        kind,
        variant,
        allow_provider_prepare,
    )?;
    Ok(SemanticEmbedder {
        backend: SemanticEmbeddingBackend::Ort {
            model,
            kind,
            assets,
            variant,
            runtime_artifact_identity,
            _windows_ml_registration: windows_ml_registration,
        },
        batch_size: policy.batch_size,
        preference,
        acquisition_source: "cache",
        acquisition_fallback: None,
        model_fingerprint: String::new(),
        backend_fingerprint: String::new(),
        canary_passed: false,
    })
}

#[cfg(ctx_semantic_fastembed)]
pub(crate) fn acquire_cpu_model_for_daemon(
    cache_dir: &Path,
) -> Result<SemanticDaemonModelAcquisition> {
    let source = match semantic_cpu_cache_snapshot(cache_dir) {
        Ok(_) => {
            maybe_cleanup_semantic_cpu_download_cache_after_cached_acquisition(cache_dir, true);
            SemanticModelAcquisitionSource::Cache
        }
        Err(error) if semantic_cpu_cache_repairable(&error) => {
            replace_cpu_model_cache_from_pinned_revision(cache_dir)?;
            SemanticModelAcquisitionSource::Download
        }
        Err(error) => return Err(error),
    };
    Ok(SemanticDaemonModelAcquisition::new(
        SemanticModelAcquisitionBackend::Cpu,
        source,
    ))
}

#[cfg(ctx_semantic_fastembed)]
pub(crate) fn acquire_accelerator_model_for_daemon(
    config: &SemanticModelConfig,
    backend: SemanticModelAcquisitionBackend,
) -> Result<SemanticDaemonModelAcquisition> {
    let cache_dir = config.paths().model_cache_dir();
    let (kind, flavor) = match backend {
        SemanticModelAcquisitionBackend::Cuda => {
            (SemanticBackendKind::OrtCuda, OnnxRuntimeFlavor::Cuda)
        }
        SemanticModelAcquisitionBackend::WindowsMl => {
            (SemanticBackendKind::WindowsMl, OnnxRuntimeFlavor::WindowsMl)
        }
        _ => return Err(anyhow!("requested backend is not an ORT accelerator")),
    };
    semantic_ort_cache_snapshot(cache_dir, SemanticOrtModelVariant::for_backend(kind)).map_err(
        |error| -> anyhow::Error {
            SemanticProvisioningRequired {
                asset: "intfloat/multilingual-e5-small@614241f",
                detail: error.to_string(),
            }
            .into()
        },
    )?;
    match installed_accelerator_runtime_identity(config.paths(), flavor)? {
        Some(_) => Ok(SemanticDaemonModelAcquisition::new(
            backend,
            SemanticModelAcquisitionSource::Cache,
        )),
        None => Err(SemanticProvisioningRequired {
            asset: flavor.asset_name(),
            detail: "verified accelerator runtime is not installed".to_owned(),
        }
        .into()),
    }
}

#[cfg(ctx_semantic_fastembed)]
#[allow(dead_code)] // Retained for focused cache/runtime regression tests.
pub(super) fn load_cached_cpu_model(
    snapshot: &Path,
    config: &SemanticModelConfig,
    policy: &SemanticEmbedPolicy,
) -> Result<fastembed::TextEmbedding> {
    load_cached_ort_model(
        snapshot,
        config,
        policy,
        SemanticBackendKind::Cpu,
        SemanticOrtModelVariant::CpuFp32,
        true,
    )
    .map(|(model, _, _)| model)
}

#[cfg(ctx_semantic_fastembed)]
pub(super) fn load_cached_ort_model(
    snapshot: &Path,
    config: &SemanticModelConfig,
    policy: &SemanticEmbedPolicy,
    kind: SemanticBackendKind,
    variant: SemanticOrtModelVariant,
    allow_provider_prepare: bool,
) -> Result<(
    fastembed::TextEmbedding,
    String,
    Option<windows_ml::WindowsMlProviderRegistration>,
)> {
    use fastembed::{
        EmbeddingModel, InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles,
        UserDefinedEmbeddingModel,
    };

    let accelerator_flavor = match kind {
        SemanticBackendKind::OrtCuda => Some(OnnxRuntimeFlavor::Cuda),
        SemanticBackendKind::WindowsMl => Some(OnnxRuntimeFlavor::WindowsMl),
        SemanticBackendKind::Cpu => None,
        SemanticBackendKind::CoreMl => {
            return Err(anyhow!(
                "Core ML cannot be initialized through ONNX Runtime"
            ));
        }
    };
    let (runtime_artifact_identity, loaded_accelerator) = if let Some(flavor) = accelerator_flavor {
        let runtime = ensure_semantic_accelerator_runtime_loaded(config.paths(), flavor).map_err(
            |error| -> anyhow::Error {
                SemanticProvisioningRequired {
                    asset: flavor.asset_name(),
                    detail: format!("{error:#}"),
                }
                .into()
            },
        )?;
        (runtime.artifact_identity.clone(), Some((runtime, flavor)))
    } else {
        let path = ensure_semantic_onnxruntime_loaded(config.paths())?;
        (
            onnx::loaded_runtime_artifact_identity()
                .unwrap_or_else(|| format!("ort-cpu|path={}", path.display())),
            None,
        )
    };
    let windows_ml_registration = if kind == SemanticBackendKind::WindowsMl {
        let (runtime, _) = loaded_accelerator
            .as_ref()
            .ok_or_else(|| anyhow!("Windows ML requires its verified runtime"))?;
        Some(
            windows_ml::register_providers(runtime, allow_provider_prepare)
                .context("register Windows ML execution providers")?,
        )
    } else {
        None
    };
    let model_info = TextEmbedding::get_model_info(&EmbeddingModel::MultilingualE5Small)?;
    let tokenizer_files = TokenizerFiles {
        tokenizer_file: read_semantic_ort_model_file(snapshot, "tokenizer.json", variant)?,
        config_file: read_semantic_ort_model_file(snapshot, "config.json", variant)?,
        special_tokens_map_file: read_semantic_ort_model_file(
            snapshot,
            "special_tokens_map.json",
            variant,
        )?,
        tokenizer_config_file: read_semantic_ort_model_file(
            snapshot,
            "tokenizer_config.json",
            variant,
        )?,
    };
    let mut user_model = UserDefinedEmbeddingModel::new(
        read_semantic_ort_model_file(snapshot, &model_info.model_file, variant)?,
        tokenizer_files,
    )
    .with_pooling(
        TextEmbedding::get_default_pooling_method(&EmbeddingModel::MultilingualE5Small)
            .unwrap_or(Pooling::Mean),
    )
    .with_quantization(TextEmbedding::get_quantization_mode(
        &EmbeddingModel::MultilingualE5Small,
    ));
    user_model.output_key = model_info.output_key.clone();
    let execution_providers = match kind {
        SemanticBackendKind::Cpu => {
            vec![ort::ep::CPU::default().build().error_on_failure()]
        }
        SemanticBackendKind::OrtCuda => {
            vec![ort::ep::CUDA::default()
                .with_device_id(0)
                .build()
                .error_on_failure()]
        }
        SemanticBackendKind::WindowsMl => {
            vec![ort::ep::DirectML::default()
                .with_device_filter(ort::ep::directml::DeviceFilter::Gpu)
                .with_performance_preference(
                    ort::ep::directml::PerformancePreference::HighPerformance,
                )
                .build()
                .error_on_failure()]
        }
        SemanticBackendKind::CoreMl => unreachable!(),
    };
    let model = TextEmbedding::try_new_from_user_defined(
        user_model,
        InitOptionsUserDefined::new()
            .with_intra_threads(policy.threads)
            .with_max_length(SEMANTIC_MAX_SEQUENCE_LENGTH)
            .with_execution_providers(execution_providers),
    )
    .with_context(|| format!("initialize semantic embedding model {SEMANTIC_MODEL_ID}"))?;
    if let Some((runtime, flavor)) = loaded_accelerator.as_ref() {
        revalidate_loaded_accelerator_runtime(runtime, *flavor)?;
    } else {
        ensure_semantic_onnxruntime_loaded(config.paths())
            .context("revalidate ONNX Runtime after CPU session initialization")?;
    }
    let runtime_artifact_identity = match windows_ml_registration.as_ref() {
        Some(registration) => {
            format!(
                "{runtime_artifact_identity}|catalog={}",
                registration.identity()
            )
        }
        None => runtime_artifact_identity,
    };
    Ok((model, runtime_artifact_identity, windows_ml_registration))
}

#[derive(Debug)]
struct SemanticBackendDegraded {
    backend: SemanticBackendKind,
    stage: &'static str,
    detail: String,
}

impl fmt::Display for SemanticBackendDegraded {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "semantic backend {} entered degraded state during {}: {}",
            self.backend.as_str(),
            self.stage,
            self.detail
        )
    }
}

impl std::error::Error for SemanticBackendDegraded {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Signed provisioning consumes this seam in a separate integration lane.
pub enum SemanticNativeAcceleratorTarget {
    CoreMl,
    Cuda,
    WindowsMl,
}

impl SemanticNativeAcceleratorTarget {
    #[allow(dead_code)] // Signed provisioning consumes this seam in a separate integration lane.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CoreMl => "coreml",
            Self::Cuda => "cuda",
            Self::WindowsMl => "windowsml",
        }
    }

    #[allow(dead_code)] // Signed provisioning consumes this seam in a separate integration lane.
    pub fn runtime_platform(self) -> Option<&'static str> {
        match self {
            Self::CoreMl => None,
            Self::Cuda => Some("linux-x64-cuda12"),
            Self::WindowsMl => Some("windows-x64"),
        }
    }

    #[cfg(all(ctx_semantic_fastembed, not(target_os = "macos")))]
    fn backend_kind(self) -> SemanticBackendKind {
        match self {
            Self::CoreMl => SemanticBackendKind::CoreMl,
            Self::Cuda => SemanticBackendKind::OrtCuda,
            Self::WindowsMl => SemanticBackendKind::WindowsMl,
        }
    }
}

#[allow(dead_code)] // Signed provisioning consumes this seam in a separate integration lane.
pub fn semantic_native_accelerator_target() -> Option<SemanticNativeAcceleratorTarget> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        return Some(SemanticNativeAcceleratorTarget::CoreMl);
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        return onnx::nvidia_accelerator_present().then_some(SemanticNativeAcceleratorTarget::Cuda);
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        return Some(SemanticNativeAcceleratorTarget::WindowsMl);
    }
    #[allow(unreachable_code)]
    None
}

#[allow(dead_code)] // Signed setup/provisioning calls this seam in a separate integration lane.
pub fn prepare_platform_semantic_acceleration(
    config: &SemanticModelConfig,
) -> Result<Option<String>> {
    #[cfg(all(ctx_semantic_fastembed, target_os = "windows", target_arch = "x86_64"))]
    {
        let runtime = onnx::ensure_semantic_accelerator_runtime_loaded(
            config.paths(),
            OnnxRuntimeFlavor::WindowsMl,
        )?;
        if !windows_ml::runtime_is_windows_ml(&runtime) {
            return Err(anyhow!("verified Windows ML runtime is incomplete"));
        }
        windows_ml::provision_catalog(&runtime).map(Some)
    }
    #[cfg(not(all(ctx_semantic_fastembed, target_os = "windows", target_arch = "x86_64")))]
    {
        let _ = config;
        Ok(None)
    }
}

#[cfg(ctx_semantic_fastembed)]
pub(super) fn acquire_accelerator_backend(
    config: &SemanticModelConfig,
    preference: SemanticBackendPreference,
    kind: SemanticBackendKind,
) -> Result<SemanticEmbedder> {
    let acquired = acquire_ort_backend(
        config,
        semantic_embed_policy_for(SemanticComputeClass::Accelerator, config),
        preference,
        kind,
        kind,
    )
    .map_err(|error| {
        anyhow::Error::from(SemanticBackendDegraded {
            backend: kind,
            stage: "initialization",
            detail: format!("{error:#}"),
        })
    })?;
    authorize_loaded_backend(acquired)
}

#[cfg(ctx_semantic_fastembed)]
pub(super) fn authorize_loaded_backend(mut embedder: SemanticEmbedder) -> Result<SemanticEmbedder> {
    let kind = embedder.backend.kind();
    let accelerator_assets = matches!(
        embedder.backend.assets_backend(),
        SemanticBackendKind::OrtCuda | SemanticBackendKind::WindowsMl
    );
    if semantic_backend_requires_contract_canary(kind, accelerator_assets, embedder.canary_passed) {
        let canary = run_semantic_contract_canary(&mut embedder);
        if let Err(error) = canary {
            if kind == SemanticBackendKind::Cpu {
                return Err(error).context("CPU fallback semantic canary failed");
            }
            return Err(SemanticBackendDegraded {
                backend: kind,
                stage: "model-contract canary",
                detail: format!("{error:#}"),
            }
            .into());
        }
        embedder.canary_passed = true;
    }
    embedder.model_fingerprint = model_fingerprint();
    embedder.backend_fingerprint = backend_fingerprint(
        &embedder.model_fingerprint,
        kind,
        &embedder.backend.runtime_artifact_identity(),
    );
    Ok(embedder)
}

#[cfg(ctx_semantic_fastembed)]
pub(super) fn semantic_backend_requires_contract_canary(
    kind: SemanticBackendKind,
    accelerator_assets: bool,
    canary_passed: bool,
) -> bool {
    !canary_passed && (kind != SemanticBackendKind::Cpu || accelerator_assets)
}

#[cfg(ctx_semantic_fastembed)]
pub(super) trait SemanticContractCanaryExecutor {
    fn embed_canary_query(&mut self, text: &str) -> Result<Vec<f32>>;
    fn embed_canary_passage(&mut self, text: &str) -> Result<Vec<f32>>;
}

#[cfg(ctx_semantic_fastembed)]
impl SemanticContractCanaryExecutor for SemanticEmbedder {
    fn embed_canary_query(&mut self, text: &str) -> Result<Vec<f32>> {
        self.embed_prepared_query(semantic_e5_query_text(text))
    }

    fn embed_canary_passage(&mut self, text: &str) -> Result<Vec<f32>> {
        self.embed_prepared_documents(vec![semantic_e5_passage_text(text)])?
            .pop()
            .ok_or_else(|| anyhow!("semantic canary document embedding is missing"))
    }
}

#[cfg(ctx_semantic_fastembed)]
pub(super) fn run_semantic_contract_canary(
    executor: &mut impl SemanticContractCanaryExecutor,
) -> Result<()> {
    let query = executor.embed_canary_query(SEMANTIC_CONTRACT_CANARY_TEXT)?;
    let passage = executor.embed_canary_passage(SEMANTIC_CONTRACT_CANARY_TEXT)?;
    let cosine = query
        .iter()
        .zip(&passage)
        .map(|(left, right)| f64::from(*left) * f64::from(*right))
        .sum::<f64>();
    if !cosine.is_finite() || cosine < 0.5 {
        return Err(anyhow!(
            "semantic canary query/passage cosine {cosine} is below 0.5"
        ));
    }
    Ok(())
}

fn model_fingerprint() -> String {
    semantic_model_contract_fingerprint()
}

fn backend_fingerprint(
    model_fingerprint: &str,
    kind: SemanticBackendKind,
    runtime_artifact_identity: &str,
) -> String {
    let descriptor = format!(
        "ctx-semantic-backend-v1|model={model_fingerprint}|adapter={}|runtime={runtime_artifact_identity}",
        kind.contract_id()
    );
    format!("sha256:{:x}", Sha256::digest(descriptor.as_bytes()))
}

#[cfg(all(ctx_semantic_fastembed, not(target_os = "macos")))]
pub(super) fn automatic_ort_accelerator_backend() -> Option<SemanticBackendKind> {
    semantic_native_accelerator_target()
        .map(SemanticNativeAcceleratorTarget::backend_kind)
        .filter(|kind| *kind != SemanticBackendKind::CoreMl)
}

pub(super) fn accelerator_fallback_reason(kind: SemanticBackendKind) -> &'static str {
    match kind {
        SemanticBackendKind::OrtCuda => "cuda_load_error",
        SemanticBackendKind::WindowsMl => "windows_ml_load_error",
        SemanticBackendKind::CoreMl => "coreml_load_error",
        SemanticBackendKind::Cpu => "cpu_load_error",
    }
}

#[cfg(ctx_semantic_fastembed)]
pub(super) fn map_daemon_accelerator_load_error(
    acquisition: SemanticDaemonModelAcquisition,
    error: anyhow::Error,
) -> anyhow::Error {
    if acquisition.allow_cpu_fallback {
        SemanticDaemonCpuFallbackRequired::new(
            accelerator_fallback_reason(acquisition.backend.kind()),
            &error,
        )
        .into()
    } else {
        error
    }
}

use std::{fmt, path::Path};

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};

use super::{
    maybe_cleanup_semantic_cpu_download_cache_after_cached_acquisition, onnx,
    onnx::{
        ensure_semantic_accelerator_runtime_loaded, ensure_semantic_onnxruntime_loaded,
        installed_accelerator_runtime_identity, revalidate_loaded_accelerator_runtime,
        OnnxRuntimeFlavor,
    },
    read_semantic_ort_model_file, replace_cpu_model_cache_from_pinned_revision,
    semantic_cpu_cache_repairable, semantic_cpu_cache_snapshot, semantic_ort_cache_snapshot,
    windows_ml, SemanticDaemonCpuFallbackRequired, SemanticDaemonModelAcquisition,
    SemanticEmbedder, SemanticEmbeddingBackend, SemanticModelAcquisitionBackend,
    SemanticModelAcquisitionSource,
};
use crate::{
    configuration::{SemanticBackendPreference, SemanticModelConfig},
    health_search::{semantic_embed_policy_for, SemanticEmbedPolicy},
    model_contract::{
        semantic_e5_passage_text, semantic_e5_query_text, semantic_model_contract_fingerprint,
        SemanticBackendKind, SemanticOrtModelVariant, SemanticProvisioningRequired,
        SEMANTIC_CONTRACT_CANARY_TEXT, SEMANTIC_MAX_SEQUENCE_LENGTH, SEMANTIC_MODEL_ID,
    },
    resource_policy::{semantic_cpu_model_load_deferred, SemanticComputeClass},
};
