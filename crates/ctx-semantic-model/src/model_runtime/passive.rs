use std::fmt;

use anyhow::Result;

use crate::health_search::semantic_model_acquisition_integrity_error;

#[cfg(all(ctx_semantic_fastembed, not(target_os = "macos")))]
use super::automatic_ort_accelerator_backend;
#[cfg(not(ctx_semantic_fastembed))]
use super::SEMANTIC_MODEL_ID;
#[cfg(ctx_semantic_fastembed)]
use super::{
    accelerator_fallback_reason, authorize_loaded_backend,
    coreml::acquire_coreml_backend_passively, cpu::acquire_ort_backend_passively,
    semantic_embed_policy_for, SemanticBackendKind, SemanticBackendPreference,
    SemanticComputeClass,
};
use super::{SemanticEmbedder, SemanticModelConfig};

/// Passive execution configuration is invalid before any backend is touched.
#[derive(Debug)]
pub struct SemanticPassiveConfigurationError {
    detail: String,
}

impl SemanticPassiveConfigurationError {
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl fmt::Display for SemanticPassiveConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "semantic passive executor configuration is invalid: {}",
            self.detail
        )
    }
}

impl std::error::Error for SemanticPassiveConfigurationError {}

/// A selected built-in executor could not be made ready without provisioning
/// or changing durable state. Callers can fall back without weakening the
/// normal acquisition path.
#[derive(Debug)]
pub struct SemanticPassiveLoadUnavailable {
    backend: &'static str,
    detail: String,
}

impl SemanticPassiveLoadUnavailable {
    fn new(backend: &'static str, detail: impl Into<String>) -> Self {
        Self {
            backend,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for SemanticPassiveLoadUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "semantic {} executor is unavailable for a passive query: {}",
            self.backend, self.detail,
        )
    }
}

impl std::error::Error for SemanticPassiveLoadUnavailable {}

#[cfg(ctx_semantic_fastembed)]
pub(super) fn acquire_semantic_embedder_passively(
    config: &SemanticModelConfig,
) -> Result<SemanticEmbedder> {
    let preference = config
        .backend_preference()
        .map_err(|error| SemanticPassiveConfigurationError::new(format!("{error:#}")))?;
    let acquired = match preference {
        SemanticBackendPreference::Cpu => acquire_cpu_backend_passively(config, preference),
        SemanticBackendPreference::CoreMl => {
            acquire_coreml_backend_passively(config, preference).and_then(authorize_loaded_backend)
        }
        SemanticBackendPreference::Cuda => {
            acquire_accelerator_backend_passively(config, preference, SemanticBackendKind::OrtCuda)
        }
        SemanticBackendPreference::WindowsMl => acquire_accelerator_backend_passively(
            config,
            preference,
            SemanticBackendKind::WindowsMl,
        ),
        SemanticBackendPreference::Auto => passive_auto_backend(config, preference),
    };
    let acquired = passive_load_result(preference.as_str(), acquired)?;
    Ok(acquired)
}

pub(super) fn passive_load_result<T>(backend: &'static str, result: Result<T>) -> Result<T> {
    match result {
        Ok(value) => Ok(value),
        Err(error) if semantic_model_acquisition_integrity_error(&error) => Err(error),
        Err(error) => {
            Err(SemanticPassiveLoadUnavailable::new(backend, format!("{error:#}")).into())
        }
    }
}

#[cfg(ctx_semantic_fastembed)]
pub(super) fn passive_fallback_unless_integrity<T, F>(primary: Result<T>, fallback: F) -> Result<T>
where
    F: FnOnce(anyhow::Error) -> Result<T>,
{
    match primary {
        Ok(value) => Ok(value),
        Err(error) if semantic_model_acquisition_integrity_error(&error) => Err(error),
        Err(error) => fallback(error),
    }
}

#[cfg(ctx_semantic_fastembed)]
fn acquire_cpu_backend_passively(
    config: &SemanticModelConfig,
    preference: SemanticBackendPreference,
) -> Result<SemanticEmbedder> {
    acquire_ort_backend_passively(
        config,
        semantic_embed_policy_for(SemanticComputeClass::Cpu, config),
        preference,
        SemanticBackendKind::Cpu,
        SemanticBackendKind::Cpu,
    )
    .and_then(authorize_loaded_backend)
}

#[cfg(ctx_semantic_fastembed)]
fn acquire_cpu_fallback_backend_passively(
    config: &SemanticModelConfig,
    preference: SemanticBackendPreference,
    accelerator: SemanticBackendKind,
) -> Result<SemanticEmbedder> {
    let policy = semantic_embed_policy_for(SemanticComputeClass::Cpu, config);
    let accelerator_model = acquire_ort_backend_passively(
        config,
        policy.clone(),
        preference,
        SemanticBackendKind::Cpu,
        accelerator,
    );
    passive_fallback_unless_integrity(accelerator_model, |accelerator_model_error| {
        let cpu_model = acquire_ort_backend_passively(
            config,
            policy,
            preference,
            SemanticBackendKind::Cpu,
            SemanticBackendKind::Cpu,
        );
        match cpu_model {
            Ok(embedder) => Ok(embedder),
            Err(error) if semantic_model_acquisition_integrity_error(&error) => Err(error),
            Err(cpu_model_error) => Err(anyhow::anyhow!(
                "passive accelerator-model CPU fallback failed ({accelerator_model_error:#}); cached fp32 CPU fallback also failed: {cpu_model_error:#}"
            )),
        }
    })
    .map(|mut embedder| {
        embedder.acquisition_fallback = Some(accelerator_fallback_reason(accelerator));
        embedder
    })
    .and_then(authorize_loaded_backend)
}

#[cfg(ctx_semantic_fastembed)]
fn acquire_accelerator_backend_passively(
    config: &SemanticModelConfig,
    preference: SemanticBackendPreference,
    kind: SemanticBackendKind,
) -> Result<SemanticEmbedder> {
    let acquired = acquire_ort_backend_passively(
        config,
        semantic_embed_policy_for(SemanticComputeClass::Accelerator, config),
        preference,
        kind,
        kind,
    )?;
    authorize_loaded_backend(acquired)
}

#[cfg(all(ctx_semantic_fastembed, target_os = "macos"))]
fn passive_auto_backend(
    config: &SemanticModelConfig,
    preference: SemanticBackendPreference,
) -> Result<SemanticEmbedder> {
    super::coreml::acquire_auto_coreml_backend_with(
        || acquire_coreml_backend_passively(config, preference).and_then(authorize_loaded_backend),
        |fallback| {
            acquire_cpu_backend_passively(config, preference).map(|mut embedder| {
                embedder.acquisition_fallback = Some(fallback);
                embedder
            })
        },
    )
}

#[cfg(all(ctx_semantic_fastembed, not(target_os = "macos")))]
fn passive_auto_backend(
    config: &SemanticModelConfig,
    preference: SemanticBackendPreference,
) -> Result<SemanticEmbedder> {
    match automatic_ort_accelerator_backend() {
        Some(kind) => passive_fallback_unless_integrity(
            acquire_accelerator_backend_passively(config, preference, kind),
            |_| acquire_cpu_fallback_backend_passively(config, preference, kind),
        ),
        None => acquire_cpu_backend_passively(config, preference),
    }
}

#[cfg(not(ctx_semantic_fastembed))]
pub(super) fn acquire_semantic_embedder_passively(
    config: &SemanticModelConfig,
) -> Result<SemanticEmbedder> {
    config
        .backend_preference()
        .map_err(|error| SemanticPassiveConfigurationError::new(format!("{error:#}")))?;
    Err(SemanticPassiveLoadUnavailable::new(
        "builtin",
        format!("semantic embedding model {SEMANTIC_MODEL_ID} is not supported on this platform"),
    )
    .into())
}
