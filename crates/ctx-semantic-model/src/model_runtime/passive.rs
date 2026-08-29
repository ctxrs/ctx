use std::fmt;

use anyhow::Result;

#[cfg(all(ctx_semantic_fastembed, not(target_os = "macos")))]
use super::automatic_ort_accelerator_backend;
#[cfg(not(ctx_semantic_fastembed))]
use super::SEMANTIC_MODEL_ID;
#[cfg(ctx_semantic_fastembed)]
use super::{
    acquire_cpu_backend, authorize_loaded_backend, coreml::acquire_coreml_backend_passively,
    cpu::acquire_ort_backend_passively, semantic_embed_policy_for, SemanticBackendKind,
    SemanticBackendPreference, SemanticComputeClass,
};
use super::{SemanticEmbedder, SemanticModelConfig};

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
    let preference = config.backend_preference()?;
    let acquired = match preference {
        SemanticBackendPreference::Cpu => acquire_cpu_backend(
            config,
            semantic_embed_policy_for(SemanticComputeClass::Cpu, config),
            preference,
        ),
        SemanticBackendPreference::CoreMl => acquire_coreml_backend_passively(config, preference),
        SemanticBackendPreference::Cuda => {
            acquire_accelerator_backend_passively(config, preference, SemanticBackendKind::OrtCuda)
        }
        SemanticBackendPreference::WindowsMl => acquire_accelerator_backend_passively(
            config,
            preference,
            SemanticBackendKind::WindowsMl,
        ),
        SemanticBackendPreference::Auto => passive_auto_backend(config, preference),
    }
    .map_err(|error| {
        SemanticPassiveLoadUnavailable::new(preference.as_str(), format!("{error:#}"))
    })?;
    authorize_loaded_backend(acquired)
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
    acquire_coreml_backend_passively(config, preference).or_else(|_| {
        acquire_cpu_backend(
            config,
            semantic_embed_policy_for(SemanticComputeClass::Cpu, config),
            preference,
        )
    })
}

#[cfg(all(ctx_semantic_fastembed, not(target_os = "macos")))]
fn passive_auto_backend(
    config: &SemanticModelConfig,
    preference: SemanticBackendPreference,
) -> Result<SemanticEmbedder> {
    match automatic_ort_accelerator_backend() {
        Some(kind) => {
            acquire_accelerator_backend_passively(config, preference, kind).or_else(|_| {
                acquire_cpu_backend(
                    config,
                    semantic_embed_policy_for(SemanticComputeClass::Cpu, config),
                    preference,
                )
            })
        }
        None => acquire_cpu_backend(
            config,
            semantic_embed_policy_for(SemanticComputeClass::Cpu, config),
            preference,
        ),
    }
}

#[cfg(not(ctx_semantic_fastembed))]
pub(super) fn acquire_semantic_embedder_passively(
    _config: &SemanticModelConfig,
) -> Result<SemanticEmbedder> {
    Err(SemanticPassiveLoadUnavailable::new(
        "builtin",
        format!("semantic embedding model {SEMANTIC_MODEL_ID} is not supported on this platform"),
    )
    .into())
}
