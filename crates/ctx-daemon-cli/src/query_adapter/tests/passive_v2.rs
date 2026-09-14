use super::*;

#[test]
fn ready_nonempty_http_uses_the_configured_authenticated_executor_without_mutation() -> Result<()> {
    let temp = semantic_tempdir()?;
    let (index, event_id) = semantic_index(temp.path())?;
    let server = LoopbackEmbeddingServer::start();
    let config = external_executor_config(&server.base_url);
    let contract = semantic_index_contract_for_selected(config.contract())?;
    drop(reconciled_ready_nonempty_store_with_contract(
        &index,
        temp.path(),
        &contract,
    )?);
    let before =
        durable_query_state_snapshot(temp.path(), &temp.path().join("semantic-model-cache"))?;
    let _environment = SemanticEnvironmentGuard::http_auth(&server.base_url);
    let adapter = SemanticQueryAdapter::foreground_read_only(temp.path(), config);

    let mut session = adapter
        .begin_query(&index)
        .map_err(|error| anyhow!(error.to_string()))?;
    session.prepare_alternative("exact selected HTTP executor")?;
    let batch = session.candidates(&default_compiled_filter(), 10)?;
    assert_eq!(batch.candidates.len(), 1);
    assert_eq!(batch.candidates[0].event.event_id, event_id);
    assert_eq!(
        durable_query_state_snapshot(temp.path(), &temp.path().join("semantic-model-cache"))?,
        before
    );

    let requests = server.finish();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].path, "/semantic-base/v2/contract");
    assert!(requests.iter().all(|request| {
        request.header("authorization") == Some("Bearer passive-semantic-test-token")
    }));
    assert_eq!(requests[1].body["input_kind"], "query");
    assert_eq!(requests[1].body["space_id"], "test-space");
    assert_eq!(requests[1].body["dimensions"], 7);
    assert_eq!(
        requests[1].body["inputs"][0]["text"],
        "exact selected HTTP executor"
    );
    Ok(())
}

#[test]
fn http_contract_drift_is_typed_nonretryable_and_does_not_mutate() -> Result<()> {
    let temp = semantic_tempdir()?;
    let (index, _) = semantic_index(temp.path())?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let endpoint = format!("http://{}/semantic-base", listener.local_addr()?);
    let config = external_executor_config(&endpoint);
    let contract = semantic_index_contract_for_selected(config.contract())?;
    drop(reconciled_ready_nonempty_store_with_contract(
        &index,
        temp.path(),
        &contract,
    )?);
    let before =
        durable_query_state_snapshot(temp.path(), &temp.path().join("semantic-model-cache"))?;
    let _environment = SemanticEnvironmentGuard::http_auth(&endpoint);
    let adapter = SemanticQueryAdapter::foreground_read_only(temp.path(), config);
    let mut session = adapter
        .begin_query(&index)
        .map_err(|error| anyhow!(error.to_string()))?;
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept contract verification");
        let request = read_http_request(&mut stream);
        write_http_json(
            &mut stream,
            &compact_json(json!({
                "schema_version": 2,
                "space_id": "drifted-test-space",
                "dimensions": 7,
            })),
        );
        request
    });

    let error = session
        .prepare_alternative("contract drift")
        .expect_err("a drifted endpoint contract must fail closed");
    assert!(matches!(
        error,
        SemanticQueryError::NotReady {
            code: "semantic_executor_unavailable",
            retryable: false,
            ..
        }
    ));
    let request = server.join().expect("join drift server");
    assert_eq!(request.method, "GET");
    assert_eq!(request.path, "/semantic-base/v2/contract");
    assert_eq!(
        durable_query_state_snapshot(temp.path(), &temp.path().join("semantic-model-cache"))?,
        before
    );
    Ok(())
}

#[test]
fn stale_projection_preflight_makes_no_http_request_or_mutation() -> Result<()> {
    let temp = semantic_tempdir()?;
    let (old_index, _) = semantic_index_revision(temp.path(), 1, true)?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let endpoint = format!("http://{}/semantic-base", listener.local_addr()?);
    let config = external_executor_config(&endpoint);
    let contract = semantic_index_contract_for_selected(config.contract())?;
    drop(reconciled_ready_nonempty_store_with_contract(
        &old_index,
        temp.path(),
        &contract,
    )?);
    let (index, _) = semantic_index_revision(temp.path(), 2, true)?;
    let before =
        durable_query_state_snapshot(temp.path(), &temp.path().join("semantic-model-cache"))?;
    let adapter = SemanticQueryAdapter::foreground_read_only(temp.path(), config);

    let error = match adapter.begin_query(&index) {
        Ok(_) => panic!("a stale exact-generation projection must fail preflight"),
        Err(error) => error,
    };
    assert!(matches!(
        error.reason(),
        Some(SemanticReason::GenerationNotAcknowledged | SemanticReason::ProjectionEventMismatch)
    ));
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "stale preflight must not contact the configured HTTP endpoint"
    );
    assert_eq!(
        durable_query_state_snapshot(temp.path(), &temp.path().join("semantic-model-cache"))?,
        before
    );
    Ok(())
}

#[test]
fn mismatched_external_space_makes_no_http_request_or_mutation() -> Result<()> {
    let temp = semantic_tempdir()?;
    let (index, _) = semantic_index(temp.path())?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let endpoint = format!("http://{}/semantic-base", listener.local_addr()?);
    let ready_config = external_executor_config(&endpoint);
    let ready_contract = semantic_index_contract_for_selected(ready_config.contract())?;
    drop(reconciled_ready_nonempty_store_with_contract(
        &index,
        temp.path(),
        &ready_contract,
    )?);
    let selected = SemanticEmbeddingExecutorConfig::http(
        &endpoint,
        crate::ExternalSemanticSpace::new("different-test-space", 7)?,
    )?;
    let before =
        durable_query_state_snapshot(temp.path(), &temp.path().join("semantic-model-cache"))?;
    let adapter = SemanticQueryAdapter::foreground_read_only(temp.path(), selected);

    let error = match adapter.begin_query(&index) {
        Ok(_) => panic!("a mismatched accepted vector space must fail passive preflight"),
        Err(error) => error,
    };
    assert_eq!(
        error.reason(),
        Some(SemanticReason::GenerationNotAcknowledged)
    );
    let SemanticQueryExecution::Foreground { executor, .. } = &adapter.execution else {
        unreachable!("passive mismatch test selected daemon execution")
    };
    assert!(!executor.is_resolved());
    assert!(matches!(
        listener.accept(),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
    ));
    assert_eq!(
        durable_query_state_snapshot(temp.path(), &temp.path().join("semantic-model-cache"))?,
        before
    );
    Ok(())
}

#[test]
fn ready_empty_ignores_an_unusable_external_executor() -> Result<()> {
    let temp = semantic_tempdir()?;
    let (index, _) = semantic_index_revision(temp.path(), 1, false)?;
    let config = external_executor_config("https://embedding.invalid/ctx");
    let contract = semantic_index_contract_for_selected(config.contract())?;
    let mut store =
        SemanticVectorStore::open(&source_backed_semantic_vector_path(temp.path()), &contract)?;
    acknowledge_empty_generation(&mut store, &index)?;
    drop(store);
    let adapter = SemanticQueryAdapter::foreground_read_only(temp.path(), config);

    let mut session = adapter
        .begin_query(&index)
        .map_err(|error| anyhow!(error.to_string()))?;
    assert_eq!(
        session.prepare_alternative("ready empty ignores executor")?,
        compact_json(json!({"query_embed_ms": null}))
    );
    let SemanticQueryExecution::Foreground { executor, .. } = &adapter.execution else {
        unreachable!("ready empty test selected foreground execution")
    };
    assert!(!executor.is_resolved());
    Ok(())
}
