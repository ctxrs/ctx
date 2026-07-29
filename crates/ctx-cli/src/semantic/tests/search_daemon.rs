use super::*;

#[cfg(ctx_semantic_fastembed)]
#[test]
fn semantic_cache_discovery_prefers_explicit_env_roots() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let data_root = temp.path().join("data");
    let explicit = temp.path().join("explicit");
    let fallback = temp.path().join("fallback");
    write_test_semantic_cache(&fallback)?;

    let env = SemanticCacheEnv {
        semantic_cache_dir: Some(explicit.clone()),
        hf_home: Some(temp.path().join("bad-hf-home")),
        current_dir: Some(temp.path().to_path_buf()),
        home: Some(temp.path().to_path_buf()),
        xdg_cache_home: Some(fallback.clone()),
        ..SemanticCacheEnv::default()
    };

    assert_eq!(
        semantic_worker_cache_dir_from_env(&data_root, &env),
        explicit
    );
    Ok(())
}

#[cfg(ctx_semantic_fastembed)]
#[test]
fn semantic_cache_discovery_finds_repo_local_fastembed_cache() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let data_root = temp.path().join("data");
    let repo_cache = temp.path().join(".fastembed_cache");
    write_test_semantic_cache(&repo_cache)?;

    let env = SemanticCacheEnv {
        current_dir: Some(temp.path().to_path_buf()),
        home: Some(temp.path().join("home")),
        ..SemanticCacheEnv::default()
    };

    assert_eq!(
        semantic_worker_cache_dir_from_env(&data_root, &env),
        repo_cache
    );
    Ok(())
}

#[cfg(ctx_semantic_fastembed)]
#[test]
fn semantic_cache_discovery_finds_common_home_cache() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let data_root = temp.path().join("data");
    let home = temp.path().join("home");
    let home_cache = home.join(".cache").join("huggingface").join("hub");
    write_test_semantic_cache(&home_cache)?;

    let env = SemanticCacheEnv {
        current_dir: Some(temp.path().join("repo")),
        home: Some(home),
        ..SemanticCacheEnv::default()
    };

    assert_eq!(
        semantic_worker_cache_dir_from_env(&data_root, &env),
        home_cache
    );
    Ok(())
}
