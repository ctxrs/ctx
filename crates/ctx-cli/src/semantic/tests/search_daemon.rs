use super::*;

#[cfg(all(ctx_semantic_fastembed, unix))]
fn short_test_query_socket_path() -> Result<(tempfile::TempDir, PathBuf)> {
    let socket_root = tempfile::Builder::new()
        .prefix("ctx-query-test-")
        .tempdir_in("/tmp")?;
    let socket_path = socket_root.path().join("q.sock");
    if socket_path.as_os_str().as_bytes().len() > DAEMON_QUERY_SOCKET_PATH_SAFE_BYTES {
        return Err(anyhow!("test daemon query socket path is too long"));
    }
    Ok((socket_root, socket_path))
}

#[test]
fn legacy_hybrid_route_requires_a_fresh_source_generation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    write_searchable_store(temp.path(), 1)?;
    let vector_path = semantic_vector_path(temp.path());
    let store = Store::open(database_path(temp.path().to_path_buf()))?;

    let error = search_packet_with_backend(
        &store,
        temp.path(),
        "semantic daemon scheduling fixture",
        &[],
        &ctx_history_search::PacketOptions::default(),
        SearchBackendArg::Hybrid,
        false,
        0.35,
        RefreshArg::Off,
        false,
    )
    .expect_err("legacy hybrid search must not fall back to Store rows");

    assert!(format!("{error:#}").contains("fresh source-backed Core generation"));
    assert!(!vector_path.exists());
    Ok(())
}

#[cfg(ctx_semantic_fastembed)]
#[test]
fn hybrid_search_reports_missing_daemon_query_service() -> Result<()> {
    for variant in [
        SemanticOrtModelVariant::CpuFp32,
        SemanticOrtModelVariant::AcceleratorO4Fp16,
    ] {
        let temp = tempfile::tempdir()?;
        let cache = temp.path().join("semantic-model-cache");
        let fixture_root = if variant == SemanticOrtModelVariant::AcceleratorO4Fp16 {
            cache.join(SEMANTIC_MANAGED_MODEL_CACHE_DIR)
        } else {
            cache
        };
        write_test_semantic_cache_variant(&fixture_root, variant)?;
        let docs = write_searchable_store(temp.path(), 1)?;
        let doc = docs.first().expect("searchable fixture doc");
        let source_text = semantic_source_text(&doc.text);
        let source_hash = semantic_document_hash(doc, &source_text);
        let mut vector_store = SemanticVectorStore::open(&semantic_vector_path(temp.path()))?;
        vector_store.upsert_chunk_embeddings(&[(
            test_chunk(doc.event_id, doc.seq, &source_hash),
            test_embedding(1.0, 0.0),
        )])?;
        drop(vector_store);

        let store = Store::open(database_path(temp.path().to_path_buf()))?;
        let hybrid_error = search_packet_with_backend(
            &store,
            temp.path(),
            "semantic daemon scheduling fixture",
            &[],
            &ctx_history_search::PacketOptions::default(),
            SearchBackendArg::Hybrid,
            true,
            0.35,
            RefreshArg::Off,
            false,
        )
        .expect_err("legacy hybrid search must require a source generation");
        assert!(format!("{hybrid_error:#}").contains("fresh source-backed Core generation"));

        let err = search_packet_with_backend(
            &store,
            temp.path(),
            "semantic daemon scheduling fixture",
            &[],
            &ctx_history_search::PacketOptions::default(),
            SearchBackendArg::Semantic,
            true,
            1.0,
            RefreshArg::Off,
            false,
        )
        .expect_err("legacy semantic search must require a source generation");
        assert!(format!("{err:#}").contains("fresh source-backed Core generation"));
    }
    Ok(())
}

#[cfg(all(ctx_semantic_fastembed, unix))]
#[test]
fn legacy_route_does_not_consume_a_stale_daemon_endpoint() -> Result<()> {
    let temp = tempfile::tempdir()?;
    write_test_semantic_cache(&temp.path().join("semantic-model-cache"))?;
    let docs = write_searchable_store(temp.path(), 1)?;
    let doc = docs.first().expect("searchable fixture doc");
    let source_text = semantic_source_text(&doc.text);
    let source_hash = semantic_document_hash(doc, &source_text);
    let mut vector_store = SemanticVectorStore::open(&semantic_vector_path(temp.path()))?;
    vector_store.upsert_chunk_embeddings(&[(
        test_chunk(doc.event_id, doc.seq, &source_hash),
        test_embedding(1.0, 0.0),
    )])?;
    drop(vector_store);
    let (_socket_root, private_path) = short_test_query_socket_path()?;
    write_daemon_query_endpoint(
        temp.path(),
        &DaemonQueryEndpoint::Unix {
            path: private_path.clone(),
            token: "0123456789abcdef0123456789abcdef".to_owned(),
        },
    )?;
    let store = Store::open(database_path(temp.path().to_path_buf()))?;

    let hybrid_error = search_packet_with_backend(
        &store,
        temp.path(),
        "semantic daemon scheduling fixture",
        &[],
        &ctx_history_search::PacketOptions::default(),
        SearchBackendArg::Hybrid,
        true,
        0.35,
        RefreshArg::Off,
        false,
    )
    .expect_err("legacy hybrid search must require a source generation");
    assert!(format!("{hybrid_error:#}").contains("fresh source-backed Core generation"));
    assert!(daemon_query_endpoint_path(temp.path()).exists());

    let error = search_packet_with_backend(
        &store,
        temp.path(),
        "semantic daemon scheduling fixture",
        &[],
        &ctx_history_search::PacketOptions::default(),
        SearchBackendArg::Semantic,
        true,
        1.0,
        RefreshArg::Off,
        false,
    )
    .expect_err("legacy semantic search must require a source generation");
    let message = format!("{error:#}");
    assert!(message.contains("fresh source-backed Core generation"));
    assert!(!message.contains(&private_path.display().to_string()));
    assert!(!message.contains("Connection refused"));
    Ok(())
}

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
