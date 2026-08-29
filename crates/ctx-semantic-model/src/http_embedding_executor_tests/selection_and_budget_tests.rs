use super::*;

#[test]
fn http_500_exhaustion_remains_retryable_on_the_same_executor() {
    let _environment = AuthEnvGuard::unset();
    let server = FakeServer::start(with_successful_contract(vec![
        Box::new(|_: &RecordedRequest| http(500, Vec::new())),
        Box::new(|_: &RecordedRequest| http(500, Vec::new())),
        embedding_reply(vec![unit_embedding(12)]),
    ]));
    let executor = HttpSemanticEmbeddingExecutor::build(&server.base_url).unwrap();

    let first = executor
        .embed_query(
            executor
                .contract()
                .prepare_query("server failure first attempt".to_owned()),
        )
        .unwrap_err();
    assert!(!semantic_embedding_failure_is_permanent(&first));
    assert!(executor.contract_verified());
    assert_eq!(
        executor
            .embed_query(
                executor
                    .contract()
                    .prepare_query("server failure recovery".to_owned()),
            )
            .unwrap(),
        unit_embedding(12)
    );
    assert_eq!(server.finish().len(), 6);
}

#[test]
fn one_deadline_bounds_handshake_canary_retries_and_all_document_batches() {
    assert!(EXECUTION_BUDGET <= Duration::from_secs(25));
    assert!(EXECUTION_BUDGET < Duration::from_secs(30));

    let _environment = AuthEnvGuard::unset();
    let executor = HttpSemanticEmbeddingExecutor::build("http://127.0.0.1:9").unwrap();
    let inputs = vec!["query: deadline probe".to_owned()];
    let started = Instant::now();
    let error = executor
        .embed(
            InputKind::Query,
            &inputs,
            Instant::now() + Duration::from_millis(40),
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("aggregate time budget"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn executors_and_selection_handle_are_send_sync_and_builtin_remains_default() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<super::super::HttpSemanticEmbeddingExecutor>();
    assert_send_sync::<SemanticEmbeddingExecutorHandle>();

    let runtime = SharedSemanticRuntime::default();
    let handle = SemanticEmbeddingExecutorHandle::build(
        SemanticEmbeddingExecutorConfig::builtin(),
        runtime,
        model_config(),
    )
    .unwrap();
    assert_eq!(handle.kind(), SemanticEmbeddingExecutorKind::Builtin);
    assert!(handle.is_builtin());
    assert_eq!(handle.endpoint(), None);
    assert!(handle.builtin_executor().is_some());
    assert!(handle.http_executor().is_none());
    assert!(!handle
        .builtin_executor()
        .unwrap()
        .shared_runtime()
        .is_loaded());
    assert_eq!(handle.executor().contract(), semantic_model_contract());

    let _environment = AuthEnvGuard::unset();
    let config = SemanticEmbeddingExecutorConfig::http("http://127.0.0.1:9").unwrap();
    let handle = SemanticEmbeddingExecutorHandle::build(
        config,
        SharedSemanticRuntime::default(),
        model_config(),
    )
    .unwrap();
    assert_eq!(handle.kind(), SemanticEmbeddingExecutorKind::Http);
    assert!(!handle.is_builtin());
    assert_eq!(handle.endpoint(), Some("http://127.0.0.1:9/"));
    assert!(handle.builtin_executor().is_none());
    assert!(handle.http_executor().is_some());
}
