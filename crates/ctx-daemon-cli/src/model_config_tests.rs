use super::*;

fn environment() -> SemanticHostEnvironment {
    SemanticHostEnvironment {
        installed_runtime_dir: Some(PathBuf::from("/default-data/runtime")),
        executable_dir: Some(PathBuf::from("/install/bin")),
        ..SemanticHostEnvironment::default()
    }
}

#[test]
fn cache_precedence_and_legacy_candidates_are_frozen() {
    let data_root = Path::new("/data");
    let mut environment = environment();
    environment.semantic_cache_dir = Some(PathBuf::from("/semantic"));
    environment.fastembed_cache_dir = Some(PathBuf::from("/fastembed"));
    environment.hf_hub_cache = Some(PathBuf::from("/hub"));
    environment.hf_home = Some(PathBuf::from("/hf-home"));
    assert_eq!(
        semantic_worker_cache_dir_from_environment(data_root, &environment),
        PathBuf::from("/semantic")
    );
    environment.semantic_cache_dir = None;
    assert_eq!(
        semantic_worker_cache_dir_from_environment(data_root, &environment),
        PathBuf::from("/fastembed")
    );
    environment.fastembed_cache_dir = None;
    assert_eq!(
        semantic_worker_cache_dir_from_environment(data_root, &environment),
        PathBuf::from("/hub")
    );
    environment.hf_hub_cache = None;
    assert_eq!(
        semantic_worker_cache_dir_from_environment(data_root, &environment),
        PathBuf::from("/hf-home")
    );

    environment.hf_home = None;
    environment.current_dir = Some(PathBuf::from("/cwd"));
    environment.xdg_cache_home = Some(PathBuf::from("/xdg"));
    environment.home = Some(PathBuf::from("/home/test"));
    assert_eq!(
        semantic_worker_default_cache_candidates(data_root, &environment),
        [
            "/data/semantic-model-cache",
            "/cwd/.fastembed_cache",
            "/xdg/fastembed",
            "/xdg/huggingface/hub",
            "/xdg/huggingface",
            "/home/test/.fastembed_cache",
            "/home/test/.cache/fastembed",
            "/home/test/.cache/huggingface/hub",
            "/home/test/.cache/huggingface",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>()
    );
}

#[test]
fn first_available_legacy_cache_is_selected_without_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let current_dir = temp.path().join("repo");
    let legacy_cache = current_dir.join(".fastembed_cache");
    ctx_semantic_model::test_support::write_test_semantic_cache(&legacy_cache).unwrap();
    let environment = SemanticHostEnvironment {
        current_dir: Some(current_dir),
        home: Some(temp.path().join("home")),
        ..environment()
    };

    assert_eq!(
        semantic_worker_cache_dir_from_environment(&data_root, &environment),
        legacy_cache
    );
    assert!(!data_root.join("semantic-model-cache").exists());
}

#[test]
fn injected_model_and_runtime_roots_are_frozen_after_composition() {
    let mut environment = environment();
    environment.semantic_cache_dir = Some(PathBuf::from("/chosen/semantic-model-cache"));
    environment.ctx_onnxruntime_dylib = Some(PathBuf::from("/explicit/libonnxruntime.so"));
    environment.ctx_onnxruntime_cache_dir = Some(PathBuf::from("/runtime-cache"));
    environment.backend_preference = Some("cpu".to_owned());
    environment.thread_override = Some(3);
    let config = semantic_model_config_from_environment(Path::new("/data"), &environment);

    environment.semantic_cache_dir = Some(PathBuf::from("/mutated"));
    environment.ctx_onnxruntime_dylib = None;
    assert_eq!(
        config.paths().model_cache_dir(),
        Path::new("/chosen/semantic-model-cache")
    );
    assert_eq!(
        semantic_runtime_cache_dir_for_model_cache(config.paths().model_cache_dir()),
        PathBuf::from("/chosen/runtime")
    );
}

#[test]
fn backend_and_coreml_controls_preserve_strict_parsing() {
    assert_eq!(
        parse_backend_preference(None).unwrap(),
        SemanticBackendPreference::Auto
    );
    assert_eq!(
        parse_backend_preference(Some("cpu")).unwrap(),
        SemanticBackendPreference::Cpu
    );
    assert!(parse_backend_preference(Some("CPU")).is_err());
    assert!(parse_backend_preference(Some("gpu")).is_err());
    assert_eq!(
        parse_coreml_compute_mode(Some(" CPU-ANE ")).unwrap(),
        Some(SemanticCoreMlComputeMode::CpuAndNeuralEngine)
    );
    assert!(parse_coreml_compute_mode(Some("")).is_err());
}

#[test]
fn coreml_compute_mode_composition_preserves_absence_and_foreground_defaults() {
    assert_eq!(parse_coreml_compute_mode(None).unwrap(), None);

    let data_root = Path::new("/data");
    let absent = semantic_model_config_from_environment(data_root, &environment());
    assert_eq!(
        absent.coreml_compute_mode().unwrap(),
        SemanticCoreMlComputeMode::All
    );
    assert_eq!(
        crate::query_adapter::foreground_coreml_model_config(absent)
            .coreml_compute_mode()
            .unwrap(),
        SemanticCoreMlComputeMode::CpuOnly
    );

    for (value, expected) in [
        ("all", SemanticCoreMlComputeMode::All),
        ("cpu", SemanticCoreMlComputeMode::CpuOnly),
        ("cpu-ane", SemanticCoreMlComputeMode::CpuAndNeuralEngine),
        ("cpu-gpu", SemanticCoreMlComputeMode::CpuAndGpu),
    ] {
        let environment = SemanticHostEnvironment {
            coreml_compute_mode: Some(value.to_owned()),
            ..environment()
        };
        let config = semantic_model_config_from_environment(data_root, &environment);
        assert_eq!(config.coreml_compute_mode().unwrap(), expected, "{value}");
        assert_eq!(
            crate::query_adapter::foreground_coreml_model_config(config)
                .coreml_compute_mode()
                .unwrap(),
            expected,
            "{value}"
        );
    }

    for value in ["", "invalid"] {
        let environment = SemanticHostEnvironment {
            coreml_compute_mode: Some(value.to_owned()),
            ..environment()
        };
        let config = semantic_model_config_from_environment(data_root, &environment);
        let error = crate::query_adapter::foreground_coreml_model_config(config)
            .coreml_compute_mode()
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("CTX_SEMANTIC_COREML_NATIVE_COMPUTE"),
            "{value:?} must remain invalid after foreground composition"
        );
    }
}

#[test]
fn invalid_controls_are_deferred_to_the_backend_that_consumes_them() {
    let mut environment = environment();
    environment.backend_preference = Some("GPU".to_owned());
    let config = semantic_model_config_from_environment(Path::new("/data"), &environment);
    let error = ctx_semantic_model::SharedSemanticRuntime::default()
        .ensure_loaded_from_cache(&config)
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "unsupported CTX_INTERNAL_SEMANTIC_BACKEND value \"GPU\"; expected auto, cpu, coreml, cuda, or windowsml"
    );

    environment.backend_preference = Some("cpu".to_owned());
    environment.coreml_compute_mode = Some("invalid".to_owned());
    let config = semantic_model_config_from_environment(Path::new("/data"), &environment);
    let error = ctx_semantic_model::SharedSemanticRuntime::default()
        .ensure_loaded_from_cache(&config)
        .unwrap_err();
    assert!(!error
        .to_string()
        .contains("CTX_SEMANTIC_COREML_NATIVE_COMPUTE"));
}

#[test]
fn runtime_cache_layout_matches_legacy_selection() {
    assert_eq!(
        semantic_runtime_cache_dir_for_model_cache(Path::new("/data/semantic-model-cache")),
        PathBuf::from("/data/runtime")
    );
    assert_eq!(
        semantic_runtime_cache_dir_for_model_cache(Path::new("/legacy/.fastembed_cache")),
        PathBuf::from("/legacy/.fastembed_cache/semantic-runtime")
    );
}
