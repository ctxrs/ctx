use std::{fs, path::Path};

use anyhow::Result;

use crate::{
    cache_paths::{SEMANTIC_HF_MODEL_CACHE_DIR, SEMANTIC_MANAGED_MODEL_CACHE_DIR},
    configuration::{
        SemanticBackendPreference, SemanticModelConfig, SemanticModelPaths,
        SemanticOnnxRuntimePaths,
    },
    health_search::{
        semantic_accelerator_model_cache_available,
        semantic_embed_policy_from_config_and_resources, semantic_model_cache_available,
        semantic_model_cache_snapshot_dir,
    },
    model_contract::{SemanticModelLoadDeferred, SemanticOrtModelVariant, SEMANTIC_MODEL_REVISION},
    model_runtime::acquire_cpu_backend,
    resource_policy::{SemanticComputeClass, SemanticSystemResources},
};

fn test_config(cache_dir: &Path) -> SemanticModelConfig {
    SemanticModelConfig::new(SemanticModelPaths::new(
        cache_dir.to_path_buf(),
        SemanticOnnxRuntimePaths::new(cache_dir.join("semantic-runtime")),
    ))
    .with_backend_preference(SemanticBackendPreference::Cpu)
}

#[test]
fn cpu_model_load_defers_before_cache_or_runtime_access_regardless_of_throttling() {
    let temp = tempfile::tempdir().expect("tempdir");
    for throttling in [true, false] {
        let config = test_config(temp.path()).with_builtin_throttling(throttling);
        let policy = semantic_embed_policy_from_config_and_resources(
            SemanticComputeClass::Cpu,
            &config,
            SemanticSystemResources {
                total_memory_bytes: Some(8 * 1024 * 1024 * 1024),
                available_memory_bytes: Some(1024),
                available_parallelism: 8,
            },
        );
        let error = match acquire_cpu_backend(&config, policy, SemanticBackendPreference::Cpu) {
            Ok(_) => panic!("low-memory acquisition should defer with throttling={throttling}"),
            Err(error) => error,
        };
        assert!(error.downcast_ref::<SemanticModelLoadDeferred>().is_some());
    }
}

#[test]
fn unthrottled_policy_ignores_internal_overrides_for_deterministic_maxima() {
    let temp = tempfile::tempdir().expect("tempdir");
    let resources = SemanticSystemResources {
        total_memory_bytes: Some(16 * 1024 * 1024 * 1024),
        available_memory_bytes: Some(8 * 1024 * 1024 * 1024),
        available_parallelism: 64,
    };
    let throttled = test_config(temp.path())
        .with_thread_override(Some(2))
        .with_batch_size_override(Some(4));
    let throttled_policy = semantic_embed_policy_from_config_and_resources(
        SemanticComputeClass::Cpu,
        &throttled,
        resources,
    );
    assert_eq!(throttled_policy.threads, 2);
    assert_eq!(throttled_policy.batch_size, 4);

    let unthrottled = throttled.with_builtin_throttling(false);
    let unthrottled_policy = semantic_embed_policy_from_config_and_resources(
        SemanticComputeClass::Cpu,
        &unthrottled,
        resources,
    );
    assert_eq!(
        unthrottled_policy.threads,
        crate::resource_policy::SEMANTIC_EMBED_THREADS_MAX
    );
    assert_eq!(
        unthrottled_policy.batch_size,
        crate::resource_policy::SEMANTIC_EMBED_BATCH_MAX
    );
    assert_eq!(
        unthrottled_policy.available_memory_bytes,
        resources.available_memory_bytes
    );
}

fn write_test_semantic_cache_variant(root: &Path, variant: SemanticOrtModelVariant) -> Result<()> {
    let snapshot = root
        .join(SEMANTIC_HF_MODEL_CACHE_DIR)
        .join("snapshots")
        .join(SEMANTIC_MODEL_REVISION);
    fs::create_dir_all(&snapshot)?;
    for file in variant.required_files() {
        let path = snapshot.join(file.path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::File::create(path)?.set_len(file.size)?;
    }
    Ok(())
}

#[test]
fn accelerator_only_model_cache_is_available() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let cache = temp.path().join("accelerator-cache");
    write_test_semantic_cache_variant(
        &cache.join(SEMANTIC_MANAGED_MODEL_CACHE_DIR),
        SemanticOrtModelVariant::AcceleratorO4Fp16,
    )?;

    assert!(semantic_model_cache_snapshot_dir(&cache).is_none());
    assert!(semantic_accelerator_model_cache_available(&cache));
    assert!(semantic_model_cache_available(&cache));
    Ok(())
}
