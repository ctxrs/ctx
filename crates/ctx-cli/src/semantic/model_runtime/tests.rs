use super::*;
use crate::semantic::SEMANTIC_CONTRACT_CANARY_TEXT;

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
    assert_eq!(
        BackendPreference::parse(Some("cuda")).unwrap(),
        BackendPreference::Cuda
    );
    assert_eq!(
        BackendPreference::parse(Some("windowsml")).unwrap(),
        BackendPreference::WindowsMl
    );
    assert!(BackendPreference::parse(Some("directml")).is_err());
    assert!(BackendPreference::parse(Some("gpu")).is_err());
    assert!(BackendPreference::parse(Some("CPU")).is_err());
}

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
    let first_error =
        Err::<&'static str, _>(anyhow!("forced Core ML inference failure")).unwrap_err();
    assert!(first_error.to_string().contains("inference failure"));

    let coreml_called = std::cell::Cell::new(false);
    let cpu_cache_called = std::cell::Cell::new(false);
    let selected = recover_coreml_after_inference_with(
        BackendPreference::Auto,
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
        BackendPreference::CoreMl,
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
                    Ok(crate::semantic::model_acquisition::CoreMlAcquisitionSource::Download)
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
    let result = acquire_cpu_backend(
        temp.path(),
        semantic_embed_policy_for(SemanticComputeClass::Cpu),
        BackendPreference::Cpu,
    );

    assert!(result.is_err());
    assert!(
        !temp.path().join(SEMANTIC_MANAGED_MODEL_CACHE_DIR).exists(),
        "cache-only foreground acquisition must not initialize downloader state"
    );
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
    assert!(normalize_and_validate_embeddings(vec![vec![0.0; SEMANTIC_DIMENSIONS]], 1).is_err());
    let mut non_finite = vec![1.0; SEMANTIC_DIMENSIONS];
    non_finite[0] = f32::NAN;
    assert!(normalize_and_validate_embeddings(vec![non_finite], 1).is_err());
}

#[test]
pub(super) fn runtime_info_keeps_space_identity_backend_independent() {
    let cpu = SemanticEmbeddingRuntimeInfo {
        preference: BackendPreference::Auto,
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
