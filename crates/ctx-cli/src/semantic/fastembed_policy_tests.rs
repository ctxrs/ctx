use super::*;

#[test]
fn cpu_model_load_defers_before_cache_or_runtime_access() {
    let temp = tempfile::tempdir().expect("tempdir");
    let policy = semantic_embed_policy_from_env_and_resources(
        SemanticComputeClass::Cpu,
        SemanticSystemResources {
            total_memory_bytes: Some(8 * 1024 * 1024 * 1024),
            available_memory_bytes: Some(1024),
            available_parallelism: 8,
        },
    );
    let error = match acquire_cpu_backend(temp.path(), policy, BackendPreference::Cpu, false) {
        Ok(_) => panic!("low-memory acquisition should defer"),
        Err(error) => error,
    };
    assert!(error.downcast_ref::<SemanticModelLoadDeferred>().is_some());
}

fn write_test_semantic_cache(root: &Path) -> Result<()> {
    let snapshot = root
        .join(SEMANTIC_HF_MODEL_CACHE_DIR)
        .join("snapshots")
        .join(SEMANTIC_MODEL_REVISION);
    fs::create_dir_all(&snapshot)?;
    for file in SEMANTIC_REQUIRED_MODEL_FILES {
        let path = snapshot.join(file.path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::File::create(path)?.set_len(file.size)?;
    }
    Ok(())
}

#[test]
fn semantic_cache_dir_override_beats_hf_home_without_sqlite_vec() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let data_root = temp.path().join("data");
    let explicit = temp.path().join("explicit");
    write_test_semantic_cache(&explicit)?;

    let env = SemanticCacheEnv {
        semantic_cache_dir: Some(explicit.clone()),
        hf_home: Some(temp.path().join("bad-hf-home")),
        ..SemanticCacheEnv::default()
    };

    assert_eq!(
        semantic_worker_cache_dir_from_env(&data_root, &env),
        explicit
    );
    Ok(())
}
