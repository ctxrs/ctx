use std::path::Path;

use super::*;
use crate::{
    SemanticCoreMlComputeMode, SemanticModelPaths, SemanticOnnxRuntimePaths,
    SEMANTIC_PASSAGE_PREFIX,
};

fn test_config(cache_dir: &Path) -> SemanticModelConfig {
    SemanticModelConfig::new(SemanticModelPaths::new(
        cache_dir.to_path_buf(),
        SemanticOnnxRuntimePaths::new(cache_dir.join("semantic-runtime")),
    ))
}

#[test]
fn passive_invalid_backend_preference_is_a_typed_configuration_failure() {
    let temporary = tempfile::tempdir().unwrap();
    let config = test_config(temporary.path())
        .with_backend_preference_error("invalid passive backend fixture".to_owned());
    let error = SharedSemanticRuntime::default()
        .ensure_loaded_passively(&config)
        .expect_err("invalid passive configuration must fail before backend loading");
    let typed = error
        .downcast_ref::<SemanticPassiveConfigurationError>()
        .expect("passive configuration error must retain its type");
    assert!(typed
        .to_string()
        .contains("invalid passive backend fixture"));
}

#[test]
fn passive_missing_cpu_cache_is_typed_and_does_not_create_the_cache() {
    let temporary = tempfile::tempdir().unwrap();
    let cache = temporary.path().join("missing-cache");
    let config = test_config(&cache).with_backend_preference(SemanticBackendPreference::Cpu);
    let error = SharedSemanticRuntime::default()
        .ensure_loaded_passively(&config)
        .expect_err("passive loading cannot provision an absent cache");
    error
        .downcast_ref::<SemanticPassiveLoadUnavailable>()
        .expect("passive cache misses must retain their retryable unavailable type");
    assert!(!cache.exists());
}

#[test]
fn passive_final_backend_failure_retains_retryable_unavailable_type() {
    let error = passive::passive_load_result::<()>(
        "coreml",
        Err(anyhow::anyhow!("cached Core ML canary failed")),
    )
    .expect_err("final passive authorization failures must be typed unavailable");
    let unavailable = error
        .downcast_ref::<SemanticPassiveLoadUnavailable>()
        .expect("final passive failure must retain its unavailable type");
    assert!(unavailable
        .to_string()
        .contains("cached Core ML canary failed"));
}

#[test]
fn fixed_shape_settings_are_strict() {
    assert_eq!(semantic_fixed_shape_from_values(None, None).unwrap(), None);
    assert_eq!(
        semantic_fixed_shape_from_values(Some("16"), Some("512")).unwrap(),
        Some((16, 512))
    );
    for values in [
        (Some("16"), None),
        (None, Some("512")),
        (Some("0"), Some("512")),
        (Some("wat"), Some("512")),
        (Some("16"), Some("-1")),
    ] {
        assert!(semantic_fixed_shape_from_values(values.0, values.1).is_err());
    }
}

#[test]
fn fixed_batch_padding_preserves_complete_batches() -> Result<()> {
    let make = |count| {
        (0..count)
            .map(|index| format!("passage: {index}"))
            .collect::<Vec<_>>()
    };
    assert!(pad_texts_to_exact_batch(make(0), 4)?.is_empty());
    assert_eq!(pad_texts_to_exact_batch(make(4), 4)?.len(), 4);
    let padded = pad_texts_to_exact_batch(make(5), 4)?;
    assert_eq!(padded.len(), 8);
    assert_eq!(&padded[..5], make(5));
    assert!(padded[5..]
        .iter()
        .all(|text| text == SEMANTIC_PASSAGE_PREFIX));
    assert!(pad_texts_to_exact_batch(make(1), 0).is_err());
    Ok(())
}
use crate::model_contract::SEMANTIC_CONTRACT_CANARY_TEXT;

#[test]
pub(super) fn daemon_acquisition_source_is_applied_only_to_matching_backend() {
    let acquisition = SemanticDaemonModelAcquisition::new(
        SemanticModelAcquisitionBackend::Cpu,
        SemanticModelAcquisitionSource::Download,
    );
    assert_eq!(acquisition.backend.as_str(), "cpu");
    assert_eq!(acquisition.source.as_str(), "download");
}

#[test]
fn auto_accelerator_load_failure_requests_matching_cpu_fallback() {
    for (backend, expected) in [
        (SemanticModelAcquisitionBackend::Cuda, "cuda_load_error"),
        (
            SemanticModelAcquisitionBackend::WindowsMl,
            "windows_ml_load_error",
        ),
    ] {
        let acquisition =
            SemanticDaemonModelAcquisition::new(backend, SemanticModelAcquisitionSource::Cache)
                .allowing_cpu_fallback_for_auto();
        let error = map_daemon_accelerator_load_error(acquisition, anyhow!("forced failure"));
        let fallback = error
            .downcast_ref::<SemanticDaemonCpuFallbackRequired>()
            .expect("automatic accelerator failure must request CPU fallback");
        assert_eq!(fallback.reason(), expected);

        let fallback_acquisition = acquisition.as_cpu_fallback_for(backend);
        assert_eq!(
            fallback_acquisition.backend,
            SemanticModelAcquisitionBackend::Cpu
        );
        assert_eq!(fallback_acquisition.assets, backend);
        assert_eq!(
            fallback_acquisition.model_variant,
            Some(SemanticOrtModelVariant::AcceleratorO4Fp16)
        );
    }
}

#[test]
fn accelerator_target_exports_signed_runtime_keys() {
    assert_eq!(SemanticNativeAcceleratorTarget::Cuda.as_str(), "cuda");
    assert_eq!(
        SemanticNativeAcceleratorTarget::CoreMl.runtime_platform(),
        None
    );
    assert_eq!(
        SemanticNativeAcceleratorTarget::Cuda.runtime_platform(),
        Some("linux-x64-cuda12")
    );
    assert_eq!(
        SemanticNativeAcceleratorTarget::WindowsMl.runtime_platform(),
        Some("windows-x64")
    );
}

struct MockSemanticContractCanary {
    query: Vec<f32>,
    passage: Vec<f32>,
    calls: Vec<&'static str>,
}

impl SemanticContractCanaryExecutor for MockSemanticContractCanary {
    fn embed_canary_query(&mut self, text: &str) -> Result<Vec<f32>> {
        assert_eq!(text, SEMANTIC_CONTRACT_CANARY_TEXT);
        self.calls.push("query");
        Ok(self.query.clone())
    }

    fn embed_canary_passage(&mut self, text: &str) -> Result<Vec<f32>> {
        assert_eq!(text, SEMANTIC_CONTRACT_CANARY_TEXT);
        self.calls.push("passage");
        Ok(self.passage.clone())
    }
}

fn semantic_canary_vector(sign: f32) -> Vec<f32> {
    let mut vector = vec![0.0; SEMANTIC_DIMENSIONS];
    vector[0] = sign;
    vector
}

#[test]
fn coreml_authorization_runs_the_query_passage_contract_canary() {
    assert!(semantic_backend_requires_contract_canary(
        SemanticBackendKind::CoreMl,
        false,
        false,
    ));
    let vector = semantic_canary_vector(1.0);
    let mut canary = MockSemanticContractCanary {
        query: vector.clone(),
        passage: vector,
        calls: Vec::new(),
    };

    run_semantic_contract_canary(&mut canary).unwrap();

    assert_eq!(canary.calls, ["query", "passage"]);
}

#[test]
fn auto_coreml_canary_failure_selects_and_requests_cpu_fallback() {
    let canary_failure = || {
        let mut canary = MockSemanticContractCanary {
            query: semantic_canary_vector(1.0),
            passage: semantic_canary_vector(-1.0),
            calls: Vec::new(),
        };
        run_semantic_contract_canary(&mut canary).unwrap_err()
    };
    let cpu_called = std::cell::Cell::new(false);
    let selected = acquire_auto_coreml_backend_with(
        || -> Result<&'static str> { Err(canary_failure()) },
        |fallback| {
            assert_eq!(fallback, "coreml_load_error");
            cpu_called.set(true);
            Ok("cpu")
        },
    )
    .unwrap();
    assert_eq!(selected, "cpu");
    assert!(cpu_called.get());

    let acquisition = SemanticDaemonModelAcquisition::verified_coreml_cache_for_test();
    let mapped = map_daemon_coreml_load_error(acquisition, canary_failure());
    let fallback = mapped
        .downcast_ref::<SemanticDaemonCpuFallbackRequired>()
        .expect("automatic daemon Core ML canary failure must request CPU fallback");
    assert_eq!(fallback.reason(), "coreml_load_error");
}

#[test]
fn clean_auto_coreml_canary_then_inference_failure_retries_provisioned_cpu() {
    let vector = semantic_canary_vector(1.0);
    let mut canary = MockSemanticContractCanary {
        query: vector.clone(),
        passage: vector,
        calls: Vec::new(),
    };
    run_semantic_contract_canary(&mut canary).unwrap();
    assert_eq!(canary.calls, ["query", "passage"]);
    let first_error = anyhow!("forced Core ML inference failure");
    assert!(first_error.to_string().contains("inference failure"));

    let coreml_called = std::cell::Cell::new(false);
    let cpu_cache_called = std::cell::Cell::new(false);
    let selected = recover_coreml_after_inference_with(
        SemanticBackendPreference::Auto,
        || {
            coreml_called.set(true);
            Ok("coreml")
        },
        || {
            cpu_cache_called.set(true);
            Ok("provisioned-cpu")
        },
    )
    .unwrap();

    assert_eq!(selected, "provisioned-cpu");
    assert!(!coreml_called.get());
    assert!(cpu_cache_called.get());
    let retry = match selected {
        "provisioned-cpu" => Ok("cpu-retry-result"),
        backend => Err(anyhow!("unexpected retry backend {backend}")),
    }
    .unwrap();
    assert_eq!(retry, "cpu-retry-result");
}

#[test]
fn explicit_coreml_inference_failure_does_not_fall_back_to_cpu() {
    let cpu_called = std::cell::Cell::new(false);
    let error = recover_coreml_after_inference_with(
        SemanticBackendPreference::CoreMl,
        || -> Result<&'static str> { Err(anyhow!("forced Core ML reacquisition failure")) },
        || {
            cpu_called.set(true);
            Ok("cpu")
        },
    )
    .unwrap_err();

    assert!(error.to_string().contains("Core ML reacquisition failure"));
    assert!(!cpu_called.get());
}

#[test]
pub(super) fn auto_coreml_non_integrity_load_failure_requests_cpu_fallback() {
    let acquisition = SemanticDaemonModelAcquisition::verified_coreml_cache_for_test();
    let error =
        map_daemon_coreml_load_error(acquisition, anyhow!("forced Core ML runtime failure"));
    let fallback = error
        .downcast_ref::<SemanticDaemonCpuFallbackRequired>()
        .expect("auto Core ML runtime failure should request daemon CPU acquisition");
    assert_eq!(fallback.reason(), "coreml_load_error");

    let integrity: anyhow::Error =
        SemanticCpuModelIntegrityError("forced integrity failure".to_owned()).into();
    let error = map_daemon_coreml_load_error(acquisition, integrity);
    assert!(
        error
            .downcast_ref::<SemanticDaemonCpuFallbackRequired>()
            .is_none(),
        "integrity failures must stay fail-closed"
    );
    assert!(semantic_model_acquisition_integrity_error(&error));

    let deferred: anyhow::Error = SemanticModelLoadDeferred {
        required_available_memory_bytes: SEMANTIC_ACCELERATOR_MODEL_LOAD_MIN_AVAILABLE_BYTES,
        available_memory_bytes: SEMANTIC_ACCELERATOR_MODEL_LOAD_MIN_AVAILABLE_BYTES - 1,
    }
    .into();
    let error = map_daemon_coreml_load_error(acquisition, deferred);
    assert!(
        error.downcast_ref::<SemanticModelLoadDeferred>().is_some(),
        "resource deferral must not trigger CPU acquisition"
    );
}

#[test]
pub(super) fn coreml_memory_deferral_precedes_bundle_downloader() {
    let bundle_downloader_called = std::cell::Cell::new(false);
    let cpu_downloader_called = std::cell::Cell::new(false);
    let floor = SEMANTIC_ACCELERATOR_MODEL_LOAD_MIN_AVAILABLE_BYTES;
    let error = acquire_auto_semantic_model_for_daemon_with(
        || {
            acquire_coreml_model_for_daemon_with(
                Some(floor - 1),
                SemanticComputeClass::Accelerator,
                || {
                    bundle_downloader_called.set(true);
                    Ok(crate::model_acquisition::CoreMlAcquisitionSource::Download)
                },
            )
        },
        || {
            cpu_downloader_called.set(true);
            Ok(SemanticDaemonModelAcquisition::verified_cpu_cache_for_test())
        },
    )
    .unwrap_err();

    let deferred = error
        .downcast_ref::<SemanticModelLoadDeferred>()
        .expect("Core ML acquisition should defer below the accelerator memory floor");
    assert_eq!(deferred.required_available_memory_bytes, floor);
    assert!(
        !bundle_downloader_called.get(),
        "memory admission must happen before the signed bundle downloader"
    );
    assert!(
        !cpu_downloader_called.get(),
        "Core ML memory deferral must not fall through to CPU acquisition"
    );
}

#[test]
pub(super) fn foreground_cache_miss_never_creates_download_state() {
    let temp = tempfile::tempdir().unwrap();
    let config = test_config(temp.path());
    let result = acquire_cpu_backend(
        &config,
        semantic_embed_policy_for(SemanticComputeClass::Cpu, &config),
        SemanticBackendPreference::Cpu,
    );

    assert!(result.is_err());
    assert!(
        !temp.path().join(SEMANTIC_MANAGED_MODEL_CACHE_DIR).exists(),
        "cache-only foreground acquisition must not initialize downloader state"
    );
}

#[test]
pub(super) fn coreml_cpu_only_uses_cpu_quiet_policy_class() {
    let cpu_only = SemanticCoreMlComputeMode::CpuOnly;
    assert_eq!(coreml_compute_class(cpu_only), SemanticComputeClass::Cpu);
    assert_eq!(coreml_compute_mode_name(cpu_only), "cpu_only");
    let all = SemanticCoreMlComputeMode::All;
    assert_eq!(coreml_compute_class(all), SemanticComputeClass::Accelerator);

    let available = 5 * 512 * 1024 * 1024;
    assert!(
        semantic_model_load_deferred(Some(available), coreml_compute_class(cpu_only)).is_none()
    );
    assert!(semantic_model_load_deferred(Some(available), coreml_compute_class(all)).is_some());
}

#[test]
fn foreground_coreml_config_defaults_to_cpu_but_honors_explicit_compute_mode() {
    let temp = tempfile::tempdir().unwrap();
    let default_config = test_config(temp.path());
    assert_eq!(
        default_config.coreml_compute_mode().unwrap(),
        SemanticCoreMlComputeMode::All
    );
    let foreground_config = default_config.with_foreground_coreml_cpu_default();
    let foreground_mode = foreground_config.coreml_compute_mode().unwrap();
    assert_eq!(foreground_mode, SemanticCoreMlComputeMode::CpuOnly);
    assert_eq!(
        coreml_compute_class(foreground_mode),
        SemanticComputeClass::Cpu
    );

    for mode in [
        SemanticCoreMlComputeMode::All,
        SemanticCoreMlComputeMode::CpuAndNeuralEngine,
        SemanticCoreMlComputeMode::CpuAndGpu,
        SemanticCoreMlComputeMode::CpuOnly,
    ] {
        let config = test_config(temp.path())
            .with_coreml_compute_mode(mode)
            .with_foreground_coreml_cpu_default();
        assert_eq!(config.coreml_compute_mode().unwrap(), mode);
    }
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
    assert!(normalize_and_validate_embeddings(vec![vec![0.0; SEMANTIC_DIMENSIONS]], 1).is_err());
    let mut non_finite = vec![1.0; SEMANTIC_DIMENSIONS];
    non_finite[0] = f32::NAN;
    assert!(normalize_and_validate_embeddings(vec![non_finite], 1).is_err());
}

#[test]
pub(super) fn runtime_info_keeps_space_identity_backend_independent() {
    let cpu = SemanticEmbeddingRuntimeInfo {
        preference: SemanticBackendPreference::Auto,
        backend: SemanticBackendKind::Cpu,
        assets_backend: SemanticBackendKind::Cpu,
        model_variant: Some(SemanticOrtModelVariant::CpuFp32),
        compute_class: SemanticComputeClass::Cpu,
        compute_mode: None,
        acquisition_source: "cache",
        acquisition_fallback: None,
        runtime_artifact_identity: "test-runtime".to_owned(),
        model_fingerprint: "test-model".to_owned(),
        backend_fingerprint: "test-backend".to_owned(),
        canary_passed: false,
    };
    let coreml = SemanticEmbeddingRuntimeInfo {
        backend: SemanticBackendKind::CoreMl,
        assets_backend: SemanticBackendKind::CoreMl,
        model_variant: None,
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
