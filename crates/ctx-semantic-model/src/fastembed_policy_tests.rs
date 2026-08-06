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
fn cpu_model_load_defers_before_cache_or_runtime_access() {
    let temp = tempfile::tempdir().expect("tempdir");
    let config = test_config(temp.path());
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
        Ok(_) => panic!("low-memory acquisition should defer"),
        Err(error) => error,
    };
    assert!(error.downcast_ref::<SemanticModelLoadDeferred>().is_some());
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
